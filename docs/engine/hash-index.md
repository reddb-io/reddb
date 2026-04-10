# Hash Index

O(1) exact-match lookups using an in-memory hash table. Significantly faster than B-tree for equality queries (`WHERE col = value`).

## When to Use

- Primary key lookups
- Email/username uniqueness
- API key validation
- Any `WHERE col = exact_value` pattern

## Creating

```sql
CREATE INDEX idx_email ON users (email) USING HASH
CREATE UNIQUE INDEX idx_api_key ON tokens (key) USING HASH
```

## How It Works

The hash index maintains a `HashMap<Vec<u8>, Vec<EntityId>>` mapping column values to entity IDs. Multi-valued by default (one key can map to multiple entities). When `UNIQUE` is specified, duplicate keys are rejected at insert time.

## Performance

| Operation | B-Tree | Hash |
|:----------|:-------|:-----|
| Exact match (`=`) | O(log n) | **O(1)** |
| Range (`>`, `<`, `BETWEEN`) | O(log n + k) | Not supported |
| Ordering (`ORDER BY`) | O(1) with scan | Not supported |

## Trade-offs

- No range queries or ordering (use B-tree for those)
- Higher memory per entry than B-tree (~48 bytes vs ~64 bytes, but no tree overhead)
- Optimal for high-cardinality equality lookups

## See Also

- [CREATE INDEX](/query/create-index.md) -- Index creation syntax
- [B-Tree Index](/engine/btree.md) -- Range-capable index
- [Bitmap Index](/engine/bitmap-index.md) -- Low-cardinality index
