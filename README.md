<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/TabularisDB/tabularis/main/.github/assets/banner-dark.png" />
    <img src="https://raw.githubusercontent.com/TabularisDB/tabularis/main/.github/assets/banner-light.png" alt="tabularis — the database client your AI agent can actually use" width="600" />
  </picture>

  <br/>

  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/TabularisDB/tabularis-trino-plugin/main/assets/trino-banner-dark.png" />
    <img src="https://raw.githubusercontent.com/TabularisDB/tabularis-trino-plugin/main/assets/trino-banner-light.png" alt="Native Trino support" width="600" />
  </picture>
</div>

# tabularis-trino-plugin

<p align="center">

[![Release](https://img.shields.io/github/v/release/TabularisDB/tabularis-trino-plugin?style=flat)](https://github.com/TabularisDB/tabularis-trino-plugin/releases)
![Downloads](https://img.shields.io/github/downloads/TabularisDB/tabularis-trino-plugin/total.svg?style=flat)
[![Release workflow](https://github.com/TabularisDB/tabularis-trino-plugin/actions/workflows/release.yml/badge.svg)](https://github.com/TabularisDB/tabularis-trino-plugin/actions/workflows/release.yml)

</p>

A [Trino](https://trino.io/) driver plugin for [Tabularis](https://github.com/TabularisDB/tabularis),
the open-source database client.

This plugin connects Tabularis to Trino through a standalone Rust executable
that implements JSON-RPC 2.0 over stdio. Browse catalogs, schemas, tables, and
views, then run SQL in the editor or SQL notebooks—including federated queries
across catalogs.

> **Requires Tabularis v0.26.0 or later** for query cancellation support.

## Table of Contents

- [Features](#features)
- [Screenshots](#screenshots)
- [Connection Configuration](#connection-configuration)
- [Installation](#installation)
  - [From the Tabularium registry](#from-the-tabularium-registry)
  - [Manual Installation](#manual-installation)
- [How It Works](#how-it-works)
- [Supported Operations](#supported-operations)
- [Building from Source](#building-from-source)
- [Development and Testing](#development-and-testing)
- [License](#license)

## Features

- **Catalog and schema browsing** — Discover catalogs, schemas, tables,
  columns, views, view definitions, and view columns visible to the Trino user.
- **Query execution** — Run SQL in the Tabularis editor and notebooks, including
  federated queries across configured catalogs.
- **Complete results** — Follow Trino's result-page links and preserve row
  order, including when a response page contains no rows. Support requested
  result limits and offsets.
- **Cancellation** — Send cancellation to Trino through its client protocol
  when Tabularis cancels a running query.
- **Query diagnostics** — Return Trino's query ID, final state, states observed
  while collecting pages, client elapsed time, and selected Trino timing/split
  statistics. Trino errors include the query ID, error code/type, and SQL
  location when the coordinator provides them.
- **Exact values** — Preserve NULL, DECIMAL, large integers, ARRAY, MAP, and ROW
  values without silent precision loss.
- **Secure authentication** — Support unauthenticated local development and
  HTTP Basic authentication over HTTPS. Password authentication over plain HTTP
  is rejected.
- **TLS configuration** — Support SSL modes, a custom CA certificate, and an
  optional client certificate/private key pair.
- **Cross-platform releases** — The release workflow builds for Linux x64/arm64,
  macOS x64/arm64, and Windows x64.

This plugin is read-only. Grid editing, CRUD, DDL actions, and Visual
EXPLAIN are not supported. Presto compatibility has not been evaluated.

## Screenshots

![Trino listed and installed in the Tabularis database picker](https://raw.githubusercontent.com/TabularisDB/tabularis-trino-plugin/main/assets/01-database-picker.png)

![Trino connection form showing the local fixture settings](https://raw.githubusercontent.com/TabularisDB/tabularis-trino-plugin/main/assets/02-connection-form.png)

![Tabularis explorer showing Trino catalogs, schemas, tables, and views](https://raw.githubusercontent.com/TabularisDB/tabularis-trino-plugin/main/assets/03-schema-browser.png)

![Ordered Trino query results with 100,000 rows and pagination controls](https://raw.githubusercontent.com/TabularisDB/tabularis-trino-plugin/main/assets/04-table-data.png)

The connection screenshot uses a local test account; the password is masked.

## Connection Configuration

| Tabularis field | Description |
| --- | --- |
| Host | Trino coordinator hostname or URL. Use an `https://` URL for HTTPS. |
| Port | Coordinator port. Defaults to `8080`, the usual Trino HTTP port (HTTPS deployments often use `443` or `8443`). The local fixture maps host port `5432` to container port `8080`, so set `5432` when using it. |
| Database name | Default Trino catalog. Enable multiple database browsing to browse additional catalogs. |
| Schema | Optional default schema for browsing and query context. |
| Username | Trino user; defaults to `tabularis` when blank. |
| Password | Optional; password authentication requires HTTPS. |
| SSL mode | Disabled, Preferred, Required, Verify CA, or Verify Identity. |
| CA certificate | Optional PEM content or a path to a PEM file. |
| Client certificate/key | Optional PEM content or file paths. Provide both when mutual TLS is required. |

The connection-string field is not used. Configure the coordinator and catalog
through the standard connection fields.

## Installation

### From the Tabularium registry

In Tabularis, open **Settings → Plugins**, search for **Trino**, and install it.
The plugin is also listed on the
[Tabularium registry](https://registry.tabularis.dev/plugins/trino).

### Manual Installation

1. Download the archive for your operating system and architecture from the
   [GitHub Releases page](https://github.com/TabularisDB/tabularis-trino-plugin/releases).
2. Extract the archive. It contains the plugin executable and `.tabularium`
   manifest.
3. Copy both files into the Trino plugin directory:

   | OS | Plugin directory |
   | --- | --- |
   | **Linux** | `~/.local/share/tabularis/plugins/drivers/trino/` |
   | **macOS** | `~/Library/Application Support/tabularis/plugins/drivers/trino/` |
   | **Windows** | `%APPDATA%\tabularis\plugins\drivers\trino\` |

4. Restart Tabularis or toggle the Trino plugin off and on in Settings.

For a local development build, use `just dev-install` from this repository.

## How It Works

The plugin is a standalone Rust executable that communicates with Tabularis
using **JSON-RPC 2.0 over stdio**:

1. Tabularis starts the plugin as a child process.
2. Tabularis sends newline-delimited JSON-RPC requests through the plugin's
   standard input; the plugin writes responses to standard output.
3. The plugin submits SQL to Trino's `/v1/statement` endpoint, follows each
   `nextUri` until the statement finishes, and sends an HTTP `DELETE` to cancel
   a running statement when requested.

Catalog and schema context comes from each connection/request. Catalogs map to
Tabularis databases, and Trino metadata visibility follows the connected
Trino user's permissions.

## Supported Operations

| Operation | Support |
| --- | --- |
| Connection test | Supported (`SELECT 1`) |
| Catalog discovery | Supported (`get_databases`) |
| Schema discovery | Supported (`get_schemas`) |
| Table and column discovery | Supported (`get_tables`, `get_columns`) |
| View discovery | Supported, including definitions and columns |
| SQL execution | Supported with full response paging, cancellation, and query diagnostics |
| Grid editing and CRUD | Not supported; plugin is read-only |
| DDL actions | Not supported |
| Visual EXPLAIN | Not supported |
| Presto | Not evaluated |

Indexes, foreign keys, and routines are not exposed by this release. Unsupported
methods return the plugin protocol's not-implemented response.

## Building from Source

### Prerequisites

- [Rust](https://www.rust-lang.org/tools/install)
- [`just`](https://just.systems/) (optional; runs the common Cargo recipes)
- Docker Compose for the local Trino fixture

### Build and install locally

```sh
just build
just dev-install
```

Or build a release executable directly:

```sh
cargo build --release
```

The binary is written to `target/release/trino-plugin` (`trino-plugin.exe` on
Windows).

## Development and Testing

Run the Rust checks:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Start and seed the local Trino 483 fixture, which exposes the `memory` and
`tpch` catalogs:

```sh
docker compose -f fixture/docker-compose.yml up -d
docker exec trino-fixture trino --execute "SELECT 1"
./fixture/seed.sh
```

The fixture maps host port `5432` to container port `8080`. Connect in
Tabularis to `127.0.0.1:5432` with catalog `memory`, username `tester`, and no
password. The in-memory catalog is cleared when the container is recreated;
rerun the seed script after a restart.

Run the JSON-RPC fixture checks with:

```sh
./scripts/e2e-fixture-test.sh
```

See [MANUAL_TEST_PLAN.md](MANUAL_TEST_PLAN.md) for app checks, including
metadata browsing, editor and notebook queries, pagination, cancellation, and
HTTPS/Basic authentication. Stop the fixture when finished:

```sh
docker compose -f fixture/docker-compose.yml down
```

## License

This project is licensed under the [Apache License 2.0](LICENSE).
