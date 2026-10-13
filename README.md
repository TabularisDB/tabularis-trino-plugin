# Trino plugin for Tabularis

A standalone Rust driver plugin that connects [Tabularis](https://github.com/TabularisDB/tabularis)
to [Trino](https://trino.io/). The plugin communicates with Tabularis using
JSON-RPC 2.0 over standard input and output.

> Query cancellation requires Tabularis `0.26.0` or newer. The plugin uses the
> host's `cancel` notification to stop running statements on Trino.

## Features

- Browse Trino catalogs, schemas, tables, columns, and views visible to the
  connected user.
- Run SQL from the Tabularis editor and notebooks, including federated queries
  across catalogs.
- Collect every Trino response page and support Tabularis result paging and
  offsets.
- Cancel timed out queries through Trino's client protocol.
- Preserve decimal and unsafe large-integer precision, including values nested
  in ARRAY, MAP, and ROW values.
- Connect without authentication for local development, or use HTTP Basic
  authentication over HTTPS. Passwords over plain HTTP are rejected.
- Configure TLS verification, a custom CA certificate, and an optional client
  certificate/private key pair.

## Supported capabilities

| Capability | Support |
|---|---|
| Connection test | `SELECT 1` against the coordinator |
| Metadata | Catalogs, schemas, tables, columns, views, view definitions and view columns |
| Query execution | Trino REST protocol with full response paging and cancellation |
| Editing and CRUD | Read-only; not supported |
| DDL actions | Not supported |
| Visual EXPLAIN | Not enabled |
| Presto | Not evaluated; this plugin targets Trino |

Indexes, foreign keys, and routines are not exposed in this release. Unsupported
write and DDL methods return the documented unsupported-method response.

## Connection configuration

| Field | Description |
|---|---|
| Host | Trino coordinator hostname or URL. Use `https://` for HTTPS. |
| Port | Coordinator port. The manifest defaults to `5432` for the local fixture; Trino commonly uses `8080`, which can be entered here. |
| Database name | Default Trino catalog. Enable multiple database browsing in Tabularis to browse more catalogs. |
| Schema | Optional schema selected in Tabularis for browsing and query context. |
| Username | Trino user; defaults to `tabularis` when blank. |
| Password | Optional; accepted only over HTTPS. |
| SSL mode | `disable`, `prefer`, `require`, `verify-ca`, or `verify-full`. |
| CA certificate | Optional PEM content or path to a PEM file. |
| Client certificate/key | Optional PEM content or file paths; provide both when mutual TLS is required. |

The connection-string field is hidden because this plugin accepts coordinator
and catalog settings through the standard connection fields.

## Install for development

Install Rust and [`just`](https://just.systems/), then run from this repository:

```sh
just dev-install
```

This builds the plugin and copies its executable and `.tabularium` manifest to
the current user's Tabularis plugin directory. Restart Tabularis or toggle the
plugin off and on to reload it.

| OS | Plugin directory |
|---|---|
| Linux | `~/.local/share/tabularis/plugins/drivers/trino/` |
| macOS | `~/Library/Application Support/tabularis/plugins/drivers/trino/` |
| Windows | `%APPDATA%\tabularis\plugins\drivers\trino\` |

Registry distribution is not published yet. Release instructions are below.

## Build and verify

```sh
just build
just test
just lint
cargo fmt --check
```

Build an optimized executable with:

```sh
cargo build --release
```

The binary is written to `target/release/trino-plugin` (Windows:
`target/release/trino-plugin.exe`).

## Local Trino fixture

The fixture runs Trino `483`, maps host port `5432` to container port `8080`,
and configures the `memory` and `tpch` catalogs.

```sh
docker compose -f fixture/docker-compose.yml up -d
docker exec trino-fixture trino --execute "SELECT 1"
./fixture/seed.sh
just dev-install
```

Connect to `127.0.0.1:5432` with catalog `memory`, username `tester`, and no
password. Enable multiple database browsing to discover the `tpch` catalog.
The memory catalog is cleared when the container is recreated; run the seed
script again after a restart.

Follow [MANUAL_TEST_PLAN.md](MANUAL_TEST_PLAN.md) for metadata, query paging,
federated query, exact-value, cancellation, and HTTPS checks. Stop the fixture
when finished:

```sh
docker compose -f fixture/docker-compose.yml down
```

## Release and registry publication

1. Set the `.tabularium` version to the release version and tag the commit with
   the same version prefixed by `v` (for example, `v0.1.0`).
2. Push the tag. The GitHub Actions workflow builds Linux x64/arm64, macOS
   x64/arm64, and Windows x64 ZIP bundles and attaches them to the release,
   together with `.tabularium` as a standalone asset.
3. Confirm all five ZIP files and the standalone manifest appear on the GitHub
   release. The tag without its `v` prefix must match the manifest version.
4. Submit the release at
   [registry.tabularis.dev/submit](https://registry.tabularis.dev/submit),
   setting the minimum Tabularis version to `0.26.0` or newer for query
   cancellation support.

## License

This project is licensed under the [Apache License 2.0](LICENSE).
