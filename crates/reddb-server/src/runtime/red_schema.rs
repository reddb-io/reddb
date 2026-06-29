//! Runtime-backed virtual `red.*` schema tables.
//!
//! The SQL parser does not currently accept schema-qualified table
//! identifiers in `FROM`, so the runtime rewrites the small virtual
//! surface it owns (`red.collections`, `red.columns`, `red.describe`,
//! `red.show_create`, `red.show_indexes`, `red.indices`, `red.policies`,
//! `red.stats`, `red.subscriptions`) to internal identifiers before normal parsing.
//! Execution then intercepts that identifier and materializes rows from the live catalog snapshot.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::*;
use crate::auth::policies::{ActionPattern, Effect, Policy, ResourcePattern, Statement};
use crate::catalog::{CollectionModel, SchemaMode};
use crate::runtime::mvcc::current_connection_id;
use crate::storage::query::ast::{CompareOp, Expr, FieldRef, Filter, PolicyAction, UnaryOp};
use crate::storage::query::sql_lowering::{effective_table_filter, effective_table_projections};
use crate::storage::schema::DataType;
use crate::storage::unified::EntityData;
use crate::storage::unified::UnifiedStore;

pub(super) const COLLECTIONS: &str = "red.collections";
pub(super) const COLLECTIONS_INTERNAL: &str = "__red_schema_collections";
pub(super) const COLUMNS: &str = "red.columns";
pub(super) const COLUMNS_INTERNAL: &str = "__red_schema_columns";
pub(super) const DESCRIBE: &str = "red.describe";
pub(super) const DESCRIBE_INTERNAL: &str = "__red_schema_describe";
pub(super) const SHOW_CREATE: &str = "red.show_create";
pub(super) const SHOW_CREATE_INTERNAL: &str = "__red_schema_show_create";
pub(super) const SHOW_INDEXES: &str = "red.show_indexes";
pub(super) const SHOW_INDEXES_INTERNAL: &str = "__red_schema_show_indexes";
pub(super) const INDICES: &str = "red.indices";
pub(super) const INDICES_INTERNAL: &str = "__red_schema_indices";
pub(super) const POLICIES: &str = "red.policies";
pub(super) const POLICIES_INTERNAL: &str = "__red_schema_policies";
pub(super) const STATS: &str = "red.stats";
pub(super) const STATS_INTERNAL: &str = "__red_schema_stats";
pub(super) const SUBSCRIPTIONS: &str = "red.subscriptions";
pub(super) const SUBSCRIPTIONS_INTERNAL: &str = "__red_schema_subscriptions";
// Issue #580 — DeclarativeRetention slice 1. Per-collection retention
// state: `(name, retention_duration, oldest_row_ts,
// expired_row_count_estimate)`. Materialised views are not subject to
// source retention by default in this slice.
pub(super) const RETENTION: &str = "red.retention";
pub(super) const RETENTION_INTERNAL: &str = "__red_schema_retention";
// Issue #583 — ContinuousMaterializedView slice 10. Per-view runtime
// state: `(name, query_text, refresh_every_ms, last_refresh_at,
// last_refresh_duration_ms, last_error, current_row_count)`.
pub(super) const MATERIALIZED_VIEWS: &str = "red.materialized_views";
pub(super) const MATERIALIZED_VIEWS_INTERNAL: &str = "__red_schema_materialized_views";
// Issue #536 — QueueLifecycle slice 9. Per-row pending-delivery drill-
// down: `(queue, group, message_id, delivery_id, attempts,
// lock_deadline, locked_by)`. Cold-scan tier (no caching) so operators
// see live state when auditing stuck consumers.
pub(super) const QUEUE_PENDING: &str = "red.queue_pending";
pub(super) const QUEUE_PENDING_INTERNAL: &str = "__red_schema_queue_pending";
// Issue #535 — QueueLifecycle slice 8. Per-queue introspection with
// queue-shaped columns: `(name, mode, depth, total_pending,
// oldest_pending_age, dlq_target, attention, internal)`. Hot fields
// (mode, depth, dlq_target, attention) come from the catalog
// descriptor; total_pending / oldest_pending_age are computed via a
// single pass over `red_queue_meta` pending rows (`queue_pending`
// legacy and `queue_pending_lc` lifecycle) so they remain consistent
// with the source of truth. `internal` exists so the `SHOW QUEUES`
// desugar can hide DLQ-target queues by default and surface them
// under `SHOW QUEUES INCLUDING INTERNAL`, mirroring the
// `red.collections.internal` contract.
pub(super) const QUEUES: &str = "red.queues";
pub(super) const QUEUES_INTERNAL: &str = "__red_schema_queues";
// Issue #577 — AnalyticsSchemaRegistry slice 2. Per-event-name schema
// versions: `(event_name, version, schema_json, registered_at)`.
pub(super) const SCHEMA_REGISTRY: &str = "red.schema_registry";
pub(super) const SCHEMA_REGISTRY_INTERNAL: &str = "__red_schema_schema_registry";
// Issue #784 — Analytics v0 metric descriptor catalog.
pub(super) const ANALYTICS_METRICS: &str = "red.analytics.metrics";
pub(super) const ANALYTICS_METRICS_INTERNAL: &str = "__red_schema_analytics_metrics";
// Issue #791 — Analytics v0 SLO descriptor catalog over SLI metrics.
pub(super) const ANALYTICS_SLOS: &str = "red.analytics.slos";
pub(super) const ANALYTICS_SLOS_INTERNAL: &str = "__red_schema_analytics_slos";
// Issue #787 — event-shaped analytics source profiles over normal collections.
pub(super) const ANALYTICS_SOURCES: &str = "red.analytics.sources";
pub(super) const ANALYTICS_SOURCES_INTERNAL: &str = "__red_schema_analytics_sources";
// Issue #655 — governance/evidence surfaces. These are read-only
// projections over runtime-owned stores, not SQL collections.
pub(super) const GOVERNANCE_REGISTRY: &str = "red.registry";
pub(super) const GOVERNANCE_REGISTRY_INTERNAL: &str = "__red_schema_registry";
pub(super) const GOVERNANCE_REGISTRY_HISTORY: &str = "red.registry_history";
pub(super) const GOVERNANCE_REGISTRY_HISTORY_INTERNAL: &str = "__red_schema_registry_history";
pub(super) const MANAGED_POLICIES: &str = "red.managed_policies";
pub(super) const MANAGED_POLICIES_INTERNAL: &str = "__red_schema_managed_policies";
pub(super) const CONTROL_EVENTS: &str = "red.control_events";
pub(super) const CONTROL_EVENTS_INTERNAL: &str = "__red_schema_control_events";
pub(super) const USERS: &str = "red.users";
pub(super) const USERS_INTERNAL: &str = "__red_schema_users";
pub(super) const API_KEYS: &str = "red.api_keys";
pub(super) const API_KEYS_INTERNAL: &str = "__red_schema_api_keys";
pub(super) const CONTROL_CAPABILITIES: &str = "red.control_capabilities";
pub(super) const CONTROL_CAPABILITIES_INTERNAL: &str = "__red_schema_control_capabilities";
// Issue #709 — ActionCatalog SQL surface. One row per entry in
// `auth::action_catalog::ACTIONS` so operators can browse the canonical
// list of policy action verbs from SQL.
pub(super) const POLICY_ACTIONS: &str = "red.policy.actions";
pub(super) const POLICY_ACTIONS_INTERNAL: &str = "__red_schema_policy_actions";
// Issue #745 — typed red.* model-shaped projections for Red UI toolbars.
// These are read-only projections over `red.collections`, declared
// column contracts, and the existing stat surfaces — *not* new sources
// of truth. The UI gains stable, type-specific columns without having
// to derive everything from the generic catalog.
pub(super) const TABLES: &str = "red.tables";
pub(super) const TABLES_INTERNAL: &str = "__red_schema_tables";
pub(super) const DOCUMENTS: &str = "red.documents";
pub(super) const DOCUMENTS_INTERNAL: &str = "__red_schema_documents";
pub(super) const KV: &str = "red.kv";
pub(super) const KV_INTERNAL: &str = "__red_schema_kv";
// Issue #746 — typed `red.vectors` / `red.graphs` model-shaped
// projections for Red UI toolbars. Like #745's trio, these are
// read-only projections over `red.collections` plus the catalog
// contract (and, for vectors, the optional introspection registry
// from #743). Rich fields that depend on later artifact / viewport
// publish points (e.g. `artifact_state`) surface explicit
// `unavailable` / NULL values rather than blocking on those slices.
pub(super) const VECTORS: &str = "red.vectors";
pub(super) const VECTORS_INTERNAL: &str = "__red_schema_vectors";
pub(super) const GRAPHS: &str = "red.graphs";
pub(super) const GRAPHS_INTERNAL: &str = "__red_schema_graphs";
// Issue #747 — typed `red.timeseries` / `red.metrics` projections for
// chart and KPI controls. `red.timeseries` is one row per `model =
// time_series` collection (with hypertable-derived chunk metadata when
// the collection was created via `CREATE HYPERTABLE`); `red.metrics`
// is one row per descriptor registered through `CREATE METRIC`,
// enriched with stable capability + retention columns the UI can lean
// on without parsing the generic catalog or the analytics descriptor
// surface.
pub(super) const TIMESERIES: &str = "red.timeseries";
pub(super) const TIMESERIES_INTERNAL: &str = "__red_schema_timeseries";
pub(super) const METRICS: &str = "red.metrics";
pub(super) const METRICS_INTERNAL: &str = "__red_schema_metrics";
// Issue #748 — per-chunk metadata for hypertables (range bounds,
// row count, sweep/expiry state) so the Red UI maintenance panel
// can render a chunks-per-hypertable table without scraping the
// internal registry.
pub(super) const HYPERTABLE_CHUNKS: &str = "red.hypertable_chunks";
pub(super) const HYPERTABLE_CHUNKS_INTERNAL: &str = "__red_schema_hypertable_chunks";
// Issue #748 — writes-by-cohort bucketing over hypertable rows.
// Emits one row per (hypertable, bucket_size_ms, bucket_start_ms)
// tuple covering the canonical 1m/5m/10m cohort sizes. `events_count`
// counts data rows by the hypertable's time column; `writes_count` is
// the actual write/throughput field — held at `NULL` (unavailable)
// until reliable WAL/operation telemetry exists, per the
// thread-discussion decision on #748.
pub(super) const TIMESERIES_WRITES: &str = "red.timeseries_writes";
pub(super) const TIMESERIES_WRITES_INTERNAL: &str = "__red_schema_timeseries_writes";
pub(super) const COMMITS: &str = "red.commits";
pub(super) const COMMITS_INTERNAL: &str = "__red_schema_commits";
pub(super) const BRANCHES: &str = "red.branches";
pub(super) const BRANCHES_INTERNAL: &str = "__red_schema_branches";
pub(super) const TAGS: &str = "red.tags";
pub(super) const TAGS_INTERNAL: &str = "__red_schema_tags";
pub(super) const STATUS: &str = "red.status";
pub(super) const STATUS_INTERNAL: &str = "__red_schema_status";
pub(super) const CONFLICTS: &str = "red.conflicts";
pub(super) const CONFLICTS_INTERNAL: &str = "__red_schema_conflicts";
pub(super) const VERSIONED: &str = "red.versioned";
pub(super) const VERSIONED_INTERNAL: &str = "__red_schema_versioned";
pub(super) const READ_ONLY_ERROR: &str = "system schema is read-only";

const COLLECTION_COLUMNS: [&str; 15] = [
    "name",
    "model",
    "schema_mode",
    "entities",
    "segments",
    "indices",
    "in_memory_bytes",
    "on_disk_bytes",
    "internal",
    "tenant_id",
    "queue_mode",
    "dimension",
    "metric",
    // Timeseries-only — populated when `CREATE TIMESERIES ... WITH
    // SESSION_KEY <col> SESSION_GAP <duration>` was used. NULL
    // otherwise. Issue #576 slice 1.
    "session_key",
    "session_gap_ms",
];

const COLUMN_COLUMNS: [&str; 7] = [
    "collection",
    "name",
    "type",
    "nullable",
    "default_value",
    "is_primary_key",
    "is_unique",
];

const DESCRIBE_COLUMNS: [&str; 5] = ["name", "type", "nullable", "default", "indexed"];

const SHOW_CREATE_COLUMNS: [&str; 1] = ["ddl"];

const SHOW_INDEX_COLUMNS: [&str; 6] = [
    "name",
    "table",
    "columns",
    "kind",
    "unique",
    "entries_indexed",
];

const INDEX_COLUMNS: [&str; 10] = [
    "collection",
    "name",
    "kind",
    "declared",
    "operational",
    "enabled",
    "build_state",
    "in_sync",
    "queryable",
    "requires_rebuild",
];

const POLICY_COLUMNS: [&str; 8] = [
    "name",
    "collection",
    "kind",
    "effect",
    "actions",
    "principals",
    "predicate",
    "enabled",
];

const STATS_COLUMNS: [&str; 10] = [
    "collection",
    "entities",
    "segments",
    "growing_count",
    "sealed_count",
    "archived_count",
    "seal_ops",
    "compact_ops",
    "last_write_ms",
    "attention_score",
];

const RETENTION_COLUMNS: [&str; 7] = [
    "name",
    "retention_duration",
    "oldest_row_ts",
    "expired_row_count_estimate",
    // Issue #584 slice 12 — sweeper observability columns.
    "last_sweep_at",
    "rows_swept_total",
    "current_rows_pending_sweep_estimate",
];

const QUEUE_COLUMNS: [&str; 8] = [
    "name",
    "mode",
    "depth",
    "total_pending",
    "oldest_pending_age",
    "dlq_target",
    "attention",
    "internal",
];

const QUEUE_PENDING_COLUMNS: [&str; 7] = [
    "queue",
    "group",
    "message_id",
    "delivery_id",
    "attempts",
    "lock_deadline",
    "locked_by",
];

const MATERIALIZED_VIEW_COLUMNS: [&str; 7] = [
    "name",
    "query_text",
    "refresh_every_ms",
    "last_refresh_at",
    "last_refresh_duration_ms",
    "last_error",
    "current_row_count",
];

const SCHEMA_REGISTRY_COLUMNS: [&str; 4] =
    ["event_name", "version", "schema_json", "registered_at"];

const ANALYTICS_METRIC_COLUMNS: [&str; 8] = [
    "path",
    "kind",
    "role",
    "created_at",
    // Issue #790 — derived metric descriptor metadata. NULL on
    // non-derived (raw) descriptors. These are stable taxonomy
    // columns, not high-cardinality dimensions: they name the
    // *inputs* the future execution layer would consume.
    "source",
    "query",
    "window_ms",
    "time_field",
];

const ANALYTICS_SLO_COLUMNS: [&str; 5] = ["path", "metric", "target", "window_ms", "created_at"];

const ANALYTICS_SOURCE_COLUMNS: [&str; 8] = [
    "name",
    "collection",
    "time_field",
    "event_field",
    "actor_field",
    "session_field",
    "properties_field",
    "created_at",
];

const GOVERNANCE_REGISTRY_COLUMNS: [&str; 12] = [
    "id",
    "version",
    "resource_type",
    "schema",
    "mutability",
    "sensitivity",
    "managed",
    "required_action",
    "required_resource",
    "evidence_requirement",
    "updated_by",
    "updated_at",
];

const GOVERNANCE_REGISTRY_HISTORY_COLUMNS: [&str; 15] = [
    "id",
    "version",
    "resource_type",
    "schema",
    "mutability",
    "sensitivity",
    "managed",
    "required_action",
    "required_resource",
    "evidence_requirement",
    "updated_by",
    "updated_at",
    "superseded_by",
    "superseded_at",
    "change_reason",
];

const MANAGED_POLICY_COLUMNS: [&str; 9] = [
    "policy_id",
    "registry_id",
    "version",
    "schema",
    "required_action",
    "required_resource",
    "evidence_requirement",
    "updated_by",
    "updated_at",
];

const CONTROL_EVENT_COLUMNS: [&str; 14] = [
    "id",
    "ts",
    "kind",
    "outcome",
    "actor_kind",
    "actor_user_id",
    "scope",
    "action",
    "resource",
    "reason",
    "matched_policy_id",
    "request_id",
    "trace_id",
    "fields_json",
];

const USER_COLUMNS: [&str; 7] = [
    "username",
    "tenant_id",
    "role",
    "enabled",
    "created_at",
    "updated_at",
    "api_key_count",
];

const API_KEY_COLUMNS: [&str; 6] = [
    "owner",
    "tenant_id",
    "name",
    "role",
    "created_at",
    "key_fingerprint",
];

const CONTROL_CAPABILITY_COLUMNS: [&str; 4] = ["action", "resource_kind", "scope", "description"];

const POLICY_ACTION_COLUMNS: [&str; 6] = [
    "name",
    "category",
    "lifecycle_state",
    "replacement",
    "since_version",
    "gates_description",
];

// Issue #745 — typed `red.tables`. `has_primary_key` is the edit
// affordance the UI keys on: tables with a primary key support row-
// level edits; the rest are append-mostly.
const TABLE_COLUMNS: [&str; 10] = [
    "name",
    "schema_mode",
    "row_count",
    "column_count",
    "index_count",
    "has_primary_key",
    "in_memory_bytes",
    "on_disk_bytes",
    "tenant_id",
    "internal",
];

// Issue #745 — typed `red.documents`. `supports_json_path` is the
// stable capability indicator the UI uses to decide whether to expose
// JSON path edit affordances. Always true for the document model.
const DOCUMENT_COLUMNS: [&str; 8] = [
    "name",
    "schema_mode",
    "document_count",
    "inferred_field_count",
    "supports_json_path",
    "in_memory_bytes",
    "on_disk_bytes",
    "internal",
];

// Issue #745 — typed `red.kv`. `key_type`/`value_type` are the
// declared key/value shape hints. `supports_prefix_scan` is a stable
// capability flag — true for the KV model, which always supports
// prefix scans through `KEYS WITH PREFIX`.
const KV_COLUMNS: [&str; 8] = [
    "name",
    "entries",
    "key_type",
    "value_type",
    "supports_prefix_scan",
    "in_memory_bytes",
    "on_disk_bytes",
    "internal",
];

