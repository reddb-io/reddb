# Mixed bulk kind-index correctness

A `SegmentManager::bulk_insert` batch containing rows, vectors, graph nodes and
graph edges registered every ID under the first item's storage type. As a
result, `get_by_kind("table")` could return graph/vector items, and a lookup of
their actual kind could omit them. The regression fails on parent
`c5f667e795bb8b8044cb5665f9ec056aec40b5ce` in flat storage, HashMap storage and
flat storage with ID gaps, before any segment is sealed.

Bulk ingestion now indexes runs of consecutive items with the same storage
type. A homogeneous batch retains batched reservation; mixed batches need no
per-item key-string allocation. `iter_kind` borrows the existing ID set under
the segment borrow/read guard instead of cloning it for every lookup.

The regression compares exact result IDs and types with a full scan, rotating
the first kind and checking growing segments, sealing, physical deletions and
paced consolidation. This exercises the storage manager directly; it is not a
claim about transaction snapshots, crash recovery or every public query path.

```sh
cargo test --locked --test grouped_chaos_drill_persistence mixed_bulk_kind_index -- --test-threads=1
```

Cost sketch: for N batch items and R consecutive type runs, O(N) type comparisons
and ID insertions, O(R) map lookups/reservations, key strings only for newly seen
types. Indexing now makes a separate pass over the batch before payload ingestion.
Kind scans still visit O(segment items) and clone matching result payloads, but
no longer allocate/copy the O(kind items) ID set. No network, disk, fsync, WAL
record, persistent format or locking protocol is added.

## Remaining pruning prerequisites

This change does **not** enable graph segment pruning. `SegmentStats` currently
counts only HashMap entities, omitting flat storage. The update indexing path
assumes an immutable kind, while mutable segment access does not enforce that
invariant. Neither summary can yet justify skipping a segment: mutable updates,
physical versions and fallback/invalidation need a separate correctness slice.
No SQLite, PostgreSQL or SurrealDB performance parity follows from this fix.
