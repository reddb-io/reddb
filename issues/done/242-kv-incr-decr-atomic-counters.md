# KV — INCR / DECR atomic counters [DONE]

GitHub: https://github.com/reddb-io/reddb/issues/242

## Status

Implemented 2026-05-09.

Scope guard: this issue is **normal KV only**. INCR/DECR must not apply to Config or Vault. Config and Vault are separate keyed Collection models under #314.

## What was built

- `KvCommand::Incr { collection, key, by, ttl_ms }` variant in `storage/query/core.rs`
- Parser in `storage/query/parser/kv.rs`: `KV INCR key [BY n] [EXPIRE dur]`, `KV DECR` as sugar for negative `by`
- `KvAtomicOps::incr()` in `runtime/impl_kv.rs`: initialises missing key at `by`, errors on non-integer, refreshes TTL
- `execute_kv_command` Incr arm in `runtime/impl_kv.rs`
- `"kv "` prefix added to SQL mode detection in `storage/query/modes/detect.rs`
- 6 parser tests + 5 runtime tests

## Test results

3465 passed, 6 pre-existing failures, 0 regressions.
