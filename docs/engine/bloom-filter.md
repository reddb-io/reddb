# Bloom Filter

Internal probabilistic structure used to avoid unnecessary disk reads. When a query checks for a key, the bloom filter can definitively say "not in this segment" -- skipping a full B-tree scan.

## How It Works

Each segment maintains a bloom filter that tracks all entity IDs and primary keys inserted into it. On read:

1. Query arrives for key X
2. Bloom filter says "definitely not here" → skip segment entirely
3. Bloom filter says "maybe here" → proceed to B-tree lookup

False positive rate: ~1% with optimal sizing. False negative rate: 0% (never misses a key that exists).

## Automatic Integration

Bloom filters are **automatically maintained** -- no configuration required. They are populated on every entity insert via `index_entity()` in the segment system.

## Segment Lifecycle

- **Growing segment**: Bloom filter accepts inserts, populated on every entity write
- **Sealed segment**: Bloom filter is frozen alongside the segment
- **Compaction**: Bloom filters from merged segments are combined via bitwise OR

## Visibility in EXPLAIN

Bloom filter pruning appears in query explain output:

```
Bloom filter pruned 3 of 5 segments
```

## Registry

The `BloomFilterRegistry` manages bloom filters across all segments and collections:

- `register_segment(collection, segment_id)` -- create bloom for new segment
- `add_key(collection, segment_id, key)` -- insert key on entity write
- `candidate_segments(collection, key)` -- return segments that might contain key
- `freeze_segment(collection, segment_id)` -- freeze on seal
- `merge_segments(collection, a, b, new_id)` -- merge on compaction

## See Also

- [Architecture](/engine/architecture.md) -- Storage engine overview
- [B-Tree Index](/engine/btree.md) -- Primary index structure
- [Memtable & Skip List](/engine/memtable.md) -- Write buffer
