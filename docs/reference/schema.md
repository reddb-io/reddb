# Schema Definition

RedDB supports both schema-free and schema-defined collections.

## Schema-Free (Default)

By default, collections accept any fields on insert. Types are inferred from the JSON input:

```bash
curl -X POST http://127.0.0.1:8080/collections/users/rows \
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

## Describe a Collection

```bash
grpcurl -plaintext \
  -d '{"collection": "users"}' \
  127.0.0.1:50051 reddb.v1.RedDb/DescribeCollection
```

## Schema Registry

The schema registry tracks all collection schemas and their evolution. It is part of the catalog and persisted alongside the data.

## Schema Coercion

When a schema is defined, inserted values are coerced to match column types. See [Validation & Coercion](/types/validation.md).

## Index Descriptors

Indexes are declared in the schema and follow the artifact lifecycle. See [Artifact Lifecycle](/reference/artifacts.md).
