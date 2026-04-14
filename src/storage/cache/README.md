# `storage/cache` — SIEVE page cache

This is reddb's buffer pool. It implements the **SIEVE** eviction algorithm:
a single sweeping hand over a circular buffer that clears `visited` bits
until it finds an unvisited entry to evict. Cheaper than CLOCK and
competitive with LRU on real-world workloads.

## Module layout

- `sieve.rs` — `PageCache<V>`, `CacheEntry<V>`, eviction hand
- `mod.rs` — re-exports

After Target 4 (`PLAN.md`), this module also contains:
- `ring.rs` — `BufferRing<V>` for sequential-scan / bulk strategies
- `strategy.rs` — `BufferAccessStrategy` enum

## Invariants

### 1. `visited` is the only signal the SIEVE hand uses

`CacheEntry::visited: AtomicBool` (`sieve.rs:48`) is set on every successful
`get` and cleared by the eviction hand in `evict_one()` (`sieve.rs:340-476`).

The hand sweeps forward; on each entry it checks `visited`:
- **`true`** → clear it and advance.
- **`false`** → that entry is the eviction victim (subject to pin check).

**Do not introduce a second eviction signal** (frequency counters, recency
queues, generation marks). The SIEVE invariant is that this single bit
captures both recency and frequency well enough for the workload — adding
state defeats the simplicity that makes it fast.

### 2. The cache **never** writes to disk

`CacheEntry::dirty: AtomicBool` (`sieve.rs:52`) only marks pages that have
diverged from disk. The cache itself has no I/O code. When a dirty page is
selected for eviction, the cache returns it to the caller, and the caller
(the pager) is responsible for going through `write_page_raw` →
`write_pages_through_dwb` → `wal.flush_until` (post-Target 3).

If you find yourself wanting to call `write` from inside `sieve.rs`, **stop
and add a method to the pager instead**. Keeping the cache I/O-free is what
lets us swap eviction policies and add ring strategies without touching
durability code.

### 3. Pinned entries are immortal until unpinned

`CacheEntry::pin_count: AtomicUsize` (`sieve.rs:54`) > 0 makes an entry
ineligible for eviction. Pinning is the caller's contract: every `pin()`
must be paired with an `unpin()`, and the calling code is responsible for
not leaking pins.

A pin held across a long-running operation **starves the cache** because
the eviction hand has to skip the entry. Keep critical sections small.

### 4. Ring strategies (post-Target 4) are isolated from the main pool

After Target 4, `PageCache::get_with(id, BufferAccessStrategy::SequentialScan)`
routes through a `BufferRing<V>` separate from the main SIEVE pool.

The hard rule: **a hit in the ring must not promote the page into the main
pool**, and a hit in the main pool **must not** populate the ring. The
whole point of the ring is to prevent sequential scans from polluting hot
data — promoting either way breaks that.

When implementing a new strategy, double-check by looking at the
`scan_does_not_pollute_main_pool` test (post-Target 4 in `tests/`).

### 5. `CacheEntry::index` is the slot position and must stay stable

`CacheEntry::index: usize` (`sieve.rs:50`) is the entry's position in the
circular slots array. The eviction hand uses this to walk forward.

**Never reorder slots** — the index is what links a key in the lookup map
to its position. If you need to shuffle entries (e.g. for a defragmenting
GC), update both the slot array and the key→index map atomically under the
write lock.

## Anti-patterns to avoid

- **Calling `get` from inside `evict_one`** — recursive lock.
- **Using `dirty` to track "modified since last query"** — `dirty` is
  *modified since last flush*. Different semantics.
- **Pinning on every read in a loop** — pinning is for "I will use this
  again soon, evict something else." Range scans should use
  `BufferAccessStrategy::SequentialScan` (post-Target 4) or just rely on
  `visited` updates.

## See also

- Pager flush path: `src/storage/engine/pager/impl.rs:686-857`
- DWB (double-write buffer): `src/storage/engine/pager/impl.rs:848`
  (`write_pages_through_dwb`)
- Page format: `src/storage/engine/page.rs`
