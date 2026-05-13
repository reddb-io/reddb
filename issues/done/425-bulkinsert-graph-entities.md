# bulkInsert for graph NODE/EDGE entities across drivers [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/425

Labels: enhancement

GitHub issue number: #425

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Type

Enhancement — driver

## What to build

`db.bulkInsert(collection, rows)` (already in JS SDK for row collections) accepts graph NODE / EDGE rows when the collection is graph-typed. Internally batches via single RPC frame.

Today: 1741 single-row graph INSERTs take ~15s over stdio. Mostly JSON-RPC handshake overhead.

## Acceptance criteria

- [x] `db.bulkInsert(coll, [{label, name, ...}, ...])` works for NODE rows.
- [x] Same for EDGE rows: `{label, from, to, ...}`.
- [x] Returns `{affected, ids: [...]}` matching #E1.
- [x] Bench: 1000 graph inserts in <2s via stdio (vs ~9s today).
- [x] Parity across drivers (JS, Python, Go, Rust at minimum).

## Progress

Completed:

- Stdio `bulk_insert` now detects graph-shaped payloads only when the target
  collection is declared `GRAPH` or `MIXED`, preserving table-row semantics for
  ordinary collections with a `label` field.
- Graph bulk insert normalizes flat node rows like `{label, name}` and edge
  rows like `{label, from, to, role}` into the existing entity create payloads.
- Stdio and RedWire server paths return `{affected, ids}` for graph rows and
  also include `ids` when the row insert path exposes inserted IDs.
- Added focused stdio and RedWire server tests for graph NODE and EDGE rows.
- JS/TS type surfaces now allow `ids` on `BulkInsertResult`.
- Rust `Reddb::bulk_insert` now returns `BulkInsertResult { affected, ids }`
  across embedded, HTTP, gRPC, and low-level RedWire paths.
- Python high-level gRPC bulk insert now includes `ids`, matching the existing
  embedded dict shape.
- Go `Conn.BulkInsert` and low-level RedWire bulk insert now return
  `BulkInsertResult` instead of discarding the server envelope.
- Internal gRPC connector bulk status carries `ids`, so Python and remote
  stdio can preserve the server response.

Verification:

- `cargo test -p reddb-io-server bulk_insert_graph`
- `cargo test -p reddb-io-client bulk_insert`
- `cargo test -p reddb-io-server --test graph_dsl_parser`
- `cargo test -p reddb-io-server --test runtime_query_behavior graph_`
- `cargo test -p reddb-io-server explain_ask`
- `cargo test -p reddb-io-server ask_pipeline`
- `cargo test -p reddb-io-server test_parse_dml_extended_literals_auto_embed_and_ask_forms`
- `cargo check -p reddb-io-server -p reddb-io-client -p reddb-io-client-connector`
- `cargo check --manifest-path drivers/python/Cargo.toml`
- `go test ./...` in `drivers/go`
- stdio benchmark: 1000 graph rows in 1.434s, `affected=1000`, `ids=1000`

Note: full `cargo test -p reddb-io-server --test runtime_query_behavior`
still has unrelated existing failures in `join_query_executes_against_real_table_rows`,
`config_reference_compares_stored_value_without_reparsing_sql`, and
`secret_reference_compares_vault_value_without_reparsing_sql`.
