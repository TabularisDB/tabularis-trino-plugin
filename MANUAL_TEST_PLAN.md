# Manual Trino plugin check

## Verification record

**Status: Passed, user-reported on 2026-10-14.** The user confirmed completing
all manual steps below, including query cancellation and the optional direct
JSON-RPC offset check. Tabularis version, OS, and Trino image tag were not
provided, so this record does not claim coverage for other environments.

Use this checklist to verify the external Trino plugin in the Tabularis app
against the repository's local Docker fixture. The fixture exposes Trino's
container port `8080` on host port `5432`.

## 1. Build and install the plugin

From the plugin repository root, run:

```sh
just dev-install
```

Wait for the build/install command to finish, then restart Tabularis (or toggle
the Trino plugin off and on in Settings). Open a new connection and choose
**Trino** as the driver. The connection dialog should show the Trino driver and
the standard General fields, as in the supplied screenshot.

If Trino is not in the driver list, check that the binary and `.tabularium`
manifest were copied to the plugin directory printed by `just dev-install`,
then restart the app and check its plugin load logs.

## 2. Start and seed Trino

In a terminal at the plugin repository root:

```sh
docker compose -f fixture/docker-compose.yml up -d
docker exec trino-fixture trino --execute "SELECT 1"
./fixture/seed.sh
```

The `SELECT 1` command should print `1`. Seeding should complete without a
Trino error. It creates `memory.demo.types_test`, `memory.demo.many_rows`, the
view `memory.demo.v_types`, and makes the TPCH `nation` table available in
`tpch.sf1`.

## 3. Enter the connection details shown in the app

In the Trino connection dialog's **General** tab, enter:

| Tabularis field | Value for this fixture |
|---|---|
| Connection string | Leave blank |
| Host | `127.0.0.1` |
| Port | `5432` |
| Username | `tester` |
| Password | Leave blank |
| Database name | `memory` |
| Browse multiple databases on this connection | Enable it for the multi-catalog browse/query checks below |
| SSL / SSL mode, if shown under Advanced | Disabled for this local HTTP fixture |

Leave the connection string blank so the Host and Port fields are used. Do not
enter a password for this HTTP fixture: this plugin intentionally rejects
password authentication over plain HTTP. The UI may retain the previous
connection's password, so clear it before testing.

Click **Test Connection**. Expected result: the app reports success. Click
**Save**, then open the connection. If there is a **Load Databases** action,
use it after enabling multiple database browsing; `memory` and `tpch` should
be available. Keep `memory` as the connection's default database/catalog.

## 4. Check metadata in the sidebar

Browse the saved Trino connection and confirm:

1. Catalogs/databases `memory` and `tpch` are visible when multiple-database
   browsing is enabled (the `memory` default alone is enough for later query
   checks if the app does not expose catalog browsing in this view).
2. Under `memory`, schema `demo` is listed.
3. Under `memory.demo`, tables `many_rows` and `types_test` are listed, and
   view `v_types` appears in the Views section.
4. Expand `types_test`. Its columns should include `dec_col`, `big_col`,
   `arr_col`, `map_col`, `row_col`, and `null_col`, with types reported by
   Trino.
5. Open or inspect `v_types`; its definition should select `dec_col` and
   `big_col` from `memory.demo.types_test`.

The visible objects are limited by the Trino user's permissions. A missing
object should be investigated as a fixture, permission, or metadata error.

## 5. Check editor and notebook queries

Run each query from the SQL editor, then repeat in a SQL notebook if that
feature is available in the installed Tabularis build.

**Nested and special values:**

```sql
SELECT dec_col, big_col, arr_col, map_col, row_col, null_col
FROM memory.demo.types_test
```

Expected: one row. Check the exact decimal `1234567890.123456789`, the large
integer `9223372036854775807`, the decimal values nested in the array and map,
the large integer nested in the row, and SQL `NULL`. The plugin may return exact
decimal and unsafe large integer values as strings; confirm the displayed text
retains every digit and that nested values remain present.

**Ordered results and paging:**

```sql
SELECT n FROM memory.demo.many_rows ORDER BY n
```

Expected: 100,000 rows in ascending order. With a grid page size of 100, page
1 starts at 1, page 2 starts at 101, and the last row is 100000. Move across
several pages and check there are no gaps, duplicates, or out-of-order rows.

**Federated query across catalogs:**

```sql
SELECT m.n, n.name
FROM memory.demo.many_rows AS m
JOIN tpch.sf1.nation AS n ON m.n = n.nationkey
ORDER BY m.n
```

Expected: 24 rows, ordered by nation key 1 through 24; TPCH nation key 0 has
no match because `many_rows.n` starts at 1. Names should come from the TPCH
catalog while `m.n` comes from the memory catalog.

### Query diagnostics

