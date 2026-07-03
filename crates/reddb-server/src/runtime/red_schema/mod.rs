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
        VirtualTableKind::Collections => {
            catalog_views::collections_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::Columns => catalog_views::columns_snapshot(runtime, visible_collections),
        VirtualTableKind::Describe => {
            catalog_views::describe_snapshot(runtime, visible_collections, query)?
        }
        VirtualTableKind::ShowCreate => {
            catalog_views::show_create_snapshot(runtime, visible_collections, query)?
        }
        VirtualTableKind::ShowIndexes => {
            catalog_views::show_indexes_snapshot(runtime, visible_collections)
        }
        VirtualTableKind::Indices => catalog_views::indices_snapshot(runtime, visible_collections),
        VirtualTableKind::Policies => {
            governance_views::policies_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::Stats => catalog_views::stats_snapshot(runtime, visible_collections),
        VirtualTableKind::Subscriptions => {
            ops_views::subscriptions_snapshot(runtime, visible_collections)
        }
        VirtualTableKind::Retention => ops_views::retention_snapshot(runtime, visible_collections),
        VirtualTableKind::MaterializedViews => ops_views::materialized_views_snapshot(runtime),
        VirtualTableKind::QueuePending => {
            ops_views::queue_pending_snapshot(runtime, visible_collections)
        }
        VirtualTableKind::Queues => {
            ops_views::queues_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::AnalyticsMetrics => ops_views::analytics_metrics_snapshot(runtime),
        VirtualTableKind::AnalyticsSlos => ops_views::analytics_slos_snapshot(runtime),
        VirtualTableKind::AnalyticsSources => {
            ops_views::analytics_sources_snapshot(runtime, visible_collections)
        }
        VirtualTableKind::SchemaRegistry => ops_views::schema_registry_snapshot(runtime),
        VirtualTableKind::GovernanceRegistry => {
            governance_views::governance_registry_snapshot(runtime)
        }
        VirtualTableKind::GovernanceRegistryHistory => {
            governance_views::governance_registry_history_snapshot(runtime)
        }
        VirtualTableKind::ManagedPolicies => governance_views::managed_policies_snapshot(runtime),
        VirtualTableKind::ControlEvents => {
            governance_views::control_events_snapshot(runtime, tenant)
        }
        VirtualTableKind::Users => governance_views::users_snapshot(runtime, tenant),
        VirtualTableKind::ApiKeys => governance_views::api_keys_snapshot(runtime, tenant),
        VirtualTableKind::ControlCapabilities => governance_views::control_capabilities_snapshot(),
        VirtualTableKind::PolicyActions => governance_views::policy_actions_snapshot(),
        VirtualTableKind::Tables => {
            model_views::tables_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::Documents => {
            model_views::documents_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::Kv => model_views::kv_snapshot(runtime, tenant, visible_collections),
        VirtualTableKind::Vectors => {
            model_views::vectors_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::Graphs => {
            model_views::graphs_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::Timeseries => {
            model_views::timeseries_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::Metrics => {
            model_views::metrics_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::HypertableChunks => {
            model_views::hypertable_chunks_snapshot(runtime, tenant, visible_collections)
        }
        VirtualTableKind::TimeseriesWrites => {
            model_views::timeseries_writes_snapshot(runtime, tenant, visible_collections, query)
        }
        VirtualTableKind::Commits => vcs_views::commits_snapshot(runtime, query)?,
        VirtualTableKind::Branches => vcs_views::refs_snapshot(runtime, Some("refs/heads/"))?,
        VirtualTableKind::Tags => vcs_views::refs_snapshot(runtime, Some("refs/tags/"))?,
        VirtualTableKind::Status => vcs_views::status_snapshot(runtime)?,
        VirtualTableKind::Conflicts => vcs_views::conflicts_snapshot(runtime)?,
        VirtualTableKind::Versioned => vcs_views::versioned_snapshot(runtime, visible_collections)?,
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

mod catalog_views;
mod governance_views;
mod helpers;
mod model_views;
mod ops_views;
mod vcs_views;

use helpers::*;

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
        let rows = super::governance_views::policy_actions_snapshot();
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
