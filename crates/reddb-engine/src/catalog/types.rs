//! Canonical collection contracts shared by native and browser runtimes.
/// Per-collection analytical-storage seam (PRD #850, Phase 1).
///
/// Declares whether a `Metrics`/`TimeSeries` collection's hypertable chunks
/// are backed by the **columnar** sealed-chunk layout
/// (the native column-block layout). In-scope
/// collections get `columnar = true` automatically; `columnar = false` (or
/// the whole config absent on older contracts) keeps the row engine.
///
/// `time_key` / `order_by_key` mirror the ClickHouse `ORDER BY` intent so
/// downstream slices (#854 granules, #856 vectorized read) can lay the
/// columns out in their natural sort order; v1 records them as the durable
/// contract without yet acting on `order_by_key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticalStorageConfig {
    /// When true, sealing a chunk routes to the columnar `ColumnBlock`
    /// writer; otherwise the row engine stays the default.
    pub columnar: bool,
    /// Column carrying the time axis (the chunk's partition key).
    pub time_key: String,
    /// Optional secondary sort key (ClickHouse `ORDER BY` tail). `None`
    /// orders by `time_key` alone.
    pub order_by_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaMode {
    Strict,
    SemiStructured,
    Dynamic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractOrigin {
    Explicit,
    Implicit,
    Migrated,
}

impl ContractOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Implicit => "implicit",
            Self::Migrated => "migrated",
        }
    }
}

/// A decoded contract is authoritative; it cannot be repaired from row contents.
#[derive(Debug)]
pub struct CollectionContractDecodeError(pub String);

impl std::fmt::Display for CollectionContractDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid collection contract: {}", self.0)
    }
}

impl std::error::Error for CollectionContractDecodeError {}

pub fn is_collection_contract_decode_error(error: &std::io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<CollectionContractDecodeError>())
}

