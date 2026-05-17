//! Embedded runtime with connection pooling, scans and health.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
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
    CycleDetector, DegreeCentrality, EigenvectorCentrality, GraphStore, IvfConfig, IvfIndex,
    IvfStats, LabelPropagation, Louvain, MetadataEntry, MetadataFilter as VectorMetadataFilter,
    MetadataValue as VectorMetadataValue, PageRank, PersonalizedPageRank, PhysicalFileHeader,
    StoredNode, StronglyConnectedComponents, WeaklyConnectedComponents, HITS,
};
use crate::storage::query::ast::{
    AlterOperation, AlterQueueQuery, AlterTableQuery, CompareOp, CreateCollectionQuery,
    CreateIndexQuery, CreateQueueQuery, CreateTableQuery, CreateTimeSeriesQuery, CreateTreeQuery,
    CreateVectorQuery, DeleteQuery, DropCollectionQuery, DropDocumentQuery, DropGraphQuery,
    DropIndexQuery, DropKvQuery, DropQueueQuery, DropTableQuery, DropTimeSeriesQuery,
    DropTreeQuery, DropVectorQuery, EventsBackfillQuery, ExplainAlterQuery, ExplainFormat,
    FieldRef, Filter, FusionStrategy, GraphCommand, HybridQuery, IndexMethod, InsertEntityType,
    InsertQuery, JoinQuery, JoinType, OrderByClause, ProbabilisticCommand, Projection, QueryExpr,
    QueueCommand, QueueSelectQuery, QueueSide, SearchCommand, TableQuery, TreeCommand,
    TruncateQuery, UpdateQuery, VectorQuery, VectorSource,
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
    pub result_blob_cache: crate::storage::cache::BlobCacheStats,
    pub kv: KvStats,
    pub metrics_ingest: MetricsIngestStats,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsTenantActivityStats {
    pub tenant: String,
    pub namespace: String,
    pub operation: String,
    pub count: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MetricsIngestStats {
    pub samples_accepted: u64,
    pub series_accepted: u64,
    pub samples_rejected: u64,
    pub series_rejected: u64,
    pub series_rejected_cardinality_budget: u64,
}

#[derive(Debug, Default)]
pub(crate) struct MetricsIngestCounters {
    samples_accepted: AtomicU64,
    series_accepted: AtomicU64,
    samples_rejected: AtomicU64,
    series_rejected: AtomicU64,
    series_rejected_cardinality_budget: AtomicU64,
}

impl MetricsIngestCounters {
    pub(crate) fn record(
        &self,
        accepted_samples: u64,
        accepted_series: u64,
        rejected_samples: u64,
        rejected_series: u64,
    ) {
        self.samples_accepted
            .fetch_add(accepted_samples, AtomicOrdering::Relaxed);
        self.series_accepted
            .fetch_add(accepted_series, AtomicOrdering::Relaxed);
        self.samples_rejected
            .fetch_add(rejected_samples, AtomicOrdering::Relaxed);
        self.series_rejected
            .fetch_add(rejected_series, AtomicOrdering::Relaxed);
    }

    pub(crate) fn record_cardinality_budget_rejections(&self, rejected_series: u64) {
        self.series_rejected_cardinality_budget
            .fetch_add(rejected_series, AtomicOrdering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> MetricsIngestStats {
        MetricsIngestStats {
            samples_accepted: self.samples_accepted.load(AtomicOrdering::Relaxed),
            series_accepted: self.series_accepted.load(AtomicOrdering::Relaxed),
            samples_rejected: self.samples_rejected.load(AtomicOrdering::Relaxed),
            series_rejected: self.series_rejected.load(AtomicOrdering::Relaxed),
            series_rejected_cardinality_budget: self
                .series_rejected_cardinality_budget
                .load(AtomicOrdering::Relaxed),
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct MetricsTenantActivityCounters {
    inner: Mutex<BTreeMap<(String, String, String), u64>>,
}

impl MetricsTenantActivityCounters {
    pub(crate) fn record(&self, tenant: &str, namespace: &str, operation: &str) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let key = (
            tenant.to_string(),
            namespace.to_string(),
            operation.to_string(),
        );
        *inner.entry(key).or_insert(0) += 1;
    }

    pub(crate) fn snapshot(&self) -> Vec<MetricsTenantActivityStats> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        inner
            .iter()
            .map(
                |((tenant, namespace, operation), count)| MetricsTenantActivityStats {
                    tenant: tenant.clone(),
                    namespace: namespace.clone(),
                    operation: operation.clone(),
                    count: *count,
                },
            )
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KvStats {
    pub puts: u64,
    pub gets: u64,
    pub deletes: u64,
    pub incrs: u64,
    pub cas_success: u64,
    pub cas_conflict: u64,
    pub watch_streams_active: u64,
    pub watch_events_emitted: u64,
    pub watch_drops: u64,
}

#[derive(Debug, Default)]
pub(crate) struct KvStatsCounters {
    puts: AtomicU64,
    gets: AtomicU64,
    deletes: AtomicU64,
    incrs: AtomicU64,
    cas_success: AtomicU64,
    cas_conflict: AtomicU64,
    watch_streams_active: AtomicU64,
    watch_events_emitted: AtomicU64,
    watch_drops: AtomicU64,
}

#[derive(Debug, Default)]
pub(crate) struct KvTagIndex {
    tag_to_entries: parking_lot::RwLock<HashMap<(String, String), HashMap<String, EntityId>>>,
    key_to_tags: parking_lot::RwLock<HashMap<(String, String), BTreeSet<String>>>,
}

impl KvTagIndex {
    pub(crate) fn replace(&self, collection: &str, key: &str, id: EntityId, tags: &[String]) {
        let entry_key = (collection.to_string(), key.to_string());
        let new_tags: BTreeSet<String> = tags
            .iter()
            .map(|tag| tag.trim())
            .filter(|tag| !tag.is_empty())
            .map(ToOwned::to_owned)
            .collect();

        let old_tags = {
            let mut key_to_tags = self.key_to_tags.write();
            if new_tags.is_empty() {
                key_to_tags.remove(&entry_key)
            } else {
                key_to_tags.insert(entry_key.clone(), new_tags.clone())
            }
        };

        let mut tag_to_entries = self.tag_to_entries.write();
        if let Some(old_tags) = old_tags {
            for tag in old_tags {
                let scoped = (collection.to_string(), tag);
                let remove_scoped = if let Some(entries) = tag_to_entries.get_mut(&scoped) {
                    entries.remove(key);
                    entries.is_empty()
                } else {
                    false
                };
                if remove_scoped {
                    tag_to_entries.remove(&scoped);
                }
            }
        }

        for tag in new_tags {
            tag_to_entries
                .entry((collection.to_string(), tag))
                .or_default()
                .insert(key.to_string(), id);
        }
    }

    pub(crate) fn remove(&self, collection: &str, key: &str) {
        let entry_key = (collection.to_string(), key.to_string());
        let old_tags = self.key_to_tags.write().remove(&entry_key);
        let Some(old_tags) = old_tags else {
            return;
        };

        let mut tag_to_entries = self.tag_to_entries.write();
        for tag in old_tags {
            let scoped = (collection.to_string(), tag);
            let remove_scoped = if let Some(entries) = tag_to_entries.get_mut(&scoped) {
                entries.remove(key);
                entries.is_empty()
            } else {
                false
            };
            if remove_scoped {
                tag_to_entries.remove(&scoped);
            }
        }
    }

    pub(crate) fn entries_for_tags(
        &self,
        collection: &str,
        tags: &[String],
    ) -> Vec<(String, EntityId)> {
        if tags.is_empty() {
            return Vec::new();
        }

        let tag_to_entries = self.tag_to_entries.read();
        let mut out: HashMap<String, EntityId> = HashMap::new();
        for tag in tags {
            let scoped = (collection.to_string(), tag.trim().to_string());
            if let Some(entries) = tag_to_entries.get(&scoped) {
                for (key, id) in entries {
                    out.entry(key.clone()).or_insert(*id);
                }
            }
        }
        out.into_iter().collect()
    }

    pub(crate) fn tags_for_key(&self, collection: &str, key: &str) -> Vec<String> {
        self.key_to_tags
            .read()
            .get(&(collection.to_string(), key.to_string()))
            .map(|tags| tags.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl KvStatsCounters {
    pub(crate) fn snapshot(&self) -> KvStats {
        KvStats {
            puts: self.puts.load(AtomicOrdering::Relaxed),
            gets: self.gets.load(AtomicOrdering::Relaxed),
            deletes: self.deletes.load(AtomicOrdering::Relaxed),
            incrs: self.incrs.load(AtomicOrdering::Relaxed),
            cas_success: self.cas_success.load(AtomicOrdering::Relaxed),
            cas_conflict: self.cas_conflict.load(AtomicOrdering::Relaxed),
            watch_streams_active: self.watch_streams_active.load(AtomicOrdering::Relaxed),
            watch_events_emitted: self.watch_events_emitted.load(AtomicOrdering::Relaxed),
            watch_drops: self.watch_drops.load(AtomicOrdering::Relaxed),
        }
    }

    pub(crate) fn incr_puts(&self) {
        self.puts.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(crate) fn incr_gets(&self) {
        self.gets.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(crate) fn incr_deletes(&self) {
        self.deletes.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(crate) fn incr_incrs(&self) {
        self.incrs.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(crate) fn incr_cas_success(&self) {
        self.cas_success.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(crate) fn incr_cas_conflict(&self) {
        self.cas_conflict.fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(crate) fn incr_watch_streams_active(&self) {
        self.watch_streams_active
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(crate) fn decr_watch_streams_active(&self) {
        self.watch_streams_active
            .fetch_sub(1, AtomicOrdering::Relaxed);
    }

    pub(crate) fn incr_watch_events_emitted(&self) {
        self.watch_events_emitted
            .fetch_add(1, AtomicOrdering::Relaxed);
    }

    pub(crate) fn add_watch_drops(&self, count: u64) {
        self.watch_drops.fetch_add(count, AtomicOrdering::Relaxed);
    }
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
    /// Names of any CTEs declared by a leading `WITH` clause. Empty
    /// for non-CTE queries. The plan tree is built against the
    /// post-inlining body, so each CTE's body is reachable inside
    /// `logical_plan` as a regular `Subquery` (or, for bare refs, the
    /// inlined Table node verbatim). This list lets renderers prepend
    /// `CteScan` markers so operators see which CTEs were resolved.
    pub cte_materializations: Vec<String>,
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

pub const METRIC_CACHE_SHADOW_DIVERGENCE_TOTAL: &str = "cache_shadow_divergence_total";
pub(crate) const ASK_ANSWER_CACHE_NAMESPACE: &str = "runtime.ask_answer_cache";
const RMW_LOCK_SHARDS: usize = 64;

struct RmwLockTable {
    shards: Vec<parking_lot::Mutex<HashMap<String, Arc<parking_lot::Mutex<()>>>>>,
}

impl RmwLockTable {
    fn new() -> Self {
        let shards = (0..RMW_LOCK_SHARDS)
            .map(|_| parking_lot::Mutex::new(HashMap::new()))
            .collect();
        Self { shards }
    }

    fn lock_for(&self, collection: &str, key: &str) -> Arc<parking_lot::Mutex<()>> {
        use std::hash::{Hash, Hasher};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        collection.hash(&mut hasher);
        key.hash(&mut hasher);
        let shard_idx = (hasher.finish() as usize) % self.shards.len();
        let map_key = format!("{collection}\u{1f}{key}");
        let mut shard = self.shards[shard_idx].lock();
        shard
            .entry(map_key)
            .or_insert_with(|| Arc::new(parking_lot::Mutex::new(())))
            .clone()
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
    query_cache: parking_lot::RwLock<crate::storage::query::planner::cache::PlanCache>,
    result_cache: parking_lot::RwLock<(
        HashMap<String, RuntimeResultCacheEntry>,
        std::collections::VecDeque<String>,
    )>,
    result_blob_cache: crate::storage::cache::BlobCache,
    result_blob_entries: parking_lot::RwLock<(
        HashMap<String, RuntimeResultCacheEntry>,
        std::collections::VecDeque<String>,
    )>,
    ask_answer_cache_entries:
        parking_lot::RwLock<(HashSet<String>, std::collections::VecDeque<String>)>,
    result_cache_shadow_divergences: std::sync::atomic::AtomicU64,
    ask_daily_spend:
        parking_lot::RwLock<HashMap<String, crate::runtime::ai::cost_guard::DailyState>>,
    /// Process-local queue message locks used to emulate `SKIP LOCKED`-style
    /// claim semantics for concurrent queue consumers inside this runtime.
    queue_message_locks: parking_lot::RwLock<HashMap<String, Arc<parking_lot::Mutex<()>>>>,
    /// Process-local read-modify-write locks. The table is sharded by
    /// `(collection, key)` and each entry has its own mutex, so unrelated keys
    /// in the same collection do not serialize behind one global lock.
    rmw_locks: RmwLockTable,
    planner_dirty_tables: parking_lot::RwLock<HashSet<String>>,
    ec_registry: Arc<crate::ec::config::EcRegistry>,
    ec_worker: crate::ec::worker::EcWorker,
    /// Optional AuthStore — injected by server boot when auth is
    /// enabled. Required for `Value::Secret` auto-encrypt/decrypt
    /// because the AES key lives in the vault KV under the
    /// `red.secret.aes_key` entry.
    auth_store: parking_lot::RwLock<Option<Arc<crate::auth::store::AuthStore>>>,
    /// Optional OAuth/OIDC JWT validator. Wired by server boot when
    /// the operator configures issuer + JWKS via env / CLI. HTTP and
    /// wire transports read this on every bearer-token request and,
    /// when the token decodes as a JWT, validate it against the
    /// configured issuer + audience + signature before falling back to
    /// the local AuthStore lookup.
    oauth_validator: parking_lot::RwLock<Option<Arc<crate::auth::oauth::OAuthValidator>>>,
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
    /// Per-collection retention sweeper state (issue #584 slice 12).
    /// Tracks `last_sweep_at_ms`, `rows_swept_total`, and the latest
    /// pending-rows estimate that feeds the three new columns on
    /// `red.retention`. In-memory only; resets across restart.
    pub(crate) retention_sweeper:
        parking_lot::RwLock<crate::runtime::retention_sweeper::RetentionSweeperState>,
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
    /// Each entry is `(collection, entity_id, stamper_xid, previous_xmax)`
    /// — the xid that stamped xmax on the tuple plus the value it
    /// replaced. For a plain transaction the
    /// stamper equals `ctx.xid`; with savepoints the stamper equals
    /// the innermost open sub-xid so ROLLBACK TO SAVEPOINT can revive
    /// only the matching subset. COMMIT drains the whole conn list
    /// and keeps the committed tombstones; ROLLBACK (whole-tx) revives them all;
    /// ROLLBACK TO SAVEPOINT revives those with `stamper_xid >=
    /// savepoint_xid`. Autocommit DELETE bypasses this map.
    pending_tombstones: parking_lot::RwLock<
        HashMap<
            u64,
            Vec<(
                String,
                crate::storage::unified::entity::EntityId,
                crate::storage::transaction::snapshot::Xid,
                crate::storage::transaction::snapshot::Xid,
            )>,
        >,
    >,
    /// Per-connection table-row UPDATE versions created by an open
    /// transaction. Each entry is `(collection, old_entity_id,
    /// new_entity_id, stamper_xid, previous_xmax)`. COMMIT keeps both physical
    /// versions and drops the pending marker; ROLLBACK revives the old
    /// version and removes the new uncommitted version.
    pending_versioned_updates: parking_lot::RwLock<
        HashMap<
            u64,
            Vec<(
                String,
                crate::storage::unified::entity::EntityId,
                crate::storage::unified::entity::EntityId,
                crate::storage::transaction::snapshot::Xid,
                crate::storage::transaction::snapshot::Xid,
            )>,
        >,
    >,
    pending_kv_watch_events:
        parking_lot::RwLock<HashMap<u64, Vec<crate::replication::cdc::KvWatchEvent>>>,
    pending_store_wal_actions:
        parking_lot::RwLock<HashMap<u64, crate::storage::unified::DeferredStoreWalActions>>,
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
    write_gate: Arc<crate::runtime::write_gate::WriteGate>,
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
    /// Append-only audit log for admin mutations (PLAN.md Phase
    /// 6.5). Lives next to the primary `.rdb` file so backup +
    /// restore flows ship it alongside the data.
    audit_log: Arc<crate::runtime::audit_log::AuditLogger>,
    /// Serverless writer-lease state machine. `None` when the operator
    /// did not opt into lease fencing (`RED_LEASE_REQUIRED` unset/false).
    /// When set, owns the {acquire/refresh/release/lost} transitions and
    /// is the single place that mutates `write_gate.set_lease_state` and
    /// records lease/* audit entries — keeping those two side-effects
    /// from drifting.
    lease_lifecycle: std::sync::OnceLock<Arc<crate::runtime::lease_lifecycle::LeaseLifecycle>>,
    /// PLAN.md Phase 11.5 — counters bumped by the replica apply
    /// loop on `Gap` / `Divergence` / `Apply` errors so /metrics
    /// surfaces them as `reddb_replica_apply_errors_total{kind}`.
    replica_apply_metrics: crate::replication::logical::ReplicaApplyMetrics,
    /// PLAN.md Phase 4.4 — per-caller QPS quotas. Disabled (no-op)
    /// when `RED_MAX_QPS_PER_CALLER` is unset.
    quota_bucket: crate::runtime::quota_bucket::QuotaBucket,
    /// Issue #120 — token → schema entity reverse index, kept current
    /// incrementally on DDL events. Consumed by AskPipeline (issue
    /// #121) Stage 2 to narrow vector-search candidates before any
    /// embedding compute. Mutated only from DDL execution paths.
    schema_vocabulary: parking_lot::RwLock<crate::runtime::schema_vocabulary::SchemaVocabulary>,
    /// Issue #205 — dedicated slow-query sink (`red-slow.log`).
    /// Built once at runtime startup; below-threshold calls pay only a
    /// single relaxed atomic load. Threshold + sample-pct come from
    /// `runtime.slow_query.threshold_ms` / `.sample_pct` (config matrix)
    /// at construction; live tuning via the config tree is a follow-up.
    slow_query_logger: Arc<crate::telemetry::slow_query_logger::SlowQueryLogger>,
    /// Process-local normal-KV operation counters. These are intentionally
    /// runtime-local; persistent accounting belongs in catalog stats.
    kv_stats: KvStatsCounters,
    metrics_ingest_stats: MetricsIngestCounters,
    metrics_tenant_activity_stats: MetricsTenantActivityCounters,
    /// Process-local normal-KV tag index used by `INVALIDATE TAGS`.
    kv_tag_index: KvTagIndex,
    /// Issue #524 — in-memory chain-tip cache per collection. Populated lazily
    /// by the first INSERT or `GET /chain-tip` call after restart and updated
    /// atomically with each chain INSERT. Backed by a single mutex so a chain
    /// INSERT serialises against concurrent submitters — the loser observes
    /// the advanced tip and surfaces `BlockchainConflict` to its caller.
    chain_tip_cache: parking_lot::Mutex<
        HashMap<String, crate::runtime::blockchain_kind::ChainTipFull>,
    >,
    /// Issue #525 — in-memory mirror of the persisted `integrity` flag per
    /// chain collection.  `true` means INSERTs must be rejected with
    /// `ChainIntegrityBroken`.  Loaded lazily from `red_config` on first
    /// access so the flag survives restart.
    chain_integrity_broken: parking_lot::Mutex<HashMap<String, bool>>,
}

#[derive(Clone)]
pub struct RedDBRuntime {
    inner: Arc<RuntimeInner>,
}

pub struct RuntimeConnection {
    id: u64,
    inner: Arc<RuntimeInner>,
}

pub mod ai;
pub mod ask_pipeline;
pub mod audit_log;
pub mod audit_query;
pub mod authorized_search;
pub mod blockchain_kind;
mod collection_contract;
pub mod config_matrix;
pub mod config_overlay;
pub mod config_watcher;
pub(crate) mod ddl;
pub mod disk_space_monitor;
mod dml_target_scan;
mod expr_eval;
mod graph_dsl;
mod health_connection;
mod impl_config;
pub(crate) mod impl_core;
mod impl_ddl;
mod impl_dml;
mod impl_ec;
mod impl_events;
mod impl_graph;
mod impl_graph_commands;
pub mod impl_kv;
mod impl_migrations;
mod impl_native;
mod impl_physical;
mod impl_probabilistic;
pub mod impl_queue;
mod impl_search;
mod impl_timeseries;
mod impl_tree;
mod impl_vcs;
mod index_store;
mod join_filter;
mod keyed_spine;
pub mod kv_watch;
pub mod lease_lifecycle;
pub mod lease_loop;
pub mod lease_timer_wheel;
pub mod lifecycle;
pub mod locking;
pub(crate) mod mutation;
mod probabilistic_store;
pub(crate) mod query_exec;
mod queue_delivery;
pub(crate) mod queue_lifecycle;
pub(crate) mod primary_queue_store;
pub mod quota_bucket;
mod record_search;
mod red_schema;
pub(crate) mod retention_filter;
pub(crate) mod retention_sweeper;
pub mod resource_limits;
pub(crate) mod scalar_evaluator;
pub mod schema_diff;
pub mod schema_vocabulary;
pub mod signed_chain;
pub mod signed_writes_kind;
pub mod snapshot_reuse;
mod statement_frame;
mod table_row_mvcc_resolver;
mod vector_index;
pub mod within_clause;
pub mod write_gate;

pub use self::graph_dsl::*;
use self::join_filter::*;
use self::query_exec::*;
use self::record_search::*;
pub use self::statement_frame::EffectiveScope;

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
        set_current_tenant, snapshot_bundle, with_snapshot_bundle, SnapshotBundle, SnapshotContext,
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
