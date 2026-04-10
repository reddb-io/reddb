# Bitmap Index

Roaring bitmap index for blazing-fast analytical queries on low-cardinality columns. Uses the `roaring` crate for compressed bitmaps.

## When to Use

- Columns with few distinct values: status, role, type, boolean flags, category
- `GROUP BY` aggregations
- `COUNT(*)` queries
- Multi-predicate filters with AND/OR

## Creating

```sql
CREATE INDEX idx_status ON orders (status) USING BITMAP
CREATE INDEX idx_role ON users (role) USING BITMAP
```

## How It Works

Each distinct value gets a roaring bitmap of entity offsets. Operations like AND, OR, and NOT are CPU-native bitwise operations on compressed bitmaps.

```
status = "active"   → bitmap {0, 1, 5, 7, 12, ...}
status = "inactive" → bitmap {2, 8, 9, ...}
status = "pending"  → bitmap {3, 4, 6, 10, 11, ...}
```

`SELECT COUNT(*) WHERE status = 'active'` becomes `bitmap["active"].len()` -- O(1).

`WHERE status = 'active' AND role = 'admin'` becomes `bitmap_and(status_active, role_admin)` -- sub-millisecond on millions of rows.

## Performance

| Query | Without Bitmap | With Bitmap |
|:------|:--------------|:------------|
| `COUNT(*) WHERE status = 'active'` | O(n) full scan | **O(1)** |
| `GROUP BY status` | O(n) scan + hash | **O(k)** k = distinct values |
| `WHERE a = 'x' AND b = 'y'` | O(n) scan | **O(min(a,b))** bitwise AND |

## When NOT to Use

- High-cardinality columns (> 1000 distinct values) -- use Hash or B-tree instead
- Range queries -- use B-tree
- Spatial queries -- use R-tree

## See Also

- [CREATE INDEX](/query/create-index.md) -- Index creation syntax
- [Hash Index](/engine/hash-index.md) -- High-cardinality exact match
- [B-Tree Index](/engine/btree.md) -- Range queries
