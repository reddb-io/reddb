# H3 Index (Spatial)

Spatial index over geographic points, built on Uber's H3 hexagonal grid and stored in the engine's ordinary disk-paged sorted (B-tree) index. Supports radius search, bounding box queries, nearest-neighbor lookups, and polygon geofences.

For the user-facing surface — coordinate shapes, resolution choice, `SEARCH SPATIAL` — read the [Spatial Search guide](/guides/spatial-search.md).

## Creating

```sql
CREATE INDEX idx_location ON sites (location) USING H3
CREATE INDEX idx_location ON sites (location) USING H3 (9)
```

Works with `GEOPOINT` columns and with document fields holding a `{lat, lon}` object, including dotted paths. The resolution defaults to `9` and must be in `0..=15`.

## How It Works

A `(lat, lon)` pair encodes to a 64-bit **cell id** at the index's resolution. That integer is the index key — so an H3 index is a sorted index of `u64`s, with no per-point resident structure and no bespoke tree.

A query covers its search area with a ring of cells, gathers the entities in those cells as **candidates**, then post-filters them exactly:

- **Radius search** sizes a kRing as `ceil(radius_km / edge_km) + 1` around the centre cell, then verifies each candidate with the haversine distance.
- **Bounding box** covers the box's cells, then compares coordinates directly.
- **Nearest-K** grows the ring outward until K candidates are found, then sorts by haversine distance.
- **Polygon** tiles the polygon, then applies an even-odd point-in-polygon test.

Cell ids are hierarchical: truncating one yields its coarser parent, which is what `H3_CELL` and `H3_PARENT` expose to `SELECT`.

## The cover is a superset

The cell ring never excludes a point inside the search area, and the exact post-filter runs on both the indexed and the unindexed path. **An H3 index changes how fast a spatial query runs, never what it returns** — same rows, same order, bit-for-bit identical distances.

RedDB falls back to a full scan when there is no H3 index, when a coordinate cannot be encoded, or when the cover grows past its cap — 128 kRing steps for `RADIUS`, 64 rings of outward expansion for `NEAREST` — at which point scanning the collection is cheaper than enumerating the cells.

## Writes

Indexing a point is a single `u64` insert into the sorted index. `CREATE INDEX` backfills from existing entities and reports the coverage as `K of N entities indexed`; `INSERT`, `UPDATE`, and `DELETE` maintain the index incrementally like any other.

Values that are not recognized geographic points are skipped rather than rejected — an entity with a malformed coordinate keeps its data and is simply absent from the index, exactly as an unsupported value is absent from a B-tree index.

## Performance

| Operation | Without H3 | With H3 |
|:----------|:-----------|:--------|
| Radius search | O(n) full scan | O(cover + candidates) |
| Bounding box | O(n) full scan | O(cover + candidates) |
| Nearest-K | O(n log k) | O(cover + candidates) |

The cover term is why resolution matters: too fine and the ring is enormous, too coarse and the candidate set is fat. See [Choosing a resolution](/guides/spatial-search.md#choosing-a-resolution).

## Migration from RTREE

`USING RTREE` was removed — the in-RAM R-tree indexed nothing and served no queries. Persisted rtree descriptors are dropped with a warning on load. See the [migration note](/guides/spatial-search.md#migration-note-using-rtree-was-removed).

## See Also

- [Spatial Search guide](/guides/spatial-search.md) -- The complete spatial surface
- [Spatial Search reference](/query/spatial-search.md) -- Query syntax
- [CREATE INDEX](/query/create-index.md) -- Index creation
- [B-Tree Index](/engine/btree.md) -- The sorted index the cells live in
- [Geo Types](/types/geo.md) -- GeoPoint, Latitude, Longitude