// Issue #746 — typed `red.vectors`. `search_capable` is the UI's edit
// affordance for SEARCH toolbars. `artifact_state` is a stable
// lifecycle bucket sourced from the vector introspection registry
// (#743); when no entry has been published it defaults to
// `unavailable`, never NULL, so the UI can render a stable badge
// without conditional rendering on missing data.
const VECTOR_COLUMNS: [&str; 10] = [
    "name",
    "dimensions",
    "metric",
    "vector_count",
    "search_capable",
    "artifact_state",
    "in_memory_bytes",
    "on_disk_bytes",
    "tenant_id",
    "internal",
];

// Issue #747 — typed `red.timeseries`. One row per `model =
// time_series` collection. Hypertable-derived columns (`time_column`,
// `chunk_interval_ms`, `chunk_count`, `oldest_ts_ms`, `newest_ts_ms`)
// are populated from the live `HypertableRegistry` when the collection
// was created via `CREATE HYPERTABLE`; standalone timeseries report
// `is_hypertable = false` and `NULL` for the chunk-derived columns.
// `retention_ms`, `session_key`, and `session_gap_ms` are sourced from
// the collection descriptor so the UI can show TTL/sessionization
// chips without a separate join.
// Issue #748 — extends the #747 column set with four operational
// indicators the Red UI hypertable / timeseries maintenance panels
// need: declared downsample policies, count and names of continuous
// aggregates that materialise from this collection, and a last-sweep
// timestamp (NULL today — per-collection sweep tracking is not yet
// wired into `RetentionRegistry::stats`, AC #3 wants missing optional
// features represented as unavailable rather than inferred).
const TIMESERIES_COLUMNS: [&str; 20] = [
    "name",
    "schema_mode",
    "is_hypertable",
    "time_column",
    "chunk_interval_ms",
    "chunk_count",
    "retention_ms",
    "session_key",
    "session_gap_ms",
    "row_count",
    "oldest_ts_ms",
    "newest_ts_ms",
    "in_memory_bytes",
    "on_disk_bytes",
    "tenant_id",
    "internal",
    "downsample_policies",
    "continuous_aggregate_count",
    "continuous_aggregate_names",
    "last_sweep_ms",
];

// Issue #748 — per-chunk hypertable metadata. One row per
// `(hypertable, chunk_start_ms)` covering the registry's chunk map.
// `min_ts_ms` / `max_ts_ms` come back `NULL` for empty chunks rather
// than reporting the sentinel `u64::MAX` minimum. `ttl_override_ms` is
// the per-chunk override (if any), `effective_ttl_ms` falls back to
// the hypertable-wide default, `expiry_ms` is `max_ts_ns +
// effective_ttl_ms` and `is_expired` is computed against `now()` at
// snapshot time — this is what the retention sweeper would drop on
// the next cycle.
const HYPERTABLE_CHUNK_COLUMNS: [&str; 12] = [
    "hypertable",
    "chunk_start_ms",
    "chunk_end_ms",
    "row_count",
    "min_ts_ms",
    "max_ts_ms",
    "sealed",
    "ttl_override_ms",
    "effective_ttl_ms",
    "expiry_ms",
    "is_expired",
    "tenant_id",
];

// Issue #748 — writes-by-cohort. `events_count` is the row count of
// the hypertable's time column falling inside this bucket; the
// thread-discussion decision (2026-05-27) on this issue requires we
// keep a *separate* column for actual write throughput that may stay
// `unavailable` until WAL/operation telemetry exists. `writes_count`
// is that field — pinned `NULL` today.
const TIMESERIES_WRITES_COLUMNS: [&str; 5] = [
    "collection",
    "bucket_size_ms",
    "bucket_start_ms",
    "events_count",
    "writes_count",
];

const COMMIT_COLUMNS: [&str; 11] = [
    "hash",
    "root_xid",
    "parents",
    "height",
    "author_name",
    "author_email",
    "committer_name",
    "committer_email",
    "message",
    "timestamp_ms",
    "signature",
];

const REF_COLUMNS: [&str; 4] = ["name", "kind", "target", "protected"];

const STATUS_COLUMNS: [&str; 8] = [
    "connection_id",
    "head_ref",
    "head_commit",
    "detached",
    "staged_changes",
    "working_changes",
    "unresolved_conflicts",
    "merge_state_id",
];

const CONFLICT_COLUMNS: [&str; 8] = [
    "id",
    "collection",
    "entity_id",
    "base",
    "ours",
    "theirs",
    "conflicting_paths",
    "merge_state_id",
];

const VERSIONED_COLUMNS: [&str; 2] = ["collection", "versioned"];

// Issue #746 — typed `red.graphs`. `supports_viewport` is the stable
// capability indicator the Red UI graph explorer keys on (graph
// viewport contract #744 has landed). `supports_algorithms` is a
// stable capability flag (true — `GRAPH CENTRALITY / SHORTEST_PATH /
// ...` are always available against any graph collection).
// `node_labels` / `edge_labels` are deterministic, sorted arrays of
// the distinct labels observed in this collection's nodes / edges
// at snapshot time.
const GRAPH_COLUMNS: [&str; 10] = [
    "name",
    "node_count",
    "edge_count",
    "node_labels",
    "edge_labels",
    "supports_viewport",
    "supports_algorithms",
    "in_memory_bytes",
    "on_disk_bytes",
    "internal",
];

// Issue #747 — typed `red.metrics`. One row per metric descriptor
// registered through `CREATE METRIC`. `labels` / `unit` / `retention_ms`
// are columns the UI can render today but the descriptor catalog does
// not yet store them — populated as `NULL` until the catalog grows
// them, so the schema is stable. `supports_prometheus_query` is a
// stable capability indicator (true — every registered metric is
// queryable through the Prometheus adapter when written into a
// metrics collection).
const METRICS_COLUMNS: [&str; 8] = [
    "path",
    "kind",
    "role",
    "labels",
    "unit",
    "retention_ms",
    "supports_prometheus_query",
    "created_at_ms",
];

const SUBSCRIPTION_COLUMNS: [&str; 11] = [
    "name",
    "collection",
    "target_queue",
    "mode",
    "ops_filter",
    "where_filter",
    "redact_fields",
    "enabled",
    "outbox_lag_ms",
    "dlq_count",
    "created_at",
];

pub(super) fn rewrite_virtual_names(query: &str) -> Option<String> {
    let mut rewritten = query.to_string();
    let mut changed = false;

    for (public, internal) in [
        (COLLECTIONS, COLLECTIONS_INTERNAL),
        (COLUMNS, COLUMNS_INTERNAL),
        (DESCRIBE, DESCRIBE_INTERNAL),
        (SHOW_CREATE, SHOW_CREATE_INTERNAL),
        (SHOW_INDEXES, SHOW_INDEXES_INTERNAL),
        (INDICES, INDICES_INTERNAL),
        (POLICY_ACTIONS, POLICY_ACTIONS_INTERNAL),
        (POLICIES, POLICIES_INTERNAL),
        (STATS, STATS_INTERNAL),
        (SUBSCRIPTIONS, SUBSCRIPTIONS_INTERNAL),
        (RETENTION, RETENTION_INTERNAL),
        (MATERIALIZED_VIEWS, MATERIALIZED_VIEWS_INTERNAL),
        (QUEUE_PENDING, QUEUE_PENDING_INTERNAL),
        (QUEUES, QUEUES_INTERNAL),
        (ANALYTICS_METRICS, ANALYTICS_METRICS_INTERNAL),
        (ANALYTICS_SLOS, ANALYTICS_SLOS_INTERNAL),
        (ANALYTICS_SOURCES, ANALYTICS_SOURCES_INTERNAL),
        (SCHEMA_REGISTRY, SCHEMA_REGISTRY_INTERNAL),
        (
            GOVERNANCE_REGISTRY_HISTORY,
            GOVERNANCE_REGISTRY_HISTORY_INTERNAL,
        ),
        (GOVERNANCE_REGISTRY, GOVERNANCE_REGISTRY_INTERNAL),
        (MANAGED_POLICIES, MANAGED_POLICIES_INTERNAL),
        (CONTROL_EVENTS, CONTROL_EVENTS_INTERNAL),
        (USERS, USERS_INTERNAL),
        (API_KEYS, API_KEYS_INTERNAL),
        (CONTROL_CAPABILITIES, CONTROL_CAPABILITIES_INTERNAL),
        (TABLES, TABLES_INTERNAL),
        (DOCUMENTS, DOCUMENTS_INTERNAL),
        (KV, KV_INTERNAL),
        (VECTORS, VECTORS_INTERNAL),
        (GRAPHS, GRAPHS_INTERNAL),
        // `red.timeseries_writes` and `red.hypertable_chunks` must
        // run before `red.timeseries` because the substring rewrite
        // would otherwise turn `red.timeseries_writes` into
        // `__red_schema_timeseries_writes` (the wrong target).
        (TIMESERIES_WRITES, TIMESERIES_WRITES_INTERNAL),
        (HYPERTABLE_CHUNKS, HYPERTABLE_CHUNKS_INTERNAL),
        (TIMESERIES, TIMESERIES_INTERNAL),
        (METRICS, METRICS_INTERNAL),
        (COMMITS, COMMITS_INTERNAL),
        (BRANCHES, BRANCHES_INTERNAL),
        (TAGS, TAGS_INTERNAL),
        (STATUS, STATUS_INTERNAL),
        (CONFLICTS, CONFLICTS_INTERNAL),
        (VERSIONED, VERSIONED_INTERNAL),
    ] {
        if let Some(next) = replace_case_insensitive_outside_quotes(&rewritten, public, internal) {
            rewritten = next;
            changed = true;
        }
    }

    changed.then_some(rewritten)
}

pub(super) fn references_system_schema(query: &str) -> bool {
    contains_case_insensitive_outside_quotes(query, "red.")
}

pub(super) fn is_system_schema_write(query: &str) -> bool {
    let trimmed = query.trim_start();
    let mut tokens = trimmed.split(|c: char| c.is_whitespace() || c == '(' || c == ';');
    let first = tokens.next().unwrap_or("");
    let second = tokens.next().unwrap_or("");
    if first.eq_ignore_ascii_case("DELETE")
        && matches_ignore_ascii_case(second, &["SECRET", "CONFIG", "VAULT"])
    {
        return false;
    }
    matches_ignore_ascii_case(first, &["INSERT", "UPDATE", "DELETE", "TRUNCATE"])
        && references_system_schema(query)
}

pub(super) fn is_virtual_table(table: &str) -> bool {
    table.eq_ignore_ascii_case(COLLECTIONS_INTERNAL)
        || table.eq_ignore_ascii_case(COLLECTIONS)
        || table.eq_ignore_ascii_case(COLUMNS_INTERNAL)
        || table.eq_ignore_ascii_case(COLUMNS)
        || table.eq_ignore_ascii_case(DESCRIBE_INTERNAL)
        || table.eq_ignore_ascii_case(DESCRIBE)
        || table.eq_ignore_ascii_case(SHOW_CREATE_INTERNAL)
        || table.eq_ignore_ascii_case(SHOW_CREATE)
        || table.eq_ignore_ascii_case(SHOW_INDEXES_INTERNAL)
        || table.eq_ignore_ascii_case(SHOW_INDEXES)
        || table.eq_ignore_ascii_case(INDICES_INTERNAL)
        || table.eq_ignore_ascii_case(INDICES)
        || table.eq_ignore_ascii_case(POLICIES_INTERNAL)
        || table.eq_ignore_ascii_case(POLICIES)
        || table.eq_ignore_ascii_case(STATS_INTERNAL)
        || table.eq_ignore_ascii_case(STATS)
        || table.eq_ignore_ascii_case(SUBSCRIPTIONS_INTERNAL)
        || table.eq_ignore_ascii_case(SUBSCRIPTIONS)
        || table.eq_ignore_ascii_case(RETENTION_INTERNAL)
        || table.eq_ignore_ascii_case(RETENTION)
        || table.eq_ignore_ascii_case(MATERIALIZED_VIEWS_INTERNAL)
        || table.eq_ignore_ascii_case(MATERIALIZED_VIEWS)
        || table.eq_ignore_ascii_case(QUEUE_PENDING_INTERNAL)
        || table.eq_ignore_ascii_case(QUEUE_PENDING)
        || table.eq_ignore_ascii_case(QUEUES_INTERNAL)
        || table.eq_ignore_ascii_case(QUEUES)
        || table.eq_ignore_ascii_case(ANALYTICS_METRICS_INTERNAL)
        || table.eq_ignore_ascii_case(ANALYTICS_METRICS)
        || table.eq_ignore_ascii_case(ANALYTICS_SLOS_INTERNAL)
        || table.eq_ignore_ascii_case(ANALYTICS_SLOS)
        || table.eq_ignore_ascii_case(ANALYTICS_SOURCES_INTERNAL)
        || table.eq_ignore_ascii_case(ANALYTICS_SOURCES)
        || table.eq_ignore_ascii_case(SCHEMA_REGISTRY_INTERNAL)
        || table.eq_ignore_ascii_case(SCHEMA_REGISTRY)
        || table.eq_ignore_ascii_case(GOVERNANCE_REGISTRY_INTERNAL)
        || table.eq_ignore_ascii_case(GOVERNANCE_REGISTRY)
        || table.eq_ignore_ascii_case(GOVERNANCE_REGISTRY_HISTORY_INTERNAL)
        || table.eq_ignore_ascii_case(GOVERNANCE_REGISTRY_HISTORY)
        || table.eq_ignore_ascii_case(MANAGED_POLICIES_INTERNAL)
        || table.eq_ignore_ascii_case(MANAGED_POLICIES)
        || table.eq_ignore_ascii_case(CONTROL_EVENTS_INTERNAL)
        || table.eq_ignore_ascii_case(CONTROL_EVENTS)
        || table.eq_ignore_ascii_case(USERS_INTERNAL)
        || table.eq_ignore_ascii_case(USERS)
        || table.eq_ignore_ascii_case(API_KEYS_INTERNAL)
        || table.eq_ignore_ascii_case(API_KEYS)
        || table.eq_ignore_ascii_case(CONTROL_CAPABILITIES_INTERNAL)
        || table.eq_ignore_ascii_case(CONTROL_CAPABILITIES)
        || table.eq_ignore_ascii_case(POLICY_ACTIONS_INTERNAL)
        || table.eq_ignore_ascii_case(POLICY_ACTIONS)
        || table.eq_ignore_ascii_case(TABLES_INTERNAL)
        || table.eq_ignore_ascii_case(TABLES)
        || table.eq_ignore_ascii_case(DOCUMENTS_INTERNAL)
        || table.eq_ignore_ascii_case(DOCUMENTS)
        || table.eq_ignore_ascii_case(KV_INTERNAL)
        || table.eq_ignore_ascii_case(KV)
        || table.eq_ignore_ascii_case(VECTORS_INTERNAL)
        || table.eq_ignore_ascii_case(VECTORS)
        || table.eq_ignore_ascii_case(GRAPHS_INTERNAL)
        || table.eq_ignore_ascii_case(GRAPHS)
        || table.eq_ignore_ascii_case(TIMESERIES_INTERNAL)
        || table.eq_ignore_ascii_case(TIMESERIES)
        || table.eq_ignore_ascii_case(METRICS_INTERNAL)
        || table.eq_ignore_ascii_case(METRICS)
        || table.eq_ignore_ascii_case(HYPERTABLE_CHUNKS_INTERNAL)
        || table.eq_ignore_ascii_case(HYPERTABLE_CHUNKS)
        || table.eq_ignore_ascii_case(TIMESERIES_WRITES_INTERNAL)
        || table.eq_ignore_ascii_case(TIMESERIES_WRITES)
        || table.eq_ignore_ascii_case(COMMITS_INTERNAL)
        || table.eq_ignore_ascii_case(COMMITS)
        || table.eq_ignore_ascii_case(BRANCHES_INTERNAL)
        || table.eq_ignore_ascii_case(BRANCHES)
        || table.eq_ignore_ascii_case(TAGS_INTERNAL)
        || table.eq_ignore_ascii_case(TAGS)
        || table.eq_ignore_ascii_case(STATUS_INTERNAL)
        || table.eq_ignore_ascii_case(STATUS)
        || table.eq_ignore_ascii_case(CONFLICTS_INTERNAL)
        || table.eq_ignore_ascii_case(CONFLICTS)
        || table.eq_ignore_ascii_case(VERSIONED_INTERNAL)
        || table.eq_ignore_ascii_case(VERSIONED)
}

