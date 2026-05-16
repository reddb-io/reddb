# GH-460: JS/TS rich SDK helpers

GitHub: https://github.com/reddb-io/reddb/issues/460

## Result

Implemented the JS/TS helper surface from the SDK Helper Spec for the embedded
SDK and kept the remote client aligned:

- `db.documents.insert/get/list/patch/delete`
- `db.kv(collection).exists/delete/list`
- exact namespaced KV key round-trip
- `rid` / `rids` insert envelopes with legacy `id` / `ids` aliases
- query builder parameterized `where()` regression fix
- README and TypeScript surface updates

The JS smoke test now covers document CRUD, KV exact namespaced keys, queue
workflow, transactions, insert/bulkInsert ids, and generic query workflows.

## Verification Notes

- `rtk git diff --check` passed.
- `rtk cargo fmt --all --check` passed.
- `rtk cargo check -p reddb-io-server` passed.
- `rtk make check` passed.
- `rtk cargo build --bin red` passed.
- `REDDB_BINARY_PATH=/home/cyber/.cache/cargo-target/debug/red rtk pnpm test` passed with 23 tests.
- `rtk cargo clippy -p reddb-io-server --all-targets -- -D warnings` failed on the existing repo-wide clippy backlog.
- `rtk pnpm typecheck` exited `1` while reporting `TypeScript: No errors found`.
