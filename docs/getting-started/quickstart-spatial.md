# Quickstart: Spatial Search

Index geographic points and query them by radius or nearest-neighbour. The
**spatial** model adds an H3 index to a `collection` (the universal
container): a `GEOPOINT` column becomes searchable by distance.

## 1. Start RedDB

```bash
docker run --rm \
  -p 5050:5050 \
  -p 55055:55055 \
  -p 5000:5000 \
  ghcr.io/reddb-io/reddb:latest
```

Connect with `red connect 127.0.0.1:55055` (or POST to
`http://127.0.0.1:5000/query`).

## 2. Create a collection with a geopoint and an H3 index

```sql
CREATE TABLE places (id INT, name TEXT, loc GEOPOINT);
CREATE INDEX idx_loc ON places (loc) USING H3;
```

## 3. Insert points

`GEOPOINT` values are `'latitude,longitude'` strings — three Paris landmarks:

```sql
INSERT INTO places (id, name, loc) VALUES (1, 'Louvre', '48.8606,2.3376');
INSERT INTO places (id, name, loc) VALUES (2, 'Eiffel Tower', '48.8584,2.2945');
INSERT INTO places (id, name, loc) VALUES (3, 'Sacre-Coeur', '48.8867,2.3431');
```

## 4. Your first meaningful result

Find everything within 5 km of the city centre, then the two nearest points:

```sql
SEARCH SPATIAL RADIUS 48.8566 2.3522 5.0 COLLECTION places COLUMN loc;
SEARCH SPATIAL NEAREST 48.8566 2.3522 K 2 COLLECTION places COLUMN loc;
```

```text
 name         | distance_km
--------------+------------
 Louvre       | 1.2
 Sacre-Coeur  | 3.4
```

## Where to go next

- [Spatial Search guide](/guides/spatial-search.md) — the complete H3 surface, end to end
- [CREATE INDEX reference](/query/create-index.md) — H3 and the other index kinds
- [Data Model Overview](/data-models/overview.md) — how spatial composes with other models
