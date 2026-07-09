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

### H3

H3 spatial index. Encodes a geographic point to a 64-bit hexagonal cell id and stores it in the disk-paged sorted index, so spatial queries prune to a ring of cells instead of scanning.

```sql
CREATE INDEX idx_location ON sites (location) USING H3
CREATE INDEX idx_location ON sites (location) USING H3 (9)   -- explicit resolution, 0..=15
```

Indexes `GEOPOINT` columns and document fields holding a `{lat, lon}` object, including dotted paths (`telemetry.gps`). The resolution defaults to `9`. `USING SPATIAL` is an alias for the default spatial backend, which is H3 at resolution 9.

**Best for:** Radius search, bounding box queries, nearest-neighbor, polygon geofences. See the [Spatial Search guide](/guides/spatial-search.md).
**Complexity:** O(cover + candidates) — the index is a pure optimization, so results match a full scan exactly.

> `USING RTREE` was removed. The in-RAM R-tree indexed nothing and served no queries; use `USING H3`. See [Migration note](/guides/spatial-search.md#migration-note-using-rtree-was-removed).

## Choosing the Right Index

| Query Pattern | Index Type |
|:--------------|:-----------|
| `WHERE id = 42` | HASH |
| `WHERE price > 100` | BTREE |
| `WHERE status = 'active'` | BITMAP |
| `SEARCH SPATIAL RADIUS 48.85 2.35 10.0 ...` | H3 |
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
- [Spatial Search guide](/guides/spatial-search.md) -- The complete H3 surface, end to end
- [Spatial Search reference](/query/spatial-search.md) -- `SEARCH SPATIAL` grammar
- [B-Tree Index](/engine/btree.md) -- B-tree internals
