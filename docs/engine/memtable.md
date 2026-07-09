# Write & Read Paths

The unified storage layer does not use a write-buffer memtable. Writes land
directly in the growing segment's entity storage; the memtable module has been
removed (ADR 0073 §5).

## Write Path

```
1. WAL write (durability)
2. Growing-segment entity storage in RAM
   - HashMap mode (default): keyed by EntityId, random-access O(1)
   - Flat-vector mode: epoch-published Vec for lock-free sequential reads
3. seal() — freezes the growing segment; builds bloom filter and zone maps
   over the now-immutable data, producing a Sealed segment
```

## Read Path

```
1. Growing segment entity storage (most recent writes)
2. Sealed segments (older, immutable; bloom filter and zone maps applied for pruning)
```

Reads never consult a write buffer. The growing segment and sealed segments are
the two tiers consulted, in that order.

## Segment Lifecycle

```
Growing (in-memory, accepts writes)
   ↓ seal() when full or manually triggered
Sealed (immutable, bloom filter + zone maps built)
   ↓ flush() for persistence
Flushed (on disk, can be mmap'd)
   ↓ archive() for cold storage
Archived (compressed, infrequently accessed)
```

## See Also

- [B-Tree Index](/engine/btree.md) -- Primary index structure
- [WAL & Recovery](/engine/wal.md) -- Write-ahead log
- [Bloom Filter](/engine/bloom-filter.md) -- Fast negative key lookups
- [Architecture](/engine/architecture.md) -- Storage engine overview
