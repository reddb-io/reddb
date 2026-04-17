# Maintenance & DDL Extras

Grab-bag of PG-compatible commands that don't fit other doc pages:
`VACUUM`, `ANALYZE`, schemas, sequences, JSON functions, CSV import.

## VACUUM & ANALYZE

### VACUUM

Triggers segment/page flush + planner stats refresh. Optional
`FULL` rewrites entities into compacted segments; without `FULL` the
command reclaims tombstoned tuples but keeps the segment layout.

```sql
VACUUM;             -- every collection
VACUUM users;        -- just users
VACUUM FULL users;   -- aggressive rewrite
```

### ANALYZE

Refreshes histograms, null counts, and distinct estimates the planner
uses for cost calculation. Run after bulk imports, large deletes, or
whenever you notice plan regressions.

```sql
ANALYZE;             -- every collection
ANALYZE users;        -- just users
```

Both commands are idempotent and safe to run during live traffic.

## Schemas

Logical namespacing over collections. PG-style `schema.table`
references.

```sql
CREATE SCHEMA app;
CREATE SCHEMA IF NOT EXISTS reporting;

CREATE TABLE app.users (id INT, email TEXT);
CREATE TABLE reporting.daily_kpi (...);

SELECT * FROM app.users;

DROP SCHEMA reporting;
DROP SCHEMA IF EXISTS staging CASCADE;  -- CASCADE accepted; tables untouched
```

Schemas are persisted in `red_config.schema.{name}` and survive
restart.

## Sequences

Persistent monotonic counters. Values backed by atomic increments
on `red_config`.

```sql
CREATE SEQUENCE order_id_seq START 1000 INCREMENT 1;
CREATE SEQUENCE IF NOT EXISTS invoice_seq;

-- allocate next value
INSERT INTO orders (id, ...) VALUES (nextval('order_id_seq'), ...);

-- read last-allocated without advancing
SELECT currval('order_id_seq');

DROP SEQUENCE order_id_seq;
```

Defaults when omitted: `START 1`, `INCREMENT 1`.

## JSON functions

Inline JSON path queries without leaving SQL. Reuses the internal
JSONPath DSL parser.

| Function | Returns |
|----------|---------|
| `json_extract(json, '$.path')` | Value at path, or NULL |
| `json_set(json, '$.path', value)` | Mutated JSON with path set |
| `json_array_length(json)` | Integer length of top-level array |
| `json_path_query(json, path)` | Matching subtree |

```sql
SELECT json_extract(payload, '$.user.email') AS email
FROM events
WHERE json_array_length(payload->'tags') > 0;

UPDATE users
SET profile = json_set(profile, '$.verified', true)
WHERE id = 42;
```

Paths follow the JSONPath subset documented in the DSL reference:
`$.field`, `$.arr[0]`, `$..recursive`, `$[*]`.

## CSV Import / COPY

### COPY FROM

PG-compatible `COPY` statement. Bulk-insert fast path — parses the
file in the server process and pushes rows through the batched
mutation engine.

```sql
COPY users FROM '/imports/users.csv' WITH (
  DELIMITER ',',
  HEADER true
);

COPY sales FROM '/tmp/q1.psv' WITH (
  DELIMITER '|',
  QUOTE '"',
  HEADER false
);
```

Column order in the file must match the table's column order (or a
`column_list` parenthesised after the table name):

```sql
COPY users (email, id, created_at) FROM '/imports/out-of-order.csv'
  WITH (HEADER true);
```

### CLI alternative

For remote files or scripted pipelines:

```bash
red import csv \
  --table  users \
  --file   /imports/users.csv \
  --header
```

## Session functions (quick reference)

Session-scoped scalars that policies and views use:

| Function | Returns |
|----------|---------|
| `CURRENT_USER()` / `SESSION_USER()` / `USER` | Authenticated username |
| `CURRENT_ROLE()` | Role name |
| `CURRENT_TENANT()` | Session tenant id |
| `NOW()` / `CURRENT_TIMESTAMP` | Current UTC timestamp |
| `CURRENT_DATE` | Current UTC date |

All are marked `Volatile` in the function catalog — the planner does
not constant-fold them across rows.

## See also

- [Transactions](transactions.md)
- [Row Level Security](../security/rls.md)
- [Multi-Tenancy](../security/multi-tenancy.md)
- [CLI Reference](../api/cli.md)
