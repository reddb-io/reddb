# Issue #244 Integration Checklist: red.collections / SHOW COLLECTIONS

Scope: final merge verification after the engine, parser/docs, and acceptance-test branches land for #244. This note is based on the current `main` paths in this worktree and intentionally does not implement the feature.

## Contract To Verify

- `SHOW COLLECTIONS` must be a SQL/admin statement, not natural-language `show ...` detection.
- The runtime result should be a row set over `red.collections`, not a bespoke transport-only envelope.
- The row source must be the authoritative collection contract/catalog view, not only `UnifiedStore::list_collections()`, because #244 is about the logical collection contract.
- Embedded, HTTP `/query`, gRPC `Query`, RedWire `MSG_QUERY`, RedWire `MSG_QUERY_BINARY`, and stdio RPC should all observe the same columns and row ordering.

## Parser Seams

- Add a `QueryExpr` variant in `crates/reddb-server/src/storage/query/core.rs` next to the existing admin `SHOW` variants (`ShowConfig`, `ShowSecrets`, `ShowTenant`, `ShowPolicies`).
- Add corresponding `SqlCommand` and `SqlAdminCommand` variants in `crates/reddb-server/src/storage/query/sql.rs`; wire both `into_query_expr` and `into_statement`.
- Extend `parse_sql_command` in `crates/reddb-server/src/storage/query/sql.rs` under the current `Token::Ident(name) if name.eq_ignore_ascii_case("SHOW")` arm. Today it accepts only `CONFIG`, `SECRET(S)`, `TENANT`, and IAM `POLICIES`/`EFFECTIVE`.
- Keep `SHOW` as an identifier unless the parser agent deliberately promotes it to a lexer keyword. If it is promoted in `lexer.rs`, `parse_frontend_statement` must also accept the new token.
- Extend `detect_mode` in `crates/reddb-server/src/storage/query/modes/detect.rs`; otherwise `SHOW COLLECTIONS` will fall through to natural-language detection because `show ` is a natural-language starter.
- Add parser tests in `crates/reddb-server/src/storage/query/parser/tests.rs` for `SHOW COLLECTIONS`, lowercase/mixed-case forms, optional semicolon, and rejection of trailing garbage.

## Runtime Seams

- Dispatch the new `QueryExpr` arm in `crates/reddb-server/src/runtime/impl_core.rs` alongside `ShowConfig` and `ShowTenant`.
- Return `RuntimeQueryResult` with `statement` and `statement_type` consistent with other read-only admin statements: likely `statement = "show_collections"` and `statement_type = "select"`.
- Build `UnifiedResult::with_columns(...)` plus `UnifiedRecord` rows so all transports reuse normal query-result encoding.
- Prefer an authoritative catalog/contract source:
  - `RedDBRuntime::catalog()` in `impl_core.rs` currently delegates to `catalog_model_snapshot`.
  - `catalog::snapshot_store_with_declarations` joins `UnifiedStore::list_collections()` with persisted `CollectionContract`s.
  - `RedDB::collection_contracts()` / `collection_contract_arc()` live under `storage/unified/devx/reddb/impl_registry.rs`.
- Verify empty-store behavior. If no user collections exist, the result should still have stable columns and zero rows unless the #244 contract explicitly includes internal collections.
- Decide and test internal collection visibility (`red_config`, future `red.collections`, VCS/internal collections). The acceptance branch should pin this so engine and docs do not drift.
- Verify ordering is deterministic, ideally lexical by collection name, matching `catalog::snapshot_store_with_declarations` output.

## Wire And Client Seams

- RedWire text query path: `crates/reddb-server/src/wire/listener.rs::handle_query` delegates to `runtime.execute_query` and then `encode_result`.
- RedWire binary query path: `handle_query_binary` delegates to `handle_query`; `encode_result` uses `result.result.columns` first, so a normal `UnifiedResult` should be enough.
- HTTP clients post to `/query`; the server-side path should already use `execute_query`, but acceptance should verify the JSON envelope columns/rows shape.
- gRPC `Query` and stdio RPC also delegate into runtime query execution; verify they preserve the same row fields and do not special-case `SHOW`.
- Watch the direct scan fast path in `wire/query_direct.rs`: it should return `None` for `SHOW COLLECTIONS` and fall through to runtime.

## Likely Conflict Points

- `crates/reddb-server/src/storage/query/core.rs`: enum variant location and match exhaustiveness across planner/cost/vector/join helpers.
- `crates/reddb-server/src/storage/query/sql.rs`: concurrent parser/docs edits will likely touch the same `SHOW` arm and admin command enums.
- `crates/reddb-server/src/storage/query/modes/detect.rs`: missing `show collections` detection is an easy integration miss.
- `crates/reddb-server/src/runtime/impl_core.rs`: large dispatch match near existing `ShowConfig`/`ShowTenant` arms.
- `crates/reddb-server/src/storage/query/planner/cost.rs`, `runtime/join_filter.rs`, and `storage/query/executors/vector.rs`: helper matches that classify every `QueryExpr` may need an added `ShowCollections` case.
- `crates/reddb-server/src/presentation/query_result_json.rs` and `wire/listener.rs`: only risky if the implementation uses `pre_serialized_json` instead of records/columns.

## Post-Merge Checks

Run after merging engine + parser/docs + acceptance branches:

```bash
cargo test -p reddb-server storage::query::parser::tests:: -- --nocapture
cargo test -p reddb-server runtime -- --nocapture
cargo test --test redwire_smoke -- --nocapture
cargo test --test integration_rpc_stdio -- --nocapture
cargo test -p reddb-client --tests -- --nocapture
```

Focused manual smoke, if a `red` binary is available:

```bash
cargo run --bin red -- query "CREATE TABLE issue244_users (id INT)"
cargo run --bin red -- query "SHOW COLLECTIONS"
REDDB_DISABLE_DIRECT_SCAN=1 cargo run --bin red -- query "SHOW COLLECTIONS"
```

Acceptance assertions to confirm:

- `SHOW COLLECTIONS` parses as SQL in `detect_mode`.
- Result has stable columns and deterministic ordering.
- At least one row appears after `CREATE TABLE issue244_users (id INT)`.
- The same collection appears through embedded runtime, HTTP/gRPC query, RedWire text, RedWire binary, and stdio RPC.
- Internal collection policy is explicit and matches docs.
