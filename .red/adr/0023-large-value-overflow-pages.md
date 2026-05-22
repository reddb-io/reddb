# ADR 0023: Large-value storage via overflow pages

Status: Proposed (2026-05-22)

Related: [ADR 0003: On-disk format v1.0 stable contract](0003-disk-format-v1.md)

## Context

The B-tree engine stores every value inline in a leaf page and enforces a hard
`MAX_VALUE_SIZE = 1024` bytes (`crates/reddb-server/src/storage/engine/btree.rs`).
Writes above that fail with `BTreeError::ValueTooLarge`. There is no overflow or
blob path, so a value larger than 1 KB simply cannot be stored.

The limit is not arbitrary. Pages are a fixed `PAGE_SIZE = 4096` bytes and a
B-tree leaf must hold at least two cells so it can always split. With both key
and value capped at 1024 (`1024 + 1024 + cell overhead ≈ 2080`), roughly two
cells fit in the ~4056 usable bytes of a leaf. The 1024 cap is effectively
`PAGE_SIZE / 4`, chosen to preserve fanout — not a number we can simply raise
without degrading the tree.

This blocks real workloads. A client ingesting markdown hit the limit with a
44,581-byte chunk. The correct fix is to overflow large values out of the leaf
rather than reject them — the same problem Postgres solves with TOAST (page-sized
slices in a side table, with a pointer left in the tuple), and SQLite/InnoDB
solve with overflow pages.

## Decision

Keep the inline cap as a *threshold for spilling*, not a hard write limit. Add an
overflow path and an LZ4 compression step ahead of it, mirroring TOAST's
"compress, then move out-of-line" sequence.

Constants:

| Constant | Value | Meaning |
| --- | --- | --- |
| `PAGE_SIZE` | `4096` | Unchanged. |
| `OVERFLOW_THRESHOLD` | `1024` | At or below this, store inline as today. Above it, the value spills. Preserves leaf fanout. |
| `MAX_VALUE_SIZE` | `268_435_456` (2^28 = 256 MB) | Above this, still reject with `ValueTooLarge`. |

Write pipeline for a value of length `N`:

1. `N ≤ 1024` → store inline in the leaf (current behavior, unchanged).
2. `N > 1024` → compress with LZ4. If the compressed form is `≤ 1024`, store it
   inline, flagged compressed.
3. Still `> 1024` after compression → write the (compressed) bytes across a chain
   of dedicated overflow pages. The leaf cell holds a pointer to the chain head
   plus the total length and a compression flag.
4. `N > 256 MB` → reject with `BTreeError::ValueTooLarge`.

Read path reverses this: follow the pointer, concatenate the overflow chain,
decompress if flagged. Reads materialize the whole value — there is no partial /
streaming read in this ADR (unlike Postgres `EXTERNAL` substring reads).

### Why 256 MB

The cap is bounded by read cost, not by ambition. With no partial read, every GET
materializes the entire value in memory and walks the full chain:

- payload per overflow page ≈ `4096 − 32 (header) − ~8 (next pointer) ≈ 4056` bytes
- 256 MB ≈ **66,200 chained pages** per value (worst case).

For comparison, Postgres' 1 GB ceiling would be ~264,700 pages here. 256 MB
(2^28) is a deliberate two-bit step down from that — a round power-of-two ceiling
that covers documents and rich payloads while keeping worst-case GET cost
bounded. LZ4 ahead of overflow means the common case (markdown, JSON, text)
rarely approaches it.

## Consequences

- Resolves the reported ingest failure; values up to 256 MB become storable.
- On-disk format changes: a new overflow page type and a leaf-cell flag for
  pointer-vs-inline and compressed-vs-raw. This touches the ADR 0003 stable
  format contract and needs a format-version bump plus migration handling for
  existing v1 files.
- MVCC/WAL interaction must be specified before implementation: overflow chains
  must participate in snapshot visibility and roll back atomically with the
  owning row (Postgres gives the TOAST table its own versions; RedDB needs the
  overflow chain tied to the same MVCC version as its leaf cell). **Open question
  — must be settled before coding.**
- `bulk_insert_sorted` and the B-tree rebuild path
  (`unified/store/impl_pages.rs`, which today *skips* oversized legacy rows) must
  learn to spill instead of skip.
- Free-page management must reclaim overflow pages on delete/update to avoid
  leaks.
- LZ4 adds a compression dependency and CPU cost on the write path; it is applied
  only above the threshold, so small-value writes are unaffected.
