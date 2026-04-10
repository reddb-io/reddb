# CREATE INDEX

Create secondary indexes on collections for faster queries. RedDB supports four index types, each optimized for different workloads.

## Syntax

```sql
CREATE [UNIQUE] INDEX [IF NOT EXISTS] <name> ON <table> (<columns>) [USING <method>]
DROP INDEX [IF EXISTS] <name> ON <table>
```

## Index Methods

### BTREE (Default)

Balanced tree index. Best for range queries (`<`, `>`, `BETWEEN`, `ORDER BY`).

```sql
CREATE INDEX idx_created ON events (created_at)
CREATE INDEX idx_price ON products (price) USING BTREE
```

**Best for:** Range scans, sorting, ordered iteration.
**Complexity:** O(log n) lookup, insert, delete.

### HASH

Hash table index. O(1) exact-match lookups, significantly faster than B-tree for equality queries.

```sql
CREATE INDEX idx_email ON users (email) USING HASH
CREATE UNIQUE INDEX idx_api_key ON tokens (key) USING HASH
```

**Best for:** `WHERE col = value` queries, primary key lookups, unique constraints.
**Complexity:** O(1) average lookup.
**Trade-off:** Cannot do range queries or ordering.

### BITMAP

Roaring bitmap index. Extremely efficient for columns with few distinct values (low cardinality).

```sql
CREATE INDEX idx_status ON orders (status) USING BITMAP
CREATE INDEX idx_role ON users (role) USING BITMAP
```

**Best for:** `WHERE status = 'active'`, `GROUP BY status`, `COUNT(*)` on categorical columns.
**Complexity:** O(1) for count, sub-millisecond AND/OR/NOT across millions of rows.
**Trade-off:** Memory-efficient only for low-cardinality columns (< 1000 distinct values).

### RTREE

R-tree spatial index. Required for efficient geo queries on `GeoPoint`, `Latitude`, `Longitude` columns.

```sql
CREATE INDEX idx_location ON sites (location) USING RTREE
```

**Best for:** Radius search, bounding box queries, nearest-neighbor. See [Spatial Search](/query/spatial-search.md).
**Complexity:** O(log n) for spatial queries.

## Choosing the Right Index

| Query Pattern | Index Type |
|:--------------|:-----------|
| `WHERE id = 42` | HASH |
| `WHERE price > 100` | BTREE |
| `WHERE status = 'active'` | BITMAP |
| `WHERE location WITHIN 10km OF (48.85, 2.35)` | RTREE |
| `ORDER BY created_at DESC` | BTREE |
| `GROUP BY category` | BITMAP |
| `SELECT COUNT(*) WHERE role = 'admin'` | BITMAP |

## Unique Indexes

```sql
CREATE UNIQUE INDEX idx_email ON users (email) USING HASH
```

Unique indexes reject duplicate values on insert. Works with HASH and BTREE methods.

## IF NOT EXISTS / IF EXISTS

```sql
CREATE INDEX IF NOT EXISTS idx_email ON users (email) USING HASH
DROP INDEX IF EXISTS idx_email ON users
```

## See Also

- [CREATE TABLE](/query/create-table.md) -- Creating collections
- [Spatial Search](/query/spatial-search.md) -- Using R-tree indexes
- [B-Tree Index](/engine/btree.md) -- B-tree internals
