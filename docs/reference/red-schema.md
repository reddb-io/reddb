# `red.*` Schema Reference

RedDB exposes internal metadata through read-only `red.*` collections. These
collections are queryable with ordinary SQL once the corresponding runtime
catalog is available.

## `red.collections`

`SHOW COLLECTIONS` is syntax sugar for a `red.collections` query that hides
internal collections:

```sql
SELECT * FROM red.collections WHERE internal = false;
```

Use `INCLUDING INTERNAL` to include runtime-owned collections and artifacts:

```sql
SHOW COLLECTIONS INCLUDING INTERNAL;
```

Filters are preserved during desugaring and are combined with the default
internal filter unless `INCLUDING INTERNAL` is present:

```sql
SHOW COLLECTIONS WHERE model = 'table';
```

Typed collection shortcuts are also syntax sugar over `red.collections`:

```sql
SHOW TABLES;
SHOW QUEUES;
SHOW VECTORS;
SHOW DOCUMENTS;
SHOW TIMESERIES;
SHOW GRAPHS;
SHOW KV;
```

These expand to `SELECT * FROM red.collections WHERE model = '<type>'`, using
`table`, `queue`, `vector`, `document`, `timeseries`, `graph`, and `kv`
respectively.

Current columns:

| Column            | Description |
|-------------------|-------------|
| `name`            | Collection name. |
| `model`           | Logical model, such as table, document, graph, vector, queue, time-series, or mixed. |
| `schema_mode`     | Schema contract mode for the collection. |
| `entities`        | Number of live entities in the collection. |
| `segments`        | Number of backing storage segments. |
| `indices`         | Secondary index names attached to the collection. |
| `in_memory_bytes` | Approximate resident memory used by collection metadata and caches. |
| `internal`        | `true` for runtime-owned collections and artifacts such as DLQs, `audit_log`, and `red_*` stores. |
| `tenant_id`       | Tenant owning the collection, or `NULL` for global/unscoped collections. |

## `red.columns`

`red.columns` exposes the column-level schema known for collections.

```sql
SELECT * FROM red.columns WHERE collection = 'users';
```

`SHOW SCHEMA <name>` is syntax sugar for:

```sql
SELECT * FROM red.columns WHERE collection = '<name>';
```

Current columns:

| Column           | Description |
|------------------|-------------|
| `collection`     | Collection name. |
| `name`           | Column or inferred top-level field name. |
| `type`           | Declared SQL type, or inferred runtime value type when available. |
| `nullable`       | Whether the column or inferred field may be `NULL` or absent. |
| `default_value`  | Declared default expression, or `NULL` when no default is known. |
| `is_primary_key` | Whether the column is declared as a primary key. |
| `is_unique`      | Whether the column has a declared unique constraint or is a primary key. |

For explicit `CREATE TABLE` collections, rows come from the stored collection
contract. Document collections without an explicit schema expose inferred
top-level fields when RedDB can inspect flattened document fields; fields missing
from at least one observed document are reported as nullable. Schemaless table
contracts with no stored schema return no rows.

## `red.indices`

`red.indices` exposes visible index metadata from the live catalog and local
runtime index store.

`SHOW INDICES` is syntax sugar for:

```sql
SELECT * FROM red.indices;
```

`SHOW INDICES ON <collection>` filters by collection:

```sql
SELECT * FROM red.indices WHERE collection = '<collection>';
```

Current columns:

| Column             | Description |
|--------------------|-------------|
| `collection`       | Collection that owns the index, or `NULL` for unscoped catalog indexes. |
| `name`             | Index name. |
| `kind`             | Index implementation kind, such as `hash`, `btree`, `bitmap`, or `spatial.rtree`. |
| `declared`         | Whether the index is declared in catalog metadata. |
| `operational`      | Whether an operational index artifact is present. |
| `enabled`          | Whether the index is enabled. |
| `build_state`      | Current build state, such as `ready`, `building`, `stale`, `failed`, or `declared-unbuilt`. |
| `in_sync`          | Whether declared and operational index state agree. |
| `queryable`        | Whether the index can currently serve queries. |
| `requires_rebuild` | Whether the index should be rebuilt before it is considered healthy. |
