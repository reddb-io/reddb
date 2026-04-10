# Spatial Search

RedDB supports spatial queries on `GeoPoint`, `Latitude`, and `Longitude` columns using an R-tree index. Find points within a radius, bounding box, or nearest neighbors.

## Prerequisites

Create an R-tree index on the spatial column:

```sql
CREATE INDEX idx_location ON sites (location) USING RTREE
```

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

Returns results sorted by distance ascending:

| Column | Description |
|:-------|:------------|
| `entity_id` | Entity ID of the matching point |
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

Returns results sorted by distance ascending.

## Distance Calculation

RedDB uses the **Haversine formula** for accurate great-circle distances on the Earth's surface. All distances are in kilometers.

## Coordinate System

- **Latitude:** -90 to 90 (North positive)
- **Longitude:** -180 to 180 (East positive)
- RedDB `GeoPoint` values store coordinates in microdegrees internally, converting automatically.

## See Also

- [CREATE INDEX](/query/create-index.md) -- Creating R-tree indexes
- [Geo Types](/types/geo.md) -- GeoPoint, Latitude, Longitude types
- [Search Commands](/query/search-commands.md) -- Other search types
