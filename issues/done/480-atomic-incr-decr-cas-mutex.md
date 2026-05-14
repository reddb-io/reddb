# Atomic INCR/DECR/CAS and `UPDATE col = col + expr` via sharded per-key mutex [AFK]

Labels: bug, correctness, needs-triage

## AFK instruction

Implement this issue as a focused vertical slice. Preserve behavior with tests/checks, commit all changes, and move this file to `issues/done/` when complete. If blocked, add a progress note and move it to `issues/blocked/`.

## Problem

`KvCommand::Incr` and table `UPDATE` with self-referential expressions (`SET col = col + N`) are **not atomic**. Both paths do read-modify-write in user-space without locking.

KV (`crates/reddb-server/src/runtime/impl_kv.rs:316`):
```rust
let existing = self.runtime.get_kv(collection, key)?;   // READ
let current  = ...;
let next     = current.checked_add(by)?;                // COMPUTE
self.runtime.delete_kv(...)?;                           // WRITE
self.runtime.create_kv(...)                             // WRITE
```

Table UPDATE (`crates/reddb-server/src/runtime/impl_dml.rs:1111`):
```rust
let ids_to_update = ...find_target_ids()?;
for entity in manager.get_many(chunk) { ... }            // READ
let assignments = materialize_update_assignments_for_entity(...)?;  // COMPUTE
apply_materialized_update_for_entity(...)?;              // WRITE
```

Two concurrent `INCR k BY 1` (or two concurrent `UPDATE t SET n = n+1 WHERE id = 1`) read the same `current`, compute `current+1`, both write `current+1`. One increment is lost.

`KvCommand::Cas` has the same shape and likely the same defect — verify and fix together.

## What to build

A sharded per-key mutex map serialising the read-compute-write critical section for these ops:

- `KvCommand::Incr` / `KvCommand::Decr` (Decr is Incr with negative `by`)
- `KvCommand::Cas`
- `UPDATE <table> SET <col> = <expr-referencing-col> WHERE ...` (single-row case is the priority; multi-row is sequential within the lock)

Lock key: `(collection_name, row_key)`. KV uses the kv key; tables use the entity id. Sharded by hash to avoid a single global mutex. Mutex is held only for the duration of the read-compute-write — released before WAL fsync if possible, otherwise across the fsync (correctness > latency on this op).

## Acceptance criteria

- [ ] Stress test: 8 threads × 1000 `INCR kv:counter BY 1` against a single key → final value is exactly 8000 (today fails).
- [ ] Stress test: 8 threads × 1000 `UPDATE counters SET n = n + 1 WHERE id = 1` → final `n` is exactly 8000.
- [ ] CAS stress test: N threads racing CAS on the same key, exactly one succeeds per round.
- [ ] No regression in single-threaded INCR/UPDATE benchmarks beyond 5% (mutex acquisition is cheap when uncontended).
- [ ] Mutex is sharded (not one global lock); two keys in the same collection never contend.
- [ ] Existing INCR/DECR/CAS/UPDATE tests still pass.

## Out of scope

- Cross-collection / cross-model atomic batches.
- Event-sourced counter mode (separate transaction log per field).
- Saga / compensation primitives.
- The mutation↔event dual-write window (tracked separately).

## Blocked by

None - can start immediately.
