# Page cache: lock-free read-hit, benign visited-bit races

The page cache (`storage/engine/page_cache.rs`) takes a per-shard `RwLock`; today a cache *hit* upgrades to the write lock solely to flip the SIEVE `visited` bit, and sequential inserts hammering the rightmost leaf turn that into the hottest contention point on the insert path. We will make the **hit path lock-free**: `visited` becomes an atomic flag set without the write lock, while **eviction, insert, remove, and any structural map mutation keep the per-shard write lock**. The cache stays a sharded **SIEVE** (8 shards) — only the metadata layout (cache-line-packed `visited`/tag arrays) and the hit-path locking change; the eviction *policy* is unchanged.

## Considered Options

- **Lock-free hit only (chosen).** Only `visited = true` on hit goes lock-free. Eviction still reads `pin_count`/`dirty` under the lock, so the only races introduced are on the `visited` bit.
- **Full set-associative CLOCK rewrite (rejected).** TigerBeetle's set-associative + CLOCK cache. Rejected because true set-associativity forces *per-set local* eviction, which is CLOCK/second-chance — abandoning SIEVE's global insertion-order semantics that RedDB chose deliberately (NSDI '24). The bottleneck is the hit-path lock, not the eviction policy.
- **Lock-free eviction too (rejected).** Running the eviction hand without the shard lock races the `pin_count`/`dirty` checks against the hand — evicting a pinned or dirty page is a data-loss-class bug, not worth the marginal eviction-path win.

## Consequences

- A lost `visited` update (hit races the eviction hand) is **performance-benign, not a correctness bug**: at worst the page is evicted one SIEVE cycle early and reloaded from disk on next access. Data integrity is unaffected because `pin_count`/`dirty` are never written by the hit path and remain governed by the shard lock on the eviction path.
- This is *not* gated by a before/after benchmark (the effort deliberately trusts the static hot-path mapping rather than landing a perf harness first), so "done" is defined as correctness-preserving + plausibly-faster, not a measured delta.
