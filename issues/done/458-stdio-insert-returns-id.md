# Stdio JSON-RPC `insert` returns `id`; `bulk_insert` returns `ids` [DONE]

Local issue number: #458

GitHub issue: not found in `reddb-io/reddb`.

## Result

Implemented the engine-side stdio JSON-RPC contract:

- `insert` now returns `{ affected, id }` for local stdio sessions.
- `bulk_insert` now returns `{ affected, ids }` with ids ordered to match
  the input payload array.
- Transactional pending envelopes remain unchanged while a stdio
  transaction is open.
- `get` now resolves the public id through `red_entity_id`, matching the
  identity column returned by table scans.

## Verification

- `cargo test -q -p reddb-io --test integration_rpc_stdio insert -- --test-threads=1`
- `cargo test -q -p reddb-io-server rpc_stdio --lib -- --test-threads=1`
- `cargo check -q -p reddb-io-server`
- `git diff --check`
