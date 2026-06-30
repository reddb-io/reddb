# Schema Definition

RedDB supports both schema-free and schema-defined collections.

## Schema-Free (Default)

By default, collections accept any fields on insert. Types are inferred from the JSON input:

```bash
curl -X POST http://127.0.0.1:5000/collections/users/rows \
  -H 'content-type: application/json' \
  -d '{"fields": {"name": "Alice", "age": 30, "active": true}}'
```

## Schema-Defined

Use `CREATE TABLE` to define a typed schema:

```sql
CREATE TABLE users (
  name Text NOT NULL,
  email Email NOT NULL,
  age Integer,
  active Boolean DEFAULT true,
  ip IpAddr,
  created_at Timestamp
)
```

## Column Definition

Each column has:

| Property | Required | Description |
|:---------|:---------|:------------|
| Name | Yes | Column identifier |
| Type | Yes | One of the 50 data types |
| `NOT NULL` | No | Reject null values |
| `DEFAULT` | No | Default value for missing fields |

## Sensitive Column Types

Use `SECRET` and `PASSWORD` when the column itself stores sensitive per-row
application data. They are schema column types, so declare them in a typed
collection with `CREATE TABLE`.

| Type | Semantics | Write syntax | Read behavior |
|:-----|:----------|:-------------|:--------------|
| `SECRET` | Encrypts each row value with AES-256-GCM using the vault AES key. Use it for API tokens, refresh tokens, private webhook secrets, and other values the application may need to decrypt later. | `SECRET('plaintext')` in `INSERT` or `UPDATE` | Returns ciphertext/masked output unless the runtime has the vault key and decrypts it for the read path. |
| `PASSWORD` | Hashes each row value with Argon2id. Use it for credentials that must only be checked, never recovered. | `PASSWORD('plaintext')` in `INSERT` or `UPDATE` | Never returns the original value. Verify candidates with `VERIFY_PASSWORD(column, 'candidate')`, which returns a boolean. |

Example schema-defined collection with both sensitive column types:

```sql
CREATE TABLE service_accounts (
  id UUID NOT NULL,
  name TEXT NOT NULL,
  api_token SECRET NOT NULL,
  login_password PASSWORD NOT NULL
)

INSERT INTO service_accounts (id, name, api_token, login_password)
VALUES (
  '550e8400-e29b-41d4-a716-446655440000',
  'billing-sync',
  SECRET('sk_live_service_token'),
  PASSWORD('candidate-password')
)

SELECT
  name,
  VERIFY_PASSWORD(login_password, 'candidate-password') AS password_ok
FROM service_accounts
```

`SECRET` columns and vault secrets solve different problems. Column-level
`SECRET` values encrypt per-row application data inside user collections. Vault
secrets, written with `SET SECRET` and referenced as `$secret.X`, store
operator or application credentials outside row data so configuration and
runtime integrations can resolve them explicitly.

## Describe a Collection

```bash
grpcurl -plaintext \
  -d '{"collection": "users"}' \
  127.0.0.1:55055 reddb.v1.RedDb/DescribeCollection
```

## Schema Registry

The schema registry tracks all collection schemas and their evolution. It is part of the catalog and persisted alongside the data.

## Schema Coercion

When a schema is defined, inserted values are coerced to match column types. See [Validation & Coercion](/types/validation.md).

## Index Descriptors

Indexes are declared in the schema and follow the artifact lifecycle. See [Artifact Lifecycle](/reference/artifacts.md).
