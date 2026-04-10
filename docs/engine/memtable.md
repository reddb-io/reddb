# Memtable & Skip List

The memtable is an in-memory write buffer backed by a sorted skip list. All writes go to the memtable first, then flush to the B-tree when a size threshold is reached.

## Write Path

```
1. WAL write (durability)
2. Memtable insert (skip list -- in memory, ordered)
3. Background flush to B-tree when threshold reached
```

## Read Path

```
1. Check memtable first (most recent writes)
2. Check sealed segments (B-tree)
3. Merge results (memtable takes precedence)
```

## Skip List

The skip list provides O(log n) insert, lookup, and range scan with sorted iteration. It serves as the backing structure for the memtable.

Key operations:

| Operation | Complexity |
|:----------|:-----------|
| Insert | O(log n) |
| Lookup | O(log n) |
| Range scan | O(log n + k) |
| Drain sorted | O(n) |

## Memtable Features

- **Tombstone markers**: Deletes insert a tombstone so reads don't fall through to older segments
- **Size tracking**: Approximate byte tracking for flush decisions
- **Configurable threshold**: Flush when memtable reaches 75% of max size (default 64 MB)
- **Sorted drain**: On flush, entries are drained in sorted order for efficient B-tree bulk insert

## Configuration

| Parameter | Default | Description |
|:----------|:--------|:------------|
| `max_bytes` | 64 MB | Maximum memtable size before flush |
| `flush_threshold` | 0.75 | Flush at 75% of max_bytes |

## See Also

- [B-Tree Index](/engine/btree.md) -- Primary index structure
- [WAL & Recovery](/engine/wal.md) -- Write-ahead log
- [Architecture](/engine/architecture.md) -- Storage engine overview