Run a successful statement through the plugin's JSON-RPC interface:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":21,"method":"execute_query","params":{"params":{"driver":"trino","host":"127.0.0.1","port":5432,"database":"memory","username":"tester","password":null,"ssl_mode":"disable"},"schema":"demo","query":"SELECT count(*) FROM many_rows"}}' | cargo run --quiet --bin trino-plugin
```

Expected: the response includes `result.query_info` with a Trino `query_id`,
final `state` (`FINISHED`), `observed_states`, `elapsed_time_ms`, and a
`trino_stats` object when Trino supplies statistics. The observed state list
may skip `QUEUED` or `RUNNING` for a fast local query. This plugin returns
diagnostics with the completed result; live progress updates in the Tabularis
UI require host support for progress notifications.

To check error diagnostics, replace the SQL with an intentionally invalid
statement such as `SELECT FROM many_rows`. Expected: the JSON-RPC error message
contains Trino's query ID and error name, plus its error code and line/column
when present. Repeat the invalid query in the Tabularis editor and confirm the
app surfaces the actionable Trino message.

## 6. Check connection errors

Edit the connection and set the port to an unused local port, for example
`5433`. Click **Test Connection**. Expected: the app shows a connection
failure with a useful message; it must not report success. Restore port `5432`
afterwards.

## 7. Check query cancellation

**Status: ☑ Passed in the user's live manual run.** The mock protocol tests
also exercise cancellation independently.

Tabularis sends cancellation when a plugin call exceeds the host's configured
call timeout. This check requires a Tabularis version that supports the `cancel`
plugin notification (v0.26.0 or newer).

1. Keep the Trino connection and fixture running.
2. In the SQL editor, run this deliberately long aggregate:

   ```sql
   SELECT count(*)
   FROM memory.demo.many_rows AS a
   CROSS JOIN memory.demo.many_rows AS b
   ```

3. Let it continue until the host call timeout is reached. The editor should
   report that the request timed out/cancelled.
4. In another Trino client, check whether that statement is still running:

   ```sql
   SELECT query_id, state, query
   FROM system.runtime.queries
   WHERE query LIKE '%many_rows%'
   ```

   Expected: the cancelled aggregate is no longer listed as queued or running.
   If it is still present, cancellation did not reach Trino.
   If it remains active, stop it from the Trino CLI using its reported query
   ID so the failed manual check does not leave work running:

   ```sql
   CALL system.runtime.kill_query('<query_id>', 'manual cancellation check cleanup')
   ```

5. Run `SELECT 1` in Tabularis. It should succeed, confirming the plugin
   process is still responsive after the cancellation notification.

## 8. Optional direct JSON-RPC offset check

This checks the plugin's explicit `offset` support independently of the grid.
Run from the repository root while the fixture is up:

```sh
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"execute_query","params":{"params":{"driver":"trino","host":"127.0.0.1","port":5432,"database":"memory","username":"tester","password":null,"ssl_mode":null},"schema":"demo","query":"SELECT n FROM many_rows ORDER BY n","limit":5,"offset":10}}' | cargo run --quiet --bin trino-plugin
```

Expected: a JSON-RPC success response with rows `n = 11` through `n = 15`,
`total_rows` equal to `100000`, and `has_more` true.

## 9. Stop the fixture

```sh
docker compose -f fixture/docker-compose.yml down
```

## 10. HTTPS and Basic authentication verification

The repository includes a Trino 483 fixture configured for HTTPS and HTTP
Basic authentication. Its local CA and server certificate must be generated
before startup. Do not use production credentials in screenshots, logs, or the
verification record.

1. Stop the regular HTTP fixture if it is running because both fixtures use
   host port `5432`:

   ```sh
   docker compose -f fixture/docker-compose.yml down
   ```

2. Generate a local CA and a separate `localhost` server certificate:

   ```sh
   ./fixture/https/generate-certs.sh
   ```

3. Create a test password with a bcrypt cost accepted by Trino. `htpasswd`'s
   default cost is too low, so specify cost 10:

   ```sh
   htpasswd -B -C 10 -c fixture/https/password.db tester
   ```

   Enter a throwaway password when prompted. Use the same password in the
   Tabularis connection form.

4. Start the HTTPS fixture and check its logs:

   ```sh
   docker compose -f fixture/https/docker-compose.yaml up -d
   docker compose -f fixture/https/docker-compose.yaml ps
   docker logs trino-https --tail 80
   ```

5. In Tabularis, create a Trino connection with these values:

   | Field | Value |
   |---|---|
   | Host | `localhost` |
   | Port | `5432` |
   | Username | `tester` |
   | Password | The throwaway password entered in step 3 |
   | Database name | `memory` |
   | SSL Mode | **Verify Identity** |
   | CA certificate | `fixture/https/certs/ca.crt` (use its absolute path if the form requires a path) |
   | Client certificate and key | Leave both empty |

   Leave the connection string blank. `localhost` matches the server
   certificate's subject alternative name.

6. Click **Test Connection**. Expected: it succeeds. Save the connection and
   run `SELECT 1` in the SQL editor. Expected: one row with value `1`.
7. Edit the connection, enter an incorrect password, and test again. Expected:
   it fails with an actionable authentication error and does not expose the
   password.
8. As a security check, configure a password while using plain HTTP. Expected:
   the plugin rejects the configuration before sending credentials.

**Status: ☑ Passed (user-confirmed on 2026-10-14).** The user confirmed the
HTTPS Basic connection/query check, invalid-password handling, and rejection of
password authentication over plain HTTP in Tabularis. The Tabularis version and
OS were not recorded.

- Tabularis version:
- Trino version: `trinodb/trino:483`
- OS:
- Authentication checks passed: HTTPS Basic; plain HTTP password rejection.
- Failure/error behavior: Invalid password rejected; plain HTTP password rejected.
- Tester/date: User-confirmed, 2026-10-14.

## Record the result

For each check, record pass/fail and include the Tabularis version, OS, Trino
image tag (or server version), and any non-secret error text. This is a manual
procedure; passing the mock protocol tests does not replace running these
checks against the app and live fixture. Step 10's live HTTPS/Basic
authentication check is user-confirmed as passed for v0.1.3.
