#!/usr/bin/env bash
# End-to-end checks for the Trino plugin against the local Docker fixture.
#
# Covers fixture seeding, plugin JSON-RPC connection and metadata methods,
# complete result paging, typed values, federated queries, offset paging,
# connection errors, unsupported methods, and plugin-to-Trino cancellation.
# Tabularis UI checks (sidebar, editor, notebooks, screenshots) and live
# HTTPS/Basic authentication still require MANUAL_TEST_PLAN.md.
#
# Requirements: Docker Compose, curl, Python 3, and the Rust toolchain.
# Usage: ./scripts/e2e-fixture-test.sh
# Optional: SKIP_CANCEL=1 skips the cancellation test.

set -u
set -o pipefail
cd "$(dirname "$0")/.." || exit 1

CONTAINER="${CONTAINER:-trino-fixture}"
HOST="${TRINO_HOST:-127.0.0.1}"
PORT="${TRINO_PORT:-5432}"
BASE="http://${HOST}:${PORT}"
USER_NAME="${TRINO_USER:-tester}"
PLUGIN_BIN="${PLUGIN_BIN:-target/debug/trino-plugin}"
CANCEL_DELAY_SECONDS="${CANCEL_DELAY_SECONDS:-1}"

PASS=0
FAIL=0
SKIP=0

pass() { echo "  PASS  $1"; PASS=$((PASS + 1)); }
fail() {
  echo "  FAIL  $1"
  if [ -n "${2:-}" ]; then echo "        $2"; fi
  FAIL=$((FAIL + 1))
}
skip() { echo "  SKIP  $1"; SKIP=$((SKIP + 1)); }

for tool in docker curl python3 cargo; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "Required command not found: $tool" >&2
    exit 2
  fi
done

# Run SQL through the fixture's Trino CLI; CSV_UNQUOTED keeps assertions simple.
q() { docker exec "$CONTAINER" trino --output-format CSV_UNQUOTED --execute "$1" 2>&1; }

check() {
  local label="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    pass "$label"
  else
    fail "$label" "expected [$expected], got [$actual]"
  fi
}

# Extra JSON fields become top-level request params; _connection overrides let
# a test supply an invalid connection value without changing the defaults.
rpc_call() {
  local method="$1" extra="${2:-}" port="${3:-$PORT}" request
  [ -n "$extra" ] || extra='{}'
  request="$(python3 - "$method" "$HOST" "$port" "$USER_NAME" "$extra" <<'PY'
import json, sys
method, host, port, username, extra = sys.argv[1:]
connection = {
    "driver": "trino", "host": host, "port": int(port),
    "database": "memory", "username": username,
    "password": None, "ssl_mode": None,
}
params = json.loads(extra)
connection.update(params.pop("_connection", {}))
params["params"] = connection
print(json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
PY
)" || return 1
  printf '%s\n' "$request" | "$PLUGIN_BIN"
}

check_rpc_contains() {
  local label="$1" method="$2" extra="$3" needle="$4" response
  response="$(rpc_call "$method" "$extra")"
  if printf '%s\n' "$response" | python3 -c '
import json, sys
try:
    response = json.load(sys.stdin)
    ok = "error" not in response and sys.argv[1] in json.dumps(response.get("result"), ensure_ascii=False)
except Exception:
    ok = False
sys.exit(0 if ok else 1)
' "$needle"; then
    pass "$label"
  else
    fail "$label" "$response"
  fi
}

check_rpc_error() {
  local label="$1" method="$2" extra="$3" expected_code="$4" response
  response="$(rpc_call "$method" "$extra")"
  if printf '%s\n' "$response" | python3 -c '
import json, sys
try:
    response = json.load(sys.stdin)
    ok = response.get("error", {}).get("code") == int(sys.argv[1])
except Exception:
    ok = False
sys.exit(0 if ok else 1)
' "$expected_code"; then
    pass "$label"
  else
    fail "$label" "$response"
  fi
}

echo "== 1. Fixture startup, readiness, and seed data =="
if ! docker compose -f fixture/docker-compose.yml up -d >/dev/null; then
  echo "Could not start the Docker fixture." >&2
  exit 1
fi
ready=""
for _ in $(seq 1 60); do
  if curl -fsS "$BASE/v1/info" 2>/dev/null | grep -q '"starting" *: *false'; then
    ready=1
    break
  fi
  sleep 2
done
if [ -z "$ready" ]; then
  fail "Trino ready at $BASE"
  echo "Cannot continue; check Docker logs with: docker logs $CONTAINER" >&2
  exit 1
fi
pass "Trino ready at $BASE"
if seed_output="$(./fixture/seed.sh 2>&1)"; then
  pass "fixture seed script ran"
else
  fail "fixture seed script ran" "$seed_output"
  exit 1
fi
if ! cargo build --quiet --bin trino-plugin; then
  fail "plugin binary builds"
  exit 1
fi
if [ ! -x "$PLUGIN_BIN" ]; then
  fail "plugin executable exists" "$PLUGIN_BIN"
  exit 1
fi

