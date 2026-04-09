# Validation & Coercion

RedDB validates typed values on write and performs automatic coercion where safe conversions are possible.

## Automatic Coercion

| Source | Target | Rule |
|:-------|:-------|:-----|
| String `"123"` | Integer | Parse as i64 |
| String `"3.14"` | Float | Parse as f64 |
| String `"true"` / `"false"` | Boolean | Case-insensitive |
| Integer `443` | Float | Widen to f64 |
| String `"10.0.0.1"` | IpAddr | Parse as IP address |
| String `"10.0.0.0/24"` | Cidr | Parse as CIDR |
| String `"alice@x.com"` | Email | Validate and lowercase |
| String `"https://..."` | Url | Validate URL format |
| String `"1.2.3"` | Semver | Parse major.minor.patch |
| String `"2024-01-15"` | Date | Parse ISO date |
| String `"US"` | Country2 | Validate ISO 3166-1 alpha-2 |
| String `"USD"` | Currency | Validate ISO 4217 |
| String `"en"` | Lang2 | Validate ISO 639-1 |

## Validation Rules

### Email

- Must contain exactly one `@` symbol
- Domain must have at least one dot
- Stored as lowercase

### Url

- Must start with `http://` or `https://`
- Must have a valid hostname

### IpAddr

- Accepts both IPv4 (`10.0.0.1`) and IPv6 (`::1`, `2001:db8::1`)
- Invalid addresses are rejected

### Cidr

- Must be in format `ip/prefix`
- Prefix must be 0-32 for IPv4

### Port

- Must be in range 0-65535

### Country2 / Country3

- Must be exactly 2 or 3 uppercase ASCII letters

### Lang2 / Lang5

- `Lang2`: exactly 2 lowercase ASCII letters
- `Lang5`: pattern `xx-XX` (e.g., `pt-BR`)

### Currency

- Exactly 3 uppercase ASCII letters

### Semver

- Format `major.minor.patch`
- Each component must be a non-negative integer

## NOT NULL Constraint

Columns marked `NOT NULL` reject null values:

```sql
CREATE TABLE users (
  name Text NOT NULL,
  email Email NOT NULL,
  phone Phone  -- nullable by default
)
```

## DEFAULT Values

Set default values for optional columns:

```sql
CREATE TABLE hosts (
  os Text DEFAULT 'linux',
  port Port DEFAULT 22,
  critical Boolean DEFAULT false
)
```

## Error Handling

Invalid values return clear error messages:

```json
{
  "ok": false,
  "error": "validation error: 'not-an-email' is not a valid email address"
}
```

> [!TIP]
> When inserting via the JSON API (HTTP/gRPC), all values arrive as JSON strings or numbers. RedDB coerces them to the target column type if a schema is defined, or stores them as-is in schema-free collections.
