# Trino driver v1 implementation plan

## Scope

The first release focuses on connecting to Trino, browsing permitted metadata,
and running SQL from the Tabularis editor and notebooks.

Included:

- Test a connection and report actionable connection or permission errors.
- List catalogs as Tabularis databases, then list schemas, tables, columns, and
  views using the current connection's catalog and schema context.
- Execute SQL and return complete, ordered results across every Trino response
  page, including responses that contain no rows.
- Convert NULL, DECIMAL, large integer, ARRAY, MAP, and ROW values without
  silently losing precision.
- Cancel a running query through the Trino client protocol.
- Ship a reproducible local fixture and a manual test plan.

Deferred until their methods are implemented and tested:

- Grid row editing and CRUD.
- DDL actions.
- Visual EXPLAIN.
- Presto compatibility (evaluate separately; do not assume Trino compatibility
  implies Presto compatibility).

## Initial connection contract

- Target Trino version: `483`, matching `fixture/docker-compose.yml`.
- Coordinator: host and port. The manifest defaults to port `8080`, the usual
  Trino HTTP port. The local fixture maps host port `5432` to `8080`, so set
  `5432` when connecting to it.
- Context: catalog and optional schema are scoped to the connection/request.
- Authentication: support username/password using HTTP Basic authentication
  over HTTPS. Permit unauthenticated HTTP for the local fixture only. Support
  the host's optional CA and client certificate/key fields.
- Catalogs and schemas are discovered from Trino and remain limited by the
  connected user's permissions.

Authentication and TLS behavior were manually checked in Tabularis for v0.1.3:
HTTPS Basic authentication succeeds, invalid credentials produce an error, and
password authentication over plain HTTP is rejected. See
[`MANUAL_TEST_PLAN.md`](MANUAL_TEST_PLAN.md). The user reported completing all
documented manual checks on 2026-10-14; Tabularis version and OS were not
recorded.

## Implementation progress

| Step | Work | Status |
|---|---|---|
| 1 | Manifest defaults and declared capabilities | Implemented; manifest hides unsupported connection-string input and enables SSL controls. Defaults to port `8080`; the local fixture uses host port `5432`, set per connection. |
| 2 | Trino REST client, paging, errors, and cancellation | Implemented with protocol tests. |
| 3 | Connection check and metadata discovery | Implemented with protocol tests. |
| 4 | Query paging and exact typed-value conversion | Implemented with conversion tests. |
| 5 | Protocol tests, seeded multi-catalog fixture, manual test plan | Implemented; all documented manual checks reported passed on 2026-10-14, including HTTPS Basic authentication and plain HTTP password rejection. |
| 6 | Setup/release docs and Rust quality gates | Setup and release instructions are documented. Verify GitHub release assets and registry publication on their respective external pages; their status is not tracked in this repository. |

## Original implementation order

1. Correct `.tabularium` defaults and capabilities for Trino and schema browsing.
2. Implement the REST client: submit statements, follow `nextUri`, handle
   queued/running/error responses, and cancel running queries.
3. Implement a real connection check and metadata discovery for catalogs,
   schemas, tables, columns, and views.
4. Implement query result paging and typed value conversion.
5. Add protocol and conversion tests; expand the fixture to cover a federated
   query across `memory` and `tpch` and add a manual test plan.
6. Update installation and release documentation, run the Rust quality gates,
   then prepare the plugin release/registry submission.

## Validation gates

Run before proposing the implementation for review:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Manually verify connection success/failure, permission-limited discovery,
editor and notebook queries, multi-page ordering (including an empty first
page), federated queries, cancellation, and representative scalar and nested
values using the local fixture and a documented Trino deployment. The user
reported completing all steps in `MANUAL_TEST_PLAN.md` on 2026-10-14; deployment
version and OS details were not recorded.

## Contributor coordination

Implementation repository: https://github.com/TabularisDB/tabularis-trino-plugin

Keep the Trino proposal/bounty linked to this repository so contributors can
coordinate: https://github.com/TabularisDB/tabularis-trino-plugin