#[derive(Debug, Clone)]
pub struct DeclaredColumnContract {
    pub name: String,
    pub data_type: String,
    pub sql_type: Option<reddb_types::SqlTypeName>,
    pub not_null: bool,
    pub default: Option<String>,
    /// Persisted, deterministic expression for a stored generated field.
    pub generated: Option<reddb_rql::schema_expression::SchemaExpression>,
    /// SQL CHECK: FALSE rejects, TRUE or NULL accepts.
    pub check: Option<reddb_rql::schema_expression::SchemaExpression>,
    pub compress: Option<u8>,
    pub unique: bool,
    pub primary_key: bool,
    pub enum_variants: Vec<String>,
    pub array_element: Option<String>,
    pub decimal_precision: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct CollectionContract {
    pub name: String,
    pub declared_model: crate::catalog::CollectionModel,
    pub schema_mode: crate::catalog::SchemaMode,
    pub origin: ContractOrigin,
    pub version: u32,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub default_ttl_ms: Option<u64>,
    pub vector_dimension: Option<usize>,
    pub vector_metric: Option<reddb_types::distance::DistanceMetric>,
    pub context_index_fields: Vec<String>,
    pub declared_columns: Vec<DeclaredColumnContract>,
    pub table_def: Option<reddb_types::TableDef>,
    /// Enabled by `CREATE TABLE ... WITH timestamps = true`. When true,
    /// the runtime auto-populates two user-visible columns
    /// `created_at` + `updated_at` (BIGINT unix-ms) sourced from the
    /// `UnifiedEntity::created_at/updated_at` fields. `created_at` is
    /// immutable after insert; `updated_at` is bumped on every mutation.
    pub timestamps_enabled: bool,
    /// Enabled by `CREATE TABLE ... WITH context_index = true` (or by
    /// naming specific `context_index_fields`). When true, every INSERT
    /// tokenises the row's text fields and populates the global context
    /// index that backs `SEARCH CONTEXT` / `SEARCH SIMILAR TEXT` / `ASK`
    /// (RAG). When false (default), inserts skip the tokenisation +
    /// 3-way RwLock write storm entirely — ~800 ns faster per insert,
    /// and SEARCH returns empty for this collection.
    ///
    /// Opt-in by design: pure OLTP tables (accounts, orders, events)
    /// pay zero indexing tax; search-oriented tables (articles, docs)
    /// flip the switch at CREATE time.
    pub context_index_enabled: bool,
    /// Metrics collections are backed by time-series storage but carry a
    /// metrics-specific raw sample retention contract.
    pub metrics_raw_retention_ms: Option<u64>,
    /// Metrics rollup tiers declared by `CREATE METRICS ... DOWNSAMPLE`.
    pub metrics_rollup_policies: Vec<String>,
    /// Metrics tenant identity source. Defaults to current tenant context and
    /// can be declared as a stable identity path for future ingestion slices.
    pub metrics_tenant_identity: Option<String>,
    /// Metrics namespace identity. v0 starts with a default namespace so
    /// series identity is namespace-aware before Prometheus ingestion exists.
    pub metrics_namespace: Option<String>,
    /// Enabled by `CREATE TABLE ... APPEND ONLY` or `WITH
    /// (append_only = true)`. When true, the runtime rejects
    /// `UPDATE` and `DELETE` against this collection at parse time
    /// with a clear error — the operator's immutability intent
    /// becomes a first-class catalog fact rather than an RLS-shaped
    /// approximation. Default `false` so legacy DDL keeps its
    /// mutable semantics.
    pub append_only: bool,
    /// Declarative subscriptions created by `WITH EVENTS`. This is
    /// metadata only in #291; event emission is wired by the outbox slice.
    pub subscriptions: Vec<crate::catalog::SubscriptionDescriptor>,
    /// Analytics views declared by `CREATE GRAPH ... WITH ANALYTICS (...)`.
    /// Persisted as part of the contract so each enabled `<graph>.<output>`
    /// virtual view survives restarts and crash recovery (issue #800).
    pub analytics_config: Vec<crate::catalog::AnalyticsViewDescriptor>,
    /// `CREATE TIMESERIES ... WITH SESSION_KEY <col>` — the column the
    /// `SESSIONIZE` operator partitions by when no key is supplied at
    /// query-time. `None` for non-timeseries collections and for
    /// timeseries created without the clause. Issue #576 slice 1.
    pub session_key: Option<String>,
    /// `CREATE TIMESERIES ... SESSION_GAP <duration>` — the default
    /// inactivity gap (milliseconds) the `SESSIONIZE` operator uses to
    /// close a session when no gap is supplied at query-time. `None`
    /// for non-timeseries collections and for timeseries created
    /// without the clause. Issue #576 slice 1.
    pub session_gap_ms: Option<u64>,
    /// `ALTER COLLECTION ... SET RETENTION <duration>` — declarative
    /// retention policy in milliseconds. `None` means retention is
    /// not enforced. Reads filter out rows older than `now -
    /// retention_duration_ms` by the collection's timestamp column.
    /// Issue #580 — DeclarativeRetention slice 1.
    pub retention_duration_ms: Option<u64>,
    /// Analytical-storage seam (PRD #850, Phase 1). When present and
    /// `columnar = true`, sealing this collection's hypertable chunks
    /// routes to the columnar `ColumnBlock` writer; `None` (the default)
    /// keeps the row engine. Decodes to `None` on sidecars written before
    /// the feature.
    pub analytical_storage: Option<crate::catalog::AnalyticalStorageConfig>,
    /// Per-collection AI policy declared by `WITH (EMBED|MODERATE|VISION
    /// (...))` (PRD #1267, issue #1271). `None` when no AI clause is
    /// present, and decodes to `None` on sidecars written before the
    /// feature (versioned/migrated with the schema). Validated against
    /// the provider capability matrix (#1269) at DDL execution time.
    pub ai_policy: Option<crate::catalog::AiPolicy>,
}
