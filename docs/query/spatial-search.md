# Spatial Search

This page is the `SEARCH SPATIAL` grammar reference. For the full picture — how coordinates get in, how to pick a resolution, and what the H3 index does or does not change about your results — read the [Spatial Search guide](/guides/spatial-search.md).

RedDB runs spatial queries over `GEOPOINT` columns and over document fields holding a `{lat, lon}` object, accelerated by an H3 index. Find points within a radius, bounding box, polygon, or nearest neighbors. Without an index, RedDB falls back to an exact full scan that returns identical results.

## Prerequisites

Create an H3 index on the spatial column:

```sql
CREATE INDEX idx_location ON sites (location) USING H3
```

`USING SPATIAL` is accepted as an alias and resolves to the same H3 disk index.

## Radius Search

Find all points within a given distance from a center point.

```sql
SEARCH SPATIAL RADIUS <lat> <lon> <radius_km>
  COLLECTION <collection> COLUMN <column> [LIMIT <n>]
```

**Example:** Find sites within 10 km of Paris (48.8566, 2.3522):

```sql
SEARCH SPATIAL RADIUS 48.8566 2.3522 10.0 COLLECTION sites COLUMN location LIMIT 50
```

`LIMIT` defaults to 100. Returns results sorted by distance ascending:

| Column | Description |
|:-------|:------------|
| `entity_id` | RedDB entity id of the matching point |
| `distance_km` | Haversine distance in kilometers |

## Bounding Box Search

Find all points within a latitude/longitude rectangle.

```sql
SEARCH SPATIAL BBOX <min_lat> <min_lon> <max_lat> <max_lon>
  COLLECTION <collection> COLUMN <column> [LIMIT <n>]
```

**Example:** Find sites in a box covering central Europe:

```sql
SEARCH SPATIAL BBOX 40.0 -10.0 55.0 20.0 COLLECTION sites COLUMN location LIMIT 100
```

`LIMIT` defaults to 100. Returns a single `entity_id` column — a box has no centre to measure a distance from.

## Nearest Neighbor Search

Find the K closest points to a location.

```sql
SEARCH SPATIAL NEAREST <lat> <lon> K <k>
  COLLECTION <collection> COLUMN <column>
```

**Example:** Find the 5 closest sites to Brussels:

```sql
SEARCH SPATIAL NEAREST 50.8503 4.3517 K 5 COLLECTION sites COLUMN location
```

`K` is required and caps the result; `NEAREST` takes no `LIMIT`. Returns `entity_id` and `distance_km`, sorted by distance ascending.

## Polygon Search (Geofencing)

Find all points inside a polygon. H3 polygon coverage prunes candidates from the index; an exact even-odd point-in-polygon post-filter decides the final result, so indexed and full-scan results are always identical.

```sql
SEARCH SPATIAL WITHIN POLYGON ((<lat> <lon>), (<lat> <lon>), (<lat> <lon>)[, ...])
  COLLECTION <collection> COLUMN <column> [LIMIT <n>]
```

**Example:** Find couriers inside a delivery zone:

```sql
SEARCH SPATIAL WITHIN POLYGON ((38.70 -77.20), (38.80 -77.20), (38.80 -77.05), (38.70 -77.05))
  COLLECTION couriers COLUMN current
```

`LIMIT` defaults to 100. Returns a single `entity_id` column.

### Polygon rules

- At least three vertices are required.
- Latitude must be in `-90..=90`; longitude must be in `-180..=180`.
- Points exactly on a vertex or edge are treated as inside.
- Self-intersecting polygons are accepted and resolved with the standard even-odd rule.
- Polygons crossing the antimeridian (the 180°/-180° meridian) are rejected.

### Cover caps and fallback

When the H3 cover of a polygon would exceed the engine's maximum cell budget (for example, a continent-scale polygon at fine resolution), the query automatically falls back to a full collection scan. Results remain byte-identical to the indexed path because the exact even-odd post-filter is always applied.

---

## Composable Geo Predicates

`GEO_DISTANCE` is a scalar function that can appear in SELECT projections, WHERE filters, and ORDER BY clauses. This lets you compose spatial filtering with arbitrary SQL predicates in a single query.

```sql
-- Projection: compute distance for each row
SELECT name, GEO_DISTANCE(location, POINT(48.8566, 2.3522)) AS dist_km
FROM sites

-- Filter: keep only rows within a radius
SELECT name, GEO_DISTANCE(location, POINT(48.8566, 2.3522)) AS dist_km
FROM sites
WHERE GEO_DISTANCE(location, POINT(48.8566, 2.3522)) < 10.0

-- Order by distance
SELECT name, GEO_DISTANCE(location, POINT(48.8566, 2.3522)) AS dist_km
FROM sites
WHERE GEO_DISTANCE(location, POINT(48.8566, 2.3522)) < 10.0
ORDER BY dist_km
LIMIT 20
```

