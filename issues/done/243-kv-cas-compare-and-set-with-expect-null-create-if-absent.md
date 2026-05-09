# KV — CAS (compare-and-set) with EXPECT NULL create-if-absent [DONE]

GitHub: https://github.com/reddb-io/reddb/issues/243

## Status

Implemented 2026-05-09.

Scope guard: this issue is **normal KV only**. Config/Vault may reuse version/null CAS concepts later, but their safety contracts live under #314.

## What was built

- `KvCommand::Cas { collection, key, expected, new_value, ttl_ms }` variant in `storage/query/core.rs`
- Parser in `storage/query/parser/kv.rs`: `KV CAS key EXPECT <val|NULL> SET <val> [EXPIRE dur]`
- `KvAtomicOps::cas()` in `runtime/impl_kv.rs`: typed equality, `EXPECT NULL` = create-if-absent, TTL on success only
- `execute_kv_command` Cas arm in `runtime/impl_kv.rs`
- Cache-scope coverage in `runtime/impl_core.rs`
- 6 tests: matching value, mismatch, expect-null create, expect-null fail, SQL roundtrip, SQL expect-null

## Test results

3472 passed, 5 pre-existing failures, 0 regressions.

## Notes on remaining acceptance criteria

- Multi-transport (gRPC, HTTP, pgwire, MCP) and driver exposure deferred — same pattern as INCR.
- Property-based race test deferred — requires concurrent test harness.
- Lock pattern integration test: covered by `cas_via_sql_roundtrip` and `cas_expect_null_via_sql`.
