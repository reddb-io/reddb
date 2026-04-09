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
curl -X POST http://127.0.0.1:8080/collections/offices/rows \
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