pub(super) fn red_query(
    runtime: &RedDBRuntime,
    virtual_name: &str,
    query: &TableQuery,
    frame: &dyn super::statement_frame::ReadFrame,
) -> RedDBResult<UnifiedResult> {
    if !is_virtual_table(virtual_name) {
        return Err(RedDBError::Query(format!(
            "unknown system schema relation `{virtual_name}`"
        )));
    }
    let virtual_kind = virtual_table_kind(virtual_name)?;

    let caller_is_admin = frame.identity().is_some_and(|(_, role)| role.can_admin())
        || (frame.identity().is_none() && frame.effective_scope().is_none());
    if !caller_is_admin && frame.effective_scope().is_none() {
        return Err(RedDBError::Query(format!(
            "{} requires an active tenant",
            virtual_kind.public_name()
        )));
    }

    let tenant = frame.effective_scope();
    let visible_collections = if caller_is_admin {
        None
    } else {
        frame.visible_collections()
    };
    let db = runtime.db();
    let mut records = match virtual_kind {
        VirtualTableKind::Collections => collections_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Columns => columns_snapshot(runtime, visible_collections),
        VirtualTableKind::Describe => describe_snapshot(runtime, visible_collections, query)?,
        VirtualTableKind::ShowCreate => show_create_snapshot(runtime, visible_collections, query)?,
        VirtualTableKind::ShowIndexes => show_indexes_snapshot(runtime, visible_collections),
        VirtualTableKind::Indices => indices_snapshot(runtime, visible_collections),
        VirtualTableKind::Policies => policies_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Stats => stats_snapshot(runtime, visible_collections),
        VirtualTableKind::Subscriptions => subscriptions_snapshot(runtime, visible_collections),
        VirtualTableKind::Retention => retention_snapshot(runtime, visible_collections),
        VirtualTableKind::MaterializedViews => materialized_views_snapshot(runtime),
        VirtualTableKind::QueuePending => queue_pending_snapshot(runtime, visible_collections),
        VirtualTableKind::Queues => queues_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::AnalyticsMetrics => analytics_metrics_snapshot(runtime),
        VirtualTableKind::AnalyticsSlos => analytics_slos_snapshot(runtime),
        VirtualTableKind::AnalyticsSources => {
            analytics_sources_snapshot(runtime, visible_collections)
        }
        VirtualTableKind::SchemaRegistry => schema_registry_snapshot(runtime),
        VirtualTableKind::GovernanceRegistry => governance_registry_snapshot(runtime),
        VirtualTableKind::GovernanceRegistryHistory => {
            governance_registry_history_snapshot(runtime)
        }
        VirtualTableKind::ManagedPolicies => managed_policies_snapshot(runtime),
        VirtualTableKind::ControlEvents => control_events_snapshot(runtime, tenant),
        VirtualTableKind::Users => users_snapshot(runtime, tenant),
        VirtualTableKind::ApiKeys => api_keys_snapshot(runtime, tenant),
        VirtualTableKind::ControlCapabilities => control_capabilities_snapshot(),
        VirtualTableKind::PolicyActions => policy_actions_snapshot(),
        VirtualTableKind::Tables => tables_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Documents => documents_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Kv => kv_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Vectors => vectors_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Graphs => graphs_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Timeseries => timeseries_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Metrics => metrics_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::HypertableChunks => {
            hypertable_chunks_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::TimeseriesWrites => {
            timeseries_writes_snapshot(runtime, tenant, visible_collections, query)
        }
        VirtualTableKind::Commits => commits_snapshot(runtime, query)?,
        VirtualTableKind::Branches => refs_snapshot(runtime, Some("refs/heads/"))?,
        VirtualTableKind::Tags => refs_snapshot(runtime, Some("refs/tags/"))?,
        VirtualTableKind::Status => status_snapshot(runtime)?,
        VirtualTableKind::Conflicts => conflicts_snapshot(runtime)?,
        VirtualTableKind::Versioned => versioned_snapshot(runtime, visible_collections)?,
    };

    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref();
    if !matches!(
        virtual_kind,
        VirtualTableKind::Describe | VirtualTableKind::ShowCreate
    ) {
        if let Some(filter) = effective_table_filter(query) {
            records.retain(|record| {
                super::join_filter::evaluate_runtime_filter_with_db(
                    Some(db.as_ref()),
                    record,
                    &filter,
                    Some(table_name),
                    table_alias,
                )
            });
        }
    }

    if !query.order_by.is_empty() {
        // Issue #769 — cap the materialized sort buffer.
        crate::runtime::materialization_limit::guard(db.as_ref(), "sort", records.len())?;
        super::join_filter::sort_records_by_order_by_with_db(
            Some(db.as_ref()),
            &mut records,
            &query.order_by,
            Some(table_name),
            table_alias,
        );
    }

    if let Some(offset) = query.offset {
        let offset = offset as usize;
        if offset >= records.len() {
            records.clear();
        } else {
            records.drain(..offset);
        }
    }
    if let Some(limit) = query.limit {
        records.truncate(limit as usize);
    }

    let projections = effective_table_projections(query);
    if !projections.is_empty()
        && !projections
            .iter()
            .any(|projection| matches!(projection, Projection::All))
    {
        records = records
            .iter()
            .map(|record| {
                super::join_filter::project_runtime_record_with_db(
                    Some(db.as_ref()),
                    record,
                    &projections,
                    Some(table_name),
                    table_alias,
                    false,
                    false,
                )
            })
            .collect();
    }

    let columns = if projections.is_empty()
        || projections
            .iter()
            .any(|projection| matches!(projection, Projection::All))
    {
        virtual_kind
            .columns()
            .iter()
            .map(|name| name.to_string())
            .collect()
    } else {
        super::join_filter::projected_columns(&records, &projections)
    };

    Ok(UnifiedResult {
        columns,
        stats: crate::storage::query::unified::QueryStats {
            rows_scanned: records.len() as u64,
            ..Default::default()
        },
        records,
        pre_serialized_json: None,
    })
}

#[derive(Debug, Clone, Copy)]
enum VirtualTableKind {
    Collections,
    Columns,
    Describe,
    ShowCreate,
    ShowIndexes,
    Indices,
    Policies,
    Stats,
    Subscriptions,
    Retention,
    MaterializedViews,
    QueuePending,
    Queues,
    AnalyticsMetrics,
    AnalyticsSlos,
    AnalyticsSources,
    SchemaRegistry,
    GovernanceRegistry,
    GovernanceRegistryHistory,
    ManagedPolicies,
    ControlEvents,
    Users,
    ApiKeys,
    ControlCapabilities,
    PolicyActions,
    Tables,
    Documents,
    Kv,
    Vectors,
    Graphs,
    Timeseries,
    Metrics,
    HypertableChunks,
    TimeseriesWrites,
    Commits,
    Branches,
    Tags,
    Status,
    Conflicts,
    Versioned,
}

impl VirtualTableKind {
    fn columns(self) -> &'static [&'static str] {
        match self {
            Self::Collections => &COLLECTION_COLUMNS,
            Self::Columns => &COLUMN_COLUMNS,
            Self::Describe => &DESCRIBE_COLUMNS,
            Self::ShowCreate => &SHOW_CREATE_COLUMNS,
            Self::ShowIndexes => &SHOW_INDEX_COLUMNS,
            Self::Indices => &INDEX_COLUMNS,
            Self::Policies => &POLICY_COLUMNS,
            Self::Stats => &STATS_COLUMNS,
            Self::Subscriptions => &SUBSCRIPTION_COLUMNS,
            Self::Retention => &RETENTION_COLUMNS,
            Self::MaterializedViews => &MATERIALIZED_VIEW_COLUMNS,
            Self::QueuePending => &QUEUE_PENDING_COLUMNS,
            Self::Queues => &QUEUE_COLUMNS,
            Self::AnalyticsMetrics => &ANALYTICS_METRIC_COLUMNS,
            Self::AnalyticsSlos => &ANALYTICS_SLO_COLUMNS,
            Self::AnalyticsSources => &ANALYTICS_SOURCE_COLUMNS,
            Self::SchemaRegistry => &SCHEMA_REGISTRY_COLUMNS,
            Self::GovernanceRegistry => &GOVERNANCE_REGISTRY_COLUMNS,
            Self::GovernanceRegistryHistory => &GOVERNANCE_REGISTRY_HISTORY_COLUMNS,
            Self::ManagedPolicies => &MANAGED_POLICY_COLUMNS,
            Self::ControlEvents => &CONTROL_EVENT_COLUMNS,
            Self::Users => &USER_COLUMNS,
            Self::ApiKeys => &API_KEY_COLUMNS,
            Self::ControlCapabilities => &CONTROL_CAPABILITY_COLUMNS,
            Self::PolicyActions => &POLICY_ACTION_COLUMNS,
            Self::Tables => &TABLE_COLUMNS,
            Self::Documents => &DOCUMENT_COLUMNS,
            Self::Kv => &KV_COLUMNS,
            Self::Vectors => &VECTOR_COLUMNS,
            Self::Graphs => &GRAPH_COLUMNS,
            Self::Timeseries => &TIMESERIES_COLUMNS,
            Self::Metrics => &METRICS_COLUMNS,
            Self::HypertableChunks => &HYPERTABLE_CHUNK_COLUMNS,
            Self::TimeseriesWrites => &TIMESERIES_WRITES_COLUMNS,
            Self::Commits => &COMMIT_COLUMNS,
            Self::Branches | Self::Tags => &REF_COLUMNS,
            Self::Status => &STATUS_COLUMNS,
            Self::Conflicts => &CONFLICT_COLUMNS,
            Self::Versioned => &VERSIONED_COLUMNS,
        }
    }

    fn public_name(self) -> &'static str {
        match self {
            Self::Collections => COLLECTIONS,
            Self::Columns => COLUMNS,
            Self::Describe => DESCRIBE,
            Self::ShowCreate => SHOW_CREATE,
            Self::ShowIndexes => SHOW_INDEXES,
            Self::Indices => INDICES,
            Self::Policies => POLICIES,
            Self::Stats => STATS,
            Self::Subscriptions => SUBSCRIPTIONS,
            Self::Retention => RETENTION,
            Self::MaterializedViews => MATERIALIZED_VIEWS,
            Self::QueuePending => QUEUE_PENDING,
            Self::Queues => QUEUES,
            Self::AnalyticsMetrics => ANALYTICS_METRICS,
            Self::AnalyticsSlos => ANALYTICS_SLOS,
            Self::AnalyticsSources => ANALYTICS_SOURCES,
            Self::SchemaRegistry => SCHEMA_REGISTRY,
            Self::GovernanceRegistry => GOVERNANCE_REGISTRY,
            Self::GovernanceRegistryHistory => GOVERNANCE_REGISTRY_HISTORY,
            Self::ManagedPolicies => MANAGED_POLICIES,
            Self::ControlEvents => CONTROL_EVENTS,
            Self::Users => USERS,
            Self::ApiKeys => API_KEYS,
            Self::ControlCapabilities => CONTROL_CAPABILITIES,
            Self::PolicyActions => POLICY_ACTIONS,
            Self::Tables => TABLES,
            Self::Documents => DOCUMENTS,
            Self::Kv => KV,
            Self::Vectors => VECTORS,
            Self::Graphs => GRAPHS,
            Self::Timeseries => TIMESERIES,
            Self::Metrics => METRICS,
            Self::HypertableChunks => HYPERTABLE_CHUNKS,
            Self::TimeseriesWrites => TIMESERIES_WRITES,
            Self::Commits => COMMITS,
            Self::Branches => BRANCHES,
            Self::Tags => TAGS,
            Self::Status => STATUS,
            Self::Conflicts => CONFLICTS,
            Self::Versioned => VERSIONED,
        }
    }
}

fn virtual_table_kind(name: &str) -> RedDBResult<VirtualTableKind> {
    if name.eq_ignore_ascii_case(COLLECTIONS_INTERNAL) || name.eq_ignore_ascii_case(COLLECTIONS) {
        return Ok(VirtualTableKind::Collections);
    }
    if name.eq_ignore_ascii_case(COLUMNS_INTERNAL) || name.eq_ignore_ascii_case(COLUMNS) {
        return Ok(VirtualTableKind::Columns);
    }
    if name.eq_ignore_ascii_case(DESCRIBE_INTERNAL) || name.eq_ignore_ascii_case(DESCRIBE) {
        return Ok(VirtualTableKind::Describe);
    }
    if name.eq_ignore_ascii_case(SHOW_CREATE_INTERNAL) || name.eq_ignore_ascii_case(SHOW_CREATE) {
        return Ok(VirtualTableKind::ShowCreate);
    }
    if name.eq_ignore_ascii_case(SHOW_INDEXES_INTERNAL) || name.eq_ignore_ascii_case(SHOW_INDEXES) {
        return Ok(VirtualTableKind::ShowIndexes);
    }
    if name.eq_ignore_ascii_case(INDICES_INTERNAL) || name.eq_ignore_ascii_case(INDICES) {
        return Ok(VirtualTableKind::Indices);
    }
    if name.eq_ignore_ascii_case(POLICIES_INTERNAL) || name.eq_ignore_ascii_case(POLICIES) {
        return Ok(VirtualTableKind::Policies);
    }
    if name.eq_ignore_ascii_case(STATS_INTERNAL) || name.eq_ignore_ascii_case(STATS) {
        return Ok(VirtualTableKind::Stats);
    }
    if name.eq_ignore_ascii_case(SUBSCRIPTIONS_INTERNAL) || name.eq_ignore_ascii_case(SUBSCRIPTIONS)
    {
        return Ok(VirtualTableKind::Subscriptions);
    }
    if name.eq_ignore_ascii_case(RETENTION_INTERNAL) || name.eq_ignore_ascii_case(RETENTION) {
        return Ok(VirtualTableKind::Retention);
    }
    if name.eq_ignore_ascii_case(MATERIALIZED_VIEWS_INTERNAL)
        || name.eq_ignore_ascii_case(MATERIALIZED_VIEWS)
    {
        return Ok(VirtualTableKind::MaterializedViews);
    }
    if name.eq_ignore_ascii_case(QUEUE_PENDING_INTERNAL) || name.eq_ignore_ascii_case(QUEUE_PENDING)
    {
        return Ok(VirtualTableKind::QueuePending);
    }
    if name.eq_ignore_ascii_case(QUEUES_INTERNAL) || name.eq_ignore_ascii_case(QUEUES) {
        return Ok(VirtualTableKind::Queues);
    }
    if name.eq_ignore_ascii_case(ANALYTICS_METRICS_INTERNAL)
        || name.eq_ignore_ascii_case(ANALYTICS_METRICS)
    {
        return Ok(VirtualTableKind::AnalyticsMetrics);
    }
    if name.eq_ignore_ascii_case(ANALYTICS_SLOS_INTERNAL)
        || name.eq_ignore_ascii_case(ANALYTICS_SLOS)
    {
        return Ok(VirtualTableKind::AnalyticsSlos);
    }
    if name.eq_ignore_ascii_case(ANALYTICS_SOURCES_INTERNAL)
        || name.eq_ignore_ascii_case(ANALYTICS_SOURCES)
    {
        return Ok(VirtualTableKind::AnalyticsSources);
    }
    if name.eq_ignore_ascii_case(SCHEMA_REGISTRY_INTERNAL)
        || name.eq_ignore_ascii_case(SCHEMA_REGISTRY)
    {
        return Ok(VirtualTableKind::SchemaRegistry);
    }
    if name.eq_ignore_ascii_case(GOVERNANCE_REGISTRY_INTERNAL)
        || name.eq_ignore_ascii_case(GOVERNANCE_REGISTRY)
    {
        return Ok(VirtualTableKind::GovernanceRegistry);
    }
    if name.eq_ignore_ascii_case(GOVERNANCE_REGISTRY_HISTORY_INTERNAL)
        || name.eq_ignore_ascii_case(GOVERNANCE_REGISTRY_HISTORY)
    {
        return Ok(VirtualTableKind::GovernanceRegistryHistory);
    }
    if name.eq_ignore_ascii_case(MANAGED_POLICIES_INTERNAL)
        || name.eq_ignore_ascii_case(MANAGED_POLICIES)
    {
        return Ok(VirtualTableKind::ManagedPolicies);
    }
    if name.eq_ignore_ascii_case(CONTROL_EVENTS_INTERNAL)
        || name.eq_ignore_ascii_case(CONTROL_EVENTS)
    {
        return Ok(VirtualTableKind::ControlEvents);
    }
    if name.eq_ignore_ascii_case(USERS_INTERNAL) || name.eq_ignore_ascii_case(USERS) {
        return Ok(VirtualTableKind::Users);
    }
    if name.eq_ignore_ascii_case(API_KEYS_INTERNAL) || name.eq_ignore_ascii_case(API_KEYS) {
        return Ok(VirtualTableKind::ApiKeys);
    }
    if name.eq_ignore_ascii_case(CONTROL_CAPABILITIES_INTERNAL)
        || name.eq_ignore_ascii_case(CONTROL_CAPABILITIES)
    {
        return Ok(VirtualTableKind::ControlCapabilities);
    }
    if name.eq_ignore_ascii_case(POLICY_ACTIONS_INTERNAL)
        || name.eq_ignore_ascii_case(POLICY_ACTIONS)
    {
        return Ok(VirtualTableKind::PolicyActions);
    }
    if name.eq_ignore_ascii_case(TABLES_INTERNAL) || name.eq_ignore_ascii_case(TABLES) {
        return Ok(VirtualTableKind::Tables);
    }
    if name.eq_ignore_ascii_case(DOCUMENTS_INTERNAL) || name.eq_ignore_ascii_case(DOCUMENTS) {
        return Ok(VirtualTableKind::Documents);
    }
    if name.eq_ignore_ascii_case(KV_INTERNAL) || name.eq_ignore_ascii_case(KV) {
        return Ok(VirtualTableKind::Kv);
    }
    if name.eq_ignore_ascii_case(VECTORS_INTERNAL) || name.eq_ignore_ascii_case(VECTORS) {
        return Ok(VirtualTableKind::Vectors);
    }
    if name.eq_ignore_ascii_case(GRAPHS_INTERNAL) || name.eq_ignore_ascii_case(GRAPHS) {
        return Ok(VirtualTableKind::Graphs);
    }
    if name.eq_ignore_ascii_case(TIMESERIES_INTERNAL) || name.eq_ignore_ascii_case(TIMESERIES) {
        return Ok(VirtualTableKind::Timeseries);
    }
    if name.eq_ignore_ascii_case(METRICS_INTERNAL) || name.eq_ignore_ascii_case(METRICS) {
        return Ok(VirtualTableKind::Metrics);
    }
    if name.eq_ignore_ascii_case(HYPERTABLE_CHUNKS_INTERNAL)
        || name.eq_ignore_ascii_case(HYPERTABLE_CHUNKS)
    {
        return Ok(VirtualTableKind::HypertableChunks);
    }
    if name.eq_ignore_ascii_case(TIMESERIES_WRITES_INTERNAL)
        || name.eq_ignore_ascii_case(TIMESERIES_WRITES)
    {
        return Ok(VirtualTableKind::TimeseriesWrites);
    }
    if name.eq_ignore_ascii_case(COMMITS_INTERNAL) || name.eq_ignore_ascii_case(COMMITS) {
        return Ok(VirtualTableKind::Commits);
    }
    if name.eq_ignore_ascii_case(BRANCHES_INTERNAL) || name.eq_ignore_ascii_case(BRANCHES) {
        return Ok(VirtualTableKind::Branches);
    }
    if name.eq_ignore_ascii_case(TAGS_INTERNAL) || name.eq_ignore_ascii_case(TAGS) {
        return Ok(VirtualTableKind::Tags);
    }
    if name.eq_ignore_ascii_case(STATUS_INTERNAL) || name.eq_ignore_ascii_case(STATUS) {
        return Ok(VirtualTableKind::Status);
    }
    if name.eq_ignore_ascii_case(CONFLICTS_INTERNAL) || name.eq_ignore_ascii_case(CONFLICTS) {
        return Ok(VirtualTableKind::Conflicts);
    }
    if name.eq_ignore_ascii_case(VERSIONED_INTERNAL) || name.eq_ignore_ascii_case(VERSIONED) {
        return Ok(VirtualTableKind::Versioned);
    }
    Err(RedDBError::Query(format!(
        "unknown system schema relation `{name}`"
    )))
}

