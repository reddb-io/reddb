# KV — INCR / DECR atomic counters [PENDING-MERGE]

GitHub: https://github.com/reddb-io/reddb/issues/242

Labels: enhancement

GitHub issue number: #242

## Status

Implementation work exists in pushed agent/integration branches. Do not reimplement from scratch; merge/review separately. This file is kept out of Ralph's top-level queue.

## Original GitHub Body

## Parent

#238

## What to build

Adds atomic counter operations to the KV DSL. `INCR key BY n EXPIRE <duration>` returns the new value in one engine round trip; `DECR key BY n` is the negative-step shorthand. The implementation lands inside the `KvAtomicOps` runtime module (introduced in #241) so concurrency safety is engine-native — no application-side `SELECT FOR UPDATE → UPDATE` retry loop, no advisory locks. Every transport and every driver gains the new methods.

## Acceptance criteria

- [ ] Parser accepts `INCR <key> [BY <n>] [EXPIRE <duration>]` and `DECR <key> [BY <n>] [EXPIRE <duration>]`. `BY` defaults to 1; negative values are accepted (so `DECR` is sugar over `INCR BY -n`).
- [ ] `KvAtomicOps::incr(coll, key, by, ttl_ms?)` returns the post-increment value as a typed integer. Atomicity is guaranteed by the engine's existing page-lock + WAL machinery — no per-callsite mutex.
- [ ] `INCR` on a missing key initialises at the `BY` value (Redis-compat).
- [ ] `INCR` against a non-integer existing value returns a typed error before mutation. The error is surfaced consistently across every transport.
- [ ] `EXPIRE` clause refreshes the TTL on the key when the increment lands, matching Redis `INCR ... EX` semantics.
- [ ] gRPC, HTTP (`POST /collections/<coll>/kv/<key>/incr?by=<n>`), pgwire, and MCP (`reddb_kv_incr` / `reddb_kv_decr`) all expose the new ops with the same envelope shape.
- [ ] Node, Python, and Rust drivers expose `db.kv.incr(key, by, ttlMs)` / `db.kv.decr(...)`.
- [ ] Property-based concurrency test: 100 concurrent `INCR key BY 1` calls converge to exactly +100. Repeats with random thread schedules to surface lost updates.
- [ ] Property-based test: interleaved `INCR` + `PUT key = value` (a write that resets the counter) still leaves the engine in a consistent, observable state.
- [ ] No regression on existing KV / table SELECTs.

## Blocked by

- #241
