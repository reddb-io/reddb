//! Embedded runtime with connection pooling, scans and health.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{RedDBError, RedDBOptions, RedDBResult};
use crate::catalog::{
    CatalogAnalyticsJobStatus, CatalogAttentionSummary, CatalogGraphProjectionStatus,
    CatalogIndexStatus, CatalogModelSnapshot, CollectionDescriptor,
};
use crate::health::{HealthProvider, HealthReport};
use crate::index::IndexCatalog;
use crate::physical::{
    ExportDescriptor, ManifestEvent, PhysicalAnalyticsJob, PhysicalGraphProjection, PhysicalLayout,
    SnapshotDescriptor,
};
use crate::serde_json::Value as JsonValue;
use crate::storage::engine::pathfinding::{AStar, BellmanFord, Dijkstra, BFS, DFS};
use crate::storage::engine::{
    BetweennessCentrality, ClosenessCentrality, ClusteringCoefficient, ConnectedComponents,
    CycleDetector, DegreeCentrality, EigenvectorCentrality, GraphEdgeType, GraphNodeType,
    GraphStore, IvfConfig, IvfIndex, IvfStats, LabelPropagation, Louvain, MetadataEntry,
    MetadataFilter as VectorMetadataFilter, MetadataValue as VectorMetadataValue, PageRank,
    PersonalizedPageRank, PhysicalFileHeader, StoredNode, StronglyConnectedComponents,
    WeaklyConnectedComponents, HITS,
};
use crate::storage::query::ast::{
    AlterOperation, AlterTableQuery, CompareOp, CreateIndexQuery, CreateQueueQuery,
    CreateTableQuery, CreateTimeSeriesQuery, CreateTreeQuery, DeleteQuery, DropIndexQuery,
    DropQueueQuery, DropTableQuery, DropTimeSeriesQuery, DropTreeQuery, ExplainAlterQuery,
    ExplainFormat, FieldRef, Filter, FusionStrategy, GraphCommand, HybridQuery, IndexMethod,
    InsertEntityType, InsertQuery, JoinQuery, JoinType, OrderByClause, ProbabilisticCommand,
    Projection, QueryExpr, QueueCommand, QueueSide, SearchCommand, TableQuery, TreeCommand,
    UpdateQuery, VectorQuery, VectorSource,
};
use crate::storage::query::is_universal_entity_source as is_universal_query_source;
use crate::storage::query::modes::{detect_mode, parse_multi, QueryMode};
use crate::storage::query::planner::{
    CanonicalLogicalPlan, CanonicalPlanner, CostEstimator, QueryPlanner,
};
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::storage::schema::Value;
use crate::storage::unified::dsl::{
    apply_filters, cosine_similarity, Filter as DslFilter, FilterOp as DslFilterOp,
    FilterValue as DslFilterValue, GraphPatternDsl, HybridQueryBuilder, MatchComponents,
    QueryResult as DslQueryResult, ScoredMatch, TextSearchBuilder,
};
use crate::storage::unified::store::{
    NativeCatalogSummary, NativeManifestSummary, NativePhysicalState, NativeRecoverySummary,
    NativeRegistrySummary,
};
use crate::storage::unified::{
    Metadata, MetadataValue as UnifiedMetadataValue, RefTarget, UnifiedMetadataFilter,
};
use crate::storage::{
    EntityData, EntityId, EntityKind, RedDB, RefType, SimilarResult, StoreStats, UnifiedEntity,
    UnifiedStore,
};

#[derive(Debug, Clone)]
pub struct ConnectionPoolConfig {
    pub max_connections: usize,
    pub max_idle: usize,
}

impl Default for ConnectionPoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 64,
            max_idle: 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanCursor {
    pub offset: usize,
}

#[derive(Debug, Clone)]
pub struct ScanPage {
    pub collection: String,
    pub items: Vec<UnifiedEntity>,
    pub next: Option<ScanCursor>,
    pub total: usize,
}