fn commits_snapshot(runtime: &RedDBRuntime, query: &TableQuery) -> RedDBResult<Vec<UnifiedRecord>> {
    let schema = Arc::new(
        COMMIT_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let hash = hash_filter(query);
    let range = if let Some(hash) = hash.clone() {
        crate::application::vcs::LogRange {
            to: Some(hash),
            limit: Some(1),
            ..Default::default()
        }
    } else {
        crate::application::vcs::LogRange::default()
    };
    Ok(runtime
        .vcs_log(crate::application::vcs::LogInput {
            connection_id: if hash.is_some() {
                0
            } else {
                current_connection_id()
            },
            range,
        })?
        .into_iter()
        .map(|commit| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(commit.hash),
                    Value::UnsignedInteger(commit.root_xid),
                    Value::Array(commit.parents.into_iter().map(Value::text).collect()),
                    Value::UnsignedInteger(commit.height),
                    Value::text(commit.author.name),
                    Value::text(commit.author.email),
                    Value::text(commit.committer.name),
                    Value::text(commit.committer.email),
                    Value::text(commit.message),
                    Value::TimestampMs(commit.timestamp_ms),
                    commit.signature.map(Value::text).unwrap_or(Value::Null),
                ],
            )
        })
        .collect())
}

fn hash_filter(query: &TableQuery) -> Option<String> {
    fn visit(filter: &Filter) -> Option<String> {
        match filter {
            Filter::Compare {
                field: FieldRef::TableColumn { column, .. },
                op: CompareOp::Eq,
                value: Value::Text(hash),
            } if column == "hash" => Some(hash.to_string()),
            Filter::And(left, right) => visit(left).or_else(|| visit(right)),
            _ => None,
        }
    }
    effective_table_filter(query).and_then(|filter| visit(&filter))
}

fn refs_snapshot(runtime: &RedDBRuntime, prefix: Option<&str>) -> RedDBResult<Vec<UnifiedRecord>> {
    let schema = Arc::new(
        REF_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    Ok(runtime
        .vcs_list_refs(prefix)?
        .into_iter()
        .map(|reference| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(reference.name),
                    Value::text(ref_kind_name(reference.kind)),
                    Value::text(reference.target),
                    Value::Boolean(reference.protected),
                ],
            )
        })
        .collect())
}

fn status_snapshot(runtime: &RedDBRuntime) -> RedDBResult<Vec<UnifiedRecord>> {
    let schema = Arc::new(
        STATUS_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let status = runtime.vcs_status(crate::application::vcs::StatusInput {
        connection_id: current_connection_id(),
    })?;
    Ok(vec![UnifiedRecord::with_schema(
        schema,
        vec![
            Value::UnsignedInteger(status.connection_id),
            status.head_ref.map(Value::text).unwrap_or(Value::Null),
            status.head_commit.map(Value::text).unwrap_or(Value::Null),
            Value::Boolean(status.detached),
            Value::UnsignedInteger(status.staged_changes as u64),
            Value::UnsignedInteger(status.working_changes as u64),
            Value::UnsignedInteger(status.unresolved_conflicts as u64),
            status
                .merge_state_id
                .map(Value::text)
                .unwrap_or(Value::Null),
        ],
    )])
}

fn conflicts_snapshot(runtime: &RedDBRuntime) -> RedDBResult<Vec<UnifiedRecord>> {
    let schema = Arc::new(
        CONFLICT_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let status = runtime.vcs_status(crate::application::vcs::StatusInput {
        connection_id: current_connection_id(),
    })?;
    let Some(merge_state_id) = status.merge_state_id else {
        return Ok(Vec::new());
    };
    Ok(runtime
        .vcs_conflicts_list(&merge_state_id)?
        .into_iter()
        .map(|conflict| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(conflict.id),
                    Value::text(conflict.collection),
                    Value::text(conflict.entity_id),
                    json_value(conflict.base),
                    json_value(conflict.ours),
                    json_value(conflict.theirs),
                    Value::Array(
                        conflict
                            .conflicting_paths
                            .into_iter()
                            .map(Value::text)
                            .collect(),
                    ),
                    Value::text(conflict.merge_state_id),
                ],
            )
        })
        .collect())
}

fn versioned_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&HashSet<String>>,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let schema = Arc::new(
        VERSIONED_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    Ok(runtime
        .vcs_list_versioned()?
        .into_iter()
        .filter(|collection| collection_is_visible(collection, visible_collections))
        .map(|collection| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![Value::text(collection), Value::Boolean(true)],
            )
        })
        .collect())
}

fn ref_kind_name(kind: crate::application::vcs::RefKind) -> &'static str {
    match kind {
        crate::application::vcs::RefKind::Branch => "branch",
        crate::application::vcs::RefKind::Tag => "tag",
        crate::application::vcs::RefKind::Head => "head",
    }
}

fn json_value(value: crate::json::Value) -> Value {
    crate::json::to_vec(&value)
        .map(Value::Json)
        .unwrap_or(Value::Null)
}

fn governance_registry_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        GOVERNANCE_REGISTRY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    runtime
        .config_registry()
        .list_active()
        .into_iter()
        .map(|entry| governance_registry_record(Arc::clone(&schema), entry))
        .collect()
}

fn governance_registry_history_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        GOVERNANCE_REGISTRY_HISTORY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let registry = runtime.config_registry();
    let mut rows = Vec::new();
    for active in registry.list_active() {
        for record in registry.history(&active.id) {
            rows.push(governance_registry_history_record(
                Arc::clone(&schema),
                record,
            ));
        }
    }
    rows
}

fn managed_policies_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        MANAGED_POLICY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    runtime
        .config_registry()
        .list_active()
        .into_iter()
        .filter(|entry| entry.managed && entry.resource_type == "policy")
        .map(|entry| {
            let policy_id = entry
                .required_resource
                .strip_prefix("policy:")
                .unwrap_or(&entry.id)
                .to_string();
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(policy_id),
                    Value::text(entry.id),
                    Value::UnsignedInteger(entry.version),
                    Value::text(entry.schema),
                    Value::text(entry.required_action),
                    Value::text(entry.required_resource),
                    Value::text(registry_evidence_requirement(entry.evidence_requirement)),
                    Value::text(entry.updated_by),
                    timestamp_ms_value(entry.updated_at_ms),
                ],
            )
        })
        .collect()
}

fn control_events_snapshot(runtime: &RedDBRuntime, tenant: Option<&str>) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        CONTROL_EVENT_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let Some(manager) = runtime
        .db()
        .store()
        .get_collection(super::control_events::CONTROL_EVENTS_COLLECTION)
    else {
        return Vec::new();
    };
    manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            if let Some(tenant) = tenant {
                match row.get_field("scope") {
                    Some(Value::Text(scope)) if scope.as_ref() == tenant => {}
                    _ => return None,
                }
            }
            Some(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                CONTROL_EVENT_COLUMNS
                    .iter()
                    .map(|column| row.get_field(column).cloned().unwrap_or(Value::Null))
                    .collect(),
            ))
        })
        .collect()
}

fn users_snapshot(runtime: &RedDBRuntime, tenant: Option<&str>) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        USER_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let auth_store = runtime.inner.auth_store.read().clone();
    let Some(auth_store) = auth_store else {
        return Vec::new();
    };
    let tenant_filter = tenant.map(Some);
    auth_store
        .list_users_scoped(tenant_filter)
        .into_iter()
        .map(|user| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(user.username),
                    user.tenant_id.map(Value::text).unwrap_or(Value::Null),
                    Value::text(user.role.as_str()),
                    Value::Boolean(user.enabled),
                    timestamp_ms_value(user.created_at),
                    timestamp_ms_value(user.updated_at),
                    Value::UnsignedInteger(user.api_keys.len() as u64),
                ],
            )
        })
        .collect()
}

fn api_keys_snapshot(runtime: &RedDBRuntime, tenant: Option<&str>) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        API_KEY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let auth_store = runtime.inner.auth_store.read().clone();
    let Some(auth_store) = auth_store else {
        return Vec::new();
    };
    let tenant_filter = tenant.map(Some);
    let mut rows = Vec::new();
    for user in auth_store.list_users_scoped(tenant_filter) {
        let owner = match user.tenant_id.as_deref() {
            Some(tenant) => format!("{tenant}/{}", user.username),
            None => user.username.clone(),
        };
        for key in user.api_keys {
            rows.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(owner.clone()),
                    user.tenant_id
                        .clone()
                        .map(Value::text)
                        .unwrap_or(Value::Null),
                    Value::text(key.name),
                    Value::text(key.role.as_str()),
                    timestamp_ms_value(key.created_at),
                    Value::text(api_key_fingerprint(&key.key)),
                ],
            ));
        }
    }
    rows
}

fn control_capabilities_snapshot() -> Vec<UnifiedRecord> {
    use crate::auth::action_catalog::{ActionCategory, ACTIONS};

    let schema = Arc::new(
        CONTROL_CAPABILITY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    // The historical snapshot was a hand-curated subset of the
    // allowlist — pure DML/DDL verbs (`select`, `insert`, …) and the
    // bare `*` wildcard were not advertised. Reproduce that subset by
    // filtering the catalog: only emit policy/admin/config/vault/other
    // entries plus their namespaced wildcards, never the catch-all `*`.
    ACTIONS
        .iter()
        .filter(|entry| {
            if entry.name == "*" {
                return false;
            }
            matches!(
                entry.category,
                ActionCategory::Policy
                    | ActionCategory::Admin
                    | ActionCategory::Config
                    | ActionCategory::Vault
                    | ActionCategory::Other
                    | ActionCategory::Wildcard
            )
        })
        .map(|entry| {
            let action = entry.name;
            let resource_kind = control_capability_resource_kind(action);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(action),
                    Value::text(resource_kind),
                    Value::text(control_capability_scope(action)),
                    Value::text(format!("{action} on {resource_kind} resources")),
                ],
            )
        })
        .collect()
}

fn policy_actions_snapshot() -> Vec<UnifiedRecord> {
    use crate::auth::action_catalog::{LifecycleState, ACTIONS};

    let schema = Arc::new(
        POLICY_ACTION_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    ACTIONS
        .iter()
        .map(|entry| {
            let (state_name, replacement, since_version) = match &entry.lifecycle_state {
                LifecycleState::Active => ("active", Value::Null, Value::Null),
                LifecycleState::Deprecated {
                    replacement,
                    since_version,
                } => (
                    "deprecated",
                    replacement.map(Value::text).unwrap_or(Value::Null),
                    Value::text(*since_version),
                ),
                LifecycleState::Removed => ("removed", Value::Null, Value::Null),
            };
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.name),
                    Value::text(entry.category.as_str()),
                    Value::text(state_name),
                    replacement,
                    since_version,
                    Value::text(entry.gates_description),
                ],
            )
        })
        .collect()
}

fn governance_registry_record(
    schema: Arc<Vec<Arc<str>>>,
    entry: crate::auth::registry::ConfigRegistryEntry,
) -> UnifiedRecord {
    UnifiedRecord::with_schema(
        schema,
        vec![
            Value::text(entry.id),
            Value::UnsignedInteger(entry.version),
            Value::text(entry.resource_type),
            Value::text(entry.schema),
            Value::text(registry_mutability(entry.mutability)),
            Value::text(registry_sensitivity(entry.sensitivity)),
            Value::Boolean(entry.managed),
            Value::text(entry.required_action),
            Value::text(entry.required_resource),
            Value::text(registry_evidence_requirement(entry.evidence_requirement)),
            Value::text(entry.updated_by),
            timestamp_ms_value(entry.updated_at_ms),
        ],
    )
}

fn governance_registry_history_record(
    schema: Arc<Vec<Arc<str>>>,
    record: crate::auth::registry::ConfigRegistryHistoryRecord,
) -> UnifiedRecord {
    let entry = record.entry;
    UnifiedRecord::with_schema(
        schema,
        vec![
            Value::text(entry.id),
            Value::UnsignedInteger(entry.version),
            Value::text(entry.resource_type),
            Value::text(entry.schema),
            Value::text(registry_mutability(entry.mutability)),
            Value::text(registry_sensitivity(entry.sensitivity)),
            Value::Boolean(entry.managed),
            Value::text(entry.required_action),
            Value::text(entry.required_resource),
            Value::text(registry_evidence_requirement(entry.evidence_requirement)),
            Value::text(entry.updated_by),
            timestamp_ms_value(entry.updated_at_ms),
            Value::text(record.superseded_by),
            timestamp_ms_value(record.superseded_at_ms),
            Value::text(record.change_reason),
        ],
    )
}

fn registry_mutability(value: crate::auth::registry::Mutability) -> &'static str {
    match value {
        crate::auth::registry::Mutability::Immutable => "immutable",
        crate::auth::registry::Mutability::MutableViaGovernance => "mutable_via_governance",
    }
}

fn registry_sensitivity(value: crate::auth::registry::Sensitivity) -> &'static str {
    match value {
        crate::auth::registry::Sensitivity::Public => "public",
        crate::auth::registry::Sensitivity::Internal => "internal",
        crate::auth::registry::Sensitivity::Confidential => "confidential",
        crate::auth::registry::Sensitivity::Secret => "secret",
    }
}

fn registry_evidence_requirement(
    value: crate::auth::registry::EvidenceRequirement,
) -> &'static str {
    match value {
        crate::auth::registry::EvidenceRequirement::None => "none",
        crate::auth::registry::EvidenceRequirement::Metadata => "metadata",
        crate::auth::registry::EvidenceRequirement::Full => "full",
    }
}

fn api_key_fingerprint(key: &str) -> String {
    format!("blake3:{}", blake3::hash(key.as_bytes()).to_hex())
}

fn control_capability_resource_kind(action: &str) -> &str {
    if action.starts_with("red.registry:") {
        "registry"
    } else if let Some((prefix, _)) = action.split_once(':') {
        prefix
    } else {
        "system"
    }
}

fn control_capability_scope(action: &str) -> &'static str {
    if action.starts_with("admin:") || action.starts_with("red.registry:") {
        "platform"
    } else {
        "tenant"
    }
}

fn timestamp_ms_value(value: u128) -> Value {
    i64::try_from(value)
        .map(Value::TimestampMs)
        .unwrap_or(Value::Null)
}

fn schema_registry_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let store = runtime.db().store();
    let schema = Arc::new(
        SCHEMA_REGISTRY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    super::analytics_schema_registry::list(store.as_ref())
        .into_iter()
        .map(|entry| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.event_name),
                    Value::UnsignedInteger(entry.version as u64),
                    Value::text(entry.schema_json),
                    Value::TimestampMs(entry.registered_at_ms as i64),
                ],
            )
        })
        .collect()
}

fn analytics_metrics_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let store = runtime.db().store();
    let schema = Arc::new(
        ANALYTICS_METRIC_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    super::metric_descriptor_catalog::list(store.as_ref())
        .into_iter()
        .map(|entry| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.path),
                    Value::text(entry.kind),
                    Value::text(entry.role),
                    timestamp_ms_value(entry.created_at_ms),
                    entry.source.map(Value::text).unwrap_or(Value::Null),
                    entry.query.map(Value::text).unwrap_or(Value::Null),
                    entry
                        .window_ms
                        .map(|ms| Value::Integer(ms as i64))
                        .unwrap_or(Value::Null),
                    entry.time_field.map(Value::text).unwrap_or(Value::Null),
                ],
            )
        })
        .collect()
}

fn analytics_slos_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let store = runtime.db().store();
    let schema = Arc::new(
        ANALYTICS_SLO_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    super::slo_descriptor_catalog::list(store.as_ref())
        .into_iter()
        .map(|entry| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.path),
                    Value::text(entry.metric_path),
                    Value::Float(entry.target),
                    Value::Integer(entry.window_ms as i64),
                    timestamp_ms_value(entry.created_at_ms),
                ],
            )
        })
        .collect()
}

fn analytics_sources_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let store = runtime.db().store();
    let schema = Arc::new(
        ANALYTICS_SOURCE_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    super::analytics_source_catalog::list(store.as_ref())
        .into_iter()
        .filter(|entry| {
            visible_collections
                .map(|visible| visible.contains(&entry.collection))
                .unwrap_or(true)
        })
        .map(|entry| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.name),
                    Value::text(entry.collection),
                    Value::text(entry.time_field),
                    Value::text(entry.event_field),
                    Value::text(entry.actor_field),
                    entry.session_field.map(Value::text).unwrap_or(Value::Null),
                    entry
                        .properties_field
                        .map(Value::text)
                        .unwrap_or(Value::Null),
                    timestamp_ms_value(entry.created_at_ms),
                ],
            )
        })
        .collect()
}

fn subscriptions_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        SUBSCRIPTION_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let contracts = runtime.db().collection_contracts();
    let created_at_by_collection: HashMap<&str, u128> = contracts
        .iter()
        .map(|contract| (contract.name.as_str(), contract.created_at_unix_ms))
        .collect();
    let mut records = Vec::new();

    for collection in snapshot.collections {
        if !collection_is_visible(&collection.name, visible_collections) {
            continue;
        }

        let created_at = created_at_by_collection
            .get(collection.name.as_str())
            .copied()
            .unwrap_or(0);
        for subscription in collection.subscriptions {
            let mode = subscription_queue_mode(store.as_ref(), &subscription.target_queue)
                .to_ascii_uppercase();
            let name = if subscription.name.is_empty() {
                format!("{}_to_{}", subscription.source, subscription.target_queue)
            } else {
                subscription.name.clone()
            };
            records.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(name),
                    Value::text(subscription.source),
                    Value::text(subscription.target_queue.clone()),
                    Value::text(mode),
                    Value::Array(
                        subscription
                            .ops_filter
                            .iter()
                            .map(|op| Value::text(op.as_str()))
                            .collect(),
                    ),
                    subscription
                        .where_filter
                        .map(Value::text)
                        .unwrap_or(Value::Null),
                    Value::Array(
                        subscription
                            .redact_fields
                            .into_iter()
                            .map(Value::text)
                            .collect(),
                    ),
                    Value::Boolean(subscription.enabled),
                    Value::UnsignedInteger(0),
                    Value::UnsignedInteger(outbox_dlq_count(
                        store.as_ref(),
                        &subscription.target_queue,
                    )),
                    Value::TimestampMs(created_at as i64),
                ],
            ));
        }
    }

    records
}

fn outbox_dlq_count(store: &UnifiedStore, target_queue: &str) -> u64 {
    let dlq = format!("{target_queue}_outbox_dlq");
    let Some(manager) = store.get_collection(&dlq) else {
        return 0;
    };
    manager
        .query_all(|entity| matches!(&entity.kind, crate::storage::EntityKind::QueueMessage { queue, .. } if queue == &dlq))
        .len() as u64
}

fn subscription_queue_mode(store: &UnifiedStore, queue: &str) -> String {
    match store.get_config(&format!("queue.{queue}.mode")) {
        Some(Value::Text(value)) => value.to_string(),
        _ => super::impl_queue::queue_mode_str(store, queue).to_string(),
    }
}

