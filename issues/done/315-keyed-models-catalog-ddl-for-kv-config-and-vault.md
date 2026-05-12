# Keyed models: catalog + DDL for KV, Config, and Vault [AFK]

GitHub: https://github.com/reddb-io/reddb/issues/315

Labels: enhancement

GitHub issue number: #315

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Parent

#314

## What to build

Introduce `Config` and `Vault` as first-class Collection models beside normal `Kv`. Add the DDL and introspection path that makes the distinction visible before runtime operations land.

## Acceptance criteria

- [x] Catalog can represent `Kv`, `Config`, and `Vault` as distinct models.
- [x] Parser accepts `CREATE KV`, `CREATE CONFIG`, and `CREATE VAULT`.
- [x] Parser accepts `DROP KV`, `DROP CONFIG`, and `DROP VAULT` with model-aware validation.
- [x] `SHOW KVS`, `SHOW CONFIGS`, and `SHOW VAULTS` filter by model.
- [x] Invalid model operations return `INVALID_OPERATION` before policy/execution.
- [x] Existing normal KV behavior remains compatible.

## Blocked by

None - can start immediately

## Implementation notes

- Added `Config` and `Vault` as persisted `CollectionModel` variants beside `Kv`, including catalog JSON, physical JSON codec, `red.collections`, and developer index inference.
- Added parser/runtime DDL support for `CREATE KV|CONFIG|VAULT`, `DROP KV|CONFIG|VAULT`, and typed `SHOW KVS|CONFIGS|VAULTS` model filters.
- Added `INVALID_OPERATION` error mapping and pre-policy model-operation validation so wrong typed operations fail before policy/execution.
- Preserved normal KV compatibility through shared keyed collection handling and regression coverage.

## Verification

- `cargo check -p reddb-server --lib --message-format=short`
- `cargo test -p reddb-server test_parse_create_keyed_models --lib --message-format=short`
- `cargo test -p reddb-server test_parse_typed_show_desugars_to_red_collections_model_filter --lib --message-format=short`
- `cargo test -p reddb-server detect::tests::test_sql_detection --lib --message-format=short`
- `cargo test -p reddb --test e2e_ddl_drop_foundation --message-format=short`
