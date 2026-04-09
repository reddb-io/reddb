# CREATE TABLE

The `CREATE TABLE` statement defines a new collection with a typed schema.

## Syntax

```sql
CREATE TABLE table_name (
  column_name DataType [NOT NULL] [DEFAULT value],
  ...
)
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
```

## Via HTTP

Create a collection via the DDL endpoint:

```bash
curl -X POST http://127.0.0.1:8080/collections \
  -H 'content-type: application/json' \
  -d '{"name": "hosts"}'
```

## Via gRPC

```bash
grpcurl -plaintext \
  -d '{"payloadJson": "{\"name\":\"hosts\"}"}' \
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
