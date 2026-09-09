# CREATE TABLE

The `CREATE TABLE` statement defines a new collection with a typed schema.

## Syntax

```sql
CREATE TABLE table_name (
  column_name DataType [NOT NULL] [DEFAULT value] [UNIQUE],
  ...
  [ [CONSTRAINT constraint_name] UNIQUE (column [, ...]) ]
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

## Uniqueness

Use a table-level constraint when the combination of columns must be unique:

```sql
CREATE TABLE memberships (
  id INT PRIMARY KEY,
  organization TEXT NOT NULL,
  username TEXT NOT NULL,
  CONSTRAINT membership_key UNIQUE (organization, username)
)
```

A username may occur in different organizations; the same pair cannot occur twice.
`UNIQUE (organization, username)` also works without a constraint name. RedDB
assigns a stable name and includes it in `SHOW CREATE TABLE`.

Uniqueness is checked on inserts and updates and survives persistent reopen.
A tuple containing `NULL` does not conflict with another tuple; use `NOT NULL`
on every key column when that is undesirable. Missing or repeated key columns
and duplicate constraint names are rejected before the table is created.
`EXPLAIN ALTER` currently rejects changes to table-level UNIQUE constraints
because its column migration planner cannot emit `ADD/DROP CONSTRAINT`.

## Stored generated columns and CHECK

TABLE collections support deterministic expressions over their declared fields:

```sql
CREATE TABLE line_items (
  quantity INTEGER NOT NULL CHECK (quantity > 0),
  unit_price_cents INTEGER NOT NULL CHECK (unit_price_cents >= 0),
  total_cents INTEGER GENERATED ALWAYS AS (quantity * unit_price_cents) STORED
)
```

Base values and defaults normalize first. Generated fields then evaluate in
dependency order, including forward references; cycles and unknown fields are
rejected at CREATE time. The generated result must satisfy its declared type and
NOT NULL constraint. CHECK evaluates the final record: FALSE rejects the write,
while TRUE or NULL passes. Use NOT NULL when an absent value must be rejected.

INSERT, UPDATE, PATCH, upsert and bulk writes use this contract. Generated values
supplied by callers or carried from a previous version are recomputed. Definitions
and stored values survive reopening the database. A semantically invalid persisted
contract prevents reopening rather than silently removing validation.

This first version supports local scalar operators, CAST, CASE, IN, BETWEEN and
null predicates. Functions, parameters, subqueries, virtual fields and schemas for
other models are not supported here. ALTER operations that change an
expression-bearing column schema or add expressions require a future validated
backfill implementation and currently fail explicitly. Older binaries must not
write databases carrying these new expressions, because they do not enforce them.

See the [examples](../../examples/collection-expressions/README.md) and
[implementation ledger](../architecture/multimodel-building-block-program.md).

## Supported Column Types

All 50 types from the [Type System](/types/overview.md) can be used as column types:

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

For sensitive per-row data, use the `SECRET` and `PASSWORD` column types. The
schema reference documents the write constructors, read behavior, and
`VERIFY_PASSWORD` comparator in
[Sensitive Column Types](/reference/schema.md#sensitive-column-types).

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

## AI policy

The `WITH (...)` option list also accepts per-collection AI policy clauses,
alongside `tenant_by` and `append_only`. The `EMBED` clause auto-embeds the
declared text fields on every write, asynchronously over CDC:

```sql
CREATE TABLE articles (id INT, title TEXT, body TEXT)
WITH (
  EMBED (fields = ('title', 'body'), provider = 'openai', model = 'text-embedding-3-small')
)
```

`MODERATE (...)` and `VISION (...)` clauses parse and persist today but are not
yet enforced (in progress). Each clause is validated against the
[provider modality matrix](../api/ai-provider-modes.md#modality-matrix) at
`CREATE TABLE` time. See [Per-collection AI policy](ai-policy.md) for the full
grammar and behaviour.

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
curl -X POST http://127.0.0.1:5000/collections \
  -H 'content-type: application/json' \
  -d '{"name": "hosts", "ttl": "60m"}'
```

## Via gRPC

```bash
grpcurl -plaintext \
  -d '{"payloadJson": "{\"name\":\"hosts\",\"ttl\":\"60m\"}"}' \
  127.0.0.1:55055 reddb.v1.RedDb/CreateCollection
```

Describe a collection's schema:

```bash
grpcurl -plaintext \
  -d '{"collection": "hosts"}' \
  127.0.0.1:55055 reddb.v1.RedDb/DescribeCollection
```

> [!TIP]
> Collections are also created implicitly when you insert the first entity. Explicit `CREATE TABLE` is only needed when you want to define a schema with type constraints upfront.
