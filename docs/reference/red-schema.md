# `red.*` Schema Reference

RedDB exposes internal metadata through read-only `red.*` collections. These
collections are queryable with ordinary SQL once the corresponding runtime
catalog is available.

This page is the canonical reference for the implemented `red.*` surface.
All documented columns are stable since RedDB 0.1 unless an individual
description says otherwise. New columns may be added according to the evolution
rules in [ADR 0011](../../.red/adr/0011-red-schema-stability-policy.md); protocol
translation remains adapter-owned per [ADR 0010](../../.red/adr/0010-wire-adapters-translate-never-duplicate.md).

Implemented relations:

| Relation          | Primary shortcut commands |
|-------------------|---------------------------|
| `red.collections` | `SHOW COLLECTIONS`, `SHOW TABLES`, `SHOW QUEUES`, `SHOW VECTORS`, `SHOW DOCUMENTS`, `SHOW TIMESERIES`, `SHOW GRAPHS`, `SHOW KV` |
| `red.columns`     | `SHOW SCHEMA <collection>` |
| `red.show_indexes` | `SHOW INDEXES`, `SHOW INDICES`, `SHOW INDEXES ON <collection>`, `SHOW INDICES ON <collection>` |
| `red.indices`     | Full index status metadata |
| `red.policies`    | `SHOW POLICIES`, `SHOW POLICIES ON <collection>` |
| `red.stats`       | `SHOW STATS`, `SHOW STATS [FOR] <collection>` |
| `red.subscriptions` | `EVENTS STATUS`, `EVENTS STATUS <collection>` |
| `red.registry` | Governance registry metadata; see [Control Evidence Matrix](../compliance/control-evidence-matrix.md). |
| `red.registry_history` | Governance registry supersession history; see [Control Evidence Matrix](../compliance/control-evidence-matrix.md). |
| `red.managed_policies` | Managed policy guardrail metadata; see [Control Evidence Matrix](../compliance/control-evidence-matrix.md). |
| `red.control_events` | Control-plane evidence ledger; see [Control Evidence Matrix](../compliance/control-evidence-matrix.md). |
| `red.users` | Minimized user evidence metadata; see [Control Evidence Matrix](../compliance/control-evidence-matrix.md). |
| `red.api_keys` | Minimized API-key evidence metadata; see [Control Evidence Matrix](../compliance/control-evidence-matrix.md). |
| `red.control_capabilities` | Governance/evidence action vocabulary; see [Control Evidence Matrix](../compliance/control-evidence-matrix.md). |

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
| `queue_mode`      | `fanout` or `work` for queue collections; `NULL` for all other models. See [Queue Modes](../data-models/queues.md#queue-modes). |
| `dimension`       | Vector dimension for vector collections; `NULL` for all other models. |
| `metric`          | Vector distance metric for vector collections; `NULL` for all other models. |
| `session_key`     | Session-key column for time-series collections created `WITH SESSION_KEY`; `NULL` otherwise. |
| `session_gap_ms`  | Session gap in milliseconds for time-series collections created `WITH SESSION_GAP`; `NULL` otherwise. |

`on_disk_bytes` is a conservative storage estimate, not a full database-file
ownership report. It walks the live collection primary B-tree when the local
page store exposes a root page, then multiplies reachable B-tree pages by the
fixed 16 KiB page size. It excludes shared file header pages, native metadata,
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

## `red.show_indexes`

`red.show_indexes` exposes the operator-facing index summary used by `SHOW
INDEXES` and `SHOW INDICES`.

`SHOW INDEXES ON <collection>` filters by table:

```sql
SELECT * FROM red.show_indexes WHERE table = '<collection>';
```

Current columns:

| Column            | Description |
|-------------------|-------------|
| `name`            | Index name |
| `table`           | Indexed table/collection |
| `columns`         | Ordered array of indexed columns |
| `kind`            | Index method (`HASH`, `BTREE`, `BITMAP`, `H3`) |
| `unique`          | Whether the index was declared unique |
| `entries_indexed` | Number of live entries in the index backing store |

## `red.indices`

`red.indices` exposes visible index metadata from the live catalog and local
runtime index store.

Current columns:

| Column             | Description |
|--------------------|-------------|
| `collection`       | Collection that owns the index, or `NULL` for unscoped catalog indexes. |
| `name`             | Index name. |
| `kind`             | Index implementation kind, such as `hash`, `btree`, `bitmap`, or `h3`. |
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

`red.stats` is the **computed**-tier profiling view. Unlike the hot and cold
catalog-snapshot tiers, reading `red.stats` triggers an on-demand profiling scan
of the target collections — it never serves a cached snapshot. See the
[freshness tiers](#freshness-tiers) section below.

The view is **long-format**: each row is one `(collection, entity, metric,
value)` tuple, so every model can share one output contract. `entity` is the
column name for per-column metrics and `NULL` for collection-wide metrics.

`SHOW STATS` is syntax sugar for:

```sql
SELECT * FROM red.stats;
```

`SHOW STATS <name>` and the equivalent `SHOW STATS FOR <name>` add a collection
filter (and scope the profiling scan to that one collection):

```sql
SELECT * FROM red.stats WHERE collection = '<name>';
```

Because the shape is long-format, individual metrics are directly
filterable/joinable:

```sql
SELECT * FROM red.stats WHERE collection = 'users' AND metric = 'distinct_count';
```

Columns:

| Column       | Description |
|--------------|-------------|
| `collection` | Collection name being profiled. |
| `entity`     | The sub-entity the metric describes: the column name for per-column metrics, or `NULL` for collection-wide metrics. |
| `metric`     | The metric name (see below). |
| `value`      | The metric value. Type varies by metric (integer counts, strings such as `distance_metric`, arrays for `most_common_values`, or `NULL` for empty time ranges). |

Row-table (`TABLE` model) metric set:

| Metric | Entity | Value |
|--------|--------|-------|
| `row_count` | `NULL` | Number of rows in the collection. |
| `current_lsn` | `NULL` | Current runtime LSN used as the freshness pin for projection health. |
| `last_materialized_lsn` | `NULL` | Last LSN durably materialized by the checkpoint/projection path. |
| `projection_lag` | `NULL` | Difference between `current_lsn` and `last_materialized_lsn`. |
| `checkpoints_completed` | `NULL` | Number of checkpoints completed by this runtime. |
| `last_checkpoint_duration_ms` | `NULL` | Duration of the last completed checkpoint in milliseconds. |
| `pending_wal_records` | `NULL` | Pending embedded WAL records in the single-file artifact; `0` for non-embedded runtimes. |
| `null_count` | column name | Number of rows where the column is `NULL` or absent. |
| `distinct_count` | column name | Number of distinct non-null values in the column. |
| `most_common_values` | column name | Array of the column's most common values (hottest first, capped). |

KV (`KV` model) metric set:

| Metric | Entity | Value |
|--------|--------|-------|
| `entry_count` | `NULL` | Number of KV entries in the collection. |
| `total_key_bytes` | `NULL` | Estimated stored key bytes. |
| `total_value_bytes` | `NULL` | Estimated stored value bytes. |
| `value_type_count` | value type | Number of values stored with that value type. |

Graph (`GRAPH` model) metric set:

| Metric | Entity | Value |
|--------|--------|-------|
| `node_count` | `NULL` | Number of graph nodes. |
| `edge_count` | `NULL` | Number of graph edges. |
| `max_degree` | `NULL` | Largest observed node degree. |
| `avg_degree` | `NULL` | Integer average degree across nodes. |
| `node_label_count` | node label/type | Number of nodes with that label/type. |
| `edge_label_count` | edge label | Number of edges with that label. |

Vector (`VECTOR` model) metric set:

| Metric | Entity | Value |
|--------|--------|-------|
| `vector_count` | `NULL` | Number of vectors in the collection. |
| `dimension` | `NULL` | Declared vector dimension, or the first observed vector dimension. |
| `distance_metric` | `NULL` | Declared vector distance metric when present. |
| `hybrid_vector_count` | `NULL` | Number of vectors with a sparse component. |

Queue (`QUEUE` model) metric set:

| Metric | Entity | Value |
|--------|--------|-------|
| `message_count` | `NULL` | Number of queue messages. |
| `pending_count` | `NULL` | Number of unacked messages that have not been delivered. |
| `delivered_count` | `NULL` | Number of unacked messages with at least one delivery attempt. |
| `acked_count` | `NULL` | Number of acknowledged messages. |
| `max_attempts_seen` | `NULL` | Highest delivery-attempt count currently observed. |

Timeseries (`TIME_SERIES` model) metric set:

| Metric | Entity | Value |
|--------|--------|-------|
| `point_count` | `NULL` | Number of points in the collection. |
| `series_count` | `NULL` | Number of distinct metric/tag series. |
| `oldest_timestamp_ns` | `NULL` | Oldest point timestamp in nanoseconds, or `NULL` when empty. |
| `newest_timestamp_ns` | `NULL` | Newest point timestamp in nanoseconds, or `NULL` when empty. |
| `metric_point_count` | metric name | Number of points for that metric. |

### Memory budget section

Every RedDB process runs under exactly one memory budget, resolved at boot
(ADR 0073 §1). It is not a property of any collection, so it is reported under
the reserved `red.memory_budget` collection label with a `NULL` `entity`:

```sql
SELECT * FROM red.stats WHERE collection = 'red.memory_budget';
```

| Metric | Entity | Value |
|--------|--------|-------|
| `resolved_bytes` | `NULL` | The budget this process runs under, in bytes. Always greater than zero — there is no unlimited mode. |
| `source` | `NULL` | The precedence tier that produced the budget: `config`, `profile-default`, `cgroup-v2`, `cgroup-v1`, or `physical-fraction`. |
| `pool_shares` | `NULL` | Per-pool budget shares. Empty until the pool-sizing slice populates it. |
| `live_accounting` | `NULL` | Live per-pool usage. Empty until the enforcement slice populates it. |

The budget is resolved once and is immutable for the process lifetime. The
resolution order is first-hit-wins: explicit operator configuration (the
`memory_budget_bytes` startup option, else the `REDDB_MEMORY_BUDGET`
environment variable), then the deployment-profile default (serverless ships a
strict default; other profiles fall through to detection), then the container
limit (cgroup v2 `memory.max`, then cgroup v1 `memory.limit_in_bytes`), and
finally a conservative fraction of physical RAM. A cgroup value of `max` means
unlimited and falls through to physical detection. A configured value that is
not a positive byte count fails the boot with an error naming the valid form —
it is never silently replaced by a default.

Because the budget is process-scoped, `SHOW STATS <collection>` never returns
these rows; the unfiltered `SHOW STATS` does.

### Scrub section

`SCRUB` verifies the current persistent store checksum coverage without repair,
truncation, or tombstoning. A healthy foreground run returns one summary row;
findings add one row per corrupt object with these columns:

| Column | Description |
|--------|-------------|
| `row_kind` | `finding` or `summary`. |
| `zone_kind` | `superblock`, `manifest`, `wal`, `page`, `segment-chunk`, or `summary`. |
| `physical_identity` | Store-local identity for the checked object. |
| `collection` | Owning collection when derivable, else `NULL`. |
| `expected_checksum` | Stored checksum authority when available. |
| `actual_checksum` | Recomputed checksum when available. |
| `fault_class` | Best-effort attribution such as `bit-rot-evidence`, `torn-write-evidence`, or `in-progress`. |
| `objects_verified` | Objects verified by this foreground run or background tick. |
| `superblock_verified` | Superblock copies verified by this foreground run or background tick. |
| `manifest_verified` | Manifest objects verified by this foreground run or background tick. |
| `wal_verified` | WAL boundary or record objects verified by this foreground run or background tick. |
| `page_verified` | Page or snapshot objects verified by this foreground run or background tick. |
| `segment_chunk_verified` | Append-only segment chunks verified by this foreground run or background tick. |
| `bytes_read` | Bytes read by this foreground run or background tick. |
| `duration_ms` | Runtime duration for this foreground run or background tick. |

Use background mode to pace large stores:

```sql
SCRUB BACKGROUND BUDGET 128;
```

Scrub observability is process-scoped and is reported under the reserved
`red.scrub` collection label in `red.stats`:

```sql
SELECT * FROM red.stats WHERE collection = 'red.scrub';
```

| Metric | Entity | Value |
|--------|--------|-------|
| `last_run_unix_ms` | `NULL` | Last scrub run timestamp in Unix milliseconds, or `0` before the first run. |
| `last_findings_count` | `NULL` | Findings reported by the last foreground run or background tick. |
| `background_status` | `NULL` | `idle`, `running`, or `complete`. |
| `background_verified_objects` | `NULL` | Number of objects reached by the active or last completed background pass. |
| `background_total_objects` | `NULL` | Total objects in the current scrub pass. |
| `verified_objects` | zone kind | Objects verified by the last scrub result for each zone. |

### Consolidation section

Consolidation is how memory comes back to the budget (ADR 0073 §5): a
collection's sealed segments merge into one segment holding only live entities,
and the tombstones they carried are garbage-collected. The merge is paced — a
maintenance tick copies a bounded number of entities and stops, carrying the
half-built segment to the next tick.

Every collection reports what its consolidations reclaimed, with a `NULL`
`entity`:

| Metric | Entity | Value |
|--------|--------|-------|
| `consolidation_runs_started` | `NULL` | Consolidations that crossed a threshold and began copying. |
| `consolidation_runs_completed` | `NULL` | Consolidations that finished the swap. A started run that never completes was discarded by a restart. |
| `consolidation_segments_merged` | `NULL` | Source segments retired by completed swaps. |
| `consolidation_tombstones_reclaimed` | `NULL` | Tombstones garbage-collected by completed swaps. |
| `consolidation_bytes_reclaimed` | `NULL` | Bytes returned to the memory budget by completed swaps. |

The thresholds that arm a consolidation are engine constants shared by every
collection, so they are reported once under the reserved `red.consolidation`
collection label:

```sql
SELECT * FROM red.stats WHERE collection = 'red.consolidation';
```

| Metric | Entity | Value |
|--------|--------|-------|
| `tombstone_ratio_threshold` | `NULL` | Dead entities over total entities across the sealed set, above which consolidation arms. |
| `fragmentation_ratio_threshold` | `NULL` | Reclaimable bytes over held bytes, above which consolidation arms. |
| `entities_per_tick` | `NULL` | Entities copied per maintenance tick — the pacing bound of ADR 0038 §3. |

A sealed-segment count above the collection's `max_sealed_segments` also arms a
consolidation, regardless of tombstones.

These three values are observable, not contract: they are tunable
implementation constants and may change between releases. Read them to
understand what the engine decided; do not build against them.

### Freshness tiers

`red.*` columns and views split across three consistency tiers:

- **hot** — fields such as `name`, `model`, `entities`, `segments`,
  `attention`, `in_memory_bytes` read directly from the live catalog snapshot on
  every query (sub-ms).
- **cold** — fields requiring B-tree walks (e.g. `on_disk_bytes`) cache for a
  short window per collection.
- **computed** — `red.stats` runs an on-demand profiling scan on every read,
  never a cached snapshot. Computation is scan-based; the columnar analytics
  projection is a planned fast-path, not a prerequisite.

## `red.subscriptions`

`red.subscriptions` exposes event subscription metadata from the live catalog.
`EVENTS STATUS` is syntax sugar for:

```sql
SELECT * FROM red.subscriptions;
```

`EVENTS STATUS <collection>` adds a collection filter:

```sql
SELECT * FROM red.subscriptions WHERE collection = '<collection>';
```

Current columns:

| Column          | Description |
|-----------------|-------------|
| `name`          | Subscription name. Unnamed legacy/default subscriptions are exposed as `<collection>_to_<target_queue>`. |
| `collection`    | Source collection whose mutations produce events. |
| `target_queue`  | Queue receiving event payloads. |
| `mode`          | Target queue mode, `FANOUT` or `WORK`. |
| `ops_filter`    | Array of explicitly configured operations (`INSERT`, `UPDATE`, `DELETE`). Empty means all supported mutation operations. |
| `where_filter`  | Stored subscription predicate text, or `NULL` when no predicate is configured. |
| `redact_fields` | Array of redact paths applied before enqueueing event payloads. |
| `enabled`       | Whether the subscription currently emits events. |
| `outbox_lag_ms` | Current outbox delivery lag in milliseconds. This is `0` for the current synchronous outbox enqueue slice. |
| `dlq_count`     | Messages currently present in `<target_queue>_outbox_dlq`. |
| `created_at`    | Subscription catalog creation timestamp. Current metadata stores this at the source collection contract granularity. |

`EVENTS BACKFILL STATUS <collection>` is reserved for the backfill runtime
slice and is not exposed by this relation yet.

See [Events](../data-models/events.md) for subscription semantics.

## Stability and evolution

The `red.*` schema is RedDB's native introspection contract. Existing stable
columns are append-only for compatibility: removals or incompatible type changes
require deprecation notice in this reference and the stability process in
[ADR 0011](../../.red/adr/0011-red-schema-stability-policy.md). Postgres-wire catalog
views translate to this native surface at the adapter boundary; the runtime does
not expose `pg_catalog` as a parallel source of truth, per
[ADR 0010](../../.red/adr/0010-wire-adapters-translate-never-duplicate.md).

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