You can also pass latitude and longitude as separate numeric literals:

```sql
SELECT name, GEO_DISTANCE(location, 48.8566, 2.3522) AS dist_km
FROM sites
WHERE GEO_DISTANCE(location, 48.8566, 2.3522) < 10.0
ORDER BY dist_km
LIMIT 20
```

Mix spatial and non-spatial predicates freely:

```sql
SELECT name, GEO_DISTANCE(location, POINT(48.8566, 2.3522)) AS dist_km
FROM stores
WHERE GEO_DISTANCE(location, POINT(48.8566, 2.3522)) < 5.0
  AND category = 'bakery'
ORDER BY dist_km
LIMIT 10
```

### H3 index acceleration

When an H3 index exists on the column used in a `GEO_DISTANCE` WHERE predicate, the planner substitutes an H3 ring lookup for the full collection scan. Use `EXPLAIN` to confirm the route:

```sql
EXPLAIN SELECT name
FROM stores
WHERE GEO_DISTANCE(location, POINT(48.8566, 2.3522)) < 5.0
```

An accelerated plan shows `geo_h3_index_seek` in the operator list. A plan without an H3 index shows a plain table scan instead.

### Parity guarantee

The H3 index is a pure speed optimization. Every result row returned by an indexed query is also returned by the equivalent full scan, and vice versa — the index never changes results, only the number of rows the engine examines before the Haversine post-filter. You do not need to re-verify results after adding an index.

---

## GeoJSON and Recognized Shapes

RedDB recognizes geographic coordinates from two value shapes. Any other shape is silently skipped rather than erroring, so rows with unrecognized coordinates are excluded from spatial results without blocking the query.

### Accepted shapes

| Shape | Example | Notes |
|:------|:--------|:------|
| Native `GeoPoint` column | `'48.8566,2.3522'` | Stored internally as microdegrees |
| JSON object with numeric lat/lon fields | `{"lat": 48.8566, "lon": 2.3522}` | Also accepts `latitude`, `longitude`, `lng` as field names |

Field names are checked in priority order: `lat`/`latitude` for latitude, `lon`/`lng`/`longitude` for longitude. Values must be numeric (integer or float). String-typed coordinate values are not recognized.

### Rejected shapes

| Shape | Reason |
|:------|:-------|
| Standard GeoJSON Point `{"type":"Point","coordinates":[-77.15, 38.76]}` | The `coordinates` array uses **longitude-first** order (GeoJSON RFC 7946 §3.1.1). RedDB's recognizer reads `lat`/`lon` named fields only — it does not parse the `coordinates` array. |
| JSON with string coordinate values `{"lat":"38.76","lon":"-77.15"}` | Coordinate values must be numeric |
| Out-of-range coordinates `{"lat":91.0,"lon":0.0}` | Latitude must be in `-90..=90` |
| Missing fields `{"lat":38.76}` | Both `lat`/`lon` (or equivalent) are required |
| Plain text `'38.76,-77.15'` | Text strings are not parsed as coordinates |

> **Standard GeoJSON note:** If your data uses the GeoJSON `{"type":"Point","coordinates":[lon,lat]}` format, extract the coordinates into `lat`/`lon` named fields before inserting, or use a document body path that points to the numeric values directly.

### Notice on empty spatial results

When a spatial query returns no rows and no geo-valued coordinates are found in the scanned collection, RedDB attaches a notice to the result:

```
no entity in 'couriers' has an indexable geo value in column 'current'
(expected GEO_POINT or {lat, lon} object).
```

This notice helps distinguish "no matches in range" from "column has no recognized coordinates."

---

## Spatial Analytics

`H3_CELL` and `H3_PARENT` turn geographic columns into H3 cell identifiers that can be grouped, counted, and joined like any other column. This is the foundation for heatmaps, regional rollups, and proximity aggregations.

### H3_CELL — point to cell

`H3_CELL(col, resolution)` maps a geographic point to its H3 cell id at the given resolution. Resolution ranges from `0` (continent scale, ~4,250 km edge length) to `15` (building scale, ~0.5 m edge length).

```sql
H3_CELL(col, resolution)  -- returns UInt64 cell id, or NULL for invalid input
```

