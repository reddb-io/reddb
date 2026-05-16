---
status: open
tag: AFK
gh: 524
---

# [AFK] gh-524: Chain insert protocol + chain-tip endpoint

GitHub: reddb-io/reddb#524 (parent #521)

## Builds on #523

Foundation already in place: blockchain_kind.rs, genesis row, mutate gate, hash computation, RESERVED_COLUMNS. This slice ENFORCES the chain on INSERT.

## What to build

### Insert validation (on `kind=chain` INSERT)

1. Caller provides `prev_hash`, `block_height`, `timestamp` + payload.
2. Validate `prev_hash == tip.hash`, `block_height == tip.block_height + 1`, `timestamp ∈ now ± 60s`.
3. Compute `hash = sha256(block_height || prev_hash || timestamp || canonical(payload))`.
4. Persist + update tip.
5. Any failure -> `409 ChainConflict` with body `{block_height, hash, timestamp, server_time}`.

### Tip endpoint

`GET /collections/:name/chain-tip` -> `{block_height, hash, timestamp, server_time}`. In-memory tip cache. Unauthenticated within access scope.

## Acceptance

- [ ] INSERT validates `prev_hash`; mismatch -> 409 ChainConflict with new tip
- [ ] INSERT validates `block_height == tip + 1`; mismatch -> 409
- [ ] INSERT validates `timestamp` ±60s; out-of-range -> 409
- [ ] Engine computes hash per spec
- [ ] Tip is in-memory cached; updated atomically with insert
- [ ] GET `/collections/:name/chain-tip` returns tip JSON
- [ ] Concurrency test: 2 writers race; loser gets 409 with new tip and retries successfully
- [ ] Final chain hash-consistent end-to-end

## Progress (2026-05-16)

**Implementation landed in this workspace** — needs cargo verification + commit by a human (cargo/git were denied for the AFK agent so neither `cargo check` nor a commit ran from this session).

### Files changed
- `crates/reddb-server/src/runtime/blockchain_kind.rs` — added `ChainTipFull`, `chain_tip_full(...)` scan helper, `chain_conflict_error(...)` → `RedDBError::InvalidOperation("BlockchainConflict:<json>")` with `{block_height, hash, timestamp, server_time, reason}` body. Module visibility raised to `pub`.
- `crates/reddb-server/src/runtime.rs` — added `chain_tip_cache: parking_lot::Mutex<HashMap<String, ChainTipFull>>` field on `RuntimeInner`.
- `crates/reddb-server/src/runtime/impl_core.rs` — initialise the cache field.
- `crates/reddb-server/src/runtime/impl_ddl.rs` — after auto-genesis, prime the cache.
- `crates/reddb-server/src/runtime/impl_dml.rs` — chain-INSERT now:
  - acquires per-collection `rmw_locks.lock_for(table, "__chain__")` for serialisation
  - reads tip from cache (fallback scan on miss)
  - if any reserved column supplied → validate `prev_hash` (32-byte Blob OR 64-char hex Text), `block_height == tip+1`, `timestamp ∈ now±60s` — mismatch surfaces `BlockchainConflict:` with current tip
  - if none supplied → auto-fill (#523 backwards-compat)
  - `hash` column is always engine-computed; user-supplied → conflict
  - updates cache after batch persists
  - added public `RedDBRuntime::chain_tip_for_collection(&self, name)` for the HTTP handler.
- `crates/reddb-server/src/server/transport.rs` — `map_runtime_error` maps `InvalidOperation("BlockchainConflict:...")` and `InvalidOperation("BlockchainCollectionImmutable...")` to **HTTP 409**.
- `crates/reddb-server/src/server/routing.rs` — `GET /collections/:name/chain-tip` → 200 with `{block_height, hash, timestamp, server_time}` JSON; 404 when collection isn't chain.

### Tests
- `crates/reddb-server/tests/runtime_blockchain_chain_insert.rs` (new, 6 cases): prev_hash mismatch, height mismatch, timestamp drift, happy-path triple + hash recompute cross-check, atomic cache advance, concurrent-writer race ending with `verify_chain == Ok`.
- `crates/reddb-server/tests/runtime_blockchain_kind.rs` — `user_supplied_reserved_columns_are_overwritten_by_engine` rewritten to assert the new 409 contract (partial reserved-col supply → `BlockchainConflict:`). This is the #523→#524 contract change.

### To do (manual)
1. `CARGO_TARGET_DIR=.target-gh524 cargo check -p reddb-server`
2. `CARGO_TARGET_DIR=.target-gh524 cargo test -p reddb-server runtime_blockchain` (both test binaries)
3. If green: commit with `Closes #524`. If not: read errors and patch — most likely sites are the new lifetimes in the chain-INSERT block (`_chain_lock_arc` / `_chain_guard`) and the `Value::Text(Arc<str>)` hex parsing branch.

## Notes

- `CARGO_TARGET_DIR=.target-gh524`
- #523's INSERT path already does some of this (auto-fills reserved cols if missing). This slice ENFORCES caller-supplied values + 409 path.
- HTTP handler likely in `crates/reddb-server/src/server/handlers_*.rs`.
- Commit `Closes #524` if all 8 acceptance pass; else `Refs`.
