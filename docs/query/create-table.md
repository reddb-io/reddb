# CREATE TABLE

The `CREATE TABLE` statement defines a new collection with a typed schema.

## Syntax

```sql
CREATE TABLE table_name (
  column_name DataType [NOT NULL] [DEFAULT value],
  ...
) [WITH TTL duration]
  [WITH CONTEXT INDEX ON (column [, ...])]
```

## Example

```sql
CREATE TABLE hosts (
  ip IpAddr NOT NULL,
  hostname Text NOT NULL,
  os Text DEFAULT 'linux',
  port Port,
  version Semver,
  location GeoPoint,
  critical Boolean DEFAULT false,
  last_seen Timestamp
)
```

## Supported Column Types

All 48 types from the [Type System](/types/overview.md) can be used as column types:

```sql
CREATE TABLE network_scan (
  target_ip Ipv4 NOT NULL,
  target_mac MacAddr,
  subnet Cidr,
  response_time Duration,
  scan_date Date NOT NULL,
  score Float,
  metadata Json
)
```

## Default TTL

Collections can declare a default retention policy directly in DDL:

```sql
CREATE TABLE sessions (
  token Text NOT NULL,
  user_id Text NOT NULL
) WITH TTL 60m
```

This TTL is persisted as collection metadata. On insert, if the item does not provide its own TTL, RedDB materializes the collection default into the item metadata.

## Context Index

Use `WITH CONTEXT INDEX ON` to declare which columns are high-value identifiers for cross-structure context search (`SEARCH CONTEXT`). RedDB prioritizes these fields when resolving relationships across collections.

### Declare Context Index Fields

```sql
CREATE TABLE customers (
  name Text,
  passport Text,
  email Text
) WITH CONTEXT INDEX ON (passport, email)
```

### Combine with TTL

`WITH CONTEXT INDEX ON` composes with `WITH TTL` in any order:

```sql
CREATE TABLE sessions (
  token Text,
  user_id Text
) WITH TTL 24 h WITH CONTEXT INDEX ON (token)
```

> [!NOTE]
> Context-indexed fields are not unique constraints. They tell RedDB which fields carry identifying information so that `SEARCH CONTEXT` can link entities across different tables automatically.

## DROP TABLE

Remove a table and all its data:

```sql
DROP TABLE temp_data
```

## ALTER TABLE

Modify an existing table schema:

```sql
-- Add a column
ALTER TABLE users ADD COLUMN phone Phone

-- Drop a column
ALTER TABLE users DROP COLUMN phone

-- Rename a column
ALTER TABLE users RENAME COLUMN name TO full_name

-- Toggle append-only mode
ALTER TABLE events SET APPEND_ONLY = true
ALTER TABLE events SET APPEND_ONLY = false

-- Opt the table in to (or out of) Git-for-Data (VCS).
-- Works retroactively — past commits become queryable via
-- `SELECT ... AS OF COMMIT '<hash>'` as soon as the flag is on.
-- See /vcs/overview.md for the full opt-in model.
ALTER TABLE users SET VERSIONED = true
ALTER TABLE sessions SET VERSIONED = false
```

## Via HTTP

Create a collection via the DDL endpoint:

```bash
curl -X POST http://127.0.0.1:8080/collections \
  -H 'content-type: application/json' \
  -d '{"name": "hosts", "ttl": "60m"}'
```

## Via gRPC

```bash
grpcurl -plaintext \
  -d '{"payloadJson": "{\"name\":\"hosts\",\"ttl\":\"60m\"}"}' \
  127.0.0.1:50051 reddb.v1.RedDb/CreateCollection
```

Describe a collection's schema:

```bash
grpcurl -plaintext \
  -d '{"collection": "hosts"}' \
  127.0.0.1:50051 reddb.v1.RedDb/DescribeCollection
```

> [!TIP]
> Collections are also created implicitly when you insert the first entity. Explicit `CREATE TABLE` is only needed when you want to define a schema with type constraints upfront.
