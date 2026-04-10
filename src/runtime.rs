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
use crate::storage::engine::pathfinding::{Dijkstra, BFS, DFS};
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
    CreateTableQuery, CreateTimeSeriesQuery, DeleteQuery, DropIndexQuery, DropQueueQuery,
    DropTableQuery, DropTimeSeriesQuery, FieldRef, Filter, FusionStrategy, GraphCommand,
    HybridQuery, IndexMethod, InsertEntityType, InsertQuery, JoinQuery, JoinType, OrderByClause,
    ProbabilisticCommand, Projection, QueryExpr, QueueCommand, QueueSide, SearchCommand,
    TableQuery, UpdateQuery, VectorQuery, VectorSource,
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
        record.set("message", Value::Text(message.to_string()));
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
    query_cache: Mutex<crate::storage::query::planner::cache::PlanCache>,
}

#[derive(Clone)]
pub struct RedDBRuntime {
    inner: Arc<RuntimeInner>,
}

pub struct RuntimeConnection {
    id: u64,
    inner: Arc<RuntimeInner>,
}

mod graph_dsl;
mod health_connection;
mod impl_core;
mod impl_ddl;
mod impl_dml;
mod impl_graph;
mod impl_graph_commands;
mod impl_native;
mod impl_physical;
mod impl_probabilistic;
mod impl_queue;
mod impl_search;
mod impl_timeseries;
mod index_store;
mod join_filter;
mod probabilistic_store;
mod query_exec;
mod record_search;

pub use self::graph_dsl::*;
use self::join_filter::*;
use self::query_exec::*;
use self::record_search::*;

/// Public helpers re-exported for use by the presentation layer.
pub mod record_search_helpers {
    use crate::storage::UnifiedEntity;
    use std::collections::BTreeSet;

    pub fn entity_type_and_capabilities(
        entity: &UnifiedEntity,
    ) -> (&'static str, BTreeSet<String>) {
        super::record_search::runtime_entity_type_and_capabilities(entity)
    }
}
