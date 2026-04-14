# `storage/index` — Cross-structure index abstraction

This module is the trait layer + reusable primitives that let the planner,
diagnostics, and segment pruning treat every concrete index — btree, hash,
bloom, zone map, HNSW, inverted, graph adjacency, temporal, heavy hitters
— uniformly.

It does **not** replace concrete index implementations. Tables, graphs,
vectors, timeseries, documents, and queues each maintain their own
structures (`src/storage/engine/btree`, `src/storage/engine/hnsw`,
`src/storage/timeseries/temporal_index`, etc.). This module just defines
the common surface those implementations *opt into*.

## Module layout

- `mod.rs` — `IndexBase`, `PointIndex`, `RangeIndex` traits + `IndexError`
- `stats.rs` — `IndexKind` enum (15 families), `IndexStats` cardinality summary
- `bloom_segment.rs` — `BloomSegment` reusable bloom header + `HasBloom` trait
- `zone_map.rs` — `ZoneMap` (min/max/null_count + HLL distinct estimate +
  bloom) with `block_skip(predicate)` helper
- `heavy_hitters.rs` — `HeavyHitters` top-k frequency sketch via Count-Min
- `registry.rs` — `IndexRegistry` central catalog (`(scope, name) →
  Arc<dyn IndexBase>`)

## Invariants

### 1. Every index implements `IndexBase` with `Send + Sync`

`IndexBase` (`mod.rs`) is the minimum contract:

```rust
pub trait IndexBase: Send + Sync {
    fn name(&self) -> &str;
    fn kind(&self) -> IndexKind;
    fn stats(&self) -> IndexStats;
    fn bloom(&self) -> Option<&BloomFilter> { None }
    fn definitely_absent(&self, key_bytes: &[u8]) -> bool { ... }
}
```

Concrete impls are stored as `Arc<dyn IndexBase>` in the registry
(`registry.rs::SharedIndex`). They are shared across threads with no
external locking — internal state must be `Send + Sync` (use `RwLock`,
`DashMap`, atomics; **not** `Cell`, `RefCell`, raw pointers).

If you add a new index type, also add a variant to `IndexKind` (`stats.rs`)
and update `IndexKind::supports_range` / `supports_ann` so the planner can
match on the family.

### 2. Bloom filters are probabilistic — `false` is authoritative, `true` is not

`BloomSegment::contains(key)` returning `true` means **possibly present**.
The caller must follow up with a real lookup.

`BloomSegment::definitely_absent(key)` returning `true` means
**guaranteed absent**. The caller may skip the real lookup entirely.

This is the only safe pruning direction. **Never** use a bloom hit as proof
of presence. The `IndexBase::definitely_absent` default routes through the
attached bloom; concrete impls may override with tighter signals (e.g. zone
map min/max for range indexes — see `ZoneMap::block_skip`).

### 3. Zone maps prune iff the filter is provably disjoint

`ZoneMap` (`zone_map.rs`) tracks `(min_key, max_key, total_count,
null_count, hll, bloom)` per block. `block_skip(&ZonePredicate)` returns
`ZoneDecision::Skip` only when the predicate is **provably** disjoint from
the recorded `[min, max]` window:

- `Equals(k)`: `k < min || k > max` → skip.
- `Range { start, end }`: `end < min || start > max` → skip.
- `IsNull`: `null_count == 0` → skip.
- `IsNotNull`: `non_null_count == 0` → skip.

When in doubt, `MustRead` is the safe answer. Adding a new predicate to
`ZonePredicate` requires deciding the disjoint condition explicitly —
**do not default to `Skip`**.

### 4. `IndexRegistry` is the single mutable mapping `(scope, name) → IndexBase`

`IndexRegistry` (`registry.rs`) is the source of truth for which indexes
exist on which storage objects. Scopes are `Table { table, column }`,
`Graph { collection }`, `Timeseries { series }` — extend the enum if you
add a new storage family.

`StatsProvider::has_index` (`src/storage/query/planner/stats_provider.rs`)
ultimately consults the registry via `RegistryProvider`. **Do not maintain
a parallel index catalog elsewhere.** Two catalogs that disagree turn into
"the planner thinks this index exists but the executor can't find it"
bugs.

### 5. Stats reported by `IndexBase::stats()` must be cheap (O(1))

The planner calls `stats()` during plan construction. It must not allocate
heavy structures, walk the index, or touch disk. If a stat is expensive to
compute, cache it inside the index and refresh on writes — the planner is
on the latency-critical path.

For approximate counts that drift over time (HLL distinct, heavy-hitters
top-k), document the drift in the impl's doc comment.

## Anti-patterns to avoid

- **Bloom + zone map double-pruning without checking both** — if the
  bloom says possibly-present but the zone map says skip, **trust the zone
  map** (it's a stronger signal for range queries).
- **Maintaining `IndexStats` outside the index** — stats live with the
  index they describe. The planner gets them via the trait.
- **Using `Arc<RwLock<dyn IndexBase>>`** — the trait already requires
  `Send + Sync`. Wrapping in `RwLock` adds contention with no benefit.

## See also

- Planner cost integration: `src/storage/query/planner/cost.rs::filter_selectivity`
- Stats provider trait: `src/storage/query/planner/stats_provider.rs`
- Concrete examples: `src/storage/engine/graph_store/secondary_index.rs`,
  `src/storage/timeseries/temporal_index.rs`
