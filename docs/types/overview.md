# Type System Overview

RedDB supports 48 native data types organized into categories. All types have efficient binary serialization and are usable as column types in table schemas.

## All Types

| ID | Type | Category | Description | Storage |
|:---|:-----|:---------|:------------|:--------|
| 1 | `Integer` | Primitive | Signed 64-bit integer | 8 bytes |
| 2 | `UnsignedInteger` | Primitive | Unsigned 64-bit integer | 8 bytes |
| 3 | `Float` | Primitive | 64-bit IEEE 754 float | 8 bytes |
| 4 | `Text` | Primitive | Variable-length UTF-8 string | variable |
| 5 | `Blob` | Primitive | Variable-length binary data | variable |
| 6 | `Boolean` | Primitive | True/false | 1 byte |
| 7 | `Timestamp` | Temporal | Unix timestamp (seconds) | 8 bytes |
| 8 | `Duration` | Temporal | Duration in milliseconds | 8 bytes |
| 9 | `IpAddr` | Network | IPv4 or IPv6 address | 4 or 16 bytes |
| 10 | `MacAddr` | Network | MAC address | 6 bytes |
| 11 | `Vector` | Special | Fixed-dimension float vector | variable |
| 12 | `Nullable` | Wrapper | Nullable wrapper for any type | variable |
| 13 | `Json` | Primitive | JSON-like structured data | variable |
| 14 | `Uuid` | Primitive | UUID (v4) | 16 bytes |
| 15 | `NodeRef` | Reference | Reference to a graph node | 8 bytes |
| 16 | `EdgeRef` | Reference | Reference to a graph edge | 8 bytes |
| 17 | `VectorRef` | Reference | Reference to a vector | 8 bytes |
| 18 | `RowRef` | Reference | Reference to a table row | 16 bytes |
| 19 | `Color` | Primitive | RGB color | 3 bytes |
| 20 | `Email` | Primitive | Validated email address | variable |
| 21 | `Url` | Primitive | Validated URL | variable |
| 22 | `Phone` | Primitive | Phone number | 8 bytes |
| 23 | `Semver` | Primitive | Semantic version (major.minor.patch) | 4 bytes |
| 24 | `Cidr` | Network | CIDR notation (IP + prefix length) | 5 bytes |
| 25 | `Date` | Temporal | Date only (days since epoch) | 4 bytes |
| 26 | `Time` | Temporal | Time only (ms since midnight) | 4 bytes |
| 27 | `Decimal` | Primitive | Fixed-point decimal | 8 bytes |
| 28 | `Enum` | Primitive | Enumerated type (variant index) | 1 byte |
| 29 | `Array` | Primitive | Homogeneous array | variable |
| 30 | `TimestampMs` | Temporal | Timestamp with millisecond precision | 8 bytes |
| 31 | `Ipv4` | Network | IPv4 address | 4 bytes |
| 32 | `Ipv6` | Network | IPv6 address | 16 bytes |
| 33 | `Subnet` | Network | Network subnet (IP + mask) | 8 bytes |
| 34 | `Port` | Network | TCP/UDP port number | 2 bytes |
| 35 | `Latitude` | Geo | Latitude in microdegrees | 4 bytes |
| 36 | `Longitude` | Geo | Longitude in microdegrees | 4 bytes |
| 37 | `GeoPoint` | Geo | Geographic point (lat + lon) | 8 bytes |
| 38 | `Country2` | Locale | ISO 3166-1 alpha-2 country code | 2 bytes |
| 39 | `Country3` | Locale | ISO 3166-1 alpha-3 country code | 3 bytes |
| 40 | `Lang2` | Locale | ISO 639-1 language code | 2 bytes |
| 41 | `Lang5` | Locale | IETF language tag (e.g. "pt-BR") | 5 bytes |
| 42 | `Currency` | Locale | ISO 4217 currency code | 3 bytes |
| 43 | `ColorAlpha` | Primitive | RGBA color with alpha | 4 bytes |
| 44 | `BigInt` | Primitive | Large signed 64-bit integer | 8 bytes |
| 45 | `KeyRef` | Reference | Reference to a KV pair | variable |
| 46 | `DocRef` | Reference | Reference to a document | variable |
| 47 | `TableRef` | Reference | Reference to a table/collection | variable |
| 48 | `PageRef` | Reference | Reference to a storage page | variable |

## Categories

### Primitive Types

The basic building blocks: `Integer`, `UnsignedInteger`, `Float`, `Text`, `Blob`, `Boolean`, `Json`, `Uuid`, `Color`, `ColorAlpha`, `Email`, `Url`, `Phone`, `Semver`, `Decimal`, `Enum`, `Array`, `BigInt`.

See [Primitive Types](/types/primitives.md).

### Network Types

Purpose-built for network data: `IpAddr`, `MacAddr`, `Cidr`, `Ipv4`, `Ipv6`, `Subnet`, `Port`.

See [Network Types](/types/network.md).

### Temporal Types

Time and date types: `Timestamp`, `Duration`, `Date`, `Time`, `TimestampMs`.

See [Temporal Types](/types/temporal.md).

### Geo Types

Geographic data: `Latitude`, `Longitude`, `GeoPoint`.

See [Geo Types](/types/geo.md).

### Locale Types

Internationalization: `Country2`, `Country3`, `Lang2`, `Lang5`, `Currency`.

See [Locale Types](/types/locale.md).

### Reference Types

Cross-entity references: `NodeRef`, `EdgeRef`, `VectorRef`, `RowRef`, `KeyRef`, `DocRef`, `TableRef`, `PageRef`.

See [Reference Types](/types/references.md).

## Usage in Schemas

Types are specified when defining table columns:

```sql
CREATE TABLE hosts (
  ip IpAddr NOT NULL,
  mac MacAddr,
  location GeoPoint,
  os Text NOT NULL,
  version Semver,
  last_seen Timestamp
)
```

## Rust API

In the embedded API, use the `Value` enum:

```rust
use reddb::Value;

let row = vec![
    ("ip", Value::IpAddr("10.0.0.1".parse()?)),
    ("name", Value::Text("web-01".into())),
    ("port", Value::Integer(443)),
    ("active", Value::Boolean(true)),
    ("score", Value::Float(0.95)),
];
```

## Type Coercion

RedDB performs automatic type coercion when possible:

| From | To | Example |
|:-----|:---|:--------|
| String `"123"` | Integer `123` | Numeric strings to integers |
| String `"true"` | Boolean `true` | Boolean strings |
| Integer `443` | Float `443.0` | Integer to float promotion |
| String `"10.0.0.1"` | IpAddr | IP address parsing |
| String `"alice@x.com"` | Email | Email validation |

See [Validation & Coercion](/types/validation.md) for the full coercion matrix.
