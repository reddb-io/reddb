# Type System Overview

RedDB has a typed schema system with two layers:

1. Standard application types you already expect in a database.
2. Native domain-aware types (network, geo, locale, references, colors) that reduce parsing and validation work in your app.

The goal is simple: keep common data easy, and make domain data first-class.

## Start with Standard Types

Use these for most business entities:

- Numbers: `Integer`, `UnsignedInteger`, `Float`, `Decimal`, `BigInt`
- Text and binary: `Text`, `Blob`, `Json`
- Booleans and identifiers: `Boolean`, `Uuid`
- Time fields: `Timestamp`, `TimestampMs`, `Date`, `Time`, `Duration`
- General collections: `Array`, `Enum`

Example schema using only standard types:

```sql
CREATE TABLE users (
  id UnsignedInteger NOT NULL,
  name Text NOT NULL,
  email Text,
  active Boolean DEFAULT true,
  profile Json,
  created_at TimestampMs NOT NULL
)
```

## Then Add Native RedDB Types

When a field has domain semantics, switch from plain `Text` to a native type.

### Network-native

- `IpAddr`, `Ipv4`, `Ipv6`
- `MacAddr`, `Cidr`, `Subnet`, `Port`

### Locale-native

- `Country2`, `Country3`
- `Lang2`, `Lang5`
- `Currency`

### Financial-native

- `AssetCode` — validated asset identifier for fiat or crypto symbols such as `USD`, `BTC`, `ETH`, `USDT`
- `Money` — exact monetary value stored as integer minor units + explicit scale + asset code

### Geo-native

- `Latitude`, `Longitude`, `GeoPoint`

### Rich primitives

- `Email`, `Url`, `Phone`, `Semver`
- `Color`, `ColorAlpha`

### Security-native

- `Secret` — transparently encrypted with AES-256-GCM using the vault's master key. Reads return `***` when the vault is sealed and the plaintext only when unsealed.
- `Password` — stored as an argon2id hash. Never round-tripped as plaintext; compare with `VERIFY_PASSWORD(column, 'candidate')`.

### Cross-model references

- `NodeRef`, `EdgeRef`, `VectorRef`, `RowRef`
- `KeyRef`, `DocRef`, `TableRef`, `PageRef`

Example with native/custom types:

```sql
CREATE TABLE assets (
  host_ip Ipv4 NOT NULL,
  service_port Port NOT NULL,
  mgmt_subnet Cidr,
  owner_country Country2,
  ui_theme Color,
  build_version Semver,
  position GeoPoint
)
```

In this model, you get semantic validation on write instead of post-processing strings in your service.

## Why This Matters

- Less parsing code in API/services.
- Fewer invalid records entering storage.
- Better schema readability: type already explains intent (`Ipv4` vs generic `Text`).
- Faster operational debugging because values are normalized.

## Practical Typing Strategy

1. Start with standard types while modeling quickly.
2. Promote hot or error-prone fields to native types (`Text` -> `Email`, `Text` -> `Ipv4`, etc.).
3. Add reference types only when you need explicit cross-model links.
4. For financial values, prefer `Money` or an explicit `amount_minor + scale + asset_code` schema over `Float`.

## Rust API Example

Use typed values directly in embedded mode:

```rust
use reddb::storage::schema::Value;

let row = vec![
    ("host_ip", Value::IpAddr("10.0.0.1".parse()?)),
    ("service_port", Value::Port(443)),
    ("owner_country", Value::Country2([b'B', b'R'])),
    ("ui_theme", Value::Color([0x1E, 0x90, 0xFF])),
    ("active", Value::Boolean(true)),
];
```

## Coercion and Validation

RedDB can coerce safe inputs (for example string to `Ipv4` or `Email`) and rejects invalid values early.

See:

- [Primitive Types](/types/primitives.md)
- [Network Types](/types/network.md)
- [Temporal Types](/types/temporal.md)
- [Geo Types](/types/geo.md)
- [Locale Types](/types/locale.md)
- [Reference Types](/types/references.md)
- [Validation & Coercion](/types/validation.md)