echo "== 2. Plugin connection and metadata JSON-RPC =="
check_rpc_contains "connection test succeeds" "test_connection" '{}' '"success": true'
check_rpc_contains "memory catalog is discoverable" "get_databases" '{}' 'memory'
check_rpc_contains "tpch catalog is discoverable" "get_databases" '{}' 'tpch'
check_rpc_contains "demo schema is discoverable" "get_schemas" '{}' 'demo'
check_rpc_contains "sf1 schema is discoverable in tpch" "get_schemas" '{"_connection":{"database":"tpch"}}' 'sf1'
check_rpc_contains "many_rows table is discoverable" "get_tables" '{"schema":"demo"}' 'many_rows'
check_rpc_contains "types_test table is discoverable" "get_tables" '{"schema":"demo"}' 'types_test'
check_rpc_contains "nation table is discoverable in tpch.sf1" "get_tables" '{"_connection":{"database":"tpch"},"schema":"sf1"}' 'nation'
check_rpc_contains "table columns are discoverable" "get_columns" '{"schema":"demo","table":"types_test"}' 'dec_col'
check_rpc_contains "DECIMAL column type is reported" "get_columns" '{"schema":"demo","table":"types_test"}' 'decimal'
check_rpc_contains "tpch nation columns are discoverable" "get_columns" '{"_connection":{"database":"tpch"},"schema":"sf1","table":"nation"}' 'nationkey'
check_rpc_contains "v_types view is discoverable" "get_views" '{"schema":"demo"}' 'v_types'
check_rpc_contains "view definition references its source table" "get_view_definition" '{"schema":"demo","view":"v_types"}' 'memory.demo.types_test'
check_rpc_contains "view columns are discoverable" "get_view_columns" '{"schema":"demo","view":"v_types"}' 'big_col'
if python3 -c '
import json
with open(".tabularium", encoding="utf-8") as f:
    capabilities = json.load(f)["capabilities"]
assert capabilities["readonly"] is True
assert capabilities["manage_tables"] is False
'; then
  pass "manifest declares the driver read-only and disables table management"
else
  fail "manifest hides unsupported editing capabilities"
fi

echo "== 3. Typed values through plugin JSON-RPC =="
typed="$(rpc_call execute_query '{"schema":"demo","query":"SELECT dec_col, big_col, arr_col, map_col, row_col, null_col FROM memory.demo.types_test"}')"
if printf '%s\n' "$typed" | python3 -c '
import json, sys
try:
    response = json.load(sys.stdin)
    row = response["result"]["rows"][0]
    expected = [
        "1234567890.123456789", "9223372036854775807",
        ["1.123456789", "2.987654321"],
        {"a": "1.123456789", "b": "2.987654321"},
        ["9223372036854775807", "x"], None,
    ]
    ok = "error" not in response and row == expected
except Exception:
    ok = False
sys.exit(0 if ok else 1)
'; then
  pass "NULL, DECIMAL, large integers, ARRAY, MAP, and ROW values are preserved"
else
  fail "typed values preserve exact and nested values" "$typed"
fi

echo "== 4. Full response paging, ordering, and explicit offset =="
all_rows="$(rpc_call execute_query '{"schema":"demo","query":"SELECT n FROM many_rows ORDER BY n"}')"
if printf '%s\n' "$all_rows" | python3 -c '
import json, sys
try:
    response = json.load(sys.stdin)
    rows = response["result"]["rows"]
    ok = "error" not in response and len(rows) == 100000
    ok = ok and rows[0] == [1] and rows[-1] == [100000]
    ok = ok and all(row == [i] for i, row in enumerate(rows, start=1))
except Exception:
    ok = False
sys.exit(0 if ok else 1)
'; then
  pass "all 100,000 rows returned once in ascending order"
else
  fail "complete ordered response paging" "plugin response was incomplete or out of order"
fi

page="$(rpc_call execute_query '{"schema":"demo","query":"SELECT n FROM many_rows ORDER BY n","limit":5,"offset":10}')"
if printf '%s\n' "$page" | python3 -c '
import json, sys
try:
    result = json.load(sys.stdin)["result"]
    ok = result["rows"] == [[11], [12], [13], [14], [15]]
    ok = ok and result["pagination"]["total_rows"] == 100000
    ok = ok and result["pagination"]["has_more"] is True
except Exception:
    ok = False
sys.exit(0 if ok else 1)
'; then
  pass "offset returns rows 11–15 with correct pagination metadata"
else
  fail "explicit offset paging" "$page"
fi

echo "== 5. Federated query through plugin JSON-RPC =="
federated="$(rpc_call execute_query '{"schema":"demo","query":"SELECT m.n, n.name FROM memory.demo.many_rows m JOIN tpch.sf1.nation n ON m.n = n.nationkey ORDER BY m.n"}')"
if printf '%s\n' "$federated" | python3 -c '
import json, sys
try:
    rows = json.load(sys.stdin)["result"]["rows"]
    ok = len(rows) == 24 and rows[0] == [1, "ARGENTINA"]
    ok = ok and rows[-1] == [24, "UNITED STATES"]
except Exception:
    ok = False
sys.exit(0 if ok else 1)
'; then
  pass "federated join returns all 24 ordered matches"
