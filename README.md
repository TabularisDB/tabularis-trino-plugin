# Trino — Tabularis Plugin

Driver plugin for [Tabularis](https://github.com/TabularisDB/tabularis).
Generated with `@tabularis/create-plugin`.

## Getting started

```bash
just dev-install       # build + install into ~/.local/share/tabularis/plugins/drivers/trino
```

Then open Tabularis — your driver appears in the connection picker. The driver checks connectivity with `SELECT 1` and can browse catalogs, schemas, tables, columns, and views.

## What's implemented

| Method | Status | Notes |
|--------|--------|-------|
| `test_connection` | implemented | submits `SELECT 1` through the Trino client protocol |
| `ping` | minimal | returns `null`; falls back to `test_connection` if missing |
| `get_databases`, `get_schemas`, `get_tables`, `get_columns` | implemented | discover Trino catalogs and schema objects visible to the connection |
| `get_views`, `get_view_definition`, `get_view_columns` | implemented | view browsing only; view creation and mutation are not supported yet |
| `get_indexes`, `get_foreign_keys`, `get_routines*`, `create_view`, `alter_view`, `drop_view` | `-32601` or empty | not implemented |
| `execute_query` | implemented | collects pages, applies page/offset, handles cancellation, and preserves exact decimal/unsafe integer values as strings |
| `explain_query` | `-32601` | Visual EXPLAIN is not enabled |
| `insert_record`, `update_record`, `delete_record` | `-32601` | implement for row editing support |
| DDL generators, batch methods | `-32601` | implement as you light up the matching UI features |

## Layout

```
src/
├── main.rs            stdio loop with concurrent query dispatch and cancellation notifications
├── rpc.rs             method dispatch + response helpers
├── error.rs           plugin error type
├── models.rs          ConnectionParams + common shapes
├── client.rs          Trino REST client, paginated results, and query cancellation
├── handlers/
│   ├── metadata.rs    databases, schemas, tables, columns, indexes, FKs, views, routines
│   ├── query.rs       test_connection, ping, execute_query, explain_query
│   ├── crud.rs        insert_record, update_record, delete_record
│   └── ddl.rs         CREATE/ALTER/DROP generators
├── utils/
│   ├── identifiers.rs quote_identifier(name) + tests
│   └── pagination.rs  paginate(query, page, size) + tests
└── bin/
    └── test_plugin.rs local REPL for simulating Tabularis calls
```

## Testing without Tabularis

```bash
just repl
# > get_tables
# { "tables": [] }
```

## Publishing

Tag a commit `v0.1.0` and push — the included GitHub Actions workflow builds for Linux (x64/arm64), macOS (x64/arm64), and Windows (x64), then attaches the zipped plugin bundles to the release. Submit a PR to `plugins/registry.json` in the Tabularis repo to publish to the in-app registry.

## References

- [Plugin guide](https://github.com/TabularisDB/tabularis/blob/main/plugins/PLUGIN_GUIDE.md)
- [Manifest schema](https://github.com/TabularisDB/tabularis/blob/main/plugins/manifest.schema.json)
- [Tabularis repo](https://github.com/TabularisDB/tabularis)

## License

Apache-2.0