#[derive(Debug, Clone)]
pub struct SystemInfo {
    pub pid: u32,
    pub cpu_cores: usize,
    pub total_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub os: String,
    pub arch: String,
    pub hostname: String,
}

impl SystemInfo {
    /// Whether the system has enough cores to benefit from parallelism.
    /// Returns false on single-core machines where thread overhead > gains.
    pub fn should_parallelize() -> bool {
        std::thread::available_parallelism()
            .map(|p| p.get() > 1)
            .unwrap_or(false)
    }

    pub fn collect() -> Self {
        Self {
            pid: std::process::id(),
            cpu_cores: std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(1),
            total_memory_bytes: Self::read_total_memory(),
            available_memory_bytes: Self::read_available_memory(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            hostname: std::env::var("HOSTNAME")
                .or_else(|_| std::env::var("COMPUTERNAME"))
                .unwrap_or_else(|_| "unknown".to_string()),
        }
    }

    #[cfg(target_os = "linux")]
    fn read_total_memory() -> u64 {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .and_then(|l| {
                        l.split_whitespace()
                            .nth(1)
                            .and_then(|v| v.parse::<u64>().ok())
                    })
                    .map(|kb| kb * 1024)
            })
            .unwrap_or(0)
    }

    #[cfg(target_os = "linux")]
    fn read_available_memory() -> u64 {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemAvailable:"))
                    .and_then(|l| {
                        l.split_whitespace()
                            .nth(1)
                            .and_then(|v| v.parse::<u64>().ok())
                    })
                    .map(|kb| kb * 1024)
            })
            .unwrap_or(0)
    }

    #[cfg(not(target_os = "linux"))]
    fn read_total_memory() -> u64 {
        0
    }

    #[cfg(not(target_os = "linux"))]
    fn read_available_memory() -> u64 {
        0
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeStats {
    pub active_connections: usize,
    pub idle_connections: usize,
    pub total_checkouts: u64,
    pub paged_mode: bool,
    pub started_at_unix_ms: u128,
    pub store: StoreStats,
    pub system: SystemInfo,
}

#[derive(Debug, Clone)]
pub struct RuntimeQueryResult {
    pub query: String,
    pub mode: QueryMode,
    pub statement: &'static str,
    pub engine: &'static str,
    pub result: UnifiedResult,
    pub affected_rows: u64,
    /// High-level statement type: "select", "insert", "update", "delete", "create", "drop", "alter"
    pub statement_type: &'static str,
}

impl RuntimeQueryResult {
    /// Construct a result representing a DML operation (insert/update/delete).
    pub fn dml_result(
        query: String,
        affected: u64,
        statement_type: &'static str,
        engine: &'static str,
    ) -> Self {
        Self {
            query,
            mode: QueryMode::Sql,
            statement: statement_type,
            engine,
            result: UnifiedResult::empty(),
            affected_rows: affected,
            statement_type,
        }
    }

    /// Construct a result representing a DDL message (create/drop/alter).
    pub fn ok_message(query: String, message: &str, statement_type: &'static str) -> Self {
        let mut result = UnifiedResult::empty();
        let mut record = UnifiedRecord::new();
        record.set("message", Value::text(message.to_string()));
        result.push(record);
        result.columns = vec!["message".to_string()];

        Self {
            query,
            mode: QueryMode::Sql,
            statement: statement_type,
            engine: "runtime-ddl",
            result,
            affected_rows: 0,
            statement_type,
        }
    }

    /// Construct a multi-column record result for read-only meta commands
    /// (EXPLAIN ALTER, schema introspection, etc.). Each row is a Vec of
    /// (column_name, value) pairs in column order.
    pub fn ok_records(
        query: String,
        columns: Vec<String>,
        rows: Vec<Vec<(String, Value)>>,
        statement_type: &'static str,
    ) -> Self {
        let mut result = UnifiedResult::empty();
        for row in rows {
            let mut record = UnifiedRecord::new();
            for (k, v) in row {
                record.set(&k, v);
            }
            result.push(record);
        }
        result.columns = columns;

        Self {
            query,
            mode: QueryMode::Sql,
            statement: statement_type,
            engine: "runtime-meta",
            result,
            affected_rows: 0,
            statement_type,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeQueryExplain {
    pub query: String,
    pub mode: QueryMode,
    pub statement: &'static str,
    pub is_universal: bool,
    pub plan_cost: crate::storage::query::planner::PlanCost,
    pub estimated_rows: f64,
    pub estimated_selectivity: f64,
    pub estimated_confidence: f64,
    pub passes_applied: Vec<String>,
    pub logical_plan: CanonicalLogicalPlan,
}

#[derive(Debug, Clone)]
pub struct RuntimeIvfMatch {
    pub entity_id: u64,
    pub distance: f32,
    pub entity: Option<UnifiedEntity>,
}

#[derive(Debug, Clone)]
pub struct RuntimeIvfSearchResult {
    pub collection: String,
    pub k: usize,
    pub n_lists: usize,
    pub n_probes: usize,
    pub stats: IvfStats,
    pub matches: Vec<RuntimeIvfMatch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeGraphDirection {
    Outgoing,
    Incoming,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeGraphTraversalStrategy {
    Bfs,
    Dfs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeGraphPathAlgorithm {
    Bfs,
    Dijkstra,
    AStar,
    BellmanFord,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphNode {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub out_edge_count: u32,
    pub in_edge_count: u32,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphEdge {
    pub source: String,
    pub target: String,
    pub edge_type: String,
    pub weight: f32,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphVisit {
    pub depth: usize,
    pub node: RuntimeGraphNode,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphNeighborhoodResult {
    pub source: String,
    pub direction: RuntimeGraphDirection,
    pub max_depth: usize,
    pub nodes: Vec<RuntimeGraphVisit>,
    pub edges: Vec<RuntimeGraphEdge>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphTraversalResult {
    pub source: String,
    pub direction: RuntimeGraphDirection,
    pub strategy: RuntimeGraphTraversalStrategy,
    pub max_depth: usize,
    pub visits: Vec<RuntimeGraphVisit>,
    pub edges: Vec<RuntimeGraphEdge>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphPath {
    pub hop_count: usize,
    pub total_weight: f64,
    pub nodes: Vec<RuntimeGraphNode>,
    pub edges: Vec<RuntimeGraphEdge>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphPathResult {
    pub source: String,
    pub target: String,
    pub direction: RuntimeGraphDirection,
    pub algorithm: RuntimeGraphPathAlgorithm,
    pub nodes_visited: usize,
    pub negative_cycle_detected: Option<bool>,
    pub path: Option<RuntimeGraphPath>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeGraphComponentsMode {
    Connected,
    Weak,
    Strong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeGraphCentralityAlgorithm {
    Degree,
    Closeness,
    Betweenness,
    Eigenvector,
    PageRank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeGraphCommunityAlgorithm {
    LabelPropagation,
    Louvain,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphComponent {
    pub id: String,
    pub size: usize,
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphComponentsResult {
    pub mode: RuntimeGraphComponentsMode,
    pub count: usize,
    pub components: Vec<RuntimeGraphComponent>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphCentralityScore {
    pub node: RuntimeGraphNode,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphDegreeScore {
    pub node: RuntimeGraphNode,
    pub in_degree: usize,
    pub out_degree: usize,
    pub total_degree: usize,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphCentralityResult {
    pub algorithm: RuntimeGraphCentralityAlgorithm,
    pub normalized: Option<bool>,
    pub iterations: Option<usize>,
    pub converged: Option<bool>,
    pub scores: Vec<RuntimeGraphCentralityScore>,
    pub degree_scores: Vec<RuntimeGraphDegreeScore>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphCommunity {
    pub id: String,
    pub size: usize,
    pub nodes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphCommunityResult {
    pub algorithm: RuntimeGraphCommunityAlgorithm,
    pub count: usize,
    pub iterations: Option<usize>,
    pub converged: Option<bool>,
    pub modularity: Option<f64>,
    pub passes: Option<usize>,
    pub communities: Vec<RuntimeGraphCommunity>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphClusteringResult {
    pub global: f64,
    pub local: Vec<RuntimeGraphCentralityScore>,
    pub triangle_count: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphHitsResult {
    pub iterations: usize,
    pub converged: bool,
    pub hubs: Vec<RuntimeGraphCentralityScore>,
    pub authorities: Vec<RuntimeGraphCentralityScore>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphCyclesResult {
    pub limit_reached: bool,
    pub cycles: Vec<RuntimeGraphPath>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphTopologicalSortResult {
    pub acyclic: bool,
    pub ordered_nodes: Vec<RuntimeGraphNode>,
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphPropertiesResult {
    pub node_count: usize,
    pub edge_count: usize,
    pub self_loop_count: usize,
    pub negative_edge_count: usize,
    pub connected_component_count: usize,
    pub weak_component_count: usize,
    pub strong_component_count: usize,
    pub is_empty: bool,
    pub is_connected: bool,
    pub is_weakly_connected: bool,
    pub is_strongly_connected: bool,
    pub is_complete: bool,
    pub is_complete_directed: bool,
    pub is_cyclic: bool,
    pub is_circular: bool,
    pub is_acyclic: bool,
    pub is_tree: bool,
    pub density: f64,
    pub density_directed: f64,
}

// ============================================================================
// Context Search types
// ============================================================================

#[derive(Debug, Clone)]
pub struct ContextSearchResult {
    pub query: String,
    pub tables: Vec<ContextEntity>,
    pub graph: ContextGraphResult,
    pub vectors: Vec<ContextEntity>,
    pub documents: Vec<ContextEntity>,
    pub key_values: Vec<ContextEntity>,
    pub connections: Vec<ContextConnection>,
    pub summary: ContextSummary,
}

#[derive(Debug, Clone)]
pub struct ContextEntity {
    pub entity: UnifiedEntity,
    pub score: f32,
    pub discovery: DiscoveryMethod,
    pub collection: String,
}

#[derive(Debug, Clone)]
pub enum DiscoveryMethod {
    Indexed {
        field: String,
    },
    GlobalScan,
    CrossReference {
        source_id: u64,
        ref_type: String,
    },
    GraphTraversal {
        source_id: u64,
        edge_type: String,
        depth: usize,
    },
    VectorQuery {
        similarity: f32,
    },
}

#[derive(Debug, Clone)]
pub struct ContextGraphResult {
    pub nodes: Vec<ContextEntity>,
    pub edges: Vec<ContextEntity>,
}

#[derive(Debug, Clone)]
pub struct ContextConnection {
    pub from_id: u64,
    pub to_id: u64,
    pub connection_type: ContextConnectionType,
    pub weight: f32,
}

#[derive(Debug, Clone)]
pub enum ContextConnectionType {
    CrossRef(String),
    GraphEdge(String),
    VectorSimilarity(f32),
}

#[derive(Debug, Clone)]
pub struct ContextSummary {
    pub total_entities: usize,
    pub direct_matches: usize,
    pub expanded_via_graph: usize,
    pub expanded_via_cross_refs: usize,
    pub expanded_via_vector_query: usize,
    pub collections_searched: usize,
    pub execution_time_us: u64,
    pub tiers_used: Vec<String>,
    pub entities_reindexed: usize,
}

struct PoolState {
    next_id: u64,
    active: usize,
    idle: Vec<u64>,
    total_checkouts: u64,
}

impl Default for PoolState {
    fn default() -> Self {
        Self {
            next_id: 1,
            active: 0,
            idle: Vec::new(),
            total_checkouts: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct RuntimeResultCacheEntry {
    result: RuntimeQueryResult,
    cached_at: std::time::Instant,
    scopes: HashSet<String>,
}

struct RuntimeInner {
    db: Arc<RedDB>,
    layout: PhysicalLayout,
    indices: IndexCatalog,
    pool_config: ConnectionPoolConfig,
    pool: Mutex<PoolState>,
    started_at_unix_ms: u128,
    probabilistic: probabilistic_store::ProbabilisticStore,
    index_store: index_store::IndexStore,
    cdc: crate::replication::cdc::CdcBuffer,
    backup_scheduler: crate::replication::scheduler::BackupScheduler,
    query_cache: parking_lot::RwLock<crate::storage::query::planner::cache::PlanCache>,
    result_cache: parking_lot::RwLock<(
        HashMap<String, RuntimeResultCacheEntry>,
        std::collections::VecDeque<String>,
    )>,
    /// Process-local queue message locks used to emulate `SKIP LOCKED`-style
    /// claim semantics for concurrent queue consumers inside this runtime.
    queue_message_locks: parking_lot::RwLock<HashMap<String, Arc<parking_lot::Mutex<()>>>>,
    planner_dirty_tables: parking_lot::RwLock<HashSet<String>>,
    ec_registry: Arc<crate::ec::config::EcRegistry>,
    ec_worker: crate::ec::worker::EcWorker,
    /// Optional AuthStore — injected by server boot when auth is
    /// enabled. Required for `Value::Secret` auto-encrypt/decrypt
    /// because the AES key lives in the vault KV under the
    /// `red.secret.aes_key` entry.
    auth_store: parking_lot::RwLock<Option<Arc<crate::auth::store::AuthStore>>>,
    /// Global serialization point for transactional commits initiated via
    /// the stdio JSON-RPC `tx.commit` path. Held only for the duration of
    /// the write-set replay so concurrent auto-committed writes can still
    /// make progress between commits. Remote/gRPC commits use a separate
    /// server-side serialization mechanism and do not touch this lock.
    commit_lock: Mutex<()>,
    /// View registry (Phase 2.1 PG parity).
    ///
    /// Holds the parsed `SELECT` body for every view created via
    /// `CREATE [MATERIALIZED] VIEW`. Queries that reference a view name
    /// substitute the stored `QueryExpr` at execution time. Materialized
    /// views additionally back onto the shared `MaterializedViewCache`
    /// (see `RuntimeInner::materialized_views`).
    ///
    /// This is in-memory only in Phase 2.1 — view definitions do not
    /// survive a restart. Persistence is a Phase 3 follow-up.
    views: parking_lot::RwLock<HashMap<String, Arc<crate::storage::query::ast::CreateViewQuery>>>,
    materialized_views: parking_lot::RwLock<crate::storage::cache::result::MaterializedViewCache>,
    /// MVCC snapshot manager (Phase 2.3 PG parity).
    ///
    /// Allocates monotonic `xid`s on BEGIN and tracks the active/aborted
    /// sets used by `Snapshot::sees` to filter tuples by visibility. Each
    /// query evaluates `entity.is_visible(snapshot.xid)` — pre-MVCC rows
    /// (`xmin == 0`) stay visible to every snapshot, preserving backward
    /// compatibility with data written before the xid fields existed.
    snapshot_manager: Arc<crate::storage::transaction::snapshot::SnapshotManager>,
    /// Connection → active transaction context map (Phase 2.3 PG parity).
    ///
    /// Keyed by connection id from `RuntimeConnection`. Populated on BEGIN,
    /// cleared on COMMIT/ROLLBACK. When a statement executes outside a
    /// transaction (autocommit path) no entry exists and writes stamp
    /// `xid=0` — identical to pre-MVCC behaviour.
    tx_contexts:
        parking_lot::RwLock<HashMap<u64, crate::storage::transaction::snapshot::TxnContext>>,
    /// Intent-lock hierarchy (IS/IX/S/X) used to break the implicit
    /// global-write serialisation in write paths. Populated at boot
    /// with `concurrency.locking.deadlock_timeout_ms` from the matrix
    /// and wired through DML/DDL dispatch in later P1 tasks.
    /// Dormant until P1.T3 flips the read path to `(Global,IS) →
    /// (Collection,IS)` and P1.T4/T5 pick up writes/DDL.
    lock_manager: Arc<crate::storage::transaction::lock::LockManager>,
    /// Perf-parity env-var overrides (`REDDB_<UP_DOTTED_KEY>`).
    /// Populated once at boot, read by every config getter; takes
    /// precedence over the persisted red_config value so operators
    /// can hot-fix a bad config by restarting with a different env
    /// var set. Keys are restricted to those declared in the matrix.
    env_config_overrides: HashMap<String, String>,
    /// Transaction-local tenant override (`SET LOCAL TENANT '<id>'`).
    /// Keyed by connection id, mirroring `tx_contexts`. Lives only while
    /// a transaction is open — `COMMIT` / `ROLLBACK` evict the entry,
    /// returning the connection to whichever session-level tenant
    /// (`SET TENANT 'x'`) was active before the transaction began.
    /// Wins over the session value but loses to a per-statement
    /// `WITHIN TENANT '<id>' …` override on the same call.
    tx_local_tenants: parking_lot::RwLock<HashMap<u64, Option<String>>>,
    /// Row-level security policies (Phase 2.5 PG parity).
    ///
    /// Keyed by `(table_name, policy_name)`; the set of tables with RLS
    /// enforcement toggled on lives in `rls_enabled_tables`. Filter
    /// enforcement hooks into the read path via `collect_rls_filters()`
    /// — see `runtime::impl_core`.
    rls_policies: parking_lot::RwLock<
        HashMap<(String, String), Arc<crate::storage::query::ast::CreatePolicyQuery>>,
    >,
    rls_enabled_tables: parking_lot::RwLock<HashSet<String>>,
    /// Foreign Data Wrapper registry (Phase 3.2 PG parity).
    ///
    /// Maps server names → wrapper instances and foreign-table names →
    /// definitions. Queries referencing a registered foreign table are
    /// re-routed to `ForeignTableRegistry::scan` by the read-path
    /// rewriter; reads against unknown names fall through to the native
    /// collection lookup.
    foreign_tables: Arc<crate::storage::fdw::ForeignTableRegistry>,
    /// Per-connection list of tuples marked for deletion by the current
    /// transaction (Phase 2.3.2b MVCC tombstones + 2.3.2e savepoints).
    /// Each entry is `(collection, entity_id, stamper_xid)` — the xid
    /// that stamped xmax on the tuple. For a plain transaction the
    /// stamper equals `ctx.xid`; with savepoints the stamper equals
    /// the innermost open sub-xid so ROLLBACK TO SAVEPOINT can revive
    /// only the matching subset. COMMIT drains the whole conn list
    /// and physical-deletes; ROLLBACK (whole-tx) revives them all;
    /// ROLLBACK TO SAVEPOINT revives those with `stamper_xid >=
    /// savepoint_xid`. Autocommit DELETE bypasses this map.
    pending_tombstones: parking_lot::RwLock<
        HashMap<
            u64,
            Vec<(
                String,
                crate::storage::unified::entity::EntityId,
                crate::storage::transaction::snapshot::Xid,
            )>,
        >,
    >,
    /// Table-scoped tenancy registry (Phase 2.5.4).
    ///
    /// Maps `table_name → tenant_column`. DML auto-fill looks here to
    /// inject `CURRENT_TENANT()` on INSERTs that omit the column, and
    /// DDL keeps the in-memory registry in sync with the
    /// `tenant_tables.*` keys in red_config. Read-side enforcement
    /// piggy-backs on the existing RLS infrastructure: every entry
    /// installs an implicit `col = CURRENT_TENANT()` policy.
    tenant_tables: parking_lot::RwLock<HashMap<String, String>>,
    /// Monotonic epoch bumped on every DDL / schema-mutating operation
    /// that calls `invalidate_plan_cache`. Prepared statements capture
    /// this at PREPARE and re-check at EXECUTE — a mismatch means the
    /// cached shape may reference dropped or renamed columns and the
    /// client must re-PREPARE.
    ddl_epoch: std::sync::atomic::AtomicU64,
    /// Public-mutation gate (PLAN.md W1).
    ///
    /// Built once at construction from the immutable subset of
    /// `RedDBOptions` (read_only flag + replication role). Every public
    /// mutation surface — SQL DML/DDL, gRPC mutating RPCs, HTTP/native
    /// wire mutations, admin maintenance endpoints, serverless
    /// lifecycle — consults `write_gate.check(WriteKind::*)` before
    /// dispatching to storage. The replica internal apply path
    /// (`LogicalChangeApplier`) reaches into the store directly and
    /// bypasses the gate by construction.
    write_gate: crate::runtime::write_gate::WriteGate,
    /// Process lifecycle state machine (PLAN.md Phase 1 — Lifecycle
    /// Contract). Drives `/health/live`, `/health/ready`,
    /// `/health/startup`, and `POST /admin/shutdown` so any
    /// orchestrator (K8s preStop, Fly autostop, ECS task drain,
    /// systemd) can coordinate without losing data.
    lifecycle: crate::runtime::lifecycle::Lifecycle,
    /// Operator-imposed resource limits (PLAN.md Phase 4.1).
    /// Read once at boot from `RED_MAX_*` env vars; consulted by
    /// observability and (in follow-up commits) the per-write
    /// enforcement points.
    resource_limits: crate::runtime::resource_limits::ResourceLimits,
}

#[derive(Clone)]
pub struct RedDBRuntime {
    inner: Arc<RuntimeInner>,
}

pub struct RuntimeConnection {
    id: u64,
    inner: Arc<RuntimeInner>,
}

pub mod config_matrix;
pub mod config_overlay;
pub mod lifecycle;
pub mod resource_limits;
mod expr_eval;
mod graph_dsl;
mod health_connection;
mod impl_core;
mod impl_ddl;
mod impl_dml;
mod impl_ec;
mod impl_graph;
mod impl_graph_commands;
mod impl_native;
mod impl_physical;
mod impl_probabilistic;
mod impl_queue;
mod impl_search;
mod impl_timeseries;
mod impl_tree;
mod impl_vcs;
mod index_store;
mod join_filter;
pub mod locking;
pub(crate) mod mutation;
mod probabilistic_store;
pub(crate) mod query_exec;
mod record_search;
pub mod schema_diff;
pub mod snapshot_reuse;
pub mod within_clause;
pub mod write_gate;

pub use self::graph_dsl::*;
use self::join_filter::*;
use self::query_exec::*;
use self::record_search::*;

/// Re-exports for transports + tests that need per-connection
/// isolation, tenant / auth thread-locals, and MVCC snapshot
/// utilities. Mirrors what PG-wire / gRPC / HTTP middleware already
/// call, and is enough to emulate independent connections in
/// integration tests.
pub mod mvcc {
    pub use super::impl_core::{
        capture_current_snapshot, clear_current_auth_identity, clear_current_connection_id,
        clear_current_snapshot, clear_current_tenant, current_connection_id, current_tenant,
        entity_visible_under_current_snapshot, entity_visible_with_context,
        set_current_auth_identity, set_current_connection_id, set_current_snapshot,
        set_current_tenant, snapshot_bundle, with_snapshot_bundle, SnapshotBundle,
        SnapshotContext,
    };
}

/// Public helpers re-exported for use by the presentation layer.
pub mod record_search_helpers {
    use crate::storage::query::UnifiedRecord;
    use crate::storage::UnifiedEntity;
    use std::collections::BTreeSet;

    pub fn entity_type_and_capabilities(
        entity: &UnifiedEntity,
    ) -> (&'static str, BTreeSet<String>) {
        super::record_search::runtime_entity_type_and_capabilities(entity)
    }

    /// Materialise any entity kind (TableRow, Node, Edge, Vector,
    /// TimeSeriesPoint, QueueMessage) into a `UnifiedRecord` whose
    /// `values` carry the native fields. Used by the RLS evaluator
    /// when a non-table collection matches a `CompareExpr` policy.
    pub fn any_record_from_entity(entity: UnifiedEntity) -> Option<UnifiedRecord> {
        super::record_search::runtime_any_record_from_entity(entity)
    }
}