fn indices_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        INDEX_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let mut rows = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for status in snapshot.index_statuses {
        if !index_collection_visible(status.collection.as_deref(), visible_collections) {
            continue;
        }
        seen.insert((status.collection.clone(), status.name.clone()));
        rows.push(index_status_record(Arc::clone(&schema), status));
    }

    for collection in snapshot.collections {
        if !visible_collections.is_none_or(|visible| visible.contains(&collection.name)) {
            continue;
        }
        for index in runtime.index_store_ref().list_indices(&collection.name) {
            let key = (Some(index.collection.clone()), index.name.clone());
            if !seen.insert(key) {
                continue;
            }
            rows.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(index.collection),
                    Value::text(index.name),
                    Value::text(index_method_kind_name(index.method)),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::text("ready"),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::Boolean(false),
                ],
            ));
        }
    }

    rows
}

fn show_indexes_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        SHOW_INDEX_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let mut rows = Vec::new();

    for collection in snapshot.collections {
        if !collection_is_visible(&collection.name, visible_collections) {
            continue;
        }
        for index in runtime.index_store_ref().list_indices(&collection.name) {
            let entries_indexed = runtime.index_store_ref().entries_indexed(&index);
            rows.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(index.name),
                    Value::text(index.collection),
                    Value::Array(index.columns.into_iter().map(Value::text).collect()),
                    Value::text(render_index_method_for_ddl(index.method)),
                    Value::Boolean(index.unique),
                    Value::UnsignedInteger(entries_indexed),
                ],
            ));
        }
    }

    rows
}

fn index_status_record(
    schema: Arc<Vec<Arc<str>>>,
    status: crate::catalog::CatalogIndexStatus,
) -> UnifiedRecord {
    UnifiedRecord::with_schema(
        schema,
        vec![
            status.collection.map(Value::text).unwrap_or(Value::Null),
            Value::text(status.name),
            Value::text(status.kind),
            Value::Boolean(status.declared),
            Value::Boolean(status.operational),
            Value::Boolean(status.enabled),
            status.build_state.map(Value::text).unwrap_or(Value::Null),
            Value::Boolean(status.in_sync),
            Value::Boolean(status.queryable),
            Value::Boolean(status.requires_rebuild),
        ],
    )
}

fn index_collection_visible(
    collection: Option<&str>,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> bool {
    visible_collections
        .is_none_or(|visible| collection.is_some_and(|collection| visible.contains(collection)))
}

fn index_method_kind_name(kind: super::index_store::IndexMethodKind) -> &'static str {
    match kind {
        super::index_store::IndexMethodKind::Hash => "hash",
        super::index_store::IndexMethodKind::BTree => "btree",
        super::index_store::IndexMethodKind::Bitmap => "bitmap",
        super::index_store::IndexMethodKind::Spatial => "spatial.rtree",
    }
}

fn describe_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&HashSet<String>>,
    query: &TableQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let collection = describe_target_collection(query)?;
    let db = runtime.db();
    let exists = db
        .catalog_model_snapshot()
        .collections
        .into_iter()
        .any(|entry| entry.name == collection);
    if !exists || !collection_is_visible(&collection, visible_collections) {
        return Err(RedDBError::Query(format!(
            "COLLECTION_NOT_FOUND: {collection}"
        )));
    }

    let contracts = db.collection_contracts();
    let Some(contract) = contracts
        .iter()
        .find(|contract| contract.name == collection)
    else {
        return Err(RedDBError::Query(format!(
            "NOT_APPLICABLE: DESCRIBE {collection} has no declared column schema"
        )));
    };
    if contract.declared_columns.is_empty() {
        return Err(RedDBError::Query(format!(
            "NOT_APPLICABLE: DESCRIBE {collection} has no declared column schema"
        )));
    }

    let schema = Arc::new(
        DESCRIBE_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let indexed_columns = runtime.index_store_ref().indexed_columns_set(&collection);
    Ok(contract
        .declared_columns
        .iter()
        .map(|column| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(column.name.clone()),
                    Value::text(
                        column
                            .sql_type
                            .as_ref()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| column.data_type.clone()),
                    ),
                    Value::Boolean(!(column.not_null || column.primary_key)),
                    column
                        .default
                        .as_deref()
                        .map(Value::text)
                        .unwrap_or(Value::Null),
                    Value::Boolean(indexed_columns.contains(&column.name)),
                ],
            )
        })
        .collect())
}

fn describe_target_collection(query: &TableQuery) -> RedDBResult<String> {
    match query.filter.as_ref() {
        Some(Filter::Compare {
            field: FieldRef::TableColumn { column, .. },
            op: CompareOp::Eq,
            value: Value::Text(collection),
        }) if column == "collection" => Ok(collection.to_string()),
        _ => Err(RedDBError::Query(
            "DESCRIBE requires a collection name".to_string(),
        )),
    }
}

fn show_create_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&HashSet<String>>,
    query: &TableQuery,
) -> RedDBResult<Vec<UnifiedRecord>> {
    let collection = show_create_target_collection(query)?;
    let db = runtime.db();
    let catalog_entry = db
        .catalog_model_snapshot()
        .collections
        .into_iter()
        .find(|entry| entry.name == collection);
    let Some(catalog_entry) = catalog_entry else {
        return Err(RedDBError::Query(format!(
            "COLLECTION_NOT_FOUND: {collection}"
        )));
    };
    if !collection_is_visible(&collection, visible_collections) {
        return Err(RedDBError::Query(format!(
            "COLLECTION_NOT_FOUND: {collection}"
        )));
    }
    if catalog_entry.model != CollectionModel::Table {
        return Err(RedDBError::Query(format!(
            "NOT_APPLICABLE: SHOW CREATE TABLE {collection} is only supported for table collections"
        )));
    }

    let contracts = db.collection_contracts();
    let Some(contract) = contracts
        .iter()
        .find(|contract| contract.name == collection)
    else {
        return Err(RedDBError::Query(format!(
            "NOT_APPLICABLE: SHOW CREATE TABLE {collection} has no declared column schema"
        )));
    };
    if contract.declared_columns.is_empty() {
        return Err(RedDBError::Query(format!(
            "NOT_APPLICABLE: SHOW CREATE TABLE {collection} has no declared column schema"
        )));
    }

    let ddl = render_show_create_table_ddl(
        contract,
        runtime.index_store_ref().list_indices(&collection),
    );
    let schema = Arc::new(
        SHOW_CREATE_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    Ok(vec![UnifiedRecord::with_schema(
        schema,
        vec![Value::text(ddl)],
    )])
}

fn show_create_target_collection(query: &TableQuery) -> RedDBResult<String> {
    match query.filter.as_ref() {
        Some(Filter::Compare {
            field: FieldRef::TableColumn { column, .. },
            op: CompareOp::Eq,
            value: Value::Text(collection),
        }) if column == "collection" => Ok(collection.to_string()),
        _ => Err(RedDBError::Query(
            "SHOW CREATE TABLE requires a table name".to_string(),
        )),
    }
}

fn render_show_create_table_ddl(
    contract: &crate::physical::CollectionContract,
    mut indices: Vec<super::index_store::RegisteredIndex>,
) -> String {
    let columns = contract
        .declared_columns
        .iter()
        .map(render_show_create_column)
        .collect::<Vec<_>>()
        .join(", ");
    let mut statements = vec![format!(
        "CREATE TABLE {} ({columns})",
        render_sql_identifier(&contract.name)
    )];

    indices.sort_by(|left, right| left.name.cmp(&right.name));
    for index in indices {
        let unique = if index.unique { "UNIQUE " } else { "" };
        let columns = index
            .columns
            .iter()
            .map(|column| render_sql_identifier(column))
            .collect::<Vec<_>>()
            .join(", ");
        statements.push(format!(
            "CREATE {unique}INDEX {} ON {} ({columns}) USING {}",
            render_sql_identifier(&index.name),
            render_sql_identifier(&contract.name),
            render_index_method_for_ddl(index.method)
        ));
    }

    format!("{};", statements.join(";\n"))
}

fn render_show_create_column(column: &crate::physical::DeclaredColumnContract) -> String {
    let mut parts = vec![
        render_sql_identifier(&column.name),
        column
            .sql_type
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_else(|| column.data_type.clone()),
    ];

    if column.not_null && !column.primary_key {
        parts.push("NOT NULL".to_string());
    }
    if let Some(default) = column.default.as_deref() {
        parts.push(format!(
            "DEFAULT = {}",
            render_show_create_default(column, default)
        ));
    }
    if let Some(compress) = column.compress {
        parts.push(format!("COMPRESS:{compress}"));
    }
    if column.unique {
        parts.push("UNIQUE".to_string());
    }
    if column.primary_key {
        parts.push("PRIMARY KEY".to_string());
    }

    parts.join(" ")
}

fn render_show_create_default(
    column: &crate::physical::DeclaredColumnContract,
    default: &str,
) -> String {
    if default.eq_ignore_ascii_case("null") {
        return "NULL".to_string();
    }
    if show_create_default_needs_quotes(column) {
        return format!("'{}'", default.replace('\'', "''"));
    }
    default.to_string()
}

fn show_create_default_needs_quotes(column: &crate::physical::DeclaredColumnContract) -> bool {
    let base = column
        .sql_type
        .as_ref()
        .map(|sql_type| sql_type.base_name())
        .unwrap_or_else(|| column.data_type.to_ascii_uppercase());
    matches!(
        base.as_str(),
        "TEXT" | "STRING" | "EMAIL" | "UUID" | "IPADDR" | "MACADDR" | "ENUM"
    )
}

fn render_index_method_for_ddl(method: super::index_store::IndexMethodKind) -> &'static str {
    match method {
        super::index_store::IndexMethodKind::Hash => "HASH",
        super::index_store::IndexMethodKind::BTree => "BTREE",
        super::index_store::IndexMethodKind::Bitmap => "BITMAP",
        super::index_store::IndexMethodKind::Spatial => "RTREE",
    }
}

fn render_sql_identifier(identifier: &str) -> String {
    identifier.to_string()
}

fn policies_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        POLICY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let mut records = Vec::new();

    let enabled = runtime.inner.rls_enabled_tables.read().clone();
    let rls_policies = runtime.inner.rls_policies.read();
    let mut rls_entries: Vec<_> = rls_policies.iter().collect();
    rls_entries.sort_by(
        |((left_collection, left_name), _), ((right_collection, right_name), _)| {
            left_collection
                .cmp(right_collection)
                .then_with(|| left_name.cmp(right_name))
        },
    );
    for ((collection, _), policy) in rls_entries {
        if !collection_is_visible(collection, visible_collections) {
            continue;
        }
        records.push(policy_record(
            &schema,
            policy.name.clone(),
            Some(collection.clone()),
            "rls",
            "allow",
            rls_actions(policy.action),
            rls_principals(policy.role.as_deref()),
            Value::text(render_filter_for_catalog(&policy.using)),
            Value::Boolean(enabled.contains(collection)),
        ));
    }
    drop(rls_policies);

    let auth_store = runtime.inner.auth_store.read().clone();
    if let Some(auth_store) = auth_store {
        for policy in auth_store.list_policies() {
            if !iam_policy_visible_to_tenant(&policy, tenant) {
                continue;
            }
            for (statement_index, statement) in policy.statements.iter().enumerate() {
                let collection_names = iam_statement_collections(statement);
                if collection_names.is_empty() {
                    records.push(iam_policy_record(
                        &schema,
                        &policy,
                        statement_index,
                        statement,
                        None,
                    ));
                    continue;
                }
                for collection in collection_names {
                    if !collection_is_visible(&collection, visible_collections) {
                        continue;
                    }
                    records.push(iam_policy_record(
                        &schema,
                        &policy,
                        statement_index,
                        statement,
                        Some(collection),
                    ));
                }
            }
        }
    }

    records
}

fn collection_is_visible(collection: &str, visible_collections: Option<&HashSet<String>>) -> bool {
    visible_collections.is_none_or(|visible| visible.contains(collection))
}

fn iam_policy_visible_to_tenant(policy: &Policy, tenant: Option<&str>) -> bool {
    match (tenant, policy.tenant.as_deref()) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(active), Some(policy_tenant)) => active == policy_tenant,
    }
}

fn policy_record(
    schema: &Arc<Vec<Arc<str>>>,
    name: String,
    collection: Option<String>,
    kind: &'static str,
    effect: &'static str,
    actions: Vec<String>,
    principals: Vec<String>,
    predicate: Value,
    enabled: Value,
) -> UnifiedRecord {
    UnifiedRecord::with_schema(
        Arc::clone(schema),
        vec![
            Value::text(name),
            collection.map(Value::text).unwrap_or(Value::Null),
            Value::text(kind),
            Value::text(effect),
            Value::Array(actions.into_iter().map(Value::text).collect()),
            Value::Array(principals.into_iter().map(Value::text).collect()),
            predicate,
            enabled,
        ],
    )
}

fn iam_policy_record(
    schema: &Arc<Vec<Arc<str>>>,
    policy: &Policy,
    statement_index: usize,
    statement: &Statement,
    collection: Option<String>,
) -> UnifiedRecord {
    let name = statement
        .sid
        .as_ref()
        .map(|sid| format!("{}:{sid}", policy.id))
        .unwrap_or_else(|| {
            if policy.statements.len() > 1 {
                format!("{}#{}", policy.id, statement_index)
            } else {
                policy.id.clone()
            }
        });
    policy_record(
        schema,
        name,
        collection,
        "iam",
        iam_effect(statement.effect),
        iam_actions(&statement.actions),
        Vec::new(),
        Value::Null,
        Value::Boolean(true),
    )
}

fn rls_actions(action: Option<PolicyAction>) -> Vec<String> {
    match action {
        Some(PolicyAction::Select) => vec!["select".to_string()],
        Some(PolicyAction::Insert) => vec!["insert".to_string()],
        Some(PolicyAction::Update) => vec!["update".to_string()],
        Some(PolicyAction::Delete) => vec!["delete".to_string()],
        None => vec!["*".to_string()],
    }
}

fn rls_principals(role: Option<&str>) -> Vec<String> {
    role.map(|role| vec![role.to_string()])
        .unwrap_or_else(|| vec!["*".to_string()])
}

fn iam_effect(effect: Effect) -> &'static str {
    match effect {
        Effect::Allow => "allow",
        Effect::Deny => "deny",
    }
}

fn iam_actions(actions: &[ActionPattern]) -> Vec<String> {
    actions.iter().map(render_action_pattern).collect()
}

fn render_action_pattern(action: &ActionPattern) -> String {
    match action {
        ActionPattern::Exact(value) => value.clone(),
        ActionPattern::Wildcard => "*".to_string(),
        ActionPattern::Prefix(prefix) => format!("{prefix}:*"),
    }
}

fn iam_statement_collections(statement: &Statement) -> Vec<String> {
    let mut out = Vec::new();
    for resource in &statement.resources {
        match resource {
            ResourcePattern::Exact { kind, name }
                if kind.eq_ignore_ascii_case("table")
                    || kind.eq_ignore_ascii_case("collection") =>
            {
                out.push(name.clone());
            }
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

fn render_filter_for_catalog(filter: &Filter) -> String {
    match filter {
        Filter::Compare { field, op, value } => {
            format!(
                "{} {} {}",
                render_field_for_catalog(field),
                op,
                crate::storage::query::renderer::render_value_sql(value)
            )
        }
        Filter::CompareFields { left, op, right } => {
            format!(
                "{} {} {}",
                render_field_for_catalog(left),
                op,
                render_field_for_catalog(right)
            )
        }
        Filter::CompareExpr { lhs, op, rhs } => {
            format!(
                "{} {} {}",
                render_expr_for_catalog(lhs),
                op,
                render_expr_for_catalog(rhs)
            )
        }
        Filter::And(left, right) => format!(
            "({}) AND ({})",
            render_filter_for_catalog(left),
            render_filter_for_catalog(right)
        ),
        Filter::Or(left, right) => format!(
            "({}) OR ({})",
            render_filter_for_catalog(left),
            render_filter_for_catalog(right)
        ),
        Filter::Not(inner) => format!("NOT ({})", render_filter_for_catalog(inner)),
        Filter::IsNull(field) => format!("{} IS NULL", render_field_for_catalog(field)),
        Filter::IsNotNull(field) => format!("{} IS NOT NULL", render_field_for_catalog(field)),
        Filter::In { field, values } => format!(
            "{} IN ({})",
            render_field_for_catalog(field),
            values
                .iter()
                .map(crate::storage::query::renderer::render_value_sql)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Filter::Between { field, low, high } => format!(
            "{} BETWEEN {} AND {}",
            render_field_for_catalog(field),
            crate::storage::query::renderer::render_value_sql(low),
            crate::storage::query::renderer::render_value_sql(high)
        ),
        Filter::Like { field, pattern } => {
            format!("{} LIKE '{}'", render_field_for_catalog(field), pattern)
        }
        Filter::StartsWith { field, prefix } => {
            format!(
                "{} STARTS WITH '{}'",
                render_field_for_catalog(field),
                prefix
            )
        }
        Filter::EndsWith { field, suffix } => {
            format!("{} ENDS WITH '{}'", render_field_for_catalog(field), suffix)
        }
        Filter::Contains { field, substring } => {
            format!(
                "{} CONTAINS '{}'",
                render_field_for_catalog(field),
                substring
            )
        }
    }
}

fn render_expr_for_catalog(expr: &Expr) -> String {
    match expr {
        Expr::Literal { value, .. } => crate::storage::query::renderer::render_value_sql(value),
        Expr::Column { field, .. } => render_field_for_catalog(field),
        Expr::Parameter { index, .. } => format!("${index}"),
        Expr::BinaryOp { op, lhs, rhs, .. } => format!(
            "{} {:?} {}",
            render_expr_for_catalog(lhs),
            op,
            render_expr_for_catalog(rhs)
        ),
        Expr::UnaryOp { op, operand, .. } => match op {
            UnaryOp::Not => format!("NOT {}", render_expr_for_catalog(operand)),
            UnaryOp::Neg => format!("-{}", render_expr_for_catalog(operand)),
        },
        Expr::Cast { inner, target, .. } => {
            format!("CAST({} AS {:?})", render_expr_for_catalog(inner), target)
        }
        Expr::FunctionCall { name, args, .. } => format!(
            "{}({})",
            name,
            args.iter()
                .map(render_expr_for_catalog)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Case { .. } => format!("{expr:?}"),
        Expr::IsNull {
            operand, negated, ..
        } => format!(
            "{} IS {}NULL",
            render_expr_for_catalog(operand),
            if *negated { "NOT " } else { "" }
        ),
        Expr::InList {
            target,
            values,
            negated,
            ..
        } => format!(
            "{} {}IN ({})",
            render_expr_for_catalog(target),
            if *negated { "NOT " } else { "" },
            values
                .iter()
                .map(render_expr_for_catalog)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } => format!(
            "{} {}BETWEEN {} AND {}",
            render_expr_for_catalog(target),
            if *negated { "NOT " } else { "" },
            render_expr_for_catalog(low),
            render_expr_for_catalog(high)
        ),
        Expr::Subquery { .. } => "(SELECT ...)".to_string(),
        Expr::WindowFunctionCall { name, args, .. } => {
            let args = args
                .iter()
                .map(render_expr_for_catalog)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}({args}) OVER (...)")
        }
    }
}

fn render_field_for_catalog(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } if table.is_empty() => column.clone(),
        FieldRef::TableColumn { table, column } => format!("{table}.{column}"),
        FieldRef::NodeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("{alias}.id"),
    }
}

fn on_disk_bytes_value(store: &crate::storage::unified::UnifiedStore, collection: &str) -> Value {
    crate::storage::disk_accountant::bytes_on_disk_for(store, collection)
        .map(Value::UnsignedInteger)
        .unwrap_or(Value::Null)
}

fn collections_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        COLLECTION_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|collection| {
            visible_collections.is_none_or(|visible| visible.contains(&collection.name))
        })
        .filter(|collection| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &collection.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let collection_tenant = collection_tenant(store.as_ref(), &collection.name);
            let visible_tenant = collection_tenant.as_deref().or(tenant);
            let internal = internal_registry.is_internal(&collection.name);
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let queue_mode = if collection.model == CollectionModel::Queue {
                Value::text(super::impl_queue::queue_mode_str(
                    store.as_ref(),
                    &collection.name,
                ))
            } else {
                Value::Null
            };
            let vector_dimension = collection
                .vector_dimension
                .map(|dimension| Value::UnsignedInteger(dimension as u64))
                .unwrap_or(Value::Null);
            let vector_metric = collection
                .vector_metric
                .map(|metric| Value::text(distance_metric_name(metric)))
                .unwrap_or(Value::Null);
            let session_key = collection
                .session_key
                .as_ref()
                .map(|key| Value::text(key.clone()))
                .unwrap_or(Value::Null);
            let session_gap_ms = collection
                .session_gap_ms
                .map(Value::UnsignedInteger)
                .unwrap_or(Value::Null);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    Value::text(collection_model_name(collection.model)),
                    Value::text(schema_mode_name(collection.schema_mode)),
                    Value::UnsignedInteger(collection.entities as u64),
                    Value::UnsignedInteger(collection.segments as u64),
                    Value::UnsignedInteger(collection.indices.len() as u64),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    Value::Boolean(internal),
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                    queue_mode,
                    vector_dimension,
                    vector_metric,
                    session_key,
                    session_gap_ms,
                ],
            )
        })
        .collect()
}

