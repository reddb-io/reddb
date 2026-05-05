# `storage/btree` — In-memory MVCC B+tree

This is reddb's **in-memory** B+tree with per-key version chains. It is the
backbone of the unified store's hot path and the only place where MVCC
visibility is currently resolved end-to-end.

> **Sister module:** `src/storage/engine/btree/` is the *page-based* on-disk
> B+tree used by the durable storage engine. The two trees share no code and
> have different invariants — read both READMEs before touching either.

This README documents the load-bearing invariants. **Every contributor
modifying this module must preserve these.** When in doubt, add an invariant
here rather than weakening it in code.

## Module layout

- `tree.rs` — `BPlusTree<K, V>` root, insert/delete/find recursive walk
- `node.rs` — `Node`, `LeafNode`, `LeafEntry`, `InternalNode`
- `version.rs` — `VersionChain<V>` MVCC chain primitive
- `snapshot.rs` — `Snapshot { txn_id, timestamp }` for visibility
- `gc.rs` — version chain compaction
- `iter.rs` — range scan iterators

## Invariants

### 1. Versions are append-only — never mutate in place

`LeafEntry` (`node.rs:293-302`) holds:

```rust
pub struct LeafEntry<K, V> {
    pub key: K,
    pub versions: VersionChain<V>,
}
```

The `VersionChain` is the **single source of MVCC truth in this module**.
Every write (`update`, `delete`) appends a new version with a fresh
`(txn_id, timestamp)` tuple. **Never overwrite a prior version**, even on
the same key in the same transaction — readers holding an older snapshot
must still see what they saw.

Garbage collection of obsolete versions happens through `gc.rs` and is the
**only** allowed mutation that removes versions. GC must verify that no
active snapshot can still observe a version before dropping it.

### 2. Splits hold the node write lock for the entire split

`insert_recursive` (`tree.rs:425-483`) takes the write guard via
`recover_write_guard(&node)` (line 437, 451) and holds it across the entire
split path. **Readers on a splitting node block.** This is the Rust analogue
of postgres' B-tree page lock — non-blocking-reader B+tree (Lehman-Yao
right-link technique) is post-MVP and tracked in `PLAN.md`.

When implementing a new write path, **never drop the write guard between
the structural decision (`split required`) and the structural change
(`internal.insert(median, right_child)`)**. Dropping the guard would let
another writer observe a partially-split node.

### 3. The `next` pointer between leaves requires the parent lock

`LeafNode.next: Option<NodeId>` (`node.rs:347`) lets range iterators walk
sideways instead of re-descending the tree. This pointer is only valid
**while the parent's write lock is held**, because a concurrent split can
re-target it.

If you write a range scan, do **not** follow `next` after dropping the
parent lock without re-reading from the parent. The current iterator in
`iter.rs` re-descends — keep that pattern unless you are also adding right-
link support to splits.

### 4. Visibility lives only in the version chain

Rows passing through this tree carry no `xmin`/`xmax` header. Visibility is
resolved exclusively in `VersionChain::get(snapshot)` (`node.rs:318`).

This means:
- The unified store **must** pass a valid `Snapshot` for every read.
- Adding row-header MVCC (postgres style) is a coordinated change that
  requires deprecating this invariant — see `PLAN.md` § Post-MVP.
- Cross-tree consistency (e.g. a row in this btree referenced from a graph
  node) is *not* enforced here. Higher layers must coordinate snapshot
  selection.

### 5. The MVCC coordinator at `src/storage/transaction/coordinator.rs` is dormant

`TransactionManager` in the transaction module is **not wired** into this
btree's read path. The active source of `txn_id` for inserts is the runtime
session (via `next_transaction_id` in `src/storage/wal/transaction.rs`).

Do **not** start using `coordinator::TransactionManager` here without
landing the wiring described in `PLAN.md` § Target 3 prereqs. Mixing the
two would give you two non-overlapping XID spaces.

## Anti-patterns to avoid

- **Cloning `LeafEntry` and mutating the clone** — the clone shares no
  version chain history with the original. Always operate on the original
  via the write guard.
- **Reading `keys` and `entries` separately** — they are parallel arrays
  (`node.rs:343-345`) and must be indexed together.
- **Using `delete` to "undo" an insert** — `delete` appends a tombstone
  version. Snapshots that started before the tombstone still see the row.

## Where to look for examples

- A correct insert: `tree.rs::insert` → `insert_recursive` → `LeafNode::insert`
- A correct read: `tree.rs::find` → `find_leaf` → `LeafEntry::get`
- A correct range: `iter.rs::RangeIter::next`
- A correct GC pass: `gc.rs::compact_versions`
