# R-Tree (Spatial Index)

R-tree index for efficient spatial queries on geographic data. Built on the `rstar` crate. Supports radius search, bounding box queries, and nearest-neighbor lookups.

## Creating

```sql
CREATE INDEX idx_location ON sites (location) USING RTREE
```

Works with `GeoPoint`, `Latitude`, and `Longitude` column types.

## Queries

See [Spatial Search](/query/spatial-search.md) for full query syntax:

```sql
SEARCH SPATIAL RADIUS 48.8566 2.3522 10.0 COLLECTION sites COLUMN location
SEARCH SPATIAL BBOX 40.0 -10.0 55.0 20.0 COLLECTION sites COLUMN location
SEARCH SPATIAL NEAREST 48.8566 2.3522 K 5 COLLECTION sites COLUMN location
```

## How It Works

The R-tree organizes spatial points into a hierarchical structure of minimum bounding rectangles (MBRs). This enables efficient pruning -- entire subtrees can be skipped when they don't intersect the query region.

- **Radius search** uses a bounding box pre-filter then Haversine distance verification
- **Bounding box** queries directly use the R-tree envelope lookup
- **Nearest-K** uses the R-tree's built-in nearest-neighbor iterator

## Performance

| Operation | Without R-Tree | With R-Tree |
|:----------|:--------------|:------------|
| Radius search | O(n) full scan | **O(log n + k)** |
| Bounding box | O(n) full scan | **O(log n + k)** |
| Nearest-K | O(n log k) | **O(log n + k)** |

## See Also

- [Spatial Search](/query/spatial-search.md) -- Query syntax
- [CREATE INDEX](/query/create-index.md) -- Index creation
- [Geo Types](/types/geo.md) -- GeoPoint, Latitude, Longitude