/// Issue #745 — typed `red.tables` projection. Model-shaped view over
/// `red.collections` filtered to `model = table`, joined with the
/// declared column contract for `has_primary_key` and `column_count`.
fn tables_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        TABLE_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let db = runtime.db();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Table)
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let contract = db.collection_contract(&collection.name);
            let column_count = contract
                .as_ref()
                .map(|c| c.declared_columns.len() as u64)
                .unwrap_or(0);
            let has_primary_key = contract
                .as_ref()
                .map(|c| c.declared_columns.iter().any(|col| col.primary_key))
                .unwrap_or(false);
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let owner_tenant = collection_tenant(store.as_ref(), &collection.name);
            let visible_tenant = owner_tenant.as_deref().or(tenant);
            let internal = internal_registry.is_internal(&collection.name);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name.clone()),
                    Value::text(schema_mode_name(collection.schema_mode)),
                    Value::UnsignedInteger(collection.entities as u64),
                    Value::UnsignedInteger(column_count),
                    Value::UnsignedInteger(collection.indices.len() as u64),
                    Value::Boolean(has_primary_key),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}

/// Issue #745 — typed `red.documents` projection. Filtered to
/// `model = document`. `inferred_field_count` reuses the same
/// inference path that `red.columns` uses for document collections,
/// so the two surfaces cannot drift.
fn documents_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        DOCUMENT_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Document)
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let (document_count, inferred_field_count) = document_counts(runtime, &collection.name);
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let internal = internal_registry.is_internal(&collection.name);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    Value::text(schema_mode_name(collection.schema_mode)),
                    Value::UnsignedInteger(document_count),
                    Value::UnsignedInteger(inferred_field_count),
                    Value::Boolean(true),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}

/// Issue #745 — typed `red.kv` projection. Filtered to
/// `model = kv`. `supports_prefix_scan` is a stable capability
/// indicator (true — KV always supports `KEYS WITH PREFIX`). The
/// declared key/value shape is reported as text-keyed with a
/// mixed-value hint when no declared contract pins it down.
fn kv_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        KV_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let db = runtime.db();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Kv)
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let contract = db.collection_contract(&collection.name);
            // KV defaults to text keys. Value shape can be pinned by a
            // declared `value` column; otherwise it's `mixed` (any
            // value type is accepted).
            let key_type = contract
                .as_ref()
                .and_then(|c| {
                    c.declared_columns
                        .iter()
                        .find(|col| col.name == "key")
                        .map(|col| {
                            col.sql_type
                                .as_ref()
                                .map(ToString::to_string)
                                .unwrap_or_else(|| col.data_type.clone())
                        })
                })
                .unwrap_or_else(|| "TEXT".to_string());
            let value_type = contract
                .as_ref()
                .and_then(|c| {
                    c.declared_columns
                        .iter()
                        .find(|col| col.name == "value")
                        .map(|col| {
                            col.sql_type
                                .as_ref()
                                .map(ToString::to_string)
                                .unwrap_or_else(|| col.data_type.clone())
                        })
                })
                .unwrap_or_else(|| "mixed".to_string());
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let internal = internal_registry.is_internal(&collection.name);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    Value::UnsignedInteger(collection.entities as u64),
                    Value::text(key_type),
                    Value::text(value_type),
                    Value::Boolean(true),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}

/// Issue #746 — typed `red.vectors` projection. Filtered to
/// `model = vector`. `dimensions` / `metric` come from the catalog
/// contract — both are NULL when undeclared (e.g. dynamic-mode
/// vector collection). `artifact_state` / `search_capable` come
/// from the vector introspection registry (#743). The registry is
/// populated lazily by the engine; when there is no published row
/// for this collection, `artifact_state` defaults to `unavailable`
/// and `search_capable` defaults to `false`, per the thread-
/// discussion decision on #746 (stable explicit values, not NULL).
fn vectors_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        VECTOR_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Vector)
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let owner_tenant = collection_tenant(store.as_ref(), &collection.name);
            let visible_tenant = owner_tenant.as_deref().or(tenant);
            let internal = internal_registry.is_internal(&collection.name);
            let dimensions = collection
                .vector_dimension
                .map(|d| Value::UnsignedInteger(d as u64))
                .unwrap_or(Value::Null);
            let metric = collection
                .vector_metric
                .map(|m| Value::text(distance_metric_name(m)))
                .unwrap_or(Value::Null);
            let introspection = runtime.vector_introspection_get(&collection.name);
            let (artifact_state, search_capable) = match introspection {
                Some(row) => (row.artifact.state.as_str(), row.vector.search_capable),
                None => ("unavailable", false),
            };
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name.clone()),
                    dimensions,
                    metric,
                    Value::UnsignedInteger(collection.entities as u64),
                    Value::Boolean(search_capable),
                    Value::text(artifact_state),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}

/// Issue #747 — typed `red.timeseries` projection. Filtered to
/// `model = time_series`. When the underlying collection was created
/// via `CREATE HYPERTABLE`, chunk-derived columns are populated from
/// the live `HypertableRegistry`; standalone timeseries report
/// `is_hypertable = false` and `NULL` for those columns.
fn timeseries_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        TIMESERIES_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let db = runtime.db();
    let registry = db.hypertables();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| {
            // `model = time_series` covers plain `CREATE TIMESERIES`;
            // `CREATE HYPERTABLE` declares a Table contract but
            // registers a hypertable spec — pick those up too so the
            // chart UI doesn't have to query two surfaces.
            c.model == CollectionModel::TimeSeries || registry.get(&c.name).is_some()
        })
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let spec = registry.get(&collection.name);
            let chunks = if spec.is_some() {
                registry.show_chunks(&collection.name)
            } else {
                Vec::new()
            };
            let is_hypertable = spec.is_some();
            let time_column = spec.as_ref().map(|s| s.time_column.clone());
            // Chunk widths are nanoseconds in the registry; expose
            // milliseconds so the UI doesn't have to convert.
            let chunk_interval_ms = spec.as_ref().map(|s| s.chunk_interval_ns / 1_000_000);
            let chunk_count = chunks.len() as u64;
            let (oldest_ns, newest_ns) =
                chunks
                    .iter()
                    .fold((None::<u64>, None::<u64>), |(oldest, newest), chunk| {
                        // Empty chunks have `min_ts_ns = u64::MAX`;
                        // skip those when computing the overall min so
                        // an empty hypertable shows `NULL` rather than
                        // `u64::MAX`.
                        let next_oldest = if chunk.row_count == 0 {
                            oldest
                        } else {
                            Some(match oldest {
                                Some(prev) => prev.min(chunk.min_ts_ns),
                                None => chunk.min_ts_ns,
                            })
                        };
                        let next_newest = if chunk.row_count == 0 {
                            newest
                        } else {
                            Some(match newest {
                                Some(prev) => prev.max(chunk.max_ts_ns),
                                None => chunk.max_ts_ns,
                            })
                        };
                        (next_oldest, next_newest)
                    });
            let oldest_ts_ms = oldest_ns.map(|ns| ns / 1_000_000);
            let newest_ts_ms = newest_ns.map(|ns| ns / 1_000_000);
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let owner_tenant = collection_tenant(store.as_ref(), &collection.name);
            let visible_tenant = owner_tenant.as_deref().or(tenant);
            let internal = internal_registry.is_internal(&collection.name);
            // Issue #748 — downsample / continuous-aggregate / sweep
            // indicators. `downsample_policies` is the comma-joined
            // list of policy specs stored at CREATE TIMESERIES time;
            // `continuous_aggregate_*` reflect aggregates whose
            // declared `source` is this collection. `last_sweep_ms` is
            // pinned `NULL` (unavailable) because the retention
            // registry tracks a global `last_sweep_unix_ns` rather
            // than per-collection sweep state — see AC #3.
            let downsample_policies = read_downsample_policies(store.as_ref(), &collection.name);
            let downsample_policies_value = if downsample_policies.is_empty() {
                Value::Null
            } else {
                Value::text(downsample_policies.join(","))
            };
            let mut ca_names: Vec<String> = db
                .continuous_aggregates()
                .list()
                .into_iter()
                .filter(|spec| spec.source == collection.name)
                .map(|spec| spec.name)
                .collect();
            ca_names.sort();
            let ca_count = ca_names.len() as u64;
            let ca_names_value = if ca_names.is_empty() {
                Value::Null
            } else {
                Value::text(ca_names.join(","))
            };
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name.clone()),
                    Value::text(schema_mode_name(collection.schema_mode)),
                    Value::Boolean(is_hypertable),
                    time_column.map(Value::text).unwrap_or(Value::Null),
                    chunk_interval_ms
                        .map(Value::UnsignedInteger)
                        .unwrap_or(Value::Null),
                    Value::UnsignedInteger(chunk_count),
                    // Retention is stored in the runtime's default-TTL
                    // map, not on the catalog descriptor — read it
                    // back the same way the retention sweeper does.
                    db.collection_default_ttl_ms(&collection.name)
                        .or(collection.retention_duration_ms)
                        .map(Value::UnsignedInteger)
                        .unwrap_or(Value::Null),
                    collection
                        .session_key
                        .clone()
                        .map(Value::text)
                        .unwrap_or(Value::Null),
                    collection
                        .session_gap_ms
                        .map(Value::UnsignedInteger)
                        .unwrap_or(Value::Null),
                    Value::UnsignedInteger(collection.entities as u64),
                    oldest_ts_ms
                        .map(Value::UnsignedInteger)
                        .unwrap_or(Value::Null),
                    newest_ts_ms
                        .map(Value::UnsignedInteger)
                        .unwrap_or(Value::Null),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                    Value::Boolean(internal),
                    downsample_policies_value,
                    Value::UnsignedInteger(ca_count),
                    ca_names_value,
                    Value::Null,
                ],
            )
        })
        .collect()
}

/// Issue #748 — read the `downsample_policies` array persisted in
/// the `red_timeseries_meta` collection by
/// `super::impl_timeseries::save_timeseries_metadata`. Returns the
/// sorted list of policy spec strings (empty when none / collection
/// is not a timeseries).
fn read_downsample_policies(store: &UnifiedStore, collection: &str) -> Vec<String> {
    const META: &str = "red_timeseries_meta";
    let Some(manager) = store.get_collection(META) else {
        return Vec::new();
    };
    let rows = manager.query_all(|entity| {
        entity.data.as_row().is_some_and(|row| {
            row.get_field("series").is_some_and(
                |value| matches!(value, Value::Text(candidate) if &**candidate == collection),
            )
        })
    });
    let mut out: Vec<String> = Vec::new();
    for row in rows {
        let Some(row_data) = row.data.as_row() else {
            continue;
        };
        let Some(Value::Array(specs)) = row_data.get_field("downsample_policies") else {
            continue;
        };
        for value in specs {
            if let Value::Text(s) = value {
                out.push(s.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Issue #748 — per-chunk hypertable metadata. One row per
/// `(hypertable, chunk_start_ns)` covering every registered
/// hypertable visible under the active tenant / scope. Empty chunks
/// report `NULL` for `min_ts_ms` / `max_ts_ms` rather than leaking
/// the registry's `u64::MAX` sentinel.
fn hypertable_chunks_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        HYPERTABLE_CHUNK_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let db = runtime.db();
    let store = db.store();
    let registry = db.hypertables();
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    let mut hypertables = registry.names();
    hypertables.sort();

    let mut rows: Vec<UnifiedRecord> = Vec::new();
    for name in hypertables {
        if !collection_is_visible(&name, visible_collections) {
            continue;
        }
        let owner_tenant = collection_tenant(store.as_ref(), &name);
        if let Some(scope) = tenant {
            if let Some(owner) = owner_tenant.as_deref() {
                if owner != scope {
                    continue;
                }
            }
        }
        let visible_tenant = owner_tenant.as_deref().or(tenant);
        let Some(spec) = registry.get(&name) else {
            continue;
        };
        let mut chunks = registry.show_chunks(&name);
        chunks.sort_by_key(|c| c.id.start_ns);
        for chunk in chunks {
            let has_rows = chunk.row_count > 0;
            let min_ts_ms = if has_rows {
                Value::UnsignedInteger(chunk.min_ts_ns / 1_000_000)
            } else {
                Value::Null
            };
            let max_ts_ms = if has_rows {
                Value::UnsignedInteger(chunk.max_ts_ns / 1_000_000)
            } else {
                Value::Null
            };
            let ttl_override_ms = chunk
                .ttl_override_ns
                .map(|ns| Value::UnsignedInteger(ns / 1_000_000))
                .unwrap_or(Value::Null);
            let effective_ttl_ns = chunk.effective_ttl_ns(spec.default_ttl_ns);
            let effective_ttl_ms = effective_ttl_ns
                .map(|ns| Value::UnsignedInteger(ns / 1_000_000))
                .unwrap_or(Value::Null);
            // `expiry_ns` is `max_ts_ns + effective_ttl_ns`; only
            // meaningful when the chunk has actually observed a row.
            let expiry_ms = match (has_rows, chunk.expiry_ns(spec.default_ttl_ns)) {
                (true, Some(ns)) => Value::UnsignedInteger(ns / 1_000_000),
                _ => Value::Null,
            };
            let is_expired = has_rows && chunk.is_expired_at(now_ns, spec.default_ttl_ns);
            rows.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(name.clone()),
                    Value::UnsignedInteger(chunk.id.start_ns / 1_000_000),
                    Value::UnsignedInteger(chunk.end_ns_exclusive / 1_000_000),
                    Value::UnsignedInteger(chunk.row_count),
                    min_ts_ms,
                    max_ts_ms,
                    Value::Boolean(chunk.sealed),
                    ttl_override_ms,
                    effective_ttl_ms,
                    expiry_ms,
                    Value::Boolean(is_expired),
                    visible_tenant.map(Value::text).unwrap_or(Value::Null),
                ],
            ));
        }
    }
    rows
}

/// Issue #748 — writes-by-cohort bucketing. For each hypertable
/// visible under the active scope, scans rows from the backing
/// segment manager, reads the time column, and accumulates a count
/// per `(bucket_size_ms, bucket_start_ms)` for each of the three
/// canonical cohort sizes (1m / 5m / 10m). Empty buckets are not
/// emitted. `writes_count` is held at `NULL` until reliable
/// WAL/operation telemetry exists — the thread-discussion decision
/// requires we distinguish event-time row counts from actual write
/// throughput, and not paper over the gap by labelling the former.
///
/// Filters: an optional `WHERE collection = 'x'` narrows to a single
/// hypertable; an optional `WHERE bucket_size_ms = N` narrows to a
/// single cohort size. Both are evaluated by inspecting the
/// `TableQuery` filter — heavier filters fall through (the row set
/// is filtered by the normal execution path after this snapshot
/// runs).
fn timeseries_writes_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
    query: &TableQuery,
) -> Vec<UnifiedRecord> {
    const BUCKET_SIZES_MS: [u64; 3] = [60_000, 300_000, 600_000];

    let schema = Arc::new(
        TIMESERIES_WRITES_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let collection_filter = extract_text_eq(query, "collection");
    let bucket_filter = extract_uint_eq(query, "bucket_size_ms");

    let db = runtime.db();
    let store = db.store();
    let registry = db.hypertables();
    let mut hypertables = registry.names();
    hypertables.sort();

    let mut rows: Vec<UnifiedRecord> = Vec::new();
    for name in hypertables {
        if let Some(want) = collection_filter.as_deref() {
            if want != name {
                continue;
            }
        }
        if !collection_is_visible(&name, visible_collections) {
            continue;
        }
        let owner_tenant = collection_tenant(store.as_ref(), &name);
        if let Some(scope) = tenant {
            if let Some(owner) = owner_tenant.as_deref() {
                if owner != scope {
                    continue;
                }
            }
        }
        let Some(spec) = registry.get(&name) else {
            continue;
        };
        let Some(manager) = store.get_collection(&name) else {
            continue;
        };
        let time_col = spec.time_column.clone();
        let mut active_sizes: Vec<u64> = match bucket_filter {
            Some(size) if BUCKET_SIZES_MS.contains(&size) => vec![size],
            Some(_) => continue, // unsupported bucket size — skip silently
            None => BUCKET_SIZES_MS.to_vec(),
        };
        active_sizes.sort();

        // Scan the backing collection once, fold every row's time
        // column into every active cohort.
        let buckets: BTreeMap<(u64, u64), u64> = manager.fold_entities_parallel(
            BTreeMap::new,
            |mut local, entity| {
                let Some(row) = entity.data.as_row() else {
                    return local;
                };
                let Some(value) = row.get_field(&time_col) else {
                    return local;
                };
                let Some(ts_ns) = value_to_unsigned_ns(value) else {
                    return local;
                };
                let ts_ms = ts_ns / 1_000_000;
                for size in &active_sizes {
                    let bucket = (ts_ms / *size) * *size;
                    *local.entry((*size, bucket)).or_insert(0) += 1;
                }
                local
            },
            |mut a, b| {
                for (k, v) in b {
                    *a.entry(k).or_insert(0) += v;
                }
                a
            },
        );

        for ((bucket_size_ms, bucket_start_ms), events_count) in buckets {
            rows.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(name.clone()),
                    Value::UnsignedInteger(bucket_size_ms),
                    Value::UnsignedInteger(bucket_start_ms),
                    Value::UnsignedInteger(events_count),
                    // `writes_count` — actual write throughput is
                    // unavailable until WAL/operation telemetry
                    // exists; AC #3 + thread-discussion require we
                    // surface NULL rather than inferring it from the
                    // event-time row count.
                    Value::Null,
                ],
            ));
        }
    }
    rows
}

