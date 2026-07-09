# Geo Types

Types for geographic coordinates and locations.

## Latitude

Geographic latitude stored as i32 microdegrees (4 bytes). Range: -90,000,000 to 90,000,000.

```rust
Value::Latitude(40689247)  // 40.689247 (Statue of Liberty)
```

## Longitude

Geographic longitude stored as i32 microdegrees (4 bytes). Range: -180,000,000 to 180,000,000.

```rust
Value::Longitude(-74044502)  // -74.044502 (Statue of Liberty)
```

## GeoPoint

Combined latitude and longitude (8 bytes total).

```sql
CREATE TABLE locations (position GeoPoint NOT NULL)
```

```rust
Value::GeoPoint(40689247, -74044502)  // 40.689247, -74.044502
```

## Recognized point shapes

Spatial search and H3 indexes recognize these point values:

| Shape | Example | Coordinate order |
|:------|:--------|:-----------------|
| `GEOPOINT` table value | `'40.689247,-74.044502'` | latitude, longitude |
| JSON object | `{"lat": 40.689247, "lon": -74.044502}` | latitude, longitude |
| JSON object aliases | `{"latitude": 40.689247, "longitude": -74.044502}` | latitude, longitude |
| GeoJSON `Point` | `{"type":"Point","coordinates":[-74.044502,40.689247]}` | longitude, latitude |

Other GeoJSON forms such as `Polygon` and `LineString`, malformed
`coordinates` arrays, non-numeric coordinates, and out-of-range coordinates are
not recognized as points.

## Example: Location Tracking

```sql
CREATE TABLE offices (
  name Text NOT NULL,
  city Text,
  position GeoPoint NOT NULL,
  country Country2
)
```

```bash
curl -X POST http://127.0.0.1:5000/collections/offices/rows \
  -H 'content-type: application/json' \
  -d '{
    "fields": {
      "name": "HQ",
      "city": "New York",
      "position": {"lat": 40.689247, "lon": -74.044502},
      "country": "US"
    }
  }'
```