else
  fail "federated query across memory and tpch" "$federated"
fi

echo "== 6. Actionable errors and unsupported-method responses =="
bad_connection="$(rpc_call test_connection '{}' 5433)"
if printf '%s\n' "$bad_connection" | python3 -c '
import json, sys
try:
    error = json.load(sys.stdin)["error"]
    ok = error["code"] != 0 and bool(error["message"])
except Exception:
    ok = False
sys.exit(0 if ok else 1)
'; then
  pass "unreachable coordinator returns an actionable error"
else
  fail "unreachable coordinator error" "$bad_connection"
fi

plain_http_password="$(rpc_call test_connection '{"_connection":{"password":"not-a-real-password"}}')"
if printf '%s\n' "$plain_http_password" | python3 -c '
import json, sys
try:
    message = json.load(sys.stdin)["error"]["message"]
    ok = "requires HTTPS" in message
except Exception:
    ok = False
sys.exit(0 if ok else 1)
'; then
  pass "password authentication is rejected over plain HTTP"
else
  fail "plain HTTP password rejection" "$plain_http_password"
fi

for method in explain_query insert_record update_record delete_record \
  get_create_table_sql get_add_column_sql get_alter_column_sql \
  get_create_index_sql get_create_foreign_key_sql drop_index drop_foreign_key; do
  check_rpc_error "$method returns unsupported-method response" "$method" '{}' '-32601'
done

echo "== 7. Cancellation notification through plugin JSON-RPC =="
if [ -n "${SKIP_CANCEL:-}" ]; then
  skip "plugin cancellation (SKIP_CANCEL set)"
else
  cancel_result="$(python3 - "$PLUGIN_BIN" "$HOST" "$PORT" "$USER_NAME" "$CANCEL_DELAY_SECONDS" <<'PY'
import json, select, subprocess, sys, time

binary, host, port, username, delay = sys.argv[1:]
request_id = 90210
sql = (
    "/* trino_plugin_e2e_cancel_marker */ "
    # Do enough per-row work to keep Trino executing until the cancel arrives.
    # A plain count over this cross join can be optimized and complete before
    # a timer-based cancellation notification is delivered.
    "SELECT sum(length(to_hex(sha256(to_utf8(CAST(a.n * b.n AS varchar)))))) "
    "FROM memory.demo.many_rows a CROSS JOIN memory.demo.many_rows b"
)
params = {
    "params": {
        "driver": "trino", "host": host, "port": int(port),
        "database": "memory", "username": username,
        "password": None, "ssl_mode": None,
    },
    "schema": "demo", "query": sql,
}
proc = subprocess.Popen(
    [binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL, text=True, bufsize=1,
)
try:
    proc.stdin.write(json.dumps({
        "jsonrpc": "2.0", "id": request_id,
        "method": "execute_query", "params": params,
    }) + "\n")
    proc.stdin.flush()
    time.sleep(float(delay))
    proc.stdin.write(json.dumps({
        "jsonrpc": "2.0", "method": "cancel",
        "params": {"id": request_id},
    }) + "\n")
    proc.stdin.flush()
    ready, _, _ = select.select([proc.stdout], [], [], 45)
    if not ready:
        raise RuntimeError("no plugin response within 45 seconds after cancellation")
    response = json.loads(proc.stdout.readline())
    error = response.get("error", {})
    if response.get("id") != request_id or "query cancelled" not in error.get("message", ""):
        raise RuntimeError("expected cancellation response; got " + json.dumps(response))
    print("PASS")
except Exception as error:
    print(str(error))
    proc.kill()
    raise SystemExit(1)
finally:
    if proc.stdin and not proc.stdin.closed:
        proc.stdin.close()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()
PY
  )"
  if [ "$cancel_result" = "PASS" ]; then
    pass "cancel notification returns the plugin's query-cancelled response"
    active="$(q "SELECT count(*) FROM system.runtime.queries WHERE query LIKE '%trino_plugin_e2e_cancel_marker%' AND query NOT LIKE '%FROM system.runtime.queries%' AND state IN ('QUEUED','PLANNING','STARTING','RUNNING')")"
    check "cancelled query is no longer active in Trino" "0" "$active"
  else
    fail "plugin cancellation reaches Trino" "$cancel_result"
    active_ids="$(q "SELECT query_id FROM system.runtime.queries WHERE query LIKE '%trino_plugin_e2e_cancel_marker%' AND query NOT LIKE '%FROM system.runtime.queries%' AND state IN ('QUEUED','PLANNING','STARTING','RUNNING')")"
    while IFS= read -r query_id; do
      [ -n "$query_id" ] && q "CALL system.runtime.kill_query('$query_id', 'e2e cleanup')" >/dev/null 2>&1 || true
    done <<< "$active_ids"
  fi
fi

echo
echo "Result: $PASS passed, $FAIL failed, $SKIP skipped"
echo "Manual app checks remain: sidebar, editor, notebook, screenshots, and live HTTPS/Basic authentication."
echo "The fixture stays running for UI checks. Stop it with: docker compose -f fixture/docker-compose.yml down"
[ "$FAIL" -eq 0 ]
