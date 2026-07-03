# Spatial Quickstart

Use this when coordinates are part of the query. The Collection is the universal
container; the spatial model is the semantic layer over geographic fields.

Start RedDB:

```bash
docker run --rm -p 5000:5000 ghcr.io/reddb-io/reddb:latest
```

Or open an embedded runtime and run the same SQL.

```sql quickstart
CREATE TABLE branches (name TEXT, city TEXT);
INSERT INTO branches (name, city) VALUES ('Paulista', 'Sao Paulo'), ('Centro', 'Rio de Janeiro');
SELECT name, city, GEO_DISTANCE(POINT(-23.5614, -46.6559), POINT(-23.55, -46.63)) AS dist_km FROM branches WHERE city = 'Sao Paulo';
```

First meaningful result: the final query returns the branch and a distance
computed from geographic points.

Where to go next: [Spatial Search](/query/spatial-search.md),
[Geo Types](/types/geo.md), and [Scalar Functions](/query/scalar-functions.md).
