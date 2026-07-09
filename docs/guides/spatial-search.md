# Spatial Search

The complete reference for RedDB's geographic surface: how coordinates get into
a collection, how the H3 index is built over them, and how `SEARCH SPATIAL`
reads them back.

Everything on this page is executable. The SQL fences run against a fresh
in-memory store in the docs CI lane (`tests/docs_spatial_guide.rs`), and the
messages quoted below are the engine's own strings, asserted by that same test.

- New to spatial? Start with the [Spatial quickstart](/getting-started/quickstart-spatial.md).
- Looking for distance/bearing math rather than search? See [Geographic Operations](/guides/geo-operations.md).
- Just want the grammar? See [Spatial Search reference](/query/spatial-search.md).

---

## The model, briefly

RedDB indexes geography with **H3**, Uber's hierarchical hexagonal grid, laid
over the engine's ordinary disk-paged sorted index.

A coordinate encodes to a single 64-bit **cell id** at a chosen resolution. That
integer is the index key — so a spatial index is a plain sorted index of
integers, not a bespoke in-memory tree. A query covers its search area with a
ring of cells, looks each cell up to gather **candidates**, then post-filters
those candidates exactly: haversine distance for `RADIUS` and `NEAREST`, a
coordinate comparison for `BBOX`, an even-odd point-in-polygon test for
`WITHIN POLYGON`.

What that buys:

- **Disk-resident.** The index lives in the pager like every other index. RAM
  holds the working set, not the dataset.
- **Single-integer writes.** Indexing a point is one `u64` insert into a sorted
  index, so `INSERT`/`UPDATE`/`DELETE` stay cheap.
- **Hierarchical.** A cell id truncates to its coarser parent, which is what
  makes `H3_CELL` / `H3_PARENT` grouping work in `SELECT`.

And what it means for your results:

> **The index is a pure optimization.** The cell ring is a *superset* of the
> answer — it may include points outside the search area, never exclude points
> inside it. The exact post-filter runs on both paths. A query with an H3 index
> and the same query without one return byte-identical rows, in the same order.