Resolution must be an integer in `0..=15`. Values outside this range return an error.

**Resolution reference:**

| Resolution | Approximate edge length | Typical use |
|:-----------|:------------------------|:------------|
| 3 | ~60 km | Country-level |
| 5 | ~9 km | City-level |
| 7 | ~1.2 km | Neighborhood |
| 9 | ~170 m | City block |
| 12 | ~10 m | Building footprint |

### Heatmap recipe

Count points per H3 cell to build a heatmap:

```sql
CREATE TABLE checkins (id INT, name TEXT, location GEOPOINT);
INSERT INTO checkins (id, name, location) VALUES
  (1, 'Louvre',        '48.8606,2.3376'),
  (2, 'Eiffel Tower',  '48.8584,2.2945'),
  (3, 'Sacre-Coeur',   '48.8867,2.3431'),
  (4, 'Notre-Dame',    '48.8530,2.3499'),
  (5, 'Marais',        '48.8566,2.3522');

SELECT H3_CELL(location, 7) AS cell, COUNT(*) AS visits
FROM checkins
GROUP BY cell
ORDER BY visits DESC
LIMIT 20
```

Rows where `location` does not carry a recognized geo value are automatically excluded from the `GROUP BY` (they produce a `NULL` cell and are dropped).

### H3_PARENT — coarser resolution rollup

`H3_PARENT(cell_id, resolution)` truncates a fine-grained cell id to its ancestor at a coarser resolution. Use this to roll multiple fine cells up into a single parent region:

```sql
H3_PARENT(cell_id, resolution)  -- returns UInt64 parent cell id, or NULL on error
```

The `resolution` must be coarser (numerically smaller) than the cell's own resolution. Requesting a parent at a finer resolution than the cell returns `NULL`.

**Rollup recipe:** aggregate at resolution 9 (city-block) and roll up to resolution 4 (metro area):

```sql
CREATE TABLE events (id INT, city TEXT, location GEOPOINT);
INSERT INTO events (id, city, location) VALUES
  (1, 'DC',  '38.760000,-77.150000'),
  (2, 'DC',  '38.760100,-77.150100'),
  (3, 'DC',  '38.761000,-77.151000'),
  (4, 'NY',  '40.712800,-74.006000');

SELECT H3_PARENT(H3_CELL(location, 9), 4) AS region,
       COUNT(*) AS event_count
FROM events
GROUP BY region
ORDER BY event_count DESC
```

### Choosing aggregation resolution

Higher resolution captures finer geographic structure but creates more groups. Lower resolution merges nearby points into fewer, larger cells. As a starting point:

- **Heatmaps for dashboards:** resolution 5–7 (city to neighborhood scale)
- **Density analysis:** resolution 7–9 (neighborhood to city-block scale)
- **Regional rollups:** resolution 3–5 (country to city scale)

Run the query at two resolutions and compare group counts to find the level that matches your data density.

---

## Distance Calculation

RedDB uses the **Haversine formula** for accurate great-circle distances on the Earth's surface. All distances are in kilometers.

For sub-millimeter accuracy on long intercontinental distances, use `GEO_DISTANCE_VINCENTY` (alias: `VINCENTY`).

## Coordinate System

- **Latitude:** -90 to 90 (North positive)
- **Longitude:** -180 to 180 (East positive)
- RedDB `GeoPoint` values store coordinates in microdegrees internally, converting automatically.

## The `COLUMN` argument

The `COLUMN` keyword is optional; the column name is not. The name resolves as a named column on a row table, a top-level document body field, or a dotted path into a document body (`telemetry.gps`). Values that are not recognized geographic points are skipped. When no entity in the collection has a recognized point in the named column, `RADIUS`, `BBOX`, and `NEAREST` return a `notice` explaining the shape mismatch.

See [Getting coordinates in](/guides/spatial-search.md#getting-coordinates-in) for the full accept/reject matrix.

## See Also

- [Spatial Search guide](/guides/spatial-search.md) — The complete H3 surface, end to end
- [CREATE INDEX](/query/create-index.md) — Creating H3 indexes
- [Geo Types](/types/geo.md) — GeoPoint, Latitude, Longitude types
- [Scalar Functions](/query/scalar-functions.md) — GEO_DISTANCE, GEO_BEARING, GEO_MIDPOINT
- [Geographic Operations](/guides/geo-operations.md) — Distance formulas, bearings, bounding boxes
- [Documents](/data-models/documents.md) — Using geo predicates on document body fields
- [Search Commands](/query/search-commands.md) — Other search types
