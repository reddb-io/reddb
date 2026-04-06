//! Embedded runtime with connection pooling, scans and health.

use std::cmp::Ordering;
use std::collections::{BTreeSet, BinaryHeap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{RedDBError, RedDBOptions, RedDBResult};
use crate::catalog::CatalogModelSnapshot;
use crate::health::{HealthProvider, HealthReport};
use crate::index::IndexCatalog;
use crate::physical::{
    ExportDescriptor, ManifestEvent, PhysicalAnalyticsJob, PhysicalGraphProjection,
    PhysicalLayout, SnapshotDescriptor,
};
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
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::storage::schema::Value;
use crate::storage::engine::pathfinding::{BFS, Dijkstra};
use crate::storage::unified::dsl::{
    cosine_similarity,
    Filter as DslFilter, FilterOp as DslFilterOp, FilterValue as DslFilterValue,
    GraphPatternDsl, HybridQueryBuilder, QueryResult as DslQueryResult, TextSearchBuilder,
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

impl RedDBRuntime {
    pub fn in_memory() -> RedDBResult<Self> {
        Self::with_options(RedDBOptions::in_memory())
    }

    pub fn with_options(options: RedDBOptions) -> RedDBResult<Self> {
        Self::with_pool(options, ConnectionPoolConfig::default())
    }

    pub fn with_pool(
        options: RedDBOptions,
        pool_config: ConnectionPoolConfig,
    ) -> RedDBResult<Self> {
        let db = Arc::new(
            RedDB::open_with_options(&options)
                .map_err(|err| RedDBError::Internal(err.to_string()))?,
        );

        Ok(Self {
            inner: Arc::new(RuntimeInner {
                db,
                layout: PhysicalLayout::from_options(&options),
                indices: IndexCatalog::register_default_vector_graph(
                    options.has_capability(crate::api::Capability::Table),
                    options.has_capability(crate::api::Capability::Graph),
                ),
                pool_config,
                pool: Mutex::new(PoolState::default()),
                started_at_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
            }),
        })
    }

    pub fn db(&self) -> Arc<RedDB> {
        Arc::clone(&self.inner.db)
    }

    pub fn acquire(&self) -> RedDBResult<RuntimeConnection> {
        let mut pool = self.inner.pool.lock().unwrap();
        if pool.active >= self.inner.pool_config.max_connections {
            return Err(RedDBError::Internal(
                "connection pool exhausted".to_string(),
            ));
        }

        let id = if let Some(id) = pool.idle.pop() {
            id
        } else {
            let id = pool.next_id;
            pool.next_id += 1;
            id
        };
        pool.active += 1;
        pool.total_checkouts += 1;
        drop(pool);

        Ok(RuntimeConnection {
            id,
            inner: Arc::clone(&self.inner),
        })
    }

    pub fn checkpoint(&self) -> RedDBResult<()> {
        self.inner
            .db
            .flush()
            .map_err(|err| RedDBError::Engine(err.to_string()))
    }

    pub fn run_maintenance(&self) -> RedDBResult<()> {
        self.inner
            .db
            .run_maintenance()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        let store = self.inner.db.store();
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| RedDBError::NotFound(collection.to_string()))?;

        let mut entities = manager.query_all(|_| true);
        entities.sort_by_key(|entity| entity.id.raw());

        let offset = cursor.map(|cursor| cursor.offset).unwrap_or(0);
        let total = entities.len();
        let end = total.min(offset.saturating_add(limit.max(1)));
        let items = if offset >= total {
            Vec::new()
        } else {
            entities[offset..end].to_vec()
        };
        let next = (end < total).then_some(ScanCursor { offset: end });

        Ok(ScanPage {
            collection: collection.to_string(),
            items,
            next,
            total,
        })
    }

    pub fn catalog(&self) -> CatalogModelSnapshot {
        self.inner.db.catalog_model_snapshot()
    }

    pub fn catalog_consistency_report(&self) -> crate::catalog::CatalogConsistencyReport {
        self.inner.db.catalog_consistency_report()
    }

    pub fn stats(&self) -> RuntimeStats {
        let pool = self.inner.pool.lock().unwrap();
        RuntimeStats {
            active_connections: pool.active,
            idle_connections: pool.idle.len(),
            total_checkouts: pool.total_checkouts,
            paged_mode: self.inner.db.is_paged(),
            started_at_unix_ms: self.inner.started_at_unix_ms,
            store: self.inner.db.stats(),
        }
    }

    pub fn execute_query(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        let mode = detect_mode(query);
        if matches!(mode, QueryMode::Unknown) {
            return Err(RedDBError::Query("unable to detect query mode".to_string()));
        }

        let expr = parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?;
        let statement = query_expr_name(&expr);

        match expr {
            QueryExpr::Graph(_) | QueryExpr::Path(_) => {
                let graph = materialize_graph(self.inner.db.store().as_ref())?;
                let result = crate::storage::query::unified::UnifiedExecutor::execute_on(
                    &graph, &expr,
                )
                .map_err(|err| RedDBError::Query(err.to_string()))?;

                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "materialized-graph",
                    result,
                })
            }
            QueryExpr::Table(table) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-table",
                result: execute_runtime_table_query(&self.inner.db, &table)?,
            }),
            QueryExpr::Join(join) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-join",
                result: execute_runtime_join_query(&self.inner.db, &join)?,
            }),
            QueryExpr::Vector(vector) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-vector",
                result: execute_runtime_vector_query(&self.inner.db, &vector)?,
            }),
            QueryExpr::Hybrid(hybrid) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-hybrid",
                result: execute_runtime_hybrid_query(&self.inner.db, &hybrid)?,
            }),
        }
    }

    pub fn search_similar(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
        min_score: f32,
    ) -> RedDBResult<Vec<SimilarResult>> {
        let mut results = self.inner.db.similar(collection, vector, k.max(1));
        if results.is_empty() && self.inner.db.store().get_collection(collection).is_none() {
            return Err(RedDBError::NotFound(collection.to_string()));
        }
        results.retain(|result| result.score >= min_score);
        Ok(results)
    }

    pub fn search_ivf(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
        n_lists: usize,
        n_probes: Option<usize>,
    ) -> RedDBResult<RuntimeIvfSearchResult> {
        let store = self.inner.db.store();
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| RedDBError::NotFound(collection.to_string()))?;

        let vectors: Vec<(u64, Vec<f32>)> = manager
            .query_all(|_| true)
            .into_iter()
            .filter_map(|entity| match &entity.data {
                EntityData::Vector(data) if !data.dense.is_empty() => {
                    Some((entity.id.raw(), data.dense.clone()))
                }
                _ => None,
            })
            .collect();

        if vectors.is_empty() {
            return Err(RedDBError::Query(format!(
                "collection '{collection}' does not contain vector entities"
            )));
        }

        let dimension = vectors[0].1.len();
        if vector.len() != dimension {
            return Err(RedDBError::Query(format!(
                "query vector dimension mismatch: expected {dimension}, got {}",
                vector.len()
            )));
        }

        let consistent: Vec<(u64, Vec<f32>)> = vectors
            .into_iter()
            .filter(|(_, item)| item.len() == dimension)
            .collect();
        if consistent.is_empty() {
            return Err(RedDBError::Query(format!(
                "collection '{collection}' does not contain consistent vector dimensions"
            )));
        }

        let probes = n_probes.unwrap_or_else(|| (n_lists.max(1) / 10).max(1));
        let mut ivf = IvfIndex::new(IvfConfig::new(dimension, n_lists.max(1)).with_probes(probes));
        let training_vectors: Vec<Vec<f32>> =
            consistent.iter().map(|(_, item)| item.clone()).collect();
        ivf.train(&training_vectors);
        ivf.add_batch_with_ids(consistent);

        let stats = ivf.stats();
        let matches = ivf
            .search_with_probes(vector, k.max(1), probes)
            .into_iter()
            .map(|result| RuntimeIvfMatch {
                entity_id: result.id,
                distance: result.distance,
                entity: self.inner.db.get(EntityId::new(result.id)),
            })
            .collect();

        Ok(RuntimeIvfSearchResult {
            collection: collection.to_string(),
            k: k.max(1),
            n_lists: stats.n_lists,
            n_probes: probes,
            stats,
            matches,
        })
    }

    pub fn search_hybrid(
        &self,
        vector: Option<Vec<f32>>,
        k: Option<usize>,
        collections: Option<Vec<String>>,
        graph_pattern: Option<RuntimeGraphPattern>,
        filters: Vec<RuntimeFilter>,
        weights: Option<RuntimeQueryWeights>,
        min_score: Option<f32>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult> {
        let mut builder = HybridQueryBuilder::new();

        if let Some(vector) = vector {
            builder = builder.similar_to(&vector, k.unwrap_or(10).max(1));
        }

        if let Some(collections) = collections {
            for collection in collections {
                builder = builder.in_collection(collection);
            }
        }

        if let Some(graph_pattern) = graph_pattern {
            builder.graph_pattern = Some(GraphPatternDsl {
                node_label: graph_pattern.node_label,
                node_type: graph_pattern.node_type,
                edge_labels: graph_pattern.edge_labels,
            });
        }

        if let Some(weights) = weights {
            builder = builder.with_weights(weights.vector, weights.graph, weights.filter);
        }

        if let Some(min_score) = min_score {
            builder = builder.min_score(min_score);
        }

        if let Some(limit) = limit {
            builder = builder.limit(limit.max(1));
        }

        builder.filters = filters
            .into_iter()
            .map(runtime_filter_to_dsl)
            .collect::<RedDBResult<Vec<_>>>()?;

        builder
            .execute(self.inner.db.store())
            .map_err(|err| RedDBError::Query(err.to_string()))
    }

    pub fn search_text(
        &self,
        query: String,
        collections: Option<Vec<String>>,
        fields: Option<Vec<String>>,
        limit: Option<usize>,
        fuzzy: bool,
    ) -> RedDBResult<DslQueryResult> {
        let mut builder = TextSearchBuilder::new(query);

        if let Some(collections) = collections {
            for collection in collections {
                builder = builder.in_collection(collection);
            }
        }

        if let Some(fields) = fields {
            for field in fields {
                builder = builder.in_field(field);
            }
        }

        if let Some(limit) = limit {
            builder = builder.limit(limit.max(1));
        }

        if fuzzy {
            builder = builder.fuzzy();
        }

        builder
            .execute(self.inner.db.store())
            .map_err(|err| RedDBError::Query(err.to_string()))
    }

    pub fn snapshots(&self) -> RedDBResult<Vec<SnapshotDescriptor>> {
        let snapshots = self.inner.db.snapshots();
        if snapshots.is_empty() {
            return Err(RedDBError::NotFound("physical metadata".to_string()));
        }
        Ok(snapshots)
    }

    pub fn create_snapshot(&self) -> RedDBResult<SnapshotDescriptor> {
        self.checkpoint()?;
        self.inner
            .db
            .snapshots()
            .last()
            .cloned()
            .ok_or_else(|| RedDBError::Internal("snapshot metadata was not recorded".to_string()))
    }

    pub fn exports(&self) -> RedDBResult<Vec<ExportDescriptor>> {
        Ok(self.inner.db.exports())
    }

    pub fn native_header(&self) -> RedDBResult<PhysicalFileHeader> {
        self.inner
            .db
            .store()
            .physical_file_header()
            .ok_or_else(|| RedDBError::NotFound("native physical header".to_string()))
    }

    pub fn native_collection_roots(
        &self,
    ) -> RedDBResult<std::collections::BTreeMap<String, u64>> {
        self.inner
            .db
            .native_collection_roots()
            .ok_or_else(|| RedDBError::NotFound("native collection roots".to_string()))
    }

    pub fn native_manifest_summary(&self) -> RedDBResult<NativeManifestSummary> {
        self.inner
            .db
            .native_manifest_summary()
            .ok_or_else(|| RedDBError::NotFound("native manifest summary".to_string()))
    }

    pub fn native_registry_summary(&self) -> RedDBResult<NativeRegistrySummary> {
        self.inner
            .db
            .native_registry_summary()
            .ok_or_else(|| RedDBError::NotFound("native registry summary".to_string()))
    }

    pub fn native_recovery_summary(&self) -> RedDBResult<NativeRecoverySummary> {
        self.inner
            .db
            .native_recovery_summary()
            .ok_or_else(|| RedDBError::NotFound("native recovery summary".to_string()))
    }

    pub fn native_catalog_summary(&self) -> RedDBResult<NativeCatalogSummary> {
        self.inner
            .db
            .native_catalog_summary()
            .ok_or_else(|| RedDBError::NotFound("native catalog summary".to_string()))
    }

    pub fn native_physical_state(&self) -> RedDBResult<NativePhysicalState> {
        self.inner
            .db
            .native_physical_state()
            .ok_or_else(|| RedDBError::NotFound("native physical state".to_string()))
    }

    pub fn native_vector_artifact_pages(
        &self,
    ) -> RedDBResult<Vec<crate::storage::unified::store::NativeVectorArtifactPageSummary>> {
        self.inner
            .db
            .native_vector_artifact_pages()
            .ok_or_else(|| RedDBError::NotFound("native vector artifact pages".to_string()))
    }

    pub fn inspect_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> RedDBResult<crate::storage::unified::devx::NativeVectorArtifactInspection> {
        self.inner
            .db
            .inspect_native_vector_artifact(collection, artifact_kind)
            .map_err(|err| {
                if err.contains("not found") || err.contains("not available") {
                    RedDBError::NotFound(err)
                } else {
                    RedDBError::Internal(err)
                }
            })
    }

    pub fn warmup_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> RedDBResult<crate::storage::unified::devx::NativeVectorArtifactInspection> {
        self.inner
            .db
            .warmup_native_vector_artifact(collection, artifact_kind)
            .map_err(|err| {
                if err.contains("not found") || err.contains("not available") {
                    RedDBError::NotFound(err)
                } else {
                    RedDBError::Internal(err)
                }
            })
    }

    pub fn inspect_native_vector_artifacts(
        &self,
    ) -> RedDBResult<crate::storage::unified::devx::NativeVectorArtifactBatchInspection> {
        self.inner
            .db
            .inspect_native_vector_artifacts()
            .map_err(|err| {
                if err.contains("not available") {
                    RedDBError::NotFound(err)
                } else {
                    RedDBError::Internal(err)
                }
            })
    }

    pub fn warmup_native_vector_artifacts(
        &self,
    ) -> RedDBResult<crate::storage::unified::devx::NativeVectorArtifactBatchInspection> {
        self.inner
            .db
            .warmup_native_vector_artifacts()
            .map_err(|err| {
                if err.contains("not available") {
                    RedDBError::NotFound(err)
                } else {
                    RedDBError::Internal(err)
                }
            })
    }

    pub fn native_header_repair_policy(&self) -> RedDBResult<String> {
        let policy = self
            .inner
            .db
            .native_header_repair_policy()
            .ok_or_else(|| RedDBError::NotFound("native physical header repair policy".to_string()))?;
        Ok(match policy {
            crate::storage::NativeHeaderRepairPolicy::InSync => "in_sync",
            crate::storage::NativeHeaderRepairPolicy::RepairNativeFromMetadata => {
                "repair_native_from_metadata"
            }
            crate::storage::NativeHeaderRepairPolicy::NativeAheadOfMetadata => {
                "native_ahead_of_metadata"
            }
        }
        .to_string())
    }

    pub fn repair_native_header_from_metadata(&self) -> RedDBResult<String> {
        let policy = self
            .inner
            .db
            .repair_native_header_from_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(match policy {
            crate::storage::NativeHeaderRepairPolicy::InSync => "in_sync",
            crate::storage::NativeHeaderRepairPolicy::RepairNativeFromMetadata => {
                "repair_native_from_metadata"
            }
            crate::storage::NativeHeaderRepairPolicy::NativeAheadOfMetadata => {
                "native_ahead_of_metadata"
            }
        }
        .to_string())
    }

    pub fn rebuild_physical_metadata_from_native_state(&self) -> RedDBResult<bool> {
        self.inner
            .db
            .rebuild_physical_metadata_from_native_state()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn repair_native_physical_state_from_metadata(&self) -> RedDBResult<bool> {
        self.inner
            .db
            .repair_native_physical_state_from_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn native_metadata_state_summary(
        &self,
    ) -> RedDBResult<crate::storage::unified::store::NativeMetadataStateSummary> {
        self.inner
            .db
            .native_metadata_state_summary()
            .ok_or_else(|| RedDBError::NotFound("native metadata state".to_string()))
    }

    pub fn physical_authority_status(
        &self,
    ) -> crate::storage::unified::devx::PhysicalAuthorityStatus {
        self.inner.db.physical_authority_status()
    }

    pub fn manifest_events(&self) -> RedDBResult<Vec<ManifestEvent>> {
        if let Some(metadata) = self.inner.db.physical_metadata() {
            return Ok(metadata.manifest_events);
        }
        if let Some(summary) = self.inner.db.native_manifest_summary() {
            return Ok(summary
                .recent_events
                .into_iter()
                .map(|event| ManifestEvent {
                    collection: event.collection,
                    object_key: event.object_key,
                    kind: match event.kind.as_str() {
                        "insert" => crate::physical::ManifestEventKind::Insert,
                        "update" => crate::physical::ManifestEventKind::Update,
                        "remove" => crate::physical::ManifestEventKind::Remove,
                        _ => crate::physical::ManifestEventKind::Checkpoint,
                    },
                    block: crate::physical::BlockReference {
                        index: event.block_index,
                        checksum: event.block_checksum,
                    },
                    snapshot_min: event.snapshot_min,
                    snapshot_max: event.snapshot_max,
                })
                .collect());
        }
        Err(RedDBError::NotFound("physical metadata".to_string()))
    }

    pub fn manifest_events_filtered(
        &self,
        collection: Option<&str>,
        kind: Option<&str>,
        since_snapshot: Option<u64>,
    ) -> RedDBResult<Vec<ManifestEvent>> {
        let mut events = self.manifest_events()?;
        if let Some(collection) = collection {
            events.retain(|event| event.collection == collection);
        }
        if let Some(kind) = kind {
            let kind = normalize_graph_token(kind);
            events.retain(|event| {
                normalize_graph_token(match event.kind {
                    crate::physical::ManifestEventKind::Insert => "insert",
                    crate::physical::ManifestEventKind::Update => "update",
                    crate::physical::ManifestEventKind::Remove => "remove",
                    crate::physical::ManifestEventKind::Checkpoint => "checkpoint",
                }) == kind
            });
        }
        if let Some(since_snapshot) = since_snapshot {
            events.retain(|event| event.snapshot_min >= since_snapshot);
        }
        Ok(events)
    }

    pub fn collection_roots(&self) -> RedDBResult<std::collections::BTreeMap<String, u64>> {
        if let Some(metadata) = self.inner.db.physical_metadata() {
            return Ok(metadata.superblock.collection_roots);
        }
        if let Some(state) = self.inner.db.native_physical_state() {
            return Ok(state.collection_roots);
        }
        Err(RedDBError::NotFound("physical metadata".to_string()))
    }

    pub fn create_export(&self, name: impl Into<String>) -> RedDBResult<ExportDescriptor> {
        self.inner
            .db
            .create_named_export(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn graph_projections(&self) -> RedDBResult<Vec<PhysicalGraphProjection>> {
        Ok(self.inner.db.declared_graph_projections())
    }

    pub fn operational_graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        self.inner.db.operational_graph_projections()
    }

    pub fn graph_projection_named(&self, name: &str) -> RedDBResult<RuntimeGraphProjection> {
        let projection = self
            .graph_projections()?
            .into_iter()
            .find(|projection| projection.name == name)
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))?;
        Ok(RuntimeGraphProjection {
            node_labels: (!projection.node_labels.is_empty()).then_some(projection.node_labels),
            node_types: (!projection.node_types.is_empty()).then_some(projection.node_types),
            edge_labels: (!projection.edge_labels.is_empty()).then_some(projection.edge_labels),
        })
    }

    pub fn save_graph_projection(
        &self,
        name: impl Into<String>,
        projection: RuntimeGraphProjection,
        source: Option<String>,
    ) -> RedDBResult<PhysicalGraphProjection> {
        self.inner
            .db
            .save_graph_projection(
                name,
                projection.node_labels.unwrap_or_default(),
                projection.node_types.unwrap_or_default(),
                projection.edge_labels.unwrap_or_default(),
                source.unwrap_or_else(|| "runtime".to_string()),
            )
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn analytics_jobs(&self) -> RedDBResult<Vec<PhysicalAnalyticsJob>> {
        Ok(self.inner.db.declared_analytics_jobs())
    }

    pub fn operational_analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        self.inner.db.operational_analytics_jobs()
    }

    pub fn record_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: std::collections::BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.inner
            .db
            .record_analytics_job(kind, projection_name, metadata)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn resolve_graph_projection(
        &self,
        projection_name: Option<&str>,
        inline: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<Option<RuntimeGraphProjection>> {
        let named = match projection_name {
            Some(name) => Some(self.graph_projection_named(name)?),
            None => None,
        };
        Ok(merge_runtime_projection(named, inline))
    }

    pub fn apply_retention_policy(&self) -> RedDBResult<()> {
        self.inner
            .db
            .enforce_retention_policy()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn indexes(&self) -> Vec<crate::PhysicalIndexState> {
        self.inner.db.operational_indexes()
    }

    pub fn declared_indexes(&self) -> Vec<crate::PhysicalIndexState> {
        self.inner.db.declared_indexes()
    }

    pub fn declared_indexes_for_collection(&self, collection: &str) -> Vec<crate::PhysicalIndexState> {
        self.inner
            .db
            .declared_indexes()
            .into_iter()
            .filter(|index| index.collection.as_deref() == Some(collection))
            .collect()
    }

    pub fn indexes_for_collection(&self, collection: &str) -> Vec<crate::PhysicalIndexState> {
        self.inner
            .db
            .operational_indexes()
            .into_iter()
            .filter(|index| index.collection.as_deref() == Some(collection))
            .collect()
    }

    pub fn set_index_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> RedDBResult<crate::PhysicalIndexState> {
        self.inner
            .db
            .set_index_enabled(name, enabled)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn warmup_index(&self, name: &str) -> RedDBResult<crate::PhysicalIndexState> {
        self.inner
            .db
            .warmup_index(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn rebuild_indexes(
        &self,
        collection: Option<&str>,
    ) -> RedDBResult<Vec<crate::PhysicalIndexState>> {
        self.inner
            .db
            .rebuild_index_registry(collection)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn graph_neighborhood(
        &self,
        node: &str,
        direction: RuntimeGraphDirection,
        max_depth: usize,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphNeighborhoodResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        ensure_graph_node(&graph, node)?;
        let edge_filters = merge_edge_filters(edge_labels, projection.as_ref());

        let mut visited: HashMap<String, usize> = HashMap::new();
        let mut queue = VecDeque::new();
        let mut nodes = Vec::new();
        let mut edges = Vec::new();
        let mut seen_edges = HashSet::new();

        visited.insert(node.to_string(), 0);
        queue.push_back((node.to_string(), 0usize));

        while let Some((current, depth)) = queue.pop_front() {
            if let Some(stored) = graph.get_node(&current) {
                nodes.push(RuntimeGraphVisit {
                    depth,
                    node: stored_node_to_runtime(stored),
                });
            }

            if depth >= max_depth {
                continue;
            }

            let mut adjacent = graph_adjacent_edges(&graph, &current, direction, edge_filters.as_ref());
            adjacent.sort_by(|left, right| left.0.cmp(&right.0));

            for (neighbor, edge) in adjacent {
                push_runtime_edge(&mut edges, &mut seen_edges, edge);
                if !visited.contains_key(&neighbor) {
                    visited.insert(neighbor.clone(), depth + 1);
                    queue.push_back((neighbor, depth + 1));
                }
            }
        }

        Ok(RuntimeGraphNeighborhoodResult {
            source: node.to_string(),
            direction,
            max_depth,
            nodes,
            edges,
        })
    }

    pub fn graph_traverse(
        &self,
        source: &str,
        direction: RuntimeGraphDirection,
        max_depth: usize,
        strategy: RuntimeGraphTraversalStrategy,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphTraversalResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        ensure_graph_node(&graph, source)?;
        let edge_filters = merge_edge_filters(edge_labels, projection.as_ref());

        let mut visits = Vec::new();
        let mut edges = Vec::new();
        let mut seen_nodes = HashSet::new();
        let mut seen_edges = HashSet::new();

        match strategy {
            RuntimeGraphTraversalStrategy::Bfs => {
                let mut queue = VecDeque::new();
                queue.push_back((source.to_string(), 0usize));
                seen_nodes.insert(source.to_string());

                while let Some((current, depth)) = queue.pop_front() {
                    if let Some(stored) = graph.get_node(&current) {
                        visits.push(RuntimeGraphVisit {
                            depth,
                            node: stored_node_to_runtime(stored),
                        });
                    }

                    if depth >= max_depth {
                        continue;
                    }

                    let mut adjacent =
                        graph_adjacent_edges(&graph, &current, direction, edge_filters.as_ref());
                    adjacent.sort_by(|left, right| left.0.cmp(&right.0));
                    for (neighbor, edge) in adjacent {
                        push_runtime_edge(&mut edges, &mut seen_edges, edge);
                        if seen_nodes.insert(neighbor.clone()) {
                            queue.push_back((neighbor, depth + 1));
                        }
                    }
                }
            }
            RuntimeGraphTraversalStrategy::Dfs => {
                let mut stack = vec![(source.to_string(), 0usize)];
                while let Some((current, depth)) = stack.pop() {
                    if !seen_nodes.insert(current.clone()) {
                        continue;
                    }

                    if let Some(stored) = graph.get_node(&current) {
                        visits.push(RuntimeGraphVisit {
                            depth,
                            node: stored_node_to_runtime(stored),
                        });
                    }

                    if depth >= max_depth {
                        continue;
                    }

                    let mut adjacent =
                        graph_adjacent_edges(&graph, &current, direction, edge_filters.as_ref());
                    adjacent.sort_by(|left, right| right.0.cmp(&left.0));
                    for (neighbor, edge) in adjacent {
                        push_runtime_edge(&mut edges, &mut seen_edges, edge);
                        if !seen_nodes.contains(&neighbor) {
                            stack.push((neighbor, depth + 1));
                        }
                    }
                }
            }
        }

        Ok(RuntimeGraphTraversalResult {
            source: source.to_string(),
            direction,
            strategy,
            max_depth,
            visits,
            edges,
        })
    }

    pub fn graph_shortest_path(
        &self,
        source: &str,
        target: &str,
        direction: RuntimeGraphDirection,
        algorithm: RuntimeGraphPathAlgorithm,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphPathResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        ensure_graph_node(&graph, source)?;
        ensure_graph_node(&graph, target)?;

        let merged_edge_filters = merge_edge_filters(edge_labels, projection.as_ref());
        let path = match (direction, merged_edge_filters.as_ref()) {
            (RuntimeGraphDirection::Outgoing, None) => {
                let result = match algorithm {
                    RuntimeGraphPathAlgorithm::Bfs => BFS::shortest_path(&graph, source, target),
                    RuntimeGraphPathAlgorithm::Dijkstra => {
                        Dijkstra::shortest_path(&graph, source, target)
                    }
                };
                RuntimeGraphPathResult {
                    source: source.to_string(),
                    target: target.to_string(),
                    direction,
                    algorithm,
                    nodes_visited: result.nodes_visited,
                    path: result.path.map(|path| path_to_runtime(&graph, &path)),
                }
            }
            _ => {
                shortest_path_runtime(
                    &graph,
                    source,
                    target,
                    direction,
                    algorithm,
                    merged_edge_filters.as_ref(),
                )?
            }
        };

        Ok(path)
    }

    pub fn graph_components(
        &self,
        mode: RuntimeGraphComponentsMode,
        min_size: usize,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphComponentsResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let min_size = min_size.max(1);
        let components = match mode {
            RuntimeGraphComponentsMode::Connected => ConnectedComponents::find(&graph)
                .components
                .into_iter()
                .filter(|component| component.size >= min_size)
                .map(|component| RuntimeGraphComponent {
                    id: component.id,
                    size: component.size,
                    nodes: component.nodes,
                })
                .collect::<Vec<_>>(),
            RuntimeGraphComponentsMode::Weak => WeaklyConnectedComponents::find(&graph)
                .components
                .into_iter()
                .filter(|component| component.len() >= min_size)
                .enumerate()
                .map(|(index, nodes)| RuntimeGraphComponent {
                    id: format!("wcc:{index}"),
                    size: nodes.len(),
                    nodes,
                })
                .collect::<Vec<_>>(),
            RuntimeGraphComponentsMode::Strong => StronglyConnectedComponents::find(&graph)
                .components
                .into_iter()
                .filter(|component| component.len() >= min_size)
                .enumerate()
                .map(|(index, nodes)| RuntimeGraphComponent {
                    id: format!("scc:{index}"),
                    size: nodes.len(),
                    nodes,
                })
                .collect::<Vec<_>>(),
        };

        Ok(RuntimeGraphComponentsResult {
            mode,
            count: components.len(),
            components,
        })
    }

    pub fn graph_centrality(
        &self,
        algorithm: RuntimeGraphCentralityAlgorithm,
        top_k: usize,
        normalize: bool,
        max_iterations: Option<usize>,
        epsilon: Option<f64>,
        alpha: Option<f64>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCentralityResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let top_k = top_k.max(1);

        match algorithm {
            RuntimeGraphCentralityAlgorithm::Degree => {
                let result = DegreeCentrality::compute(&graph);
                let mut degree_scores = Vec::new();
                let mut pairs: Vec<_> = result
                    .total_degree
                    .iter()
                    .map(|(node_id, total_degree)| (node_id.clone(), *total_degree))
                    .collect();
                pairs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
                pairs.truncate(top_k);

                for (node_id, total_degree) in pairs {
                    if let Some(node) = graph.get_node(&node_id) {
                        degree_scores.push(RuntimeGraphDegreeScore {
                            node: stored_node_to_runtime(node),
                            in_degree: result.in_degree.get(&node_id).copied().unwrap_or(0),
                            out_degree: result.out_degree.get(&node_id).copied().unwrap_or(0),
                            total_degree,
                        });
                    }
                }

                Ok(RuntimeGraphCentralityResult {
                    algorithm,
                    normalized: None,
                    iterations: None,
                    converged: None,
                    scores: Vec::new(),
                    degree_scores,
                })
            }
            RuntimeGraphCentralityAlgorithm::Closeness => {
                let result = ClosenessCentrality::compute(&graph);
                Ok(RuntimeGraphCentralityResult {
                    algorithm,
                    normalized: None,
                    iterations: None,
                    converged: None,
                    scores: top_runtime_scores(&graph, result.scores, top_k),
                    degree_scores: Vec::new(),
                })
            }
            RuntimeGraphCentralityAlgorithm::Betweenness => {
                let result = BetweennessCentrality::compute(&graph, normalize);
                Ok(RuntimeGraphCentralityResult {
                    algorithm,
                    normalized: Some(result.normalized),
                    iterations: None,
                    converged: None,
                    scores: top_runtime_scores(&graph, result.scores, top_k),
                    degree_scores: Vec::new(),
                })
            }
            RuntimeGraphCentralityAlgorithm::Eigenvector => {
                let mut runner = EigenvectorCentrality::new();
                if let Some(max_iterations) = max_iterations {
                    runner.max_iterations = max_iterations.max(1);
                }
                if let Some(epsilon) = epsilon {
                    runner.epsilon = epsilon.max(0.0);
                }
                let result = runner.compute(&graph);
                Ok(RuntimeGraphCentralityResult {
                    algorithm,
                    normalized: None,
                    iterations: Some(result.iterations),
                    converged: Some(result.converged),
                    scores: top_runtime_scores(&graph, result.scores, top_k),
                    degree_scores: Vec::new(),
                })
            }
            RuntimeGraphCentralityAlgorithm::PageRank => {
                let mut runner = PageRank::new();
                if let Some(max_iterations) = max_iterations {
                    runner = runner.max_iterations(max_iterations.max(1));
                }
                if let Some(alpha) = alpha {
                    runner = runner.alpha(alpha);
                }
                if let Some(epsilon) = epsilon {
                    runner = runner.epsilon(epsilon);
                }
                let result = runner.run(&graph);
                Ok(RuntimeGraphCentralityResult {
                    algorithm,
                    normalized: None,
                    iterations: Some(result.iterations),
                    converged: Some(result.converged),
                    scores: top_runtime_scores(&graph, result.scores, top_k),
                    degree_scores: Vec::new(),
                })
            }
        }
    }

    pub fn graph_communities(
        &self,
        algorithm: RuntimeGraphCommunityAlgorithm,
        min_size: usize,
        max_iterations: Option<usize>,
        resolution: Option<f64>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCommunityResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let min_size = min_size.max(1);

        match algorithm {
            RuntimeGraphCommunityAlgorithm::LabelPropagation => {
                let mut runner = LabelPropagation::new();
                if let Some(max_iterations) = max_iterations {
                    runner = runner.max_iterations(max_iterations.max(1));
                }
                let result = runner.run(&graph);
                let communities = result
                    .communities
                    .into_iter()
                    .filter(|community| community.size >= min_size)
                    .map(|community| RuntimeGraphCommunity {
                        id: community.label,
                        size: community.size,
                        nodes: community.nodes,
                    })
                    .collect::<Vec<_>>();
                Ok(RuntimeGraphCommunityResult {
                    algorithm,
                    count: communities.len(),
                    iterations: Some(result.iterations),
                    converged: Some(result.converged),
                    modularity: None,
                    passes: None,
                    communities,
                })
            }
            RuntimeGraphCommunityAlgorithm::Louvain => {
                let mut runner = Louvain::new();
                if let Some(max_iterations) = max_iterations {
                    runner = runner.max_iterations(max_iterations.max(1));
                }
                if let Some(resolution) = resolution {
                    runner = runner.resolution(resolution.max(0.0));
                }
                let result = runner.run(&graph);
                let mut communities = result
                    .community_sizes()
                    .into_iter()
                    .filter(|(_, size)| *size >= min_size)
                    .map(|(id, size)| RuntimeGraphCommunity {
                        id: format!("community:{id}"),
                        size,
                        nodes: result.get_community(id),
                    })
                    .collect::<Vec<_>>();
                communities.sort_by(|left, right| {
                    right.size.cmp(&left.size).then_with(|| left.id.cmp(&right.id))
                });
                Ok(RuntimeGraphCommunityResult {
                    algorithm,
                    count: communities.len(),
                    iterations: None,
                    converged: None,
                    modularity: Some(result.modularity),
                    passes: Some(result.passes),
                    communities,
                })
            }
        }
    }

    pub fn graph_clustering(
        &self,
        top_k: usize,
        include_triangles: bool,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphClusteringResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let top_k = top_k.max(1);
        let result = ClusteringCoefficient::compute(&graph);
        let triangle_count = if include_triangles {
            Some(crate::storage::engine::TriangleCounting::count(&graph).count)
        } else {
            None
        };

        Ok(RuntimeGraphClusteringResult {
            global: result.global,
            local: top_runtime_scores(&graph, result.local, top_k),
            triangle_count,
        })
    }

    pub fn graph_personalized_pagerank(
        &self,
        seeds: Vec<String>,
        top_k: usize,
        alpha: Option<f64>,
        epsilon: Option<f64>,
        max_iterations: Option<usize>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCentralityResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        if seeds.is_empty() {
            return Err(RedDBError::Query("personalized pagerank requires at least one seed".to_string()));
        }
        for seed in &seeds {
            ensure_graph_node(&graph, seed)?;
        }

        let mut runner = PersonalizedPageRank::new(seeds);
        if let Some(alpha) = alpha {
            runner = runner.alpha(alpha);
        }
        if let Some(epsilon) = epsilon {
            runner = runner.epsilon(epsilon);
        }
        if let Some(max_iterations) = max_iterations {
            runner = runner.max_iterations(max_iterations.max(1));
        }
        let result = runner.run(&graph);

        Ok(RuntimeGraphCentralityResult {
            algorithm: RuntimeGraphCentralityAlgorithm::PageRank,
            normalized: None,
            iterations: Some(result.iterations),
            converged: Some(result.converged),
            scores: top_runtime_scores(&graph, result.scores, top_k.max(1)),
            degree_scores: Vec::new(),
        })
    }

    pub fn graph_hits(
        &self,
        top_k: usize,
        epsilon: Option<f64>,
        max_iterations: Option<usize>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphHitsResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let mut runner = HITS::new();
        if let Some(epsilon) = epsilon {
            runner.epsilon = epsilon.max(0.0);
        }
        if let Some(max_iterations) = max_iterations {
            runner.max_iterations = max_iterations.max(1);
        }
        let result = runner.compute(&graph);

        Ok(RuntimeGraphHitsResult {
            iterations: result.iterations,
            converged: result.converged,
            hubs: top_runtime_scores(&graph, result.hub_scores, top_k.max(1)),
            authorities: top_runtime_scores(&graph, result.authority_scores, top_k.max(1)),
        })
    }

    pub fn graph_cycles(
        &self,
        max_length: usize,
        max_cycles: usize,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCyclesResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let result = CycleDetector::new()
            .max_length(max_length.max(2))
            .max_cycles(max_cycles.max(1))
            .find(&graph);

        Ok(RuntimeGraphCyclesResult {
            limit_reached: result.limit_reached,
            cycles: result
                .cycles
                .into_iter()
                .map(|cycle| cycle_to_runtime(&graph, cycle))
                .collect(),
        })
    }

    pub fn graph_topological_sort(
        &self,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphTopologicalSortResult> {
        let graph = materialize_graph_with_projection(self.inner.db.store().as_ref(), projection.as_ref())?;
        let ordered_nodes = match DFS::topological_sort(&graph) {
            Some(order) => order
                .into_iter()
                .filter_map(|id| graph.get_node(&id))
                .map(stored_node_to_runtime)
                .collect(),
            None => Vec::new(),
        };

        Ok(RuntimeGraphTopologicalSortResult {
            acyclic: !ordered_nodes.is_empty() || graph.node_count() == 0,
            ordered_nodes,
        })
    }
}

impl HealthProvider for RedDBRuntime {
    fn health(&self) -> HealthReport {
        let pool = self.inner.pool.lock().unwrap();
        let mut report = self.inner.db.health();
        report = report.with_diagnostic("runtime.mode", if self.inner.layout.is_persistent() {
            "persistent"
        } else {
            "in-memory"
        });
        report = report.with_diagnostic("runtime.active_connections", pool.active.to_string());
        report = report.with_diagnostic("runtime.idle_connections", pool.idle.len().to_string());
        report.with_diagnostic(
            "runtime.max_connections",
            self.inner.pool_config.max_connections.to_string(),
        )
    }
}

impl RuntimeConnection {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn db(&self) -> Arc<RedDB> {
        Arc::clone(&self.inner.db)
    }

    pub fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        RedDBRuntime {
            inner: Arc::clone(&self.inner),
        }
        .scan_collection(collection, cursor, limit)
    }
}

impl Drop for RuntimeConnection {
    fn drop(&mut self) {
        let mut pool = self.inner.pool.lock().unwrap();
        pool.active = pool.active.saturating_sub(1);
        if pool.idle.len() < self.inner.pool_config.max_idle {
            pool.idle.push(self.id);
        }
    }
}

fn execute_runtime_table_query(db: &RedDB, query: &TableQuery) -> RedDBResult<UnifiedResult> {
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);
    let records = scan_runtime_table_records(db, query)?;

    let projected_records: Vec<UnifiedRecord> = records
        .iter()
        .map(|record| project_runtime_record(record, &query.columns, Some(table_name), Some(table_alias)))
        .collect();
    let columns = projected_columns(&projected_records, &query.columns);

    Ok(UnifiedResult {
        columns,
        records: projected_records,
        stats: Default::default(),
    })
}

fn execute_runtime_join_query(db: &RedDB, query: &JoinQuery) -> RedDBResult<UnifiedResult> {
    let left_query = match query.left.as_ref() {
        QueryExpr::Table(table) => table,
        _ => {
            return Err(RedDBError::Query(
                "runtime joins currently require a table expression on the left side".to_string(),
            ))
        }
    };

    let left_table_name = left_query.table.as_str();
    let left_table_alias = left_query.alias.as_deref().unwrap_or(left_table_name);
    let left_records = scan_runtime_table_records(db, left_query)?;

    let (right_records, right_table_name, right_table_alias) = match query.right.as_ref() {
        QueryExpr::Graph(_) | QueryExpr::Path(_) => {
            let graph = materialize_graph(db.store().as_ref())?;
            let result = crate::storage::query::unified::UnifiedExecutor::execute_on(
                &graph,
                query.right.as_ref(),
            )
            .map_err(|err| RedDBError::Query(err.to_string()))?;
            (result.records, None, None)
        }
        QueryExpr::Table(table) => (
            scan_runtime_table_records(db, table)?,
            Some(table.table.as_str()),
            Some(table.alias.as_deref().unwrap_or(table.table.as_str())),
        ),
        other => {
            return Err(RedDBError::Query(format!(
                "runtime joins do not yet support right-side {} expressions",
                query_expr_name(other)
            )))
        }
    };

    let mut matched_right = vec![false; right_records.len()];
    let mut records = Vec::new();

    for left_record in &left_records {
        let mut matched = false;
        for (index, right_record) in right_records.iter().enumerate() {
            if join_condition_matches(
                left_record,
                Some(left_table_name),
                Some(left_table_alias),
                right_record,
                right_table_name,
                right_table_alias,
                query,
            ) {
                matched = true;
                matched_right[index] = true;
                records.push(merge_join_records(
                    Some(left_record),
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }

        if !matched && matches!(query.join_type, JoinType::LeftOuter) {
            records.push(merge_join_records(Some(left_record), None, left_query, None));
        }
    }

    if matches!(query.join_type, JoinType::RightOuter) {
        for (matched, right_record) in matched_right.into_iter().zip(right_records.iter()) {
            if !matched {
                records.push(merge_join_records(
                    None,
                    Some(right_record),
                    left_query,
                    right_table_alias.or(right_table_name),
                ));
            }
        }
    }

    let columns = collect_visible_columns(&records);

    Ok(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
    })
}

fn execute_runtime_vector_query(db: &RedDB, query: &VectorQuery) -> RedDBResult<UnifiedResult> {
    let vector = resolve_runtime_vector_source(db, &query.query_vector)?;
    let min_score = query.threshold.unwrap_or(f32::MIN);
    let matches = runtime_vector_matches(db, query, &vector)?
        .into_iter()
        .filter(|item| item.score >= min_score)
        .collect::<Vec<_>>();

    let records = matches
        .into_iter()
        .map(runtime_vector_record_from_match)
        .collect();

    Ok(UnifiedResult {
        columns: vec![
            "entity_id".to_string(),
            "score".to_string(),
            "collection".to_string(),
            "content".to_string(),
            "dimension".to_string(),
        ],
        records,
        stats: Default::default(),
    })
}

fn runtime_vector_matches(
    db: &RedDB,
    query: &VectorQuery,
    vector: &[f32],
) -> RedDBResult<Vec<SimilarResult>> {
    if query.filter.is_none() {
        return Ok(db.similar(&query.collection, vector, query.k.max(1)));
    }

    let manager = db
        .store()
        .get_collection(&query.collection)
        .ok_or_else(|| RedDBError::NotFound(query.collection.clone()))?;
    let filter = query.filter.as_ref().unwrap();

    let mut results: Vec<SimilarResult> = manager
        .query_all(|_| true)
        .into_iter()
        .filter(|entity| runtime_vector_entity_matches_filter(db, &query.collection, entity, filter))
        .filter_map(|entity| {
            let score = runtime_entity_vector_similarity(&entity, vector);
            (score > 0.0).then_some(SimilarResult {
                entity_id: entity.id,
                score,
                entity,
            })
        })
        .collect();

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(Ordering::Equal)
    });
    results.truncate(query.k.max(1));
    Ok(results)
}

fn execute_runtime_hybrid_query(db: &RedDB, query: &HybridQuery) -> RedDBResult<UnifiedResult> {
    let structured = execute_runtime_expr(db, query.structured.as_ref())?;
    let vector = execute_runtime_vector_query(db, &query.vector)?;

    let mut structured_map = HashMap::new();
    let mut structured_rank = HashMap::new();
    for (index, record) in structured.records.iter().cloned().enumerate() {
        if let Some(key) = runtime_record_identity_key(&record) {
            structured_rank.insert(key.clone(), index);
            structured_map.insert(key, record);
        }
    }

    let mut vector_map = HashMap::new();
    let mut vector_rank = HashMap::new();
    for (index, record) in vector.records.iter().cloned().enumerate() {
        if let Some(key) = runtime_record_identity_key(&record) {
            vector_rank.insert(key.clone(), index);
            vector_map.insert(key, record);
        }
    }

    let ordered_keys = hybrid_candidate_keys(
        &structured_map,
        &vector_map,
        &query.fusion,
    );

    let mut scored_records = Vec::new();
    for key in ordered_keys {
        let structured_record = structured_map.get(&key);
        let vector_record = vector_map.get(&key);
        let s_rank = structured_rank.get(&key).copied();
        let v_rank = vector_rank.get(&key).copied();
        let s_score = structured_record
            .as_ref()
            .map_or(0.0, |record| runtime_structured_score(record, s_rank));
        let v_score = vector_record
            .as_ref()
            .map_or(0.0, runtime_vector_score);

        let score = match &query.fusion {
            FusionStrategy::Rerank { weight } => {
                if structured_record.is_none() {
                    continue;
                }
                ((1.0 - *weight as f64) * s_score) + ((*weight as f64) * v_score)
            }
            FusionStrategy::FilterThenSearch | FusionStrategy::SearchThenFilter => {
                if structured_record.is_none() || vector_record.is_none() {
                    continue;
                }
                v_score
            }
            FusionStrategy::Intersection => {
                if structured_record.is_none() || vector_record.is_none() {
                    continue;
                }
                (s_score + v_score) / 2.0
            }
            FusionStrategy::Union {
                structured_weight,
                vector_weight,
            } => ((*structured_weight as f64) * s_score) + ((*vector_weight as f64) * v_score),
            FusionStrategy::RRF { k } => {
                let mut total = 0.0;
                if let Some(rank) = s_rank {
                    total += 1.0 / (*k as f64 + rank as f64 + 1.0);
                }
                if let Some(rank) = v_rank {
                    total += 1.0 / (*k as f64 + rank as f64 + 1.0);
                }
                total
            }
        };

        let mut record = merge_hybrid_records(structured_record, vector_record);
        record.set("hybrid_score", Value::Float(score));
        record.set(
            "structured_score",
            if structured_record.is_some() {
                Value::Float(s_score)
            } else {
                Value::Null
            },
        );
        record.set(
            "vector_score",
            if vector_record.is_some() {
                Value::Float(v_score)
            } else {
                Value::Null
            },
        );
        record.set(
            "structured_rank",
            s_rank
                .map(|value| Value::UnsignedInteger(value as u64))
                .unwrap_or(Value::Null),
        );
        record.set(
            "vector_rank",
            v_rank
                .map(|value| Value::UnsignedInteger(value as u64))
                .unwrap_or(Value::Null),
        );
        scored_records.push((score, record));
    }

    scored_records.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(Ordering::Equal)
    });

    let mut records: Vec<UnifiedRecord> = scored_records.into_iter().map(|(_, record)| record).collect();
    if let Some(limit) = query.limit {
        records.truncate(limit);
    }

    Ok(UnifiedResult {
        columns: collect_visible_columns(&records),
        records,
        stats: Default::default(),
    })
}

fn execute_runtime_expr(db: &RedDB, expr: &QueryExpr) -> RedDBResult<UnifiedResult> {
    match expr {
        QueryExpr::Graph(_) | QueryExpr::Path(_) => {
            let graph = materialize_graph(db.store().as_ref())?;
            crate::storage::query::unified::UnifiedExecutor::execute_on(&graph, expr)
                .map_err(|err| RedDBError::Query(err.to_string()))
        }
        QueryExpr::Table(table) => execute_runtime_table_query(db, table),
        QueryExpr::Join(join) => execute_runtime_join_query(db, join),
        QueryExpr::Vector(vector) => execute_runtime_vector_query(db, vector),
        QueryExpr::Hybrid(hybrid) => execute_runtime_hybrid_query(db, hybrid),
    }
}

fn scan_runtime_table_records(db: &RedDB, query: &TableQuery) -> RedDBResult<Vec<UnifiedRecord>> {
    let manager = db
        .store()
        .get_collection(&query.table)
        .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);

    let mut records: Vec<UnifiedRecord> = manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(runtime_table_record_from_entity)
        .collect();

    if let Some(filter) = query.filter.as_ref() {
        records.retain(|record| {
            evaluate_runtime_filter(record, filter, Some(table_name), Some(table_alias))
        });
    }

    if !query.order_by.is_empty() {
        records.sort_by(|left, right| {
            compare_runtime_order(
                left,
                right,
                &query.order_by,
                Some(table_name),
                Some(table_alias),
            )
        });
    }

    let offset = query.offset.unwrap_or(0) as usize;
    let limit = query.limit.map(|value| value as usize);
    let iter = records.into_iter().skip(offset);
    Ok(match limit {
        Some(limit) => iter.take(limit).collect(),
        None => iter.collect(),
    })
}

fn runtime_table_record_from_entity(entity: UnifiedEntity) -> Option<UnifiedRecord> {
    let row = match entity.data {
        EntityData::Row(row) => row,
        _ => return None,
    };

    let mut record = UnifiedRecord::new();

    if let EntityKind::TableRow { row_id, .. } = &entity.kind {
        record.set("row_id", Value::UnsignedInteger(*row_id));
    }

    record.set("_entity_id", Value::UnsignedInteger(entity.id.raw()));
    record.set("_collection", Value::Text(entity.kind.collection().to_string()));
    record.set("_kind", Value::Text(entity.kind.storage_type().to_string()));
    record.set("_created_at", Value::UnsignedInteger(entity.created_at));
    record.set("_updated_at", Value::UnsignedInteger(entity.updated_at));
    record.set("_sequence_id", Value::UnsignedInteger(entity.sequence_id));

    if let Some(named) = row.named {
        for (key, value) in named {
            record.set(&key, value);
        }
    } else {
        for (index, value) in row.columns.into_iter().enumerate() {
            record.set(&format!("c{index}"), value);
        }
    }

    Some(record)
}

fn resolve_runtime_vector_source(db: &RedDB, source: &VectorSource) -> RedDBResult<Vec<f32>> {
    match source {
        VectorSource::Literal(vector) => Ok(vector.clone()),
        VectorSource::Reference {
            collection: _,
            vector_id,
        } => {
            let entity = db
                .get(EntityId::new(*vector_id))
                .ok_or_else(|| RedDBError::NotFound(format!("vector:{vector_id}")))?;
            match entity.data {
                EntityData::Vector(data) => Ok(data.dense),
                _ => Err(RedDBError::Query(format!(
                    "entity {vector_id} is not a vector source"
                ))),
            }
        }
        VectorSource::Text(_) => Err(RedDBError::Query(
            "text-to-embedding vector queries are parsed but not yet wired into /query"
                .to_string(),
        )),
        VectorSource::Subquery(_) => Err(RedDBError::Query(
            "subquery vector sources are parsed but not yet wired into /query".to_string(),
        )),
    }
}

fn runtime_vector_record_from_match(item: SimilarResult) -> UnifiedRecord {
    let mut record = UnifiedRecord::new();
    record.set("entity_id", Value::UnsignedInteger(item.entity_id.raw()));
    record.set("_entity_id", Value::UnsignedInteger(item.entity_id.raw()));
    record.set("score", Value::Float(item.score as f64));
    record.set(
        "collection",
        Value::Text(item.entity.kind.collection().to_string()),
    );
    record.set(
        "_collection",
        Value::Text(item.entity.kind.collection().to_string()),
    );
    record.set(
        "_kind",
        Value::Text(item.entity.kind.storage_type().to_string()),
    );
    apply_runtime_identity_hints(&mut record, &item.entity);

    match item.entity.data {
        EntityData::Vector(data) => {
            record.set("dimension", Value::UnsignedInteger(data.dense.len() as u64));
            if let Some(content) = data.content {
                record.set("content", Value::Text(content));
            } else {
                record.set("content", Value::Null);
            }
        }
        EntityData::Row(row) => {
            record.set("dimension", Value::Null);
            if let Some(named) = row.named {
                for (key, value) in named {
                    record.set(&key, value);
                }
            }
        }
        EntityData::Node(node) => {
            record.set("dimension", Value::Null);
            for (key, value) in node.properties {
                record.set(&key, value);
            }
        }
        EntityData::Edge(edge) => {
            record.set("dimension", Value::Null);
            record.set("weight", Value::Float(edge.weight as f64));
            for (key, value) in edge.properties {
                record.set(&key, value);
            }
        }
    }

    record
}

fn hybrid_candidate_keys(
    structured: &HashMap<String, UnifiedRecord>,
    vector: &HashMap<String, UnifiedRecord>,
    fusion: &FusionStrategy,
) -> Vec<String> {
    let structured_keys: BTreeSet<String> = structured.keys().cloned().collect();
    let vector_keys: BTreeSet<String> = vector.keys().cloned().collect();

    match fusion {
        FusionStrategy::Rerank { .. } => structured_keys.into_iter().collect(),
        FusionStrategy::FilterThenSearch | FusionStrategy::SearchThenFilter | FusionStrategy::Intersection => {
            structured_keys
                .intersection(&vector_keys)
                .cloned()
                .collect()
        }
        FusionStrategy::Union { .. } | FusionStrategy::RRF { .. } => structured_keys
            .union(&vector_keys)
            .cloned()
            .collect(),
    }
}

fn runtime_record_identity_key(record: &UnifiedRecord) -> Option<String> {
    for key in [
        "_source_row",
        "_source_node",
        "_source_edge",
        "_source_entity",
        "_linked_identity",
    ] {
        if let Some(value) = record.values.get(key) {
            return Some(format!("link:{}", runtime_identity_fragment(value)?));
        }
    }

    if let Some(value) = record.values.get("entity_id").or_else(|| record.values.get("_entity_id")) {
        return Some(format!("entity:{}", runtime_identity_fragment(value)?));
    }

    if let (Some(collection), Some(row_id)) = (
        record.values.get("_collection").and_then(runtime_value_text),
        record.values.get("row_id").or_else(|| record.values.get("id")),
    ) {
        return Some(format!(
            "row:{collection}:{}",
            runtime_identity_fragment(row_id)?
        ));
    }

    if let Some((alias, node)) = record.nodes.iter().next() {
        return Some(format!("node:{alias}:{}", node.id));
    }

    if let Some(value) = record
        .values
        .iter()
        .find_map(|(key, value)| key.ends_with(".id").then_some(value))
    {
        return Some(format!("ref:{}", runtime_identity_fragment(value)?));
    }

    if let Some(value) = record.values.get("id") {
        return Some(format!("id:{}", runtime_identity_fragment(value)?));
    }

    record
        .paths
        .first()
        .and_then(|path| path.nodes.first())
        .map(|node| format!("path:{node}"))
}

fn runtime_identity_fragment(value: &Value) -> Option<String> {
    match value {
        Value::Integer(value) => Some(value.to_string()),
        Value::UnsignedInteger(value) => Some(value.to_string()),
        Value::Float(value) => Some(value.to_string()),
        Value::Text(value) => Some(value.clone()),
        Value::NodeRef(value) => Some(value.clone()),
        Value::EdgeRef(value) => Some(value.clone()),
        Value::RowRef(table, row_id) => Some(format!("{table}:{row_id}")),
        Value::VectorRef(collection, vector_id) => Some(format!("{collection}:{vector_id}")),
        _ => runtime_value_text(value),
    }
}

fn apply_runtime_identity_hints(record: &mut UnifiedRecord, entity: &UnifiedEntity) {
    for cross_ref in &entity.cross_refs {
        let value = match cross_ref.ref_type {
            RefType::VectorToRow | RefType::NodeToRow => Some(Value::RowRef(
                cross_ref.target_collection.clone(),
                cross_ref.target.raw(),
            )),
            RefType::VectorToNode | RefType::RowToNode => Some(Value::NodeRef(format!(
                "{}:{}",
                cross_ref.target_collection, cross_ref.target
            ))),
            RefType::RowToEdge | RefType::EdgeToVector => Some(Value::EdgeRef(format!(
                "{}:{}",
                cross_ref.target_collection, cross_ref.target
            ))),
            _ => Some(Value::Text(format!(
                "{}:{}",
                cross_ref.target_collection, cross_ref.target
            ))),
        };

        if let Some(value) = value {
            match cross_ref.ref_type {
                RefType::VectorToRow | RefType::NodeToRow => {
                    record.values.insert("_source_row".to_string(), value.clone());
                    record
                        .values
                        .entry("_linked_identity".to_string())
                        .or_insert(value);
                }
                RefType::VectorToNode | RefType::RowToNode => {
                    record.values.insert("_source_node".to_string(), value.clone());
                    record
                        .values
                        .entry("_linked_identity".to_string())
                        .or_insert(value);
                }
                RefType::RowToEdge | RefType::EdgeToVector => {
                    record.values.insert("_source_edge".to_string(), value.clone());
                    record
                        .values
                        .entry("_linked_identity".to_string())
                        .or_insert(value);
                }
                _ => {
                    record
                        .values
                        .entry("_source_entity".to_string())
                        .or_insert(value.clone());
                    record
                        .values
                        .entry("_linked_identity".to_string())
                        .or_insert(value);
                }
            }
        }
    }
}

fn runtime_vector_entity_matches_filter(
    db: &RedDB,
    collection: &str,
    entity: &UnifiedEntity,
    filter: &VectorMetadataFilter,
) -> bool {
    let metadata = db
        .store()
        .get_metadata(collection, entity.id)
        .unwrap_or_else(Metadata::new);
    let entry = runtime_metadata_entry(&metadata);
    filter.matches(&entry)
}

fn runtime_metadata_entry(metadata: &Metadata) -> MetadataEntry {
    let mut entry = MetadataEntry::new();
    for (key, value) in metadata.iter() {
        if let Some(converted) = runtime_vector_metadata_value(value) {
            entry.insert(key.clone(), converted);
        }
    }
    entry
}

fn runtime_vector_metadata_value(value: &UnifiedMetadataValue) -> Option<VectorMetadataValue> {
    match value {
        UnifiedMetadataValue::Null => Some(VectorMetadataValue::Null),
        UnifiedMetadataValue::Bool(value) => Some(VectorMetadataValue::Bool(*value)),
        UnifiedMetadataValue::Int(value) => Some(VectorMetadataValue::Integer(*value)),
        UnifiedMetadataValue::Float(value) => Some(VectorMetadataValue::Float(*value)),
        UnifiedMetadataValue::String(value) => Some(VectorMetadataValue::String(value.clone())),
        UnifiedMetadataValue::Timestamp(value) => Some(VectorMetadataValue::Integer(*value as i64)),
        UnifiedMetadataValue::Reference(target) => {
            Some(VectorMetadataValue::String(runtime_ref_target_string(target)))
        }
        UnifiedMetadataValue::References(targets) => Some(VectorMetadataValue::String(
            targets
                .iter()
                .map(runtime_ref_target_string)
                .collect::<Vec<_>>()
                .join(","),
        )),
        UnifiedMetadataValue::Array(values) => Some(VectorMetadataValue::String(
            values
                .iter()
                .filter_map(runtime_vector_metadata_value)
                .map(|value| match value {
                    VectorMetadataValue::String(value) => value,
                    VectorMetadataValue::Integer(value) => value.to_string(),
                    VectorMetadataValue::Float(value) => value.to_string(),
                    VectorMetadataValue::Bool(value) => value.to_string(),
                    VectorMetadataValue::Null => "null".to_string(),
                })
                .collect::<Vec<_>>()
                .join(","),
        )),
        UnifiedMetadataValue::Object(_) | UnifiedMetadataValue::Bytes(_) | UnifiedMetadataValue::Geo { .. } => None,
    }
}

fn runtime_ref_target_string(target: &RefTarget) -> String {
    match target {
        RefTarget::TableRow { table, row_id } => format!("{table}:{row_id}"),
        RefTarget::Node {
            collection,
            node_id,
        } => format!("{collection}:{node_id}"),
        RefTarget::Edge {
            collection,
            edge_id,
        } => format!("{collection}:{edge_id}"),
        RefTarget::Vector {
            collection,
            vector_id,
        } => format!("{collection}:{vector_id}"),
        RefTarget::Entity {
            collection,
            entity_id,
        } => format!("{collection}:{entity_id}"),
    }
}

fn runtime_entity_vector_similarity(entity: &UnifiedEntity, query: &[f32]) -> f32 {
    let mut best_similarity = 0.0f32;

    for emb in &entity.embeddings {
        best_similarity = best_similarity.max(cosine_similarity(query, &emb.vector));
    }

    if let EntityData::Vector(vec_data) = &entity.data {
        best_similarity = best_similarity.max(cosine_similarity(query, &vec_data.dense));
    }

    best_similarity
}

fn runtime_structured_score(record: &UnifiedRecord, rank: Option<usize>) -> f64 {
    if let Some(value) = record.values.get("score").or_else(|| record.values.get("hybrid_score")) {
        if let Some(number) = runtime_value_number(value) {
            return number;
        }
    }

    rank.map(|value| 1.0 / (value as f64 + 1.0)).unwrap_or(0.0)
}

fn runtime_vector_score(record: &UnifiedRecord) -> f64 {
    record
        .values
        .get("score")
        .and_then(runtime_value_number)
        .unwrap_or(0.0)
}

fn merge_hybrid_records(
    structured: Option<&UnifiedRecord>,
    vector: Option<&UnifiedRecord>,
) -> UnifiedRecord {
    let mut merged = structured.cloned().unwrap_or_default();

    if let Some(vector_record) = vector {
        for (key, value) in &vector_record.values {
            if let Some(existing) = merged.values.get(key) {
                if existing != value {
                    merged.values.insert(format!("vector.{key}"), value.clone());
                }
            } else {
                merged.values.insert(key.clone(), value.clone());
            }
        }

        for (alias, node) in &vector_record.nodes {
            merged.nodes.entry(alias.clone()).or_insert_with(|| node.clone());
        }
        for (alias, edge) in &vector_record.edges {
            merged.edges.entry(alias.clone()).or_insert_with(|| edge.clone());
        }
        merged.paths.extend(vector_record.paths.clone());
        merged
            .vector_results
            .extend(vector_record.vector_results.clone());
    }

    merged
}

fn merge_join_records(
    left: Option<&UnifiedRecord>,
    right: Option<&UnifiedRecord>,
    left_query: &TableQuery,
    right_prefix: Option<&str>,
) -> UnifiedRecord {
    let left_table_name = left_query.table.as_str();
    let left_table_alias = left_query.alias.as_deref().unwrap_or(left_table_name);
    let mut merged = UnifiedRecord::new();

    if let Some(left_record) = left {
        merged = project_runtime_record(
            left_record,
            &left_query.columns,
            Some(left_table_name),
            Some(left_table_alias),
        );
    }

    if let Some(right_record) = right {
        for (key, value) in &right_record.values {
            if merged.values.contains_key(key) {
                if let Some(prefix) = right_prefix {
                    merged.values.insert(format!("{prefix}.{key}"), value.clone());
                }
            } else {
                merged.values.insert(key.clone(), value.clone());
            }
        }

        for (alias, node) in &right_record.nodes {
            merged.nodes.insert(alias.clone(), node.clone());
        }
        for (alias, edge) in &right_record.edges {
            merged.edges.insert(alias.clone(), edge.clone());
        }
        merged.paths.extend(right_record.paths.clone());
        merged
            .vector_results
            .extend(right_record.vector_results.clone());
    }

    merged
}

fn join_condition_matches(
    left_record: &UnifiedRecord,
    left_table_name: Option<&str>,
    left_table_alias: Option<&str>,
    right_record: &UnifiedRecord,
    right_table_name: Option<&str>,
    right_table_alias: Option<&str>,
    query: &JoinQuery,
) -> bool {
    let left_value = resolve_runtime_field(
        left_record,
        &query.on.left_field,
        left_table_name,
        left_table_alias,
    );
    let right_value = resolve_runtime_field(
        right_record,
        &query.on.right_field,
        right_table_name,
        right_table_alias,
    );

    match (left_value.as_ref(), right_value.as_ref()) {
        (Some(left), Some(right)) => compare_runtime_values(left, right, CompareOp::Eq),
        _ => false,
    }
}

fn project_runtime_record(
    source: &UnifiedRecord,
    projections: &[Projection],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> UnifiedRecord {
    let select_all = projections.is_empty() || projections.iter().any(|item| matches!(item, Projection::All));
    let mut record = UnifiedRecord::new();
    record.nodes = source.nodes.clone();
    record.edges = source.edges.clone();
    record.paths = source.paths.clone();
    record.vector_results = source.vector_results.clone();

    if select_all {
        for key in visible_value_keys(source) {
            if let Some(value) = source.values.get(&key) {
                record.values.insert(key, value.clone());
            }
        }
    }

    for projection in projections {
        if matches!(projection, Projection::All) {
            continue;
        }

        let label = projection_name(projection);
        let value = match projection {
            Projection::Column(column) => source.values.get(column).cloned(),
            Projection::Alias(column, _) => source.values.get(column).cloned(),
            Projection::Field(field, _) => {
                resolve_runtime_field(source, field, table_name, table_alias)
            }
            Projection::Expression(filter, _) => Some(Value::Boolean(evaluate_runtime_filter(
                source, filter, table_name, table_alias,
            ))),
            Projection::Function(_, _) => Some(Value::Null),
            Projection::All => None,
        };

        record
            .values
            .insert(label, value.unwrap_or(Value::Null));
    }

    record
}

fn projected_columns(records: &[UnifiedRecord], projections: &[Projection]) -> Vec<String> {
    if projections.is_empty() || projections.iter().any(|item| matches!(item, Projection::All)) {
        return collect_visible_columns(records);
    }

    projections
        .iter()
        .filter(|projection| !matches!(projection, Projection::All))
        .map(projection_name)
        .collect()
}

fn collect_visible_columns(records: &[UnifiedRecord]) -> Vec<String> {
    let mut columns = BTreeSet::new();
    for record in records {
        for key in visible_value_keys(record) {
            columns.insert(key);
        }
    }
    columns.into_iter().collect()
}

fn visible_value_keys(record: &UnifiedRecord) -> Vec<String> {
    let mut keys: Vec<String> = record
        .values
        .keys()
        .filter(|key| !key.starts_with('_'))
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn projection_name(projection: &Projection) -> String {
    match projection {
        Projection::All => "*".to_string(),
        Projection::Column(column) => column.clone(),
        Projection::Alias(_, alias) => alias.clone(),
        Projection::Function(name, _) => name.clone(),
        Projection::Expression(_, alias) => alias.clone().unwrap_or_else(|| "expr".to_string()),
        Projection::Field(field, alias) => alias.clone().unwrap_or_else(|| field_ref_name(field)),
    }
}

fn field_ref_name(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } => {
            if table.is_empty() {
                column.clone()
            } else {
                format!("{table}.{column}")
            }
        }
        FieldRef::NodeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("{alias}.id"),
    }
}

fn evaluate_runtime_filter(
    record: &UnifiedRecord,
    filter: &Filter,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    match filter {
        Filter::Compare { field, op, value } => resolve_runtime_field(record, field, table_name, table_alias)
            .as_ref()
            .is_some_and(|candidate| compare_runtime_values(candidate, value, *op)),
        Filter::And(left, right) => {
            evaluate_runtime_filter(record, left, table_name, table_alias)
                && evaluate_runtime_filter(record, right, table_name, table_alias)
        }
        Filter::Or(left, right) => {
            evaluate_runtime_filter(record, left, table_name, table_alias)
                || evaluate_runtime_filter(record, right, table_name, table_alias)
        }
        Filter::Not(inner) => !evaluate_runtime_filter(record, inner, table_name, table_alias),
        Filter::IsNull(field) => resolve_runtime_field(record, field, table_name, table_alias)
            .map(|value| value == Value::Null)
            .unwrap_or(true),
        Filter::IsNotNull(field) => resolve_runtime_field(record, field, table_name, table_alias)
            .map(|value| value != Value::Null)
            .unwrap_or(false),
        Filter::In { field, values } => resolve_runtime_field(record, field, table_name, table_alias)
            .as_ref()
            .is_some_and(|candidate| values.iter().any(|value| compare_runtime_values(candidate, value, CompareOp::Eq))),
        Filter::Between { field, low, high } => resolve_runtime_field(record, field, table_name, table_alias)
            .as_ref()
            .is_some_and(|candidate| {
                compare_runtime_values(candidate, low, CompareOp::Ge)
                    && compare_runtime_values(candidate, high, CompareOp::Le)
            }),
        Filter::Like { field, pattern } => resolve_runtime_field(record, field, table_name, table_alias)
            .as_ref()
            .and_then(runtime_value_text)
            .is_some_and(|value| like_matches(&value, pattern)),
        Filter::StartsWith { field, prefix } => resolve_runtime_field(record, field, table_name, table_alias)
            .as_ref()
            .and_then(runtime_value_text)
            .is_some_and(|value| value.starts_with(prefix)),
        Filter::EndsWith { field, suffix } => resolve_runtime_field(record, field, table_name, table_alias)
            .as_ref()
            .and_then(runtime_value_text)
            .is_some_and(|value| value.ends_with(suffix)),
        Filter::Contains { field, substring } => resolve_runtime_field(record, field, table_name, table_alias)
            .as_ref()
            .and_then(runtime_value_text)
            .is_some_and(|value| value.contains(substring)),
    }
}

fn compare_runtime_order(
    left: &UnifiedRecord,
    right: &UnifiedRecord,
    clauses: &[OrderByClause],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Ordering {
    for clause in clauses {
        let left_value = resolve_runtime_field(left, &clause.field, table_name, table_alias);
        let right_value = resolve_runtime_field(right, &clause.field, table_name, table_alias);
        let ordering = compare_runtime_optional_values(
            left_value.as_ref(),
            right_value.as_ref(),
            clause.nulls_first,
        );

        if ordering != Ordering::Equal {
            return if clause.ascending {
                ordering
            } else {
                ordering.reverse()
            };
        }
    }

    Ordering::Equal
}

fn compare_runtime_optional_values(
    left: Option<&Value>,
    right: Option<&Value>,
    nulls_first: bool,
) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Some(_), None) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(Value::Null), Some(Value::Null)) => Ordering::Equal,
        (Some(Value::Null), Some(_)) => {
            if nulls_first {
                Ordering::Less
            } else {
                Ordering::Greater
            }
        }
        (Some(_), Some(Value::Null)) => {
            if nulls_first {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        }
        (Some(left), Some(right)) => runtime_partial_cmp(left, right).unwrap_or(Ordering::Equal),
    }
}

fn resolve_runtime_field(
    record: &UnifiedRecord,
    field: &FieldRef,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Option<Value> {
    match field {
        FieldRef::TableColumn { table, column } => {
            if !table.is_empty() {
                if let Some(value) = record.values.get(&format!("{table}.{column}")) {
                    return Some(value.clone());
                }

                let matches_context =
                    Some(table.as_str()) == table_name || Some(table.as_str()) == table_alias;
                if !matches_context {
                    return None;
                }
            }

            record.values.get(column).cloned()
        }
        FieldRef::NodeProperty { alias, property } => {
            if let Some(value) = record.values.get(&format!("{alias}.{property}")) {
                return Some(value.clone());
            }

            let node = record.nodes.get(alias)?;
            match property.as_str() {
                "id" => Some(Value::NodeRef(node.id.clone())),
                "label" => Some(Value::Text(node.label.clone())),
                "type" | "node_type" => Some(Value::Text(format!("{:?}", node.node_type))),
                _ => None,
            }
        }
        FieldRef::EdgeProperty { alias, property } => {
            if let Some(value) = record.values.get(&format!("{alias}.{property}")) {
                return Some(value.clone());
            }

            let edge = record.edges.get(alias)?;
            match property.as_str() {
                "from" | "source" => Some(Value::NodeRef(edge.from.clone())),
                "to" | "target" => Some(Value::NodeRef(edge.to.clone())),
                "type" | "edge_type" | "label" => {
                    Some(Value::Text(format!("{:?}", edge.edge_type)))
                }
                "weight" => Some(Value::Float(edge.weight as f64)),
                _ => None,
            }
        }
        FieldRef::NodeId { alias } => record
            .nodes
            .get(alias)
            .map(|node| Value::NodeRef(node.id.clone()))
            .or_else(|| record.values.get(&format!("{alias}.id")).cloned()),
    }
}

fn compare_runtime_values(left: &Value, right: &Value, op: CompareOp) -> bool {
    match op {
        CompareOp::Eq => runtime_values_equal(left, right),
        CompareOp::Ne => !runtime_values_equal(left, right),
        CompareOp::Lt => runtime_partial_cmp(left, right).is_some_and(|ord| ord == Ordering::Less),
        CompareOp::Le => runtime_partial_cmp(left, right)
            .is_some_and(|ord| matches!(ord, Ordering::Less | Ordering::Equal)),
        CompareOp::Gt => {
            runtime_partial_cmp(left, right).is_some_and(|ord| ord == Ordering::Greater)
        }
        CompareOp::Ge => runtime_partial_cmp(left, right)
            .is_some_and(|ord| matches!(ord, Ordering::Greater | Ordering::Equal)),
    }
}

fn runtime_values_equal(left: &Value, right: &Value) -> bool {
    if let (Some(left), Some(right)) = (runtime_value_number(left), runtime_value_number(right)) {
        return left == right;
    }

    if let (Some(left), Some(right)) = (runtime_value_text(left), runtime_value_text(right)) {
        return left == right;
    }

    if let (Value::Boolean(left), Value::Boolean(right)) = (left, right) {
        return left == right;
    }

    left == right
}

fn runtime_partial_cmp(left: &Value, right: &Value) -> Option<Ordering> {
    if let (Some(left), Some(right)) = (runtime_value_number(left), runtime_value_number(right)) {
        return left.partial_cmp(&right);
    }

    if let (Some(left), Some(right)) = (runtime_value_text(left), runtime_value_text(right)) {
        return Some(left.cmp(&right));
    }

    match (left, right) {
        (Value::Boolean(left), Value::Boolean(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

fn runtime_value_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        Value::Timestamp(value) => Some(*value as f64),
        Value::Duration(value) => Some(*value as f64),
        _ => None,
    }
}

fn runtime_value_text(value: &Value) -> Option<String> {
    match value {
        Value::Text(value) => Some(value.clone()),
        Value::NodeRef(value) => Some(value.clone()),
        Value::EdgeRef(value) => Some(value.clone()),
        Value::RowRef(table, row_id) => Some(format!("{table}:{row_id}")),
        Value::VectorRef(collection, vector_id) => Some(format!("{collection}:{vector_id}")),
        Value::IpAddr(value) => Some(value.to_string()),
        Value::MacAddr(value) => Some(format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            value[0], value[1], value[2], value[3], value[4], value[5]
        )),
        Value::Uuid(value) => Some(
            value
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
        ),
        Value::Boolean(value) => Some(value.to_string()),
        Value::Integer(value) => Some(value.to_string()),
        Value::UnsignedInteger(value) => Some(value.to_string()),
        Value::Float(value) => Some(value.to_string()),
        Value::Timestamp(value) => Some(value.to_string()),
        Value::Duration(value) => Some(value.to_string()),
        Value::Null => None,
        Value::Blob(_) | Value::Vector(_) | Value::Json(_) => None,
    }
}

fn like_matches(value: &str, pattern: &str) -> bool {
    like_matches_bytes(value.as_bytes(), pattern.as_bytes())
}

fn like_matches_bytes(value: &[u8], pattern: &[u8]) -> bool {
    if pattern.is_empty() {
        return value.is_empty();
    }

    match pattern[0] {
        b'%' => {
            like_matches_bytes(value, &pattern[1..])
                || (!value.is_empty() && like_matches_bytes(&value[1..], pattern))
        }
        b'_' => !value.is_empty() && like_matches_bytes(&value[1..], &pattern[1..]),
        byte => !value.is_empty() && value[0] == byte && like_matches_bytes(&value[1..], &pattern[1..]),
    }
}

fn query_expr_name(expr: &QueryExpr) -> &'static str {
    match expr {
        QueryExpr::Table(_) => "table",
        QueryExpr::Graph(_) => "graph",
        QueryExpr::Join(_) => "join",
        QueryExpr::Path(_) => "path",
        QueryExpr::Vector(_) => "vector",
        QueryExpr::Hybrid(_) => "hybrid",
    }
}

fn materialize_graph(store: &UnifiedStore) -> RedDBResult<GraphStore> {
    materialize_graph_with_projection(store, None)
}

fn materialize_graph_with_projection(
    store: &UnifiedStore,
    projection: Option<&RuntimeGraphProjection>,
) -> RedDBResult<GraphStore> {
    let graph = GraphStore::new();
    let entities = store.query_all(|_| true);
    let node_label_filters = projection
        .and_then(|projection| normalize_token_filter_list(projection.node_labels.clone()));
    let node_type_filters = projection
        .and_then(|projection| normalize_token_filter_list(projection.node_types.clone()));
    let edge_label_filters = projection
        .and_then(|projection| normalize_token_filter_list(projection.edge_labels.clone()));
    let mut allowed_nodes = HashSet::new();

    for (_, entity) in &entities {
        if let EntityKind::GraphNode { label, node_type } = &entity.kind {
            if !matches_graph_node_projection(
                label,
                node_type,
                node_label_filters.as_ref(),
                node_type_filters.as_ref(),
            ) {
                continue;
            }
            graph
                .add_node(
                    &entity.id.raw().to_string(),
                    label,
                    graph_node_type(node_type),
                )
                .map_err(|err| RedDBError::Query(err.to_string()))?;
            allowed_nodes.insert(entity.id.raw().to_string());
        }
    }

    for (_, entity) in &entities {
        if let EntityKind::GraphEdge {
            label,
            from_node,
            to_node,
            weight,
        } = &entity.kind
        {
            if !allowed_nodes.contains(from_node) || !allowed_nodes.contains(to_node) {
                continue;
            }
            if !matches_graph_edge_projection(label, edge_label_filters.as_ref()) {
                continue;
            }
            let resolved_weight = match &entity.data {
                EntityData::Edge(edge) => edge.weight,
                _ => *weight as f32 / 1000.0,
            };

            graph
                .add_edge(
                    from_node,
                    to_node,
                    graph_edge_type(label),
                    resolved_weight,
                )
                .map_err(|err| RedDBError::Query(err.to_string()))?;
        }
    }

    Ok(graph)
}

fn normalize_token_filter_list(values: Option<Vec<String>>) -> Option<BTreeSet<String>> {
    values
        .map(|values| {
            values
                .into_iter()
                .map(|value| normalize_graph_token(&value))
                .filter(|value| !value.is_empty())
                .collect::<BTreeSet<_>>()
        })
        .filter(|set| !set.is_empty())
}

fn matches_graph_node_projection(
    label: &str,
    node_type: &str,
    label_filters: Option<&BTreeSet<String>>,
    node_type_filters: Option<&BTreeSet<String>>,
) -> bool {
    let label_ok = label_filters.map_or(true, |filters| {
        filters.contains(&normalize_graph_token(label))
    });
    let node_type_ok = node_type_filters.map_or(true, |filters| {
        filters.contains(&normalize_graph_token(node_type))
    });
    label_ok && node_type_ok
}

fn matches_graph_edge_projection(
    label: &str,
    edge_filters: Option<&BTreeSet<String>>,
) -> bool {
    edge_filters.map_or(true, |filters| filters.contains(&normalize_graph_token(label)))
}

fn ensure_graph_node(graph: &GraphStore, id: &str) -> RedDBResult<()> {
    if graph.has_node(id) {
        Ok(())
    } else {
        Err(RedDBError::NotFound(id.to_string()))
    }
}

fn stored_node_to_runtime(node: StoredNode) -> RuntimeGraphNode {
    RuntimeGraphNode {
        id: node.id,
        label: node.label,
        node_type: node.node_type.as_str().to_string(),
        out_edge_count: node.out_edge_count,
        in_edge_count: node.in_edge_count,
    }
}

fn path_to_runtime(
    graph: &GraphStore,
    path: &crate::storage::engine::pathfinding::Path,
) -> RuntimeGraphPath {
    let nodes = path
        .nodes
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(stored_node_to_runtime)
        .collect();

    let mut edges = Vec::new();
    for index in 0..path.edge_types.len() {
        let Some(source) = path.nodes.get(index) else {
            continue;
        };
        let Some(target) = path.nodes.get(index + 1) else {
            continue;
        };
        let Some(edge_type) = path.edge_types.get(index) else {
            continue;
        };
        let weight = graph
            .outgoing_edges(source)
            .into_iter()
            .find(|(candidate_type, candidate_target, _)| {
                *candidate_type == *edge_type && candidate_target == target
            })
            .map(|(_, _, weight)| weight)
            .unwrap_or(0.0);
        edges.push(RuntimeGraphEdge {
            source: source.clone(),
            target: target.clone(),
            edge_type: edge_type.as_str().to_string(),
            weight,
        });
    }

    RuntimeGraphPath {
        hop_count: path.len(),
        total_weight: path.total_weight,
        nodes,
        edges,
    }
}

fn cycle_to_runtime(
    graph: &GraphStore,
    cycle: crate::storage::engine::Cycle,
) -> RuntimeGraphPath {
    let nodes = cycle
        .nodes
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(stored_node_to_runtime)
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    let mut total_weight = 0.0;

    for window in cycle.nodes.windows(2) {
        let Some(source) = window.first() else {
            continue;
        };
        let Some(target) = window.get(1) else {
            continue;
        };
        if let Some((edge_type, _, weight)) = graph
            .outgoing_edges(source)
            .into_iter()
            .find(|(_, candidate_target, _)| candidate_target == target)
        {
            total_weight += weight as f64;
            edges.push(RuntimeGraphEdge {
                source: source.clone(),
                target: target.clone(),
                edge_type: edge_type.as_str().to_string(),
                weight,
            });
        }
    }

    RuntimeGraphPath {
        hop_count: cycle.length,
        total_weight,
        nodes,
        edges,
    }
}

fn normalize_edge_filters(edge_labels: Option<Vec<String>>) -> Option<BTreeSet<String>> {
    edge_labels.map(|labels| {
        labels
            .into_iter()
            .map(|label| normalize_graph_token(&label))
            .filter(|label| !label.is_empty())
            .collect()
    }).filter(|set: &BTreeSet<String>| !set.is_empty())
}

fn merge_edge_filters(
    edge_labels: Option<Vec<String>>,
    projection: Option<&RuntimeGraphProjection>,
) -> Option<BTreeSet<String>> {
    let mut merged = BTreeSet::new();

    if let Some(filters) = normalize_edge_filters(edge_labels) {
        merged.extend(filters);
    }

    if let Some(filters) = projection
        .and_then(|projection| normalize_token_filter_list(projection.edge_labels.clone()))
    {
        merged.extend(filters);
    }

    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

fn merge_runtime_projection(
    base: Option<RuntimeGraphProjection>,
    overlay: Option<RuntimeGraphProjection>,
) -> Option<RuntimeGraphProjection> {
    let merge_list = |left: Option<Vec<String>>, right: Option<Vec<String>>| -> Option<Vec<String>> {
        let mut values = BTreeSet::new();
        if let Some(left) = left {
            values.extend(left);
        }
        if let Some(right) = right {
            values.extend(right);
        }
        if values.is_empty() {
            None
        } else {
            Some(values.into_iter().collect())
        }
    };

    let Some(_) = base.clone().or(overlay.clone()) else {
        return None;
    };

    Some(RuntimeGraphProjection {
        node_labels: merge_list(
            base.as_ref().and_then(|projection| projection.node_labels.clone()),
            overlay.as_ref().and_then(|projection| projection.node_labels.clone()),
        ),
        node_types: merge_list(
            base.as_ref().and_then(|projection| projection.node_types.clone()),
            overlay.as_ref().and_then(|projection| projection.node_types.clone()),
        ),
        edge_labels: merge_list(
            base.as_ref().and_then(|projection| projection.edge_labels.clone()),
            overlay.as_ref().and_then(|projection| projection.edge_labels.clone()),
        ),
    })
}

fn edge_allowed(edge_type: GraphEdgeType, filters: Option<&BTreeSet<String>>) -> bool {
    filters.map_or(true, |filters| {
        filters.contains(&normalize_graph_token(edge_type.as_str()))
    })
}

fn graph_adjacent_edges(
    graph: &GraphStore,
    node: &str,
    direction: RuntimeGraphDirection,
    edge_filters: Option<&BTreeSet<String>>,
) -> Vec<(String, RuntimeGraphEdge)> {
    let mut adjacent = Vec::new();

    if matches!(direction, RuntimeGraphDirection::Outgoing | RuntimeGraphDirection::Both) {
        for (edge_type, target, weight) in graph.outgoing_edges(node) {
            if edge_allowed(edge_type, edge_filters) {
                adjacent.push((
                    target.clone(),
                    RuntimeGraphEdge {
                        source: node.to_string(),
                        target,
                        edge_type: edge_type.as_str().to_string(),
                        weight,
                    },
                ));
            }
        }
    }

    if matches!(direction, RuntimeGraphDirection::Incoming | RuntimeGraphDirection::Both) {
        for (edge_type, source, weight) in graph.incoming_edges(node) {
            if edge_allowed(edge_type, edge_filters) {
                adjacent.push((
                    source.clone(),
                    RuntimeGraphEdge {
                        source,
                        target: node.to_string(),
                        edge_type: edge_type.as_str().to_string(),
                        weight,
                    },
                ));
            }
        }
    }

    adjacent
}

fn push_runtime_edge(
    edges: &mut Vec<RuntimeGraphEdge>,
    seen_edges: &mut HashSet<(String, String, String, u32)>,
    edge: RuntimeGraphEdge,
) {
    let key = (
        edge.source.clone(),
        edge.target.clone(),
        edge.edge_type.clone(),
        edge.weight.to_bits(),
    );
    if seen_edges.insert(key) {
        edges.push(edge);
    }
}

#[derive(Clone)]
struct RuntimeDijkstraState {
    node: String,
    cost: f64,
}

impl PartialEq for RuntimeDijkstraState {
    fn eq(&self, other: &Self) -> bool {
        self.node == other.node && self.cost == other.cost
    }
}

impl Eq for RuntimeDijkstraState {}

impl Ord for RuntimeDijkstraState {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .cost
            .partial_cmp(&self.cost)
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for RuntimeDijkstraState {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn shortest_path_runtime(
    graph: &GraphStore,
    source: &str,
    target: &str,
    direction: RuntimeGraphDirection,
    algorithm: RuntimeGraphPathAlgorithm,
    edge_filters: Option<&BTreeSet<String>>,
) -> RedDBResult<RuntimeGraphPathResult> {
    let mut nodes_visited = 0;
    let path = match algorithm {
        RuntimeGraphPathAlgorithm::Bfs => {
            let mut queue = VecDeque::new();
            let mut visited = HashSet::new();
            let mut previous: HashMap<String, (String, RuntimeGraphEdge)> = HashMap::new();

            queue.push_back(source.to_string());
            visited.insert(source.to_string());

            while let Some(current) = queue.pop_front() {
                nodes_visited += 1;
                if current == target {
                    break;
                }
                let mut adjacent = graph_adjacent_edges(graph, &current, direction, edge_filters);
                adjacent.sort_by(|left, right| left.0.cmp(&right.0));
                for (neighbor, edge) in adjacent {
                    if visited.insert(neighbor.clone()) {
                        previous.insert(neighbor.clone(), (current.clone(), edge));
                        queue.push_back(neighbor);
                    }
                }
            }

            rebuild_runtime_path(graph, source, target, &previous)
        }
        RuntimeGraphPathAlgorithm::Dijkstra => {
            let mut dist: HashMap<String, f64> = HashMap::new();
            let mut previous: HashMap<String, (String, RuntimeGraphEdge)> = HashMap::new();
            let mut heap = BinaryHeap::new();

            dist.insert(source.to_string(), 0.0);
            heap.push(RuntimeDijkstraState {
                node: source.to_string(),
                cost: 0.0,
            });

            while let Some(RuntimeDijkstraState { node, cost }) = heap.pop() {
                nodes_visited += 1;
                if node == target {
                    break;
                }
                if let Some(best) = dist.get(&node) {
                    if cost > *best {
                        continue;
                    }
                }

                let mut adjacent = graph_adjacent_edges(graph, &node, direction, edge_filters);
                adjacent.sort_by(|left, right| left.0.cmp(&right.0));
                for (neighbor, edge) in adjacent {
                    let next_cost = cost + edge.weight as f64;
                    if dist.get(&neighbor).map_or(true, |best| next_cost < *best) {
                        dist.insert(neighbor.clone(), next_cost);
                        previous.insert(neighbor.clone(), (node.clone(), edge));
                        heap.push(RuntimeDijkstraState {
                            node: neighbor,
                            cost: next_cost,
                        });
                    }
                }
            }

            rebuild_runtime_path(graph, source, target, &previous)
        }
    };

    Ok(RuntimeGraphPathResult {
        source: source.to_string(),
        target: target.to_string(),
        direction,
        algorithm,
        nodes_visited,
        path,
    })
}

fn rebuild_runtime_path(
    graph: &GraphStore,
    source: &str,
    target: &str,
    previous: &HashMap<String, (String, RuntimeGraphEdge)>,
) -> Option<RuntimeGraphPath> {
    if source != target && !previous.contains_key(target) {
        return None;
    }

    let mut node_ids = vec![target.to_string()];
    let mut edges = Vec::new();
    let mut current = target.to_string();

    while current != source {
        let (parent, edge) = previous.get(&current)?.clone();
        edges.push(edge);
        node_ids.push(parent.clone());
        current = parent;
    }

    node_ids.reverse();
    edges.reverse();

    let total_weight = edges.iter().map(|edge| edge.weight as f64).sum();
    let nodes = node_ids
        .iter()
        .filter_map(|id| graph.get_node(id))
        .map(stored_node_to_runtime)
        .collect();

    Some(RuntimeGraphPath {
        hop_count: node_ids.len().saturating_sub(1),
        total_weight,
        nodes,
        edges,
    })
}

fn top_runtime_scores(
    graph: &GraphStore,
    scores: HashMap<String, f64>,
    top_k: usize,
) -> Vec<RuntimeGraphCentralityScore> {
    let mut pairs: Vec<_> = scores.into_iter().collect();
    pairs.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    pairs.truncate(top_k.max(1));
    pairs
        .into_iter()
        .filter_map(|(node_id, score)| {
            graph.get_node(&node_id).map(|node| RuntimeGraphCentralityScore {
                node: stored_node_to_runtime(node),
                score,
            })
        })
        .collect()
}

fn graph_node_type(input: &str) -> GraphNodeType {
    match normalize_graph_token(input).as_str() {
        "host" => GraphNodeType::Host,
        "service" => GraphNodeType::Service,
        "credential" => GraphNodeType::Credential,
        "vulnerability" => GraphNodeType::Vulnerability,
        "endpoint" => GraphNodeType::Endpoint,
        "technology" | "tech" => GraphNodeType::Technology,
        "user" => GraphNodeType::User,
        "domain" => GraphNodeType::Domain,
        "certificate" | "cert" => GraphNodeType::Certificate,
        _ => GraphNodeType::Endpoint,
    }
}

fn graph_edge_type(input: &str) -> GraphEdgeType {
    match normalize_graph_token(input).as_str() {
        "hasservice" => GraphEdgeType::HasService,
        "hasendpoint" => GraphEdgeType::HasEndpoint,
        "usestech" | "usestechnology" => GraphEdgeType::UsesTech,
        "authaccess" | "hascredential" => GraphEdgeType::AuthAccess,
        "affectedby" => GraphEdgeType::AffectedBy,
        "contains" => GraphEdgeType::Contains,
        "connectsto" | "connects" => GraphEdgeType::ConnectsTo,
        "relatedto" | "related" => GraphEdgeType::RelatedTo,
        "hasuser" => GraphEdgeType::HasUser,
        "hascert" | "hascertificate" => GraphEdgeType::HasCert,
        _ => GraphEdgeType::RelatedTo,
    }
}

fn normalize_graph_token(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

#[derive(Debug, Clone)]
pub struct RuntimeGraphPattern {
    pub node_label: Option<String>,
    pub node_type: Option<String>,
    pub edge_labels: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RuntimeGraphProjection {
    pub node_labels: Option<Vec<String>>,
    pub node_types: Option<Vec<String>>,
    pub edge_labels: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeQueryWeights {
    pub vector: f32,
    pub graph: f32,
    pub filter: f32,
}

#[derive(Debug, Clone)]
pub struct RuntimeFilter {
    pub field: String,
    pub op: String,
    pub value: Option<RuntimeFilterValue>,
}

#[derive(Debug, Clone)]
pub enum RuntimeFilterValue {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    List(Vec<RuntimeFilterValue>),
    Range(Box<RuntimeFilterValue>, Box<RuntimeFilterValue>),
}

fn runtime_filter_to_dsl(filter: RuntimeFilter) -> RedDBResult<DslFilter> {
    Ok(DslFilter {
        field: filter.field,
        op: parse_runtime_filter_op(&filter.op)?,
        value: match filter.value {
            Some(value) => runtime_filter_value_to_dsl(value),
            None => DslFilterValue::Null,
        },
    })
}

fn parse_runtime_filter_op(op: &str) -> RedDBResult<DslFilterOp> {
    match op.trim().to_ascii_lowercase().as_str() {
        "eq" | "equals" => Ok(DslFilterOp::Equals),
        "ne" | "not_equals" | "not-equals" => Ok(DslFilterOp::NotEquals),
        "gt" | "greater_than" | "greater-than" => Ok(DslFilterOp::GreaterThan),
        "gte" | "greater_than_or_equals" | "greater-than-or-equals" => {
            Ok(DslFilterOp::GreaterThanOrEquals)
        }
        "lt" | "less_than" | "less-than" => Ok(DslFilterOp::LessThan),
        "lte" | "less_than_or_equals" | "less-than-or-equals" => {
            Ok(DslFilterOp::LessThanOrEquals)
        }
        "contains" => Ok(DslFilterOp::Contains),
        "starts_with" | "starts-with" => Ok(DslFilterOp::StartsWith),
        "ends_with" | "ends-with" => Ok(DslFilterOp::EndsWith),
        "in" | "in_list" | "in-list" => Ok(DslFilterOp::In),
        "between" => Ok(DslFilterOp::Between),
        "is_null" | "is-null" => Ok(DslFilterOp::IsNull),
        "is_not_null" | "is-not-null" => Ok(DslFilterOp::IsNotNull),
        other => Err(RedDBError::Query(format!(
            "unsupported hybrid filter op: {other}"
        ))),
    }
}

fn runtime_filter_value_to_dsl(value: RuntimeFilterValue) -> DslFilterValue {
    match value {
        RuntimeFilterValue::String(value) => DslFilterValue::String(value),
        RuntimeFilterValue::Int(value) => DslFilterValue::Int(value),
        RuntimeFilterValue::Float(value) => DslFilterValue::Float(value),
        RuntimeFilterValue::Bool(value) => DslFilterValue::Bool(value),
        RuntimeFilterValue::Null => DslFilterValue::Null,
        RuntimeFilterValue::List(values) => DslFilterValue::List(
            values
                .into_iter()
                .map(runtime_filter_value_to_dsl)
                .collect(),
        ),
        RuntimeFilterValue::Range(start, end) => DslFilterValue::Range(
            Box::new(runtime_filter_value_to_dsl(*start)),
            Box::new(runtime_filter_value_to_dsl(*end)),
        ),
    }
}
