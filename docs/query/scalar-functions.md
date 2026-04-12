# Scalar Functions

Scalar functions operate on individual values and can be used in SELECT projections. They evaluate once per row, unlike aggregate functions (COUNT, SUM, AVG) which operate across rows.

---

## Geographic Functions

| Function | Arguments | Returns | Description |
|:---------|:----------|:--------|:------------|
| `GEO_DISTANCE(col, POINT(lat, lon))` | GeoPoint column + POINT literal | Float (km) | Haversine great-circle distance |
| `GEO_DISTANCE_VINCENTY(col, POINT(lat, lon))` | GeoPoint column + POINT literal | Float (km) | WGS-84 geodesic distance (sub-mm accuracy) |
| `GEO_BEARING(col, POINT(lat, lon))` | GeoPoint column + POINT literal | Float (degrees) | Initial bearing [0, 360) |
| `GEO_MIDPOINT(col1, col2)` | Two GeoPoint columns | GeoPoint | Geographic midpoint on great-circle arc |

Aliases: `HAVERSINE(...)` = `GEO_DISTANCE(...)`, `VINCENTY(...)` = `GEO_DISTANCE_VINCENTY(...)`

### POINT Literal

The `POINT(lat, lon)` syntax creates an inline geographic reference. Latitude comes first (geographic convention).

```sql
POINT(-23.550520, -46.633308)  -- São Paulo
POINT(48.8566, 2.3522)         -- Paris
POINT(0.0, 0.0)                -- Null Island
```

### Examples

```sql
-- Nearest stores
SELECT name, GEO_DISTANCE(location, POINT(-23.55, -46.63)) AS dist_km
FROM stores
ORDER BY dist_km
LIMIT 10

-- Bearing to headquarters
SELECT branch, GEO_BEARING(coords, POINT(40.71, -74.00)) AS heading
FROM offices

-- Midpoint between origin and destination
SELECT flight_id, GEO_MIDPOINT(departure, arrival) AS waypoint
FROM flights
```

---

## String Functions

| Function | Arguments | Returns | Description |
|:---------|:----------|:--------|:------------|
| `UPPER(col)` | Text column | Text | Convert to uppercase |
| `LOWER(col)` | Text column | Text | Convert to lowercase |
| `LENGTH(col)` | Text, Blob, or Array column | Integer | Length in characters, bytes, or elements |

### Examples

```sql
SELECT UPPER(name) FROM users
SELECT LOWER(email) FROM contacts
SELECT name, LENGTH(description) AS desc_len FROM products
```

---

## Numeric Functions

| Function | Arguments | Returns | Description |
|:---------|:----------|:--------|:------------|
| `ABS(col)` | Integer or Float column | Same type | Absolute value |
| `ROUND(col)` | Float column | Float | Round to nearest integer |

### Examples

```sql
SELECT name, ABS(balance) AS abs_balance FROM accounts
SELECT item, ROUND(price) AS rounded_price FROM products
```

---

## General Functions

| Function | Arguments | Returns | Description |
|:---------|:----------|:--------|:------------|
| `COALESCE(col1, col2, ...)` | Two or more columns | First non-null | Returns the first non-NULL argument |

### Examples

```sql
SELECT COALESCE(nickname, full_name) AS display_name FROM users
SELECT COALESCE(phone, email, 'no contact') AS contact FROM customers
```

---

## Using with Aliases and ORDER BY

All scalar functions support the `AS` alias syntax. The alias can then be used in `ORDER BY`:

```sql
SELECT name, GEO_DISTANCE(location, POINT(-23.55, -46.63)) AS dist
FROM stores
ORDER BY dist
LIMIT 20
```

---

## Aggregate vs Scalar

| Type | Operates on | Examples |
|:-----|:------------|:--------|
| **Aggregate** | All rows → single value | `COUNT(*)`, `SUM(col)`, `AVG(col)`, `MIN(col)`, `MAX(col)` |
| **Scalar** | One row → one value | `GEO_DISTANCE(...)`, `UPPER(...)`, `ABS(...)`, `COALESCE(...)` |

You can mix scalar and non-function columns in the same SELECT:

```sql
SELECT name, city, GEO_DISTANCE(location, POINT(-23.55, -46.63)) AS dist
FROM stores
ORDER BY dist
```

You cannot currently mix aggregate and scalar functions in the same query (use a subquery or two separate queries).
