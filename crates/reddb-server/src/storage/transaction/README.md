# `storage/transaction` — Transaction coordinator (DORMANT)

> **⚠️ Read this first.** The `TransactionManager` / `MvccCoordinator` defined
> in this module is **not wired** to any production write path. It is
> architectural scaffolding that exists for future MVCC work but **does
> not** participate in any reddb operation today.
>
> The active transaction manager is `src/storage/wal/transaction.rs`,
> instantiated in `src/storage/engine/database.rs:230`. **That** is the one
> backing real durability and the stdio `tx.*` JSON-RPC methods.

## Module layout

- `coordinator.rs` — `TransactionManager` (dormant), 4 isolation levels,
  savepoints, lock manager, conflict detection
- `lock.rs` — `LockManager`, lock modes, wait-for graph
- `mod.rs` — re-exports

## Invariants

### 1. This module is dormant. Do not use it from production code paths.

`coordinator::TransactionManager::new` (`coordinator.rs:334`) is called
**only** in test code under that file. No write path, no read path, no
runtime instantiation references it.

The dormancy is intentional: this scaffolding lets us prototype real MVCC
without touching the runtime. **Wiring it into production requires a
coordinated change** — see `PLAN.md` § Post-MVP for the full plan
(MVCC row headers + WAL-first + lock manager promotion).

If you need transactional behavior right now, go through the active path:

```rust
// src/storage/wal/transaction.rs
use crate::storage::wal::transaction::TransactionManager as ActiveTM;
```

### 2. Active XIDs come from `SnapshotManager`, not from this module

The production XID allocator is
`storage::transaction::snapshot::SnapshotManager`. The active page-level
WAL transaction manager (`src/storage/wal/transaction.rs`) receives an
`Arc<SnapshotManager>` and calls `begin()` / `commit()` / `rollback()` on
that shared authority for its WAL `tx_id`s. Row MVCC xids, savepoint
sub-xids, autocommit born-committed xids, and page-WAL transaction ids
therefore live in one monotonic space.

On startup, runtime MVCC rehydrates the floor by scanning persisted
`xmin` / `xmax` values, while page-WAL recovery reports the highest WAL
transaction id it observed so the snapshot manager can advance past any
replayed WAL ids before new transactions begin.

`coordinator::TransactionManager` has its own `next_id: AtomicU64`
(`coordinator.rs:319`) because this module is still dormant test
scaffolding. **Do not use that allocator for production xids.** Mixing it
with the active snapshot manager gives you a second XID space and breaks
cross-layer correlation.

When (not if) we promote `coordinator` to production, its allocator must
be retired or changed to delegate to `SnapshotManager`. Until then, do
**not** call `coordinator::TransactionManager` outside its tests.

### 3. MVCC visibility lives in the btree version chain, not in row headers

The btree at `src/storage/btree/node.rs:300` carries:

```rust
pub struct LeafEntry<K, V> {
    pub key: K,
    pub versions: VersionChain<V>,
}
```

This is the **only** MVCC source for in-memory tables today. Rows in the
unified store (`src/storage/unified/entity.rs`) carry no `xmin`/`xmax`
header bytes.

If you add a row-header MVCC field, you are also signing up for a format
migration and a coordinator wiring. **Don't do it as a side effect of
another change** — open a PR titled "MVCC row headers" so reviewers know.

### 4. `wal.sync()` on commit/rollback is the durability boundary

`src/storage/wal/transaction.rs:207, 242` — the active TM calls
`wal.sync()` on `commit()` and `rollback()`. That is the moment durability
is guaranteed.

Sub-transaction operations (intermediate inserts, savepoints in the
dormant coordinator) **do not** offer durability. Drivers and callers
should treat any state between begin and commit as volatile.

The stdio `tx.commit` flow (`src/rpc_stdio.rs`) inherits this boundary:
`with_commit_lock { for op in write_set { execute_query(op) } }` →
underlying execute_query path eventually calls `wal.sync()` per
auto-committed sub-statement. Strict atomicity across all buffered ops is
**not** guaranteed today (documented in `rpc_stdio.rs::Session`).

### 5. Lock manager is dormant alongside the coordinator

`lock.rs::LockManager` exists with wait-for graph + deadlock detection,
but is only used by `coordinator::TransactionManager`. The active path has
exactly **one** synchronization primitive: `RuntimeInner.commit_lock:
Mutex<()>`, used by `tx.commit` to serialize replays.

A multi-granularity lock manager (intent locks, row/page/table hierarchy)
is post-MVP. Adding row locks here without first promoting `coordinator`
is a no-op at best and a performance regression at worst.

## Anti-patterns to avoid

- **Importing `coordinator::TransactionManager` from runtime code** — you
  will silently use a dormant manager that is not synchronized with anything.
- **Allocating XIDs from `coordinator::TransactionManager::next_id`** —
  see invariant 2.
- **Adding row headers to `RowData`** without a coordinated MVCC migration
  PR.

## See also

- Active TM: `src/storage/wal/transaction.rs`
- BTree version chain: `src/storage/btree/node.rs:300`, `src/storage/btree/version.rs`
- Stdio commit flow: `src/rpc_stdio.rs::Session`
- Future plan: `PLAN.md` § Post-MVP — MVCC row headers