/// Extract `WHERE <column> = '<text>'` from a TableQuery filter for
/// snapshot-time pushdown. Returns `None` if the filter is missing,
/// not an equality, or compares a different column.
fn extract_text_eq(query: &TableQuery, column: &str) -> Option<String> {
    match query.filter.as_ref()? {
        Filter::Compare {
            field: FieldRef::TableColumn { column: c, .. },
            op: CompareOp::Eq,
            value: Value::Text(text),
        } if c == column => Some(text.to_string()),
        _ => None,
    }
}

/// Extract `WHERE <column> = <unsigned int>` (accepting Int / BigInt
/// / Unsigned variants). Returns `None` if the filter is missing or
/// not an integer equality on the requested column.
fn extract_uint_eq(query: &TableQuery, column: &str) -> Option<u64> {
    match query.filter.as_ref()? {
        Filter::Compare {
            field: FieldRef::TableColumn { column: c, .. },
            op: CompareOp::Eq,
            value,
        } if c == column => match value {
            Value::UnsignedInteger(n) => Some(*n),
            Value::Integer(n) | Value::BigInt(n) if *n >= 0 => Some(*n as u64),
            _ => None,
        },
        _ => None,
    }
}

/// Convert a `Value` representing a unix-nanosecond timestamp into a
/// `u64`. Mirrors the loose acceptance the hypertable INSERT path
/// already applies (`Value::Integer | BigInt | UnsignedInteger`).
fn value_to_unsigned_ns(value: &Value) -> Option<u64> {
    match value {
        Value::UnsignedInteger(n) => Some(*n),
        Value::Integer(n) | Value::BigInt(n) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

/// Issue #746 — typed `red.graphs` projection. Filtered to
/// `model = graph`. Per-collection node / edge counts are produced by
/// a single scan over the collection's segment manager (the catalog
/// snapshot's `entities` total lumps nodes and edges together for
/// graph collections, which the UI cannot split). `node_labels` /
/// `edge_labels` are deterministic sorted arrays so test assertions
/// and the toolbar both see a stable shape. `supports_viewport` is
/// the stable indicator the explorer keys on; the viewport contract
/// itself landed in #744.
fn graphs_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        GRAPH_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Graph)
        .filter(|c| collection_is_visible(&c.name, visible_collections))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let (node_count, edge_count, node_labels, edge_labels) =
                graph_counts(store.as_ref(), &collection.name);
            let in_memory_bytes = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats().total_memory_bytes as u64)
                .unwrap_or(0);
            let on_disk_bytes = on_disk_bytes_value(store.as_ref(), &collection.name);
            let internal = internal_registry.is_internal(&collection.name);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    Value::UnsignedInteger(node_count),
                    Value::UnsignedInteger(edge_count),
                    Value::Array(node_labels.into_iter().map(Value::text).collect()),
                    Value::Array(edge_labels.into_iter().map(Value::text).collect()),
                    Value::Boolean(true),
                    Value::Boolean(true),
                    Value::UnsignedInteger(in_memory_bytes),
                    on_disk_bytes,
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}

/// Issue #746 — single-pass scan over a graph collection's segment
/// manager that returns `(node_count, edge_count, node_labels,
/// edge_labels)`. Labels are deduplicated and returned in sorted
/// order so the typed projection has a stable shape across calls.
fn graph_counts(store: &UnifiedStore, collection: &str) -> (u64, u64, Vec<String>, Vec<String>) {
    let Some(manager) = store.get_collection(collection) else {
        return (0, 0, Vec::new(), Vec::new());
    };
    let mut node_count: u64 = 0;
    let mut edge_count: u64 = 0;
    let mut node_labels: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut edge_labels: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for entity in manager.query_all(|_| true) {
        match &entity.kind {
            crate::storage::EntityKind::GraphNode(node) => {
                node_count = node_count.saturating_add(1);
                if !node.node_type.is_empty() {
                    node_labels.insert(node.node_type.clone());
                }
            }
            crate::storage::EntityKind::GraphEdge(edge) => {
                edge_count = edge_count.saturating_add(1);
                if !edge.label.is_empty() {
                    edge_labels.insert(edge.label.clone());
                }
            }
            _ => {}
        }
    }
    (
        node_count,
        edge_count,
        node_labels.into_iter().collect(),
        edge_labels.into_iter().collect(),
    )
}

/// Issue #747 — typed `red.metrics` projection. One row per metric
/// descriptor registered through `CREATE METRIC`. `labels` / `unit` /
/// `retention_ms` columns exist for schema stability but are populated
/// as `NULL` until the descriptor catalog tracks them. Descriptors
/// are not tenant-owned today, so the visibility behavior matches
/// `red.analytics.metrics`: cluster admins and tenant sessions both
/// see the full catalog. `_tenant` and `_visible_collections` arguments
/// are accepted for shape parity with the other typed-relation
/// snapshots and to leave room for future tenant scoping without
/// breaking callers.
fn metrics_snapshot(
    runtime: &RedDBRuntime,
    _tenant: Option<&str>,
    _visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let store = runtime.db().store();
    let schema = Arc::new(
        METRICS_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    super::metric_descriptor_catalog::list(store.as_ref())
        .into_iter()
        .map(|entry| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.path),
                    Value::text(entry.kind),
                    Value::text(entry.role),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Boolean(true),
                    timestamp_ms_value(entry.created_at_ms),
                ],
            )
        })
        .collect()
}

/// Issue #745 — count the rows that look like documents (have a JSON
/// or text `body` field) and the distinct top-level field names seen
/// across them. Mirrors `infer_document_columns` so the two surfaces
/// cannot drift.
fn document_counts(runtime: &RedDBRuntime, collection: &str) -> (u64, u64) {
    let mut document_count: u64 = 0;
    let mut field_names: HashSet<String> = HashSet::new();
    for (_, entity) in runtime
        .db()
        .store()
        .query_all(|entity| entity.kind.collection() == collection)
    {
        let EntityData::Row(row) = entity.data else {
            continue;
        };
        if !row
            .iter_fields()
            .any(|(name, value)| name == "body" && matches!(value, Value::Json(_) | Value::Text(_)))
        {
            continue;
        }
        document_count = document_count.saturating_add(1);
        for (name, _) in row.iter_fields() {
            field_names.insert(name.to_string());
        }
        // Post-cutover (PRD-1398) the canonical document lives only in the
        // binary `body` container; offset-read its top-level fields so the
        // count matches the columns surfaced by `infer_document_columns`.
        if let Some(Value::Json(bytes)) = row.get_field("body") {
            if let Some(body_fields) = crate::document_body::body_fields(bytes) {
                for (name, _) in body_fields {
                    field_names.insert(name);
                }
            }
        }
    }
    (document_count, field_names.len() as u64)
}

/// Issue #580 — DeclarativeRetention slice 1. Per-collection retention
/// state: `(name, retention_duration, oldest_row_ts,
/// expired_row_count_estimate)`. Materialised views are not subject to
/// source retention in this slice.
fn retention_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        RETENTION_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let db = runtime.db();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    snapshot
        .collections
        .into_iter()
        .filter(|collection| {
            visible_collections.is_none_or(|visible| visible.contains(&collection.name))
        })
        .map(|collection| {
            let contract = db.collection_contract(&collection.name);
            let retention_ms = contract.as_ref().and_then(|c| c.retention_duration_ms);
            let ts_column = contract
                .as_ref()
                .and_then(crate::runtime::retention_filter::resolve_timestamp_column);

            // Cheap-ish single pass: walk the collection once,
            // tracking the min timestamp and counting expired rows.
            // The acceptance criterion explicitly allows an
            // approximation here; we deliberately keep the scan
            // simple rather than reach for zone-map min lookups
            // that don't exist on the schemaless `created_at`
            // axis yet.
            let cutoff = retention_ms.map(|ret| (now_ms as i64).saturating_sub(ret as i64));
            let mut oldest_ts: Option<i64> = None;
            let mut expired_count: u64 = 0;
            if let Some(manager) = store.get_collection(&collection.name) {
                manager.for_each_entity(|entity| {
                    let ts = match ts_column.as_deref() {
                        Some("created_at") => Some(entity.created_at as i64),
                        Some("updated_at") => Some(entity.updated_at as i64),
                        Some(name) => entity
                            .data
                            .as_row()
                            .and_then(|row| row.get_field(name))
                            .and_then(value_as_ms),
                        None => Some(entity.created_at as i64),
                    };
                    if let Some(t) = ts {
                        oldest_ts = Some(match oldest_ts {
                            Some(prev) => prev.min(t),
                            None => t,
                        });
                        if let Some(c) = cutoff {
                            if t < c {
                                expired_count = expired_count.saturating_add(1);
                            }
                        }
                    }
                    true
                });
            }

            let retention_value = retention_ms
                .map(Value::UnsignedInteger)
                .unwrap_or(Value::Null);
            let oldest_value = oldest_ts.map(Value::BigInt).unwrap_or(Value::Null);
            // Issue #584 slice 12 — sweeper state. `last_sweep_at == 0`
            // means the collection has never been ticked; surface as
            // NULL rather than the unix epoch.
            let sweeper_state = runtime.inner.retention_sweeper.read().get(&collection.name);
            let last_sweep_at = if sweeper_state.last_sweep_at_ms == 0 {
                Value::Null
            } else {
                Value::TimestampMs(sweeper_state.last_sweep_at_ms as i64)
            };
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    retention_value,
                    oldest_value,
                    Value::UnsignedInteger(expired_count),
                    last_sweep_at,
                    Value::UnsignedInteger(sweeper_state.rows_swept_total),
                    Value::UnsignedInteger(sweeper_state.last_pending_estimate),
                ],
            )
        })
        .collect()
}

fn materialized_views_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        MATERIALIZED_VIEW_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let entries = runtime.materialized_view_metadata();
    entries
        .into_iter()
        .map(|m| {
            let refresh_every = m
                .refresh_every_ms
                .map(Value::UnsignedInteger)
                .unwrap_or(Value::Null);
            let last_refresh_at = if m.last_refresh_at_ms == 0 {
                Value::Null
            } else {
                Value::TimestampMs(m.last_refresh_at_ms as i64)
            };
            let last_error = m.last_error.clone().map(Value::text).unwrap_or(Value::Null);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(m.name),
                    Value::text(m.query_text),
                    refresh_every,
                    last_refresh_at,
                    Value::UnsignedInteger(m.last_refresh_duration_ms),
                    last_error,
                    Value::UnsignedInteger(m.current_row_count),
                ],
            )
        })
        .collect()
}

/// Issue #536 — per-row pending-delivery drill-down.
///
/// Reads from the `red_queue_meta` rows that the user-facing
/// `QUEUE READ` path writes (`kind = "queue_pending"` legacy and
/// `kind = "queue_pending_lc"` lifecycle). Cold scan, no caching:
/// every read walks the live meta collection.
///
/// Field mapping from the meta-row schema to the public columns:
/// - `consumer`            -> `locked_by`
/// - `delivery_count - 1`  -> `attempts` (delivery_count is incremented
///   to 1 on the first deliver, so attempts
///   starts at 0 and rises on NACK/redelivery)
/// - `delivered_at_ns + queue.lock_deadline_ms`
///   -> `lock_deadline` (the legacy plumbing does
///   not persist the deadline; derive it
///   from the queue descriptor's
///   `lock_deadline_ms`).
/// - opaque `delivery_id`  composed from `(queue, group, message_id,
///   delivery_count)`. The legacy plumbing has
///   no first-class delivery_id; this string
///   is stable for a given delivery instance
///   and changes when the message is
///   re-delivered (delivery_count bumps).
fn queue_pending_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    use crate::storage::query::DEFAULT_QUEUE_LOCK_DEADLINE_MS;

    let schema = Arc::new(
        QUEUE_PENDING_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let Some(manager) = store.get_collection("red_queue_meta") else {
        return Vec::new();
    };

    // Queue → lock_deadline_ms lookup from the catalog descriptor hot
    // fields. Falls back to the engine-wide default when unset.
    let snapshot = runtime.db().catalog_model_snapshot();
    let queue_lock_ms: HashMap<String, u64> = snapshot
        .collections
        .iter()
        .filter_map(|c| c.queue_lock_deadline_ms.map(|ms| (c.name.clone(), ms)))
        .collect();

    let mut records = Vec::new();
    let attempts_by_key: HashMap<(String, String, u64), u64> = manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "kind").as_deref() == Some("queue_attempts_lc"))
        })
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            Some((
                (
                    row_text(row, "queue")?,
                    row_text(row, "group")?,
                    row_u64(row, "message_id")?,
                ),
                row_u64(row, "attempts").unwrap_or(1),
            ))
        })
        .collect();
    let mut seen_pending: HashSet<(String, String, u64)> = HashSet::new();
    let entities = manager.query_all(|entity| {
        entity.data.as_row().is_some_and(|row| {
            matches!(
                row_text(row, "kind").as_deref(),
                Some("queue_pending") | Some("queue_pending_lc")
            )
        })
    });
    for entity in entities {
        let Some(row) = entity.data.as_row() else {
            continue;
        };
        let Some(queue) = row_text(row, "queue") else {
            continue;
        };
        if !collection_is_visible(&queue, visible_collections) {
            continue;
        }
        let Some(group) = row_text(row, "group") else {
            continue;
        };
        let Some(message_id) = row_u64(row, "message_id") else {
            continue;
        };
        if !seen_pending.insert((queue.clone(), group.clone(), message_id)) {
            continue;
        }
        let kind = row_text(row, "kind").unwrap_or_default();
        let consumer = row_text(row, "consumer").unwrap_or_default();

        let lock_ms = queue_lock_ms
            .get(&queue)
            .copied()
            .unwrap_or(DEFAULT_QUEUE_LOCK_DEADLINE_MS);
        let (lock_deadline_ms, delivery_count, delivery_id) = if kind == "queue_pending_lc" {
            let deadline_ns = row_u64(row, "lock_deadline_ns").unwrap_or(0);
            let delivery_count = attempts_by_key
                .get(&(queue.clone(), group.clone(), message_id))
                .copied()
                .unwrap_or(1);
            let delivery_id = row_text(row, "delivery_id").unwrap_or_default();
            (deadline_ns / 1_000_000, delivery_count, delivery_id)
        } else {
            let delivered_at_ns = row_u64(row, "delivered_at_ns").unwrap_or(0);
            let delivery_count = row_u64(row, "delivery_count").unwrap_or(1);
            let lock_deadline_ms = (delivered_at_ns / 1_000_000).saturating_add(lock_ms);
            let delivery_id = format!("{queue}:{group}:{message_id}:{delivery_count}");
            (lock_deadline_ms, delivery_count, delivery_id)
        };
        let attempts = delivery_count.saturating_sub(1);

        records.push(UnifiedRecord::with_schema(
            Arc::clone(&schema),
            vec![
                Value::text(queue),
                Value::text(group),
                Value::UnsignedInteger(message_id),
                Value::text(delivery_id),
                Value::UnsignedInteger(attempts),
                Value::TimestampMs(lock_deadline_ms as i64),
                Value::text(consumer),
            ],
        ));
    }
    records
}

