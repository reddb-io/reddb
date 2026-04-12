# Geographic Operations

RedDB includes a built-in geographic computation module with distance calculations, bearings, midpoints, and bounding boxes. All functions work on the WGS-84 ellipsoid (Vincenty) or spherical model (Haversine) with no external dependencies.

---

## Coordinate Storage

RedDB stores geographic coordinates as **microdegrees** (degrees x 1,000,000) in signed 32-bit integers. This gives 6 decimal places of precision (~0.11m at the equator) with minimal storage overhead.

| Type | Storage | Size | Example |
|:-----|:--------|:-----|:--------|
| `GeoPoint` | `(lat_micro, lon_micro)` | 8 bytes | `POINT(-23.550520, -46.633308)` → `(-23550520, -46633308)` |
| `Latitude` | `lat_micro` | 4 bytes | `-23550520` |
| `Longitude` | `lon_micro` | 4 bytes | `-46633308` |

---

## Distance Functions

### Haversine (spherical model)

Great-circle distance assuming a perfect sphere of radius 6,371 km. Fast and sufficient for most applications.

**Accuracy**: ~0.3% error (up to ~20 km over 6,000 km distances).

### Vincenty (WGS-84 ellipsoid)

Geodesic distance using the Vincenty inverse formula on the WGS-84 ellipsoid. Iterative algorithm that converges in 3-8 iterations for most point pairs.

**Accuracy**: sub-millimeter. Falls back to Haversine for antipodal points where Vincenty does not converge.

### When to use which

| Scenario | Recommendation |
|:---------|:---------------|
| City-scale distances (< 100 km) | Haversine — fast, error negligible |
| Country-scale distances (100-1000 km) | Haversine — ~0.1% error |
| Intercontinental distances | Vincenty — sub-mm accuracy |
| Aviation, geodesy, surveying | Vincenty — required precision |
| High-throughput batch processing | Haversine — 2-3x faster |

---

## SQL Functions

### GEO_DISTANCE

Returns the great-circle distance in kilometers between a column and a point.

```sql
-- Distance from each store to a reference point
SELECT name, GEO_DISTANCE(location, POINT(-23.55, -46.63)) AS dist_km
FROM stores
ORDER BY dist_km

-- Find the 10 nearest stores
SELECT name, GEO_DISTANCE(location, POINT(-23.55, -46.63)) AS dist
FROM stores
ORDER BY dist
LIMIT 10
```

### GEO_DISTANCE_VINCENTY

Same as GEO_DISTANCE but uses the Vincenty formula for sub-millimeter accuracy.

```sql
SELECT name, GEO_DISTANCE_VINCENTY(location, POINT(40.71, -74.00)) AS dist
FROM airports
ORDER BY dist
```

### GEO_BEARING

Returns the initial bearing (forward azimuth) in degrees [0, 360) from a column to a point. North = 0, East = 90, South = 180, West = 270.

```sql
SELECT name, GEO_BEARING(location, POINT(-23.55, -46.63)) AS direction
FROM landmarks
```

### GEO_MIDPOINT

Returns the geographic midpoint between two GeoPoint columns as a new GeoPoint.

```sql
SELECT GEO_MIDPOINT(origin, destination) AS midpoint
FROM flights
```

### POINT literal

The `POINT(lat, lon)` literal creates a geographic reference point in SQL expressions.

```sql
-- Latitude first, longitude second (geographic convention)
POINT(-23.550520, -46.633308)  -- São Paulo
POINT(48.8566, 2.3522)         -- Paris
POINT(51.5074, -0.1278)        -- London
```

---

## HTTP API

All geo endpoints accept JSON bodies and return JSON responses.

### POST /geo/distance

Calculate the distance between two points.

```bash
curl -X POST localhost:8080/geo/distance -d '{
  "from": {"lat": -23.55, "lon": -46.63},
  "to": {"lat": -22.91, "lon": -43.17},
  "method": "vincenty"
}'
```

Response:
```json
{
  "ok": true,
  "distance_km": 357.29,
  "distance_m": 357286.42,
  "method": "vincenty"
}
```

The `method` field is optional and defaults to `"haversine"`. Set to `"vincenty"` for ellipsoidal accuracy.

### POST /geo/bearing

Calculate the bearing between two points.

```bash
curl -X POST localhost:8080/geo/bearing -d '{
  "from": {"lat": 0, "lon": 0},
  "to": {"lat": 1, "lon": 1}
}'
```

Response:
```json
{
  "ok": true,
  "initial_bearing": 44.99,
  "final_bearing": 225.01
}
```

### POST /geo/midpoint

Calculate the geographic midpoint on the great-circle arc.

```bash
curl -X POST localhost:8080/geo/midpoint -d '{
  "from": {"lat": -23.55, "lon": -46.63},
  "to": {"lat": 40.71, "lon": -74.00}
}'
```

### POST /geo/destination

Calculate the destination point given a starting point, bearing, and distance.

```bash
curl -X POST localhost:8080/geo/destination -d '{
  "lat": -23.55,
  "lon": -46.63,
  "bearing": 0,
  "distance_km": 100
}'
```

Response:
```json
{
  "ok": true,
  "lat": -22.65,
  "lon": -46.63
}
```

### POST /geo/bounding-box

Calculate a conservative bounding box around a center point.

```bash
curl -X POST localhost:8080/geo/bounding-box -d '{
  "lat": -23.55,
  "lon": -46.63,
  "radius_km": 10
}'
```

Response:
```json
{
  "ok": true,
  "min_lat": -23.64,
  "min_lon": -46.73,
  "max_lat": -23.46,
  "max_lon": -46.53
}
```

---

## Spatial Search (SEARCH SPATIAL)

RedDB also supports dedicated spatial search commands for radius, bounding box, and nearest-neighbor queries:

```sql
-- Find all entities within 50 km of a point
SEARCH SPATIAL RADIUS -23.55 -46.63 50
COLLECTION stores COLUMN location LIMIT 100

-- Bounding box search
SEARCH SPATIAL BBOX -24.0 -47.0 -23.0 -46.0
COLLECTION stores COLUMN location

-- K nearest neighbors
SEARCH SPATIAL NEAREST -23.55 -46.63 K 10
COLLECTION stores COLUMN location
```

These commands use an R-tree spatial index for efficient queries on large datasets.

---

## Available Functions Summary

| Function | Input | Output | Model |
|:---------|:------|:-------|:------|
| `haversine_km` | two points | km (f64) | Spherical |
| `haversine_m` | two points | meters (f64) | Spherical |
| `vincenty_km` | two points | km (f64) | WGS-84 |
| `vincenty_m` | two points | meters (f64) | WGS-84 |
| `bearing` | two points | degrees [0, 360) | Spherical |
| `final_bearing` | two points | degrees [0, 360) | Spherical |
| `midpoint` | two points | (lat, lon) | Spherical |
| `destination` | point + bearing + distance | (lat, lon) | Spherical |
| `bounding_box` | center + radius | (min_lat, min_lon, max_lat, max_lon) | Approximation |
| `polygon_area_km2` | vertices | km² | Spherical |
