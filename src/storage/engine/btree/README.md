# `storage/engine/btree` — Page-based on-disk B+tree

This is the **on-disk** B+tree used by the durable storage engine. Pages live
in the SIEVE buffer pool (`src/storage/cache/`), are flushed by the pager
(`src/storage/engine/pager/`), and recovered from the WAL on startup.

> **Sister module:** `src/storage/btree/` is the *in-memory* MVCC B+tree
> with version chains. It has different invariants. Do not copy code between
> the two without re-reading both READMEs.

## Module layout

- `impl.rs` — `BTree` root, `insert`/`delete`/`search`/`range`
- Page format lives in `src/storage/engine/page.rs` — see that module's
  header comment.

## Invariants

### 1. Lock order is root → child, never the inverse

When a write path holds a child page latch, **it must not acquire the
parent latch**. The lock order is strictly root-down.

If you need to modify a parent (split propagation, root replacement),
**release the child first**, re-acquire root → child path with write locks
top-down. This is the rule that prevents deadlock between concurrent
inserters.

### 2. Splits are not latch-coupled — acquire the parent write lock first

Reddb's current split path is **not** the postgres Lehman-Yao right-link
scheme. We do not have right-sibling pointers in interior pages, and
readers cannot tolerate a split in progress.

Concrete rule: before splitting a child node, acquire the parent's write
lock. Hold it across the entire split (allocate sibling, copy keys, update
parent separator). Releasing early lets a concurrent reader miss the new
sibling.

Adopting Lehman-Yao is tracked in `PLAN.md` § Post-MVP. **Do not start
following sibling pointers in readers** until that work lands — the
sibling pointer field exists in the page header but is currently maintained
only by writers.

### 3. Every mutation must stamp `page.header.lsn`

Page header (`src/storage/engine/page.rs:135-160`) reserves bytes 20-28 for
`lsn: u64`. After Target 3 (`PLAN.md` § WAL-first), every mutation **must**:

1. Append a WAL record describing the change via `wal.append(...)` and
   capture the returned LSN.
2. Stamp the modified page's `header.lsn` with that LSN before releasing
   the page back to the cache.

Pages with `lsn == 0` are treated as "no WAL guarantee" and are flushed
without consulting the WAL — this is correct for freelist and header pages
that are guarded by the double-write buffer (DWB), but **wrong** for any
page that contains user data.

If you add a new write path: grep for `wal.append` to find the canonical
pattern, mirror it, and verify with the audit `grep -rn 'write_page(' src/`.

### 4. `right_child` is meaningful only on interior pages

`PageHeader.right_child: u32` (`page.rs:155`) is the page id of the
right-most child of an interior node. On leaf pages this field must be
**zero** and is ignored on read. Writers that confuse the two will scribble
on the leaf's data area on round-trip.

The page type discriminant (`page_type: PageType`) is the source of truth
for which fields are live — branch on it, not on heuristics.

### 5. Checksum invalidates on any byte mutation

`PageHeader.checksum: u32` (`page.rs:159`) is a CRC32 of the page content.
It is recomputed in `Page::update_checksum()` and verified on `Page::load()`.

Mutating any byte without recomputing the checksum **silently corrupts the
page** on the next read after eviction. The pager's write path is
responsible for calling `update_checksum()` — if you bypass the pager (e.g.
to construct a freelist trunk page in a test), you must call it yourself.

## Anti-patterns to avoid

- **Reading `right_child` without checking `page_type`** — undefined on
  leaves.
- **Writing through the cache without `mark_dirty`** — the page never gets
  flushed and survives only as long as it stays pinned.
- **Holding a page latch across an `await` point** — the page cache is
  synchronous; locking across awaits is a deadlock waiting to happen.

## See also

- WAL: `src/storage/wal/README.md` (TODO) and `src/storage/wal/writer.rs`
- Buffer pool: `src/storage/cache/README.md`
- Page format: `src/storage/engine/page.rs:114-160`