RedDB falls back to a full scan, silently and correctly, when there is no H3
index on the column, when a coordinate cannot be encoded, or when the cell
cover would be larger than scanning the collection outright (see
[Choosing a resolution](#choosing-a-resolution)).

---

## Getting coordinates in

There are two entry points, and they share **one recognition seam** — the same
code decides what counts as a point for full scans, for H3 index builds, and
for the diagnostics below. A shape either works everywhere or nowhere.

### Row tables: the `GEOPOINT` type

Declare the column as `GEOPOINT` and write `'latitude,longitude'`:

```sql
CREATE TABLE stores (id INT, name TEXT, location GEOPOINT);

INSERT INTO stores (id, name, location) VALUES (1, 'Louvre', '48.8606,2.3376');
INSERT INTO stores (id, name, location) VALUES (2, 'Eiffel Tower', '48.8584,2.2945');
INSERT INTO stores (id, name, location) VALUES (3, 'Sacre-Coeur', '48.8867,2.3431');
INSERT INTO stores (id, name, location) VALUES (4, 'Gare de Lyon', '48.8443,2.3743');
```

`GEOPOINT` stores latitude and longitude as signed microdegrees (degrees ×
1,000,000) in 8 bytes — about 11 cm of precision at the equator. See
[Geo Types](/types/geo.md).

### Document collections: the `{lat, lon}` object convention

Documents have no column types, so a geographic point is an **object with a
latitude member and a longitude member**:

```sql
CREATE DOCUMENT couriers;

INSERT INTO couriers DOCUMENT VALUES
  ({"name":"alice","position":{"lat":38.7600,"lon":-77.1500}}),
  ({"name":"bob","position":{"latitude":38.7550,"longitude":-77.1600}}),
  ({"name":"carol","position":{"lat":38.7700,"lng":-77.1000}}),
  ({"name":"dave","position":{"lat":39,"lon":-77}});
```

`dave` shows that members may be **integers or floats** — both are JSON numbers.

Nesting is fine; you address it with a dotted path when you index and search:

```sql
CREATE DOCUMENT fleet;

INSERT INTO fleet DOCUMENT VALUES
  ({"vehicle":"van-1","telemetry":{"gps":{"lat":38.7600,"lon":-77.1500}}}),
  ({"vehicle":"van-2","telemetry":{"gps":{"lat":38.8000,"lon":-77.2000}}});
```

### Recognized shapes

| Shape | Example | Recognized |
|:------|:--------|:-----------|
| `GEOPOINT` value | `location GEOPOINT` holding `'48.8606,2.3376'` | Yes |
| `{lat, lon}` object | `{"lat":38.76,"lon":-77.15}` | Yes |
| `{latitude, longitude}` object | `{"latitude":38.76,"longitude":-77.15}` | Yes |
| `{lat, lng}` object | `{"lat":38.76,"lng":-77.15}` | Yes |
| Integer members | `{"lat":39,"lon":-77}` | Yes |
| Extra members alongside | `{"lat":38.76,"lon":-77.15,"accuracy":5}` | Yes |
| Sibling `lat`/`lon` columns | a row or node with `lat` and `lon` fields | Yes, via fallback (see below) |
| GeoJSON `Point` | `{"type":"Point","coordinates":[-77.15,38.76]}` | Yes — coordinates are **longitude-first** (see below) |

The latitude member is resolved as `lat`, then `latitude`. The longitude member
is resolved as `lon`, then `lng`, then `longitude`. Key matching is
**case-sensitive**: `Lat`, `LAT`, and `Longitude` are not recognized.

### Rejected shapes

| Shape | Example | Why |
|:------|:--------|:----|
| String coordinates | `{"lat":"38.76","lon":"-77.15"}` | Members must be JSON numbers |
| String pair | `"38.76,-77.15"` | Not an object |
| Array coordinates | `[38.76,-77.15]` | Not an object |
| GeoJSON non-`Point` | `{"type":"LineString","coordinates":[[-77.15,38.76],[-77.2,38.8]]}` | Only `Point` is a single coordinate pair — see below |
| Missing member | `{"lat":38.76}` | Both members required |
| Null member | `{"lat":38.76,"lon":null}` | Members must be JSON numbers |
| Latitude out of range | `{"lat":91,"lon":0}` | Latitude must be in `-90..=90` |
| Longitude out of range | `{"lat":0,"lon":-181}` | Longitude must be in `-180..=180` |
| Non-finite | `NaN` / `±inf` members | Not a WGS-84 point |

A rejected value is **skipped, never an error**. The entity keeps its data; it
is simply absent from the H3 index and invisible to spatial searches — exactly
as an unsupported value is absent from a B-tree index. The
[coverage message](#coverage-reporting) and the
[zero-geo notice](#the-zero-geo-notice) are how RedDB tells you a shape mismatch
happened.

### GeoJSON `Point` — and only `Point`

GeoJSON `{"type":"Point","coordinates":[lon, lat]}` **is recognized** (gh-1943).
The `coordinates` array is read longitude-first, exactly as GeoJSON RFC 7946
§3.1.1 demands — a point that renders correctly on a GeoJSON map lands in the
same place here, with no manual swapping.

The rest of the GeoJSON geometry set (`LineString`, `Polygon`, `MultiPolygon`,
…) is deliberately **not** recognized: RedDB indexes points, and only `Point`
carries a single coordinate pair. A non-`Point` GeoJSON value is skipped like
any other unrecognized shape.

```json
{"type":"Point","coordinates":[-77.15, 38.76]}
```

indexes at the same location as

```json
{"lat": 38.76, "lon": -77.15}
```

### The row/node fallback

When the named column does not resolve to a recognized point on a **row** or
**node** entity, RedDB scans that entity's other fields for the first
recognizable geographic value — including a `lat`/`latitude` field paired with
a `lon`/`lng`/`longitude` sibling.

**Document collections do not fall back.** If the named field (or dotted path)
is not a recognized point, the document is skipped. This is intentional: a
document body has no schema, so a fallback scan would silently search a field
you did not ask for.

---

## Indexing

### CREATE INDEX ... USING H3

```sql
CREATE INDEX idx_stores_loc ON stores (location) USING H3;
```

The resolution is an optional parenthesised argument:

```sql
CREATE INDEX idx_couriers_pos ON couriers (position) USING H3 (9);
```

| | |
|:--|:--|
| Syntax | `CREATE INDEX <name> ON <collection> (<column>) USING H3 [(<resolution>)]` |
| Resolution range | `0` (coarsest) .. `15` (finest), inclusive |
| Default resolution | `9` |
| Alias | `USING SPATIAL` — the default spatial backend, which is H3 at resolution 9 |

A resolution outside `0..=15` is a parse error:

```
H3 resolution must be an integer 0..=15, got Integer(16)
```

### Dotted-path columns

A document body path is indexed by naming it:

```sql
CREATE INDEX idx_fleet_gps ON fleet (telemetry.gps) USING H3;
```

A dotted path must be the only indexed column; combining it with other columns
is rejected with `document path indexes currently support a single indexed column`.

### Coverage reporting

`CREATE INDEX ... USING H3` reports how many existing entities produced a cell,
so a shape mismatch is visible the moment you build the index rather than the
first time a search comes back empty:

```
index 'idx_stores_loc' created on 'stores' (location) using H3 (4 of 4 entities indexed)
```

When a **non-empty** collection indexes **nothing**, the message elaborates —
it names the column and the shapes it expected:

```sql
CREATE DOCUMENT sensors;

INSERT INTO sensors DOCUMENT VALUES
  ({"id":1,"spot":"38.76,-77.15"}),
  ({"id":2,"spot":{"type":"LineString","coordinates":[[-77.15,38.76],[-77.2,38.8]]}}),
  ({"id":3,"spot":{"lat":38.76}});

CREATE INDEX idx_sensors_spot ON sensors (spot) USING H3;
```

```
index 'idx_sensors_spot' created on 'sensors' (spot) using H3 (0 of 3 entities indexed — no indexable geo value in 'spot'; expected GEO_POINT, {lat, lon} object, or GeoJSON Point)
```

An **empty** collection gets the plain form, `0 of 0 entities indexed`, with no
elaboration — indexing nothing is the correct outcome when there is nothing to
index, and a hint there would be noise.

### Index before data, or data before index

Both orders reach the same state.

- **Index first.** The index is empty at creation and every subsequent `INSERT`
  adds its cell.
- **Data first.** `CREATE INDEX` backfills from the existing entities; the
  `K of N` count above *is* that backfill.

### UPDATE and DELETE

The H3 index is maintained incrementally on every write, like any other index.
An `UPDATE` that moves a point removes the old cell and inserts the new one; a
`DELETE` removes the cell. An `UPDATE` that replaces a recognized point with a
rejected shape removes the entity from the index, and the entity stops matching
spatial searches — no stale hit survives.

---

## Searching

Three verbs read the index — plus `WITHIN POLYGON`, documented in the
[reference](/query/spatial-search.md#polygon-search). All latitudes and
longitudes are degrees; all distances are **kilometres**.

### Syntax

```
SEARCH SPATIAL RADIUS  <lat> <lon> <radius_km>              COLLECTION <c> COLUMN <col> [LIMIT <n>]
SEARCH SPATIAL BBOX    <min_lat> <min_lon> <max_lat> <max_lon> COLLECTION <c> COLUMN <col> [LIMIT <n>]
SEARCH SPATIAL NEAREST <lat> <lon> K <k>                    COLLECTION <c> COLUMN <col>
```

`COLLECTION` is required. `LIMIT` defaults to **100** for `RADIUS` and `BBOX`;
`NEAREST` has no `LIMIT` — `K` is its result cap and is required.

### The `COLUMN` argument

The `COLUMN` **keyword** is optional; the column **name** is not. There is no
default column — you always name the field you want searched.

The name resolves, in order, as:

1. a named column on a row table (`location`),
2. a top-level document body field (`position`),
3. a dotted path into a document body (`telemetry.gps`).

If it resolves to something that is not a recognized point, that entity is
skipped — with the row/node fallback and the document no-fallback rule from
[above](#the-rownode-fallback).

### Result columns

| Verb | Columns | Ordering |
|:-----|:--------|:---------|
| `RADIUS` | `entity_id`, `distance_km` | distance ascending |
| `NEAREST` | `entity_id`, `distance_km` | distance ascending |
| `BBOX` | `entity_id` | scan order |
| `WITHIN POLYGON` | `entity_id` | scan order |

`entity_id` is the RedDB entity id; `distance_km` is the haversine great-circle
distance in kilometres. `BBOX` and `WITHIN POLYGON` return no distance — neither
query has a centre to measure from.

### Over a row table

Every store within 3 km of the Paris city centre:

```sql
SEARCH SPATIAL RADIUS 48.8566 2.3522 3.0 COLLECTION stores COLUMN location;
```

```text
 entity_id | distance_km
-----------+---------------------
      1026 | 1.1570046974814778
      1029 | 2.117879817147935
```

Two things to read off that result. Sacre-Coeur (3.41 km) and the Eiffel Tower
(4.23 km) are absent — correctly, they are beyond 3 km. And `entity_id` is the
**engine-assigned entity id**, not the `id` column you inserted: `1026` is the
Louvre, `1029` the Gare de Lyon. Join back to your own key by selecting it.

The two nearest, regardless of distance — the same two rows, since `NEAREST`
ranks rather than filters:

```sql
SEARCH SPATIAL NEAREST 48.8566 2.3522 K 2 COLLECTION stores COLUMN location;
```

A bounding box over the Right Bank. No distance column, and the rows come back
in scan order rather than sorted:

```sql
SEARCH SPATIAL BBOX 48.85 2.30 48.89 2.40 COLLECTION stores COLUMN location LIMIT 10;
```

```text
 entity_id
-----------
      1028
      1026
```

### Over a document collection

The named body field:

```sql
SEARCH SPATIAL RADIUS 38.76 -77.15 5.0 COLLECTION couriers COLUMN position;
```

A dotted path into a nested body:

```sql
SEARCH SPATIAL NEAREST 38.76 -77.15 K 1 COLLECTION fleet COLUMN telemetry.gps;
```

A geofence, as a polygon (vertices are `(lat lon)` pairs, at least three,
implicitly closed):

```sql
SEARCH SPATIAL WITHIN POLYGON ((38.70 -77.20), (38.80 -77.20), (38.80 -77.05), (38.70 -77.05))
  COLLECTION couriers COLUMN position;
```

### The zero-geo notice

When a spatial search returns **no rows** because **no entity in the collection
has a recognized point in that column** — as opposed to simply having none in
range — the result carries a `notice`:

```sql
SEARCH SPATIAL RADIUS 38.76 -77.15 10.0 COLLECTION sensors COLUMN spot;
```

```
no entity in 'sensors' has an indexable geo value in column 'spot' (expected GEO_POINT, {lat, lon} object, or GeoJSON Point).
```

Over HTTP the string appears as the `notice` field of the query response.

The notice is deliberately narrow. It is **absent** when:

- the collection is empty — nothing to mismatch;
- at least one entity has a recognized point and the query simply missed it.

So an empty result *with* a notice means "your data is shaped wrong", and an
empty result *without* one means "nothing is there". `WITHIN POLYGON` does not
currently emit this notice; a shape mismatch under a polygon search returns an
empty result with no hint.

---

## Choosing a resolution

The resolution sets the cell size, and the cell size sets how many cells a query
has to enumerate. RedDB covers a radius query with a ring of
`ceil(radius_km / edge_km) + 1` cells around the centre cell — so the resolution
you pick should put your *typical* search radius within a handful of cell edges.

| Resolution | Average hexagon edge | Suits a typical radius of |
|:-----------|:---------------------|:--------------------------|
| `5` | ~9.9 km | tens to hundreds of km — regional |
| `7` | ~1.4 km | a few km — city |
| `9` (default) | ~0.20 km | hundreds of metres to a few km — neighbourhood |
| `11` | ~0.029 km | tens of metres — building |
| `13` | ~0.0041 km | metres — indoor |

The tradeoff is symmetric:

- **Too fine** for the radius → the cover is enormous. Enumerating millions of
  tiny cells costs more than reading the collection.
- **Too coarse** for the radius → each cell holds many points, so the candidate
  set is fat and the exact post-filter throws most of it away.

Resolution `9` is the default because a ~200 m cell edge lands in the middle of
the radii people actually query.

### The full-scan fallback

RedDB caps a `RADIUS` cover ring at **128** steps. When
`ceil(radius_km / edge_km) + 1` exceeds that — a wide radius over a fine
resolution — the index is skipped and the collection is scanned. `NEAREST`
expands outward at most 64 rings before doing the same. The fallback also
applies when there is no H3 index at all, or when a coordinate will not encode.

This is a performance cliff, never a correctness one. The exact post-filter is
the same on both paths, so **the fallback returns the same rows in the same
order**. If a spatial query is slower than you expect, check that the
resolution suits the radius before you look anywhere else.

---

## Migration note: `USING RTREE` was removed

Earlier versions accepted `CREATE INDEX ... USING RTREE`. That index was an
in-RAM `rstar` tree that **indexed nothing and served no queries** — every
`SEARCH SPATIAL` fell through to a full scan whether or not it existed, and it
cost a rebuild of the whole tree on every restart. It was removed rather than
fixed, because H3 over the disk-paged sorted index does the job the R-tree
promised.

`USING RTREE` is now a parse error, with the replacement spelled out:

```
USING RTREE was removed: the in-RAM R-tree indexed nothing and served no queries. Use USING H3 — same SEARCH SPATIAL surface, disk-resident, maintained on every write. Example: CREATE INDEX idx_loc ON events (gpsLocation) USING H3
```

There is nothing to migrate in your data — the R-tree held none of it. Change
the DDL and rebuild:

```sql
-- Before: CREATE INDEX idx_loc ON stores (location) USING RTREE
CREATE INDEX idx_loc_h3 ON stores (location) USING H3;
```

Stores written by an older version may still carry a persisted `rtree` index
descriptor. On load, RedDB drops it and logs a warning:

```
dropping retired RTREE index descriptor during load; the in-RAM R-tree indexed nothing and served no queries
```

The descriptor is not recreated. Recreate the index with `USING H3` when you
want one — no data is lost either way, because a spatial search without an index
is a full scan that returns the same rows.

---

## See also

- [Spatial quickstart](/getting-started/quickstart-spatial.md) — five minutes, one model
- [Spatial Search reference](/query/spatial-search.md) — grammar, polygon rules
- [Spatial on documents](/data-models/documents.md#spatial-on-documents) — the document walkthrough
- [CREATE INDEX](/query/create-index.md) — every index method
- [H3 Index (engine)](/engine/h3-index.md) — how the cells reach the pager
- [Geographic Operations](/guides/geo-operations.md) — `GEO_DISTANCE`, bearings, midpoints
- [Geo Types](/types/geo.md) — `GeoPoint`, `Latitude`, `Longitude`