/// Issue #535 — QueueLifecycle slice 8.
///
/// Per-queue introspection backing `red.queues` and the repointed
/// `SHOW QUEUES` desugar. Hot fields (`mode`, `depth`, `dlq_target`,
/// `attention`) come from the catalog descriptor — sub-ms reads, no
/// B-tree walk per row. `total_pending` and `oldest_pending_age` are
/// derived from a single pass over `red_queue_meta` queue_pending
/// rows so they cannot drift from the source of truth that
/// `red.queue_pending` and `queue_pending_gauge` already render
/// from.
fn queues_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    use crate::storage::query::DEFAULT_QUEUE_LOCK_DEADLINE_MS;

    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        QUEUE_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    let now_ms: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // Single pass: per-queue (count, oldest delivered_at_ns).
    let queue_lock_ms: HashMap<String, u64> = snapshot
        .collections
        .iter()
        .filter_map(|c| c.queue_lock_deadline_ms.map(|ms| (c.name.clone(), ms)))
        .collect();
    let mut per_queue: HashMap<String, (u64, u64)> = HashMap::new();
    let mut seen_pending: HashSet<(String, String, u64)> = HashSet::new();
    if let Some(manager) = store.get_collection("red_queue_meta") {
        let entities = manager.query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                matches!(
                    row_text(row, "kind").as_deref(),
                    Some("queue_pending") | Some("queue_pending_lc")
                )
            })
        });
        for entity in entities {
            let Some(row) = entity.data.as_row() else {
                continue;
            };
            let Some(queue) = row_text(row, "queue") else {
                continue;
            };
            let group = row_text(row, "group").unwrap_or_default();
            let message_id = row_u64(row, "message_id").unwrap_or(0);
            if !seen_pending.insert((queue.clone(), group, message_id)) {
                continue;
            }
            let delivered_at_ns = match row_text(row, "kind").as_deref() {
                Some("queue_pending_lc") => {
                    let lock_ms = queue_lock_ms
                        .get(&queue)
                        .copied()
                        .unwrap_or(DEFAULT_QUEUE_LOCK_DEADLINE_MS);
                    row_u64(row, "lock_deadline_ns")
                        .unwrap_or(0)
                        .saturating_sub(lock_ms.saturating_mul(1_000_000))
                }
                _ => row_u64(row, "delivered_at_ns").unwrap_or(0),
            };
            let entry = per_queue.entry(queue).or_insert((0, u64::MAX));
            entry.0 = entry.0.saturating_add(1);
            if delivered_at_ns > 0 && delivered_at_ns < entry.1 {
                entry.1 = delivered_at_ns;
            }
        }
    }

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Queue)
        .filter(|c| visible_collections.is_none_or(|visible| visible.contains(&c.name)))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let mode_value = collection
                .queue_mode
                .map(|m| Value::text(m.as_str().to_ascii_uppercase()))
                .unwrap_or_else(|| {
                    Value::text(
                        super::impl_queue::queue_mode_str(store.as_ref(), &collection.name)
                            .to_ascii_uppercase(),
                    )
                });
            let (total_pending, oldest_age_ms) = match per_queue.get(&collection.name) {
                Some(&(count, oldest_ns)) if count > 0 && oldest_ns != u64::MAX => {
                    let oldest_ms = oldest_ns / 1_000_000;
                    let age = now_ms.saturating_sub(oldest_ms);
                    (count, Some(age))
                }
                Some(&(count, _)) => (count, None),
                None => (0, None),
            };
            let oldest_value = oldest_age_ms
                .map(Value::UnsignedInteger)
                .unwrap_or(Value::Null);
            let dlq_value = collection
                .queue_dlq_target
                .clone()
                .map(Value::text)
                .unwrap_or(Value::Null);
            let internal = internal_registry.is_internal(&collection.name);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name.clone()),
                    mode_value,
                    Value::UnsignedInteger(collection.entities as u64),
                    Value::UnsignedInteger(total_pending),
                    oldest_value,
                    dlq_value,
                    Value::Boolean(collection.attention_required),
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}

fn row_u64(row: &crate::storage::unified::entity::RowData, field: &str) -> Option<u64> {
    match row.get_field(field)? {
        Value::UnsignedInteger(v) => Some(*v),
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        _ => None,
    }
}

fn value_as_ms(value: &crate::storage::schema::Value) -> Option<i64> {
    use crate::storage::schema::Value;
    match value {
        Value::TimestampMs(v) => Some(*v),
        Value::Timestamp(v) => Some(v.saturating_mul(1_000)),
        Value::BigInt(v) => Some(*v),
        Value::UnsignedInteger(v) => i64::try_from(*v).ok(),
        Value::Integer(v) => Some(*v),
        _ => None,
    }
}

fn distance_metric_name(metric: crate::storage::engine::distance::DistanceMetric) -> &'static str {
    match metric {
        crate::storage::engine::distance::DistanceMetric::L2 => "l2",
        crate::storage::engine::distance::DistanceMetric::Cosine => "cosine",
        crate::storage::engine::distance::DistanceMetric::InnerProduct => "inner_product",
    }
}

fn stats_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        STATS_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();

    snapshot
        .collections
        .into_iter()
        .filter(|collection| {
            visible_collections.is_none_or(|visible| visible.contains(&collection.name))
        })
        .map(|collection| {
            let manager_stats = store
                .get_collection(&collection.name)
                .map(|manager| manager.stats());
            let entities = manager_stats
                .as_ref()
                .map(|stats| stats.total_entities)
                .unwrap_or(collection.entities);
            let growing_count = manager_stats
                .as_ref()
                .map(|stats| stats.growing_count)
                .unwrap_or(0);
            let sealed_count = manager_stats
                .as_ref()
                .map(|stats| stats.sealed_count)
                .unwrap_or(0);
            let archived_count = manager_stats
                .as_ref()
                .map(|stats| stats.archived_count)
                .unwrap_or(0);
            let segments = manager_stats
                .as_ref()
                .map(|stats| stats.growing_count + stats.sealed_count + stats.archived_count)
                .unwrap_or(collection.segments);
            let seal_ops = manager_stats
                .as_ref()
                .map(|stats| stats.seal_ops)
                .unwrap_or(0);
            let compact_ops = manager_stats
                .as_ref()
                .map(|stats| stats.compact_ops)
                .unwrap_or(0);

            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    Value::UnsignedInteger(entities as u64),
                    Value::UnsignedInteger(segments as u64),
                    Value::UnsignedInteger(growing_count as u64),
                    Value::UnsignedInteger(sealed_count as u64),
                    Value::UnsignedInteger(archived_count as u64),
                    Value::UnsignedInteger(seal_ops),
                    Value::UnsignedInteger(compact_ops),
                    Value::Null,
                    Value::UnsignedInteger(collection.attention_score as u64),
                ],
            )
        })
        .collect()
}

struct InternalCollectionRegistry {
    dlqs: HashSet<String>,
}

impl InternalCollectionRegistry {
    fn from_store(store: &UnifiedStore) -> Self {
        Self {
            dlqs: discover_queue_dlqs(store),
        }
    }

    fn is_internal(&self, collection: &str) -> bool {
        collection.starts_with("red_")
            || collection.starts_with("red.")
            || collection == "audit_log"
            || collection == "__tenant_iso"
            || collection.starts_with("__tenant_")
            || collection.starts_with("__policy_")
            || self.dlqs.contains(collection)
    }
}

fn discover_queue_dlqs(store: &UnifiedStore) -> HashSet<String> {
    const QUEUE_META_COLLECTION: &str = "red_queue_meta";

    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return HashSet::new();
    };

    manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "kind").as_deref() == Some("queue_config"))
        })
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            row_text(row, "dlq")
        })
        .collect()
}

fn columns_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let db = runtime.db();
    let mut records = Vec::new();
    let schema = Arc::new(
        COLUMN_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let snapshot = db.catalog_model_snapshot();
    let contracts = db.collection_contracts();
    let contracts_by_name: HashMap<_, _> = contracts
        .iter()
        .map(|contract| (contract.name.as_str(), contract))
        .collect();

    for collection in snapshot.collections {
        if visible_collections.is_some_and(|visible| !visible.contains(&collection.name)) {
            continue;
        }
        let Some(contract) = contracts_by_name.get(collection.name.as_str()).copied() else {
            continue;
        };

        if !contract.declared_columns.is_empty() {
            records.extend(contract.declared_columns.iter().map(|column| {
                column_record(
                    Arc::clone(&schema),
                    &collection.name,
                    &column.name,
                    column
                        .sql_type
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| column.data_type.clone()),
                    !(column.not_null || column.primary_key),
                    column.default.as_deref(),
                    column.primary_key,
                    column.unique || column.primary_key,
                )
            }));
        } else if collection.model == CollectionModel::Document
            || contract.declared_model == CollectionModel::Document
        {
            records.extend(infer_document_columns(
                runtime,
                &collection.name,
                Arc::clone(&schema),
            ));
        }
    }

    records
}

fn column_record(
    schema: Arc<Vec<Arc<str>>>,
    collection: &str,
    name: &str,
    data_type: String,
    nullable: bool,
    default_value: Option<&str>,
    is_primary_key: bool,
    is_unique: bool,
) -> UnifiedRecord {
    UnifiedRecord::with_schema(
        schema,
        vec![
            Value::text(collection),
            Value::text(name),
            Value::text(data_type),
            Value::Boolean(nullable),
            default_value.map(Value::text).unwrap_or(Value::Null),
            Value::Boolean(is_primary_key),
            Value::Boolean(is_unique),
        ],
    )
}

#[derive(Debug, Clone)]
struct InferredColumn {
    data_type: Option<DataType>,
    seen: usize,
    saw_null: bool,
}

fn infer_document_columns(
    runtime: &RedDBRuntime,
    collection: &str,
    schema: Arc<Vec<Arc<str>>>,
) -> Vec<UnifiedRecord> {
    let mut fields: BTreeMap<String, InferredColumn> = BTreeMap::new();
    let mut document_count = 0usize;

    for (_, entity) in runtime
        .db()
        .store()
        .query_all(|entity| entity.kind.collection() == collection)
    {
        let EntityData::Row(row) = entity.data else {
            continue;
        };
        if !row
            .iter_fields()
            .any(|(name, value)| name == "body" && matches!(value, Value::Json(_) | Value::Text(_)))
        {
            continue;
        }

        document_count += 1;

        // Record every stored row field, plus the top-level fields derived
        // from the binary document body. Post-cutover (PRD-1398) documents
        // store the canonical document only inside the binary `body`
        // container and no longer promote top-level columns onto the row, so
        // schema inference must offset-read the body's top-level fields —
        // mirroring the GET presentation derive in `named_fields_json`.
        let mut recorded: Vec<(String, Value)> = row
            .iter_fields()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect();
        if let Some(Value::Json(bytes)) = row.get_field("body") {
            if let Some(body_fields) = crate::document_body::body_fields(bytes) {
                recorded.extend(body_fields);
            }
        }

        for (name, value) in recorded {
            let entry = fields.entry(name).or_insert(InferredColumn {
                data_type: None,
                seen: 0,
                saw_null: false,
            });
            entry.seen += 1;
            if value.is_null() {
                entry.saw_null = true;
                continue;
            }
            let value_type = value.data_type();
            entry.data_type = match entry.data_type {
                None => Some(value_type),
                Some(existing) if existing == value_type => Some(existing),
                Some(_) => Some(DataType::Unknown),
            };
        }
    }

    if document_count == 0 {
        return Vec::new();
    }

    fields
        .into_iter()
        .map(|(name, inferred)| {
            let data_type = inferred
                .data_type
                .filter(|data_type| *data_type != DataType::Unknown)
                .map(|data_type| data_type.to_string())
                .unwrap_or_else(|| "UNKNOWN".to_string());
            let nullable = inferred.saw_null || inferred.seen < document_count;
            column_record(
                Arc::clone(&schema),
                collection,
                &name,
                data_type,
                nullable,
                None,
                false,
                false,
            )
        })
        .collect()
}

fn row_text(row: &crate::storage::unified::entity::RowData, field: &str) -> Option<String> {
    match row.get_field(field)?.clone() {
        Value::Text(value) => Some(value.to_string()),
        Value::NodeRef(value) => Some(value),
        Value::EdgeRef(value) => Some(value),
        Value::TableRef(value) => Some(value),
        _ => None,
    }
}

fn collection_tenant(
    store: &crate::storage::unified::UnifiedStore,
    collection: &str,
) -> Option<String> {
    match store.get_config(&format!("red.collection_tenants.{collection}")) {
        Some(Value::Text(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn collection_model_name(model: CollectionModel) -> &'static str {
    match model {
        CollectionModel::Table => "table",
        CollectionModel::Document => "document",
        CollectionModel::Graph => "graph",
        CollectionModel::Vector => "vector",
        CollectionModel::Hll => "hll",
        CollectionModel::Sketch => "sketch",
        CollectionModel::Filter => "filter",
        CollectionModel::Kv => "kv",
        CollectionModel::Config => "config",
        CollectionModel::Vault => "vault",
        CollectionModel::Mixed => "mixed",
        CollectionModel::TimeSeries => "time_series",
        CollectionModel::Queue => "queue",
        CollectionModel::Metrics => "metrics",
    }
}

fn schema_mode_name(mode: SchemaMode) -> &'static str {
    match mode {
        SchemaMode::Strict => "strict",
        SchemaMode::SemiStructured => "semi_structured",
        SchemaMode::Dynamic => "dynamic",
    }
}

fn contains_case_insensitive_outside_quotes(haystack: &str, needle: &str) -> bool {
    find_case_insensitive_outside_quotes(haystack, needle).is_some()
}

fn matches_ignore_ascii_case(value: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn replace_case_insensitive_outside_quotes(
    haystack: &str,
    needle: &str,
    replacement: &str,
) -> Option<String> {
    let mut out = String::new();
    let mut rest = haystack;
    let mut changed = false;

    while let Some(idx) = find_case_insensitive_outside_quotes(rest, needle) {
        out.push_str(&rest[..idx]);
        out.push_str(replacement);
        rest = &rest[idx + needle.len()..];
        changed = true;
    }

    if changed {
        out.push_str(rest);
        Some(out)
    } else {
        None
    }
}

fn find_case_insensitive_outside_quotes(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let bytes = haystack.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => {
                if in_single && bytes.get(i + 1) == Some(&b'\'') {
                    i += 2;
                    continue;
                }
                in_single = !in_single;
                i += 1;
                continue;
            }
            b'"' if !in_single => {
                in_double = !in_double;
                i += 1;
                continue;
            }
            _ => {}
        }

        if !in_single
            && !in_double
            && i + needle_bytes.len() <= bytes.len()
            && bytes[i..i + needle_bytes.len()].eq_ignore_ascii_case(needle_bytes)
        {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_columns_includes_queue_mode() {
        assert!(COLLECTION_COLUMNS.contains(&"queue_mode"));
        // Two timeseries session columns were added by #576 slice 1.
        assert!(COLLECTION_COLUMNS.contains(&"session_key"));
        assert!(COLLECTION_COLUMNS.contains(&"session_gap_ms"));
        assert_eq!(COLLECTION_COLUMNS.len(), 15);
    }

    #[test]
    fn subscription_columns_match_status_contract() {
        assert_eq!(
            SUBSCRIPTION_COLUMNS,
            [
                "name",
                "collection",
                "target_queue",
                "mode",
                "ops_filter",
                "where_filter",
                "redact_fields",
                "enabled",
                "outbox_lag_ms",
                "dlq_count",
                "created_at",
            ]
        );
    }

    #[test]
    fn rewrite_skips_quoted_literals() {
        let rewritten =
            rewrite_virtual_names("SELECT 'red.collections' AS x FROM red.collections").unwrap();
        assert_eq!(
            rewritten,
            "SELECT 'red.collections' AS x FROM __red_schema_collections"
        );
    }

    #[test]
    fn rewrite_handles_multiple_virtual_tables() {
        let rewritten = rewrite_virtual_names(
            "SELECT * FROM red.indices WHERE collection IN (SELECT name FROM red.collections)",
        )
        .unwrap();
        assert_eq!(
            rewritten,
            "SELECT * FROM __red_schema_indices WHERE collection IN (SELECT name FROM __red_schema_collections)"
        );
    }

    // Issue #745 — typed `red.tables` / `red.documents` / `red.kv`
    // each rewrite to their own dedicated internal identifier so the
    // model-shaped projections do not collide with `red.collections`.
    #[test]
    fn rewrite_handles_typed_model_relations() {
        let tables = rewrite_virtual_names("SELECT * FROM red.tables").expect("tables");
        assert_eq!(tables, "SELECT * FROM __red_schema_tables");
        let documents = rewrite_virtual_names("SELECT * FROM red.documents").expect("documents");
        assert_eq!(documents, "SELECT * FROM __red_schema_documents");
        let kv = rewrite_virtual_names("SELECT * FROM red.kv").expect("kv");
        assert_eq!(kv, "SELECT * FROM __red_schema_kv");
    }

    // Issue #746 — typed `red.vectors` / `red.graphs` join the trio
    // from #745 with the same public→internal rewrite contract.
    #[test]
    fn rewrite_handles_typed_vector_and_graph_relations() {
        let vectors = rewrite_virtual_names("SELECT * FROM red.vectors").expect("vectors");
        assert_eq!(vectors, "SELECT * FROM __red_schema_vectors");
        let graphs = rewrite_virtual_names("SELECT * FROM red.graphs").expect("graphs");
        assert_eq!(graphs, "SELECT * FROM __red_schema_graphs");
    }

    #[test]
    fn rewrite_handles_red_subscriptions() {
        let rewritten = rewrite_virtual_names("SELECT * FROM red.subscriptions").unwrap();
        assert_eq!(rewritten, "SELECT * FROM __red_schema_subscriptions");
    }

    // Issue #709 — `red.policy.actions` is a new sibling of
    // `red.policies` and must rewrite to its own internal name even
    // though the public name shares the `red.po` prefix.
    #[test]
    fn rewrite_handles_red_policy_actions_and_red_policies_independently() {
        let actions =
            rewrite_virtual_names("SELECT * FROM red.policy.actions").expect("policy.actions");
        assert_eq!(actions, "SELECT * FROM __red_schema_policy_actions");
        let policies = rewrite_virtual_names("SELECT * FROM red.policies").expect("policies");
        assert_eq!(policies, "SELECT * FROM __red_schema_policies");
    }

    // Issue #709 — Active rows surface NULL for the deprecation
    // columns and Deprecated rows surface the replacement +
    // since_version pair. The snapshot encoder is the single source
    // of truth shared by the SQL virtual table and the HTTP
    // introspection surface; both contracts pivot on this shape.
    #[test]
    fn policy_actions_snapshot_encodes_lifecycle_columns() {
        use crate::storage::schema::Value;
        let rows = policy_actions_snapshot();
        assert_eq!(rows.len(), crate::auth::action_catalog::ACTIONS.len());

        let active = rows
            .iter()
            .find(|row| row.get("name") == Some(&Value::text("policy:put")))
            .expect("policy:put row");
        assert_eq!(active.get("category"), Some(&Value::text("policy")));
        assert_eq!(active.get("lifecycle_state"), Some(&Value::text("active")));
        assert_eq!(active.get("replacement"), Some(&Value::Null));
        assert_eq!(active.get("since_version"), Some(&Value::Null));
        assert!(matches!(
            active.get("gates_description"),
            Some(Value::Text(_))
        ));

        let deprecated = rows
            .iter()
            .find(|row| row.get("name") == Some(&Value::text("vault:unseal_history")))
            .expect("vault:unseal_history row");
        assert_eq!(
            deprecated.get("lifecycle_state"),
            Some(&Value::text("deprecated"))
        );
        assert_eq!(
            deprecated.get("replacement"),
            Some(&Value::text("vault:read_metadata"))
        );
        assert_eq!(deprecated.get("since_version"), Some(&Value::text("0.5.0")));
    }
}
