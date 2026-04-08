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
    ExportDescriptor, ManifestEvent, PhysicalAnalyticsJob, PhysicalGraphProjection,
    PhysicalLayout, SnapshotDescriptor,
};
use crate::serde_json::Value as JsonValue;
use crate::storage::engine::{
    BetweennessCentrality, ClosenessCentrality, ClusteringCoefficient, ConnectedComponents,
    CycleDetector, DegreeCentrality, EigenvectorCentrality, GraphEdgeType, GraphNodeType,
    GraphStore, HITS, IvfConfig, IvfIndex, IvfStats, LabelPropagation, Louvain, MetadataEntry,
    MetadataFilter as VectorMetadataFilter, MetadataValue as VectorMetadataValue, PageRank,
    PersonalizedPageRank, PhysicalFileHeader, StoredNode,
    StronglyConnectedComponents, WeaklyConnectedComponents,
};
use crate::storage::query::ast::{
    CompareOp, FieldRef, Filter, FusionStrategy, HybridQuery, JoinQuery, JoinType,
    OrderByClause, Projection, QueryExpr, TableQuery, VectorQuery, VectorSource,
};
use crate::storage::query::modes::{detect_mode, parse_multi, QueryMode};
use crate::storage::query::planner::{
    CanonicalLogicalPlan, CanonicalPlanner, CostEstimator, QueryPlanner,
};
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::storage::query::is_universal_entity_source as is_universal_query_source;
use crate::storage::schema::Value;
use crate::storage::engine::pathfinding::{BFS, Dijkstra};
use crate::storage::unified::dsl::{
    cosine_similarity,
    Filter as DslFilter, FilterOp as DslFilterOp, FilterValue as DslFilterValue,
    apply_filters, GraphPatternDsl, HybridQueryBuilder, QueryResult as DslQueryResult,
    ScoredMatch, TextSearchBuilder,
};
use crate::storage::{
    EntityData, EntityId, EntityKind, RedDB, RefType, SimilarResult, StoreStats, UnifiedEntity,
    UnifiedStore,
};
use crate::storage::unified::{Metadata, MetadataValue as UnifiedMetadataValue, RefTarget};
use crate::storage::unified::store::{
    NativeCatalogSummary, NativeManifestSummary, NativePhysicalState, NativeRecoverySummary,
    NativeRegistrySummary,
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
pub struct RuntimeStats {
    pub active_connections: usize,
    pub idle_connections: usize,
    pub total_checkouts: u64,
    pub paged_mode: bool,
    pub started_at_unix_ms: u128,
    pub store: StoreStats,
}

#[derive(Debug, Clone)]
pub struct RuntimeQueryResult {
    pub query: String,
    pub mode: QueryMode,
    pub statement: &'static str,
    pub engine: &'static str,
    pub result: UnifiedResult,
}

#[derive(Debug, Clone)]
pub struct RuntimeQueryExplain {
    pub query: String,
    pub mode: QueryMode,
    pub statement: &'static str,
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
}

#[derive(Clone)]
pub struct RedDBRuntime {
    inner: Arc<RuntimeInner>,
}

pub struct RuntimeConnection {
    id: u64,
    inner: Arc<RuntimeInner>,
}


mod impl_core;
mod impl_search;
mod impl_native;
mod impl_physical;
mod impl_graph;
mod health_connection;
mod query_exec;
mod record_search;
mod join_filter;
mod graph_dsl;

use self::graph_dsl::*;
use self::join_filter::*;
use self::query_exec::*;
use self::record_search::*;
