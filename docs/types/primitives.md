# Primitive Types

The core data types for general-purpose storage.

## Integer

Signed 64-bit integer. Range: -9,223,372,036,854,775,808 to 9,223,372,036,854,775,807.

```sql
CREATE TABLE counters (value Integer NOT NULL)
```

```rust
Value::Integer(42)
```

## UnsignedInteger

Unsigned 64-bit integer. Range: 0 to 18,446,744,073,709,551,615.

```rust
Value::UnsignedInteger(1000000)
```

## Float

64-bit IEEE 754 double-precision floating point.

```rust
Value::Float(3.14159)
```

## Text

Variable-length UTF-8 string. No maximum length.

```rust
Value::Text("Hello, RedDB!".into())
```

## Blob

Variable-length binary data. Stored as raw bytes.

```rust
Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF])
```

## Boolean

True or false.

```rust
Value::Boolean(true)
```

## Json

JSON-like structured data. Accepts any valid JSON object.

```rust
Value::Json(json!({"key": "value", "nested": {"a": 1}}))
```

## Uuid

128-bit universally unique identifier (v4).

```rust
Value::Uuid("550e8400-e29b-41d4-a716-446655440000".parse()?)
```

## Color / ColorAlpha

RGB (3 bytes) and RGBA (4 bytes) color values.

```rust
Value::Color(0xFF, 0x00, 0x00)      // Red
Value::ColorAlpha(0xFF, 0x00, 0x00, 0x80)  // Semi-transparent red
```

## Email

Validated email address, stored lowercase.

```rust
Value::Email("alice@example.com".into())
```

## Url

Validated URL.

```rust
Value::Url("https://example.com/api".into())
```

## Phone

Phone number stored as digits.

```rust
Value::Phone(15551234567)
```

## Semver

Semantic version packed into a single `u32`: `major * 1,000,000 + minor * 1,000 + patch`.

```rust
Value::Semver(1, 2, 3)  // "1.2.3"
```

## Decimal

Fixed-point decimal with configurable precision. Stored as i64.

```rust
Value::Decimal(19999)  // Represents 199.99 with 2 decimal places
```

## Enum

Enumerated type stored as a u8 variant index.

```rust
Value::Enum(2)  // Third variant (0-indexed)
```

## Array

Homogeneous array of values.

```rust
Value::Array(vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)])
```

## BigInt

Alias for signed 64-bit integer, used for large numeric values.

```rust
Value::BigInt(9999999999999)
```

## Password

Credentials that must be stored but never retrieved in plaintext. Values are hashed with **argon2id** (64 MB, 3 passes, 4 lanes) before storage, and the hash itself is masked in every output formatter.

```sql
CREATE TABLE users (
  username Text NOT NULL,
  password Password NOT NULL
)

-- Use the PASSWORD(...) literal constructor to mark a value for hashing.
-- The runtime strips the sentinel and applies argon2id before write.
INSERT INTO users (username, password) VALUES ('alice', PASSWORD('MyP@ss123'));

-- VERIFY_PASSWORD(column, candidate) runs argon2id verify against the
-- stored hash and returns a boolean.
SELECT username, VERIFY_PASSWORD(password, 'MyP@ss123') AS ok FROM users;

-- Raw SELECT of the password column never surfaces the plaintext or
-- the hash — every formatter masks Value::Password as '***'.
SELECT username, password FROM users;
```

```rust
// Internal representation: argon2id hash string "argon2id$<salt>$<hash>"
Value::Password(hash_string)
```

Notes:
- `PASSWORD('...')` and `VERIFY_PASSWORD(column, 'candidate')` are the only sanctioned ways to write and match credentials. The plaintext is never stored on disk.
- `Password` columns cannot be indexed, ordered, or aggregated — the value is opaque to the planner by design.
- `Display`, `fmt::Debug`, and JSON output all render masked as `"***"`.

## Secret

Sensitive payloads (API keys, tokens, connection strings) that must live in the database but never leak in plaintext. Reserved for **AES-256-GCM** field-level encryption keyed by the vault's `red.secret.aes_key`.

```sql
CREATE TABLE integrations (
  name Text NOT NULL,
  api_key Secret NOT NULL
)
```

```rust
// Internal representation: ciphertext bytes (nonce + ciphertext + tag)
Value::Secret(ciphertext_bytes)
```

> [!NOTE]
> The type layer, binary format, and output masking are already in place — sealed secrets always render as `"***"`. Auto-encryption on INSERT via `SECRET('...')` is pending the runtime → vault wiring. Until then, `INSERT ... VALUES (SECRET('...'))` returns an error and pre-encrypted bytes must be inserted directly via the embedded API.
