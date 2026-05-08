# `red.*` Schema Reference

RedDB exposes internal metadata through read-only `red.*` collections. These
collections are queryable with ordinary SQL once the corresponding runtime
catalog is available.

Implemented relations:

| Relation          | Primary shortcut commands |
|-------------------|---------------------------|
| `red.collections` | `SHOW COLLECTIONS`, `SHOW TABLES`, `SHOW QUEUES`, `SHOW VECTORS`, `SHOW DOCUMENTS`, `SHOW TIMESERIES`, `SHOW GRAPHS`, `SHOW KV` |
| `red.columns`     | `SHOW SCHEMA <collection>` |
| `red.indices`     | `SHOW INDICES`, `SHOW INDICES ON <collection>` |
| `red.policies`    | `SHOW POLICIES`, `SHOW POLICIES ON <collection>` |
| `red.stats`       | `SHOW STATS`, `SHOW STATS <collection>` |

The `red.*` relations are virtual runtime tables, not user collections. `SELECT`
queries support ordinary projection, `WHERE`, `ORDER BY`, `LIMIT`, and `OFFSET`
clauses after the virtual table has been materialized. `INSERT`, `UPDATE`, and
`DELETE` against `red.*` fail with `system schema is read-only`.

Non-admin identities must have an active tenant before reading `red.*`; otherwise
the query fails with an active-tenant requirement. Tenant-scoped reads are limited
to the caller's visible collections. Cluster admins and tenant-less local/admin
execution can read across tenant scopes.

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

Unlike `SHOW COLLECTIONS`, typed shortcuts currently only add the model filter;
they do not automatically add `internal = false`.

Current columns:

| Column            | Description |
|-------------------|-------------|
| `name`            | Collection name. |
| `model`           | Logical model, such as `table`, `document`, `graph`, `vector`, `queue`, `time_series`, or `mixed`. |
| `schema_mode`     | Schema contract mode for the collection. |
| `entities`        | Number of live entities in the collection. |
| `segments`        | Number of backing storage segments. |
| `indices`         | Number of secondary index declarations attached to the collection. |
| `in_memory_bytes` | Approximate resident memory used by collection metadata and caches. |
| `on_disk_bytes`   | Approximate primary B-tree bytes currently reachable from the collection root. Cached for up to 30 seconds. |
| `internal`        | `true` for runtime-owned collections and artifacts such as DLQs, `audit_log`, and `red_*` stores. |
| `tenant_id`       | Tenant owning the collection, or `NULL` for global/unscoped collections. |

`on_disk_bytes` is a conservative storage estimate, not a full database-file
ownership report. It walks the live collection primary B-tree when the local
page store exposes a root page, then multiplies reachable B-tree pages by the
fixed 4 KiB page size. It excludes shared file header pages, native metadata,
freelist pages, WAL bytes, double-write buffers, sidecar files, and collection
artifacts that are not reachable from the primary B-tree root.

Internal collection detection currently marks queue DLQs, `audit_log`,
`red_*` stores, `__tenant_iso`, `__tenant_*`, and `__policy_*` collections as
internal. Direct `SELECT` queries over `red.collections` include those rows unless
the query filters them out.

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

## `red.policies`

`SHOW POLICIES` is syntax sugar for:

```sql
SELECT * FROM red.policies;
```

`SHOW POLICIES ON <name>` narrows that scan to one collection:

```sql
SELECT * FROM red.policies WHERE collection = '<name>';
```

Current columns:

| Column       | Description |
|--------------|-------------|
| `name`       | Policy name. IAM statement rows use the policy id, suffixed with `:<sid>` or `#<index>` when one policy has multiple statements. |
| `collection` | Collection targeted by the policy when it can be resolved from the local registry. |
| `kind`       | `rls` for row-level security policies or `iam` for IAM policy statements. |
| `effect`     | `allow` or `deny`. RLS policies are represented as `allow` predicates. |
| `actions`    | Action names the policy covers. RLS `ALL` is shown as `*`. |
| `principals` | RLS roles, or `*` when the RLS policy applies to all roles. |
| `predicate`  | Raw-ish RLS predicate text rendered from the stored predicate AST, or `NULL` for IAM policies. |
| `enabled`    | Whether the policy is active. IAM policy documents are shown as enabled when stored; RLS follows the collection's row-level-security flag. |

Limitations: IAM policy attachments are currently exposed by principal, not by
collection, so `red.policies` only reports IAM rows whose statement resources
resolve to exact `table:<collection>` or `collection:<collection>` resources.
The `principals` column is empty for IAM rows until attachment enumeration is
available from the auth registry.

## `red.stats`

`red.stats` exposes one operational stats row per visible collection. `SHOW
STATS` is syntax sugar for:

```sql
SELECT * FROM red.stats;
```

`SHOW STATS <name>` adds a collection filter:

```sql
SELECT * FROM red.stats WHERE collection = '<name>';
```

Current columns:

| Column            | Description |
|-------------------|-------------|
| `collection`      | Collection name. |
| `entities`        | Live entity count from `ManagerStats` when available, otherwise the catalog snapshot count. |
| `segments`        | Segment count from `ManagerStats` when available, otherwise the catalog snapshot count. |
| `growing_count`   | Number of growing segments reported by `ManagerStats`, or `0` when unavailable. |
| `sealed_count`    | Number of sealed segments reported by `ManagerStats`, or `0` when unavailable. |
| `archived_count`  | Number of archived segments reported by `ManagerStats`, or `0` when unavailable. |
| `seal_ops`        | Number of seal operations reported by `ManagerStats`, or `0` when unavailable. |
| `compact_ops`     | Number of compaction operations reported by `ManagerStats`, or `0` when unavailable. |
| `last_write_ms`   | Last write timestamp in Unix milliseconds. Currently `NULL` because collection-level write timestamps are not exposed by `ManagerStats` or the catalog snapshot APIs. |
| `attention_score` | Catalog attention score for the collection. Larger numbers indicate more severe drift, rebuild, rematerialization, or rerun needs. |

## `SHOW SAMPLE`

`SHOW SAMPLE <collection>` is syntax sugar for a limited collection scan:

```sql
SELECT * FROM <collection> LIMIT 10;
```

An explicit limit overrides the default:

```sql
SHOW SAMPLE users LIMIT 5;
```

`SHOW SAMPLE` uses the normal `SELECT` execution path, including tenant filters
and the usual missing-collection errors. It does not accept `WHERE` or
`ORDER BY`; use an explicit `SELECT` when filtering or ordering is required.

## HTTP catalog deprecation

Granular `GET /catalog/*` endpoints are deprecated in favor of `POST /query`
against `red.*` relations where an equivalent implemented relation exists. See
[`docs/api/deprecated-catalog-endpoints.md`](../api/deprecated-catalog-endpoints.md)
for endpoint-specific substitutes and sunset headers. The canonical implemented
index relation is `red.indices`.
