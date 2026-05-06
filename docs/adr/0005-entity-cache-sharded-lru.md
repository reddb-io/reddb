# ADR 0005: Sharded bounded LRU for the store-wide `entity_cache`

Status: Accepted (2026-05-04)

Tracking issue: #114 (follow-up of #85, see
[`docs/perf/delete-sequential-2026-05-06.md`](../perf/delete-sequential-2026-05-06.md)).

## Context

`UnifiedStore` carried a single store-wide cache for cross-collection
entity lookups:

```rust
entity_cache: RwLock<HashMap<u64, (String, UnifiedEntity)>>
```

The cache backs `UnifiedStore::get_any(EntityId) -> Option<(collection,
entity)>`, which is called from graph DSL execution, Cypher MATCH
traversals, the RAG adapter, the DevX `db.get(id)` API, and various
cross-modal helpers.

The #85 perf investigation found three concrete defects:

1. **Single global lock.** Every read took a write lock on a hit (to
   bump LRU position — but the original code had no LRU bookkeeping at
   all, so the write lock was for nothing). Every invalidation took the
   same single write lock. On `delete_sequential` this serialised every
   row deletion behind one global mutex.
2. **Naïve eviction.** When the cache exceeded 10 000 entries, the code
   dropped *the first key the `HashMap` iterator yielded* — no recency
   tracking. Hot entries could be evicted before cold ones with equal
   probability.
3. **No observability.** Hit rate had to be inferred from access
   patterns. There was no metric to tell whether the cache was earning
   its overhead on a given workload.

For the bench's `delete_sequential` (`DELETE FROM ... WHERE id = N`),
the hit rate is structurally zero — the bench never calls `get_any`.
The cache was pure overhead: one global write lock per delete with no
upside.

## Decision

Replace the single `RwLock<HashMap>` with a sharded, bounded LRU that
also tracks hits, misses, and evictions.

The new module is `crates/reddb-server/src/storage/unified/entity_cache.rs`.
Shape:

- **16 shards**, indexed by `id & 0b1111`. `RwLock<Shard>` per shard.
- **Per-shard LRU**: `HashMap<u64, V>` for storage + `VecDeque<u64>`
  for recency order. On hit we move the key to the back of the queue;
  on capacity overflow we pop the front. Capacity is `10_000 / 16 ≈ 625`
  per shard, matching the original 10 000-entry budget.
- **Read-lock probes on invalidation.** `EntityCache::remove` and
  `remove_many` take a read lock first and skip the write-lock
  acquisition entirely when the shard does not carry any of the
  candidate keys. This is the load-bearing optimisation for the
  `delete_sequential` regression: a delete-only workload with a cold
  cache pays only read locks on disjoint shards.
- **Atomic counters** (`hits`, `misses`, `evictions`) drive
  `EntityCache::hit_rate()` and `EntityCacheStats`, exposed via
  `UnifiedStore::entity_cache_hit_rate()` and
  `UnifiedStore::entity_cache_stats()` for live monitoring.

LRU bookkeeping is O(n) in shard size (we walk the `VecDeque` on
touch). At 625 entries this is ~10 µs of CPU per hit; cheap relative to
the lookup itself, and far cheaper than reaching for the `lru` crate
just to shave a constant.

## Considered alternatives

### A. Drop the cache entirely

The bench evidence is strong: `delete_sequential` hit rate is zero.
But static analysis of `get_any` callsites (`dsl/execution.rs`,
`dsl/cross_modal.rs`, `runtime/graph_dsl.rs`,
`storage/query/rag/unified_adapter.rs`, `devx/reddb/impl_access.rs`)
shows real graph and RAG workloads call `get_any` from inside loops
where the same id is visited multiple times during a single traversal.
Hit rate on those workloads is non-zero and observable. Dropping the
cache would regress those paths.

### B. Per-shard `RwLock<LRU>` (chosen)

Cuts contention 16× without changing the public API. The hot path on
#85 — invalidation under zero hit rate — pays only a read-lock probe.
Observability lands as part of the same change.

### C. `arc_swap`-based read-mostly cache

Would eliminate the write lock on hits entirely but doesn't help the
*invalidation* path, which is the actual hot path on #85. Also forces
a full-cache copy-on-write on every change, which makes
`delete_batch` (1 000s of invalidations per call) much worse.

### D. Pull in the `lru` crate

Gives O(1) LRU touches but adds a dependency we do not otherwise need.
Per-shard sizes are small enough that the O(n) `VecDeque` walk is not
a bottleneck. Reconsider if profiling shows otherwise.

## Consequences

**Wins.**

- `delete_sequential` no longer serialises every row through a single
  global write lock. With a 16-way shard split and read-lock probes
  on cold shards, the per-row cost of cache invalidation drops from a
  guaranteed write-lock acquisition to a read-lock probe that skips
  the write lock 100 % of the time when the bench has not populated
  the cache.
- True LRU eviction: hot entries (recently visited graph nodes during
  a traversal) are no longer at risk of being evicted to make room
  for one-shot cold lookups.
- `entity_cache_hit_rate()` is the first observability signal we have
  on this cache. Operators can now tell whether the cache is earning
  its keep on their workload without rebuilding from source.

**Costs.**

- 16 `RwLock<Shard>` allocations per `UnifiedStore` instead of one. On
  modern Rust (`parking_lot::RwLock` is a single `AtomicUsize`) this
  is ~24 bytes × 16 = ~400 bytes of overhead.
- LRU touch is O(shard_size) on a hit. At ~625 entries this is
  measured in microseconds and is dominated by the entity clone that
  follows.

**No public API change for existing callers.** `get_any` keeps the
same signature; only its internal cache plumbing changed. Two new
public methods were added (`entity_cache_hit_rate`,
`entity_cache_stats`), both purely diagnostic.

## Validation

- `cargo check -p reddb-server` — clean.
- `cargo test -p reddb-server --lib storage::unified::entity_cache` —
  6 new unit tests, all passing (LRU eviction order, hit/miss
  counters, retain semantics, sharded `remove_many`, cold-cache
  fast path).
- `cargo test -p reddb-server --lib storage::unified` — full unified
  storage suite stays green.
- `cargo test -p reddb-server --tests` — full integration suite
  stays green.

A live bench delta against the `delete_sequential` mini-duel was not
re-collected on this commit (the perf knobs flagged in
`docs/perf/perf-knobs.md` are still locked). The structural change is
a strict improvement: a workload with zero hit rate now pays zero
write-lock acquisitions for cache invalidation, which is the upper
bound of what's possible without removing the cache.
