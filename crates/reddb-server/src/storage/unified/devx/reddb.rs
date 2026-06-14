//! RedDB - Main Entry Point
//!
//! Unified Database with best-in-class developer experience for Tables, Graphs, and Vectors.

use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Debug,
};

use super::super::{
    EntityData, EntityId, EntityKind, StoreStats, UnifiedEntity, UnifiedStore, UnifiedStoreConfig,
};
use super::batch::BatchBuilder;
use super::builders::{
    DocumentBuilder, EdgeBuilder, KvBuilder, NodeBuilder, RowBuilder, VectorBuilder,
};
use super::helpers::cosine_similarity;
use super::query::QueryBuilder;
use super::refs::{NodeRef, TableRef, VectorRef};
use super::types::{LinkedEntity, SimilarResult};
use super::{IndexConfig, Preprocessor, SharedPreprocessors};
use crate::api::{Capability, CatalogSnapshot, CollectionStats, RedDBOptions, StorageMode};
use crate::catalog::{
    consistency_report, snapshot_store_with_declarations, CatalogConsistencyReport,
    CatalogDeclarations, CatalogIndexStatus, CatalogModelSnapshot, CollectionDescriptor,
    CollectionModel,
};
use crate::health::{storage_file_health, HealthReport, HealthState};
use crate::index::{IndexCatalog, IndexConfig as RuntimeIndexConfig, IndexKind};
use crate::physical::{
    ExportDescriptor, PhysicalAnalyticsJob, PhysicalGraphProjection, PhysicalIndexState,
    PhysicalMetadataFile,
};
use crate::replication::{primary::PrimaryReplication, ReplicationRole};
use crate::serde_json::Value as JsonValue;
use crate::storage::engine::{HnswIndex, IvfConfig, IvfIndex, IvfStats, PhysicalFileHeader};
use crate::storage::schema::Value;
use crate::storage::unified::store::{
    NativeCatalogCollectionSummary, NativeCatalogSummary, NativeExportSummary,
    NativeManifestSummary, NativeMetadataStateSummary, NativePhysicalState, NativeRecoverySummary,
    NativeRegistryIndexSummary, NativeRegistryJobSummary, NativeRegistryProjectionSummary,
    NativeRegistrySummary, NativeSnapshotSummary, NativeVectorArtifactPageSummary,
    NativeVectorArtifactSummary,
};

/// RedDB - Unified Database with Best-in-Class DevX
///
/// Single entry point for Tables, Graphs, and Vectors with full
/// metadata support and cross-referencing.
pub struct RedDB {
    store: Arc<UnifiedStore>,
    /// Preprocessing hooks
    preprocessors: SharedPreprocessors,
    /// Index configuration
    index_config: IndexConfig,
    /// Persistence path
    path: Option<PathBuf>,
    /// Construction/runtime options
    options: RedDBOptions,
    /// Whether the current persistence backend is page-based.
    paged_mode: bool,
    /// Per-collection HNSW vector index cache for fast approximate nearest neighbor search.
    /// Lazily built on first vector similarity query per collection.
    vector_indexes: RwLock<HashMap<String, CachedVectorIndex>>,
    /// Default TTL policy declared at the collection level, in milliseconds.
    collection_ttl_defaults_ms: RwLock<HashMap<String, u64>>,
    /// In-memory cache of collection contracts keyed by collection name.
    /// Populated lazily from `physical_metadata()` and invalidated on
    /// `save_collection_contract` / `remove_collection_contract`.
    /// Avoids reparsing the whole PhysicalMetadataFile JSON on every
    /// `collection_contract(name)` lookup — which happens 3× per insert
    /// (ensure_model, enforce_uniqueness, normalize_fields) and dominated
    /// the insert hot path at ~30%.
    pub(crate) collection_contract_cache:
        RwLock<Option<Arc<HashMap<String, Arc<crate::physical::CollectionContract>>>>>,
    /// Optional remote storage backend for snapshot transport.
    pub(crate) remote_backend: Option<Arc<dyn crate::storage::backend::RemoteBackend>>,
    /// Optional CAS-capable handle for backends that implement
    /// `AtomicRemoteBackend`. Mirrors `RedDBOptions::remote_backend_atomic`
    /// — see that field for semantics.
    pub(crate) remote_backend_atomic: Option<Arc<dyn crate::storage::backend::AtomicRemoteBackend>>,
    /// Remote object key used by the remote backend.
    pub(crate) remote_key: Option<String>,
    /// Primary replication state (only present when role is Primary).
    pub(crate) replication: Option<Arc<PrimaryReplication>>,
    /// Quorum coordinator for multi-region commits (Phase 2.6 PG parity).
    ///
    /// Only present when role is Primary. Write path calls
    /// `quorum.wait_for_quorum(lsn)` after appending to the primary WAL
    /// to block until the configured quorum of replicas has acked. When
    /// the config is `Async` (default), this returns instantly — same
    /// semantics as pre-Phase-2.6 RedDB.
    pub(crate) quorum: Option<Arc<crate::replication::quorum::QuorumCoordinator>>,
    /// Eventual consistency registry (embedded mode support).
    pub(crate) ec_registry: Arc<crate::ec::config::EcRegistry>,
    /// Lazily-initialised ML runtime (model registry + job queue +
    /// semantic cache). Created on first access by the SQL layer so
    /// `ML_CLASSIFY`, `SEMANTIC_CACHE_GET/PUT`, and friends have a
    /// shared handle without forcing every instantiation path to
    /// know about it.
    pub(crate) ml_runtime: std::sync::OnceLock<crate::storage::ml::MlRuntime>,
    /// Shared semantic cache used by `SEMANTIC_CACHE_*` scalars.
    /// Separate from `MlRuntime` because cache config is runtime-only
    /// and doesn't need the job queue — keep it a standalone `Arc`.
    pub(crate) semantic_cache: std::sync::OnceLock<Arc<crate::storage::ml::SemanticCache>>,
    /// Hypertable registry — populated by `CREATE HYPERTABLE` DDL,
    /// consumed by chunk routing, retention sweeps, and `SHOW
    /// HYPERTABLES`. Lazy so startup stays cheap when no hypertables
    /// exist.
    pub(crate) hypertables:
        std::sync::OnceLock<Arc<crate::storage::timeseries::HypertableRegistry>>,
    /// Continuous-aggregate engine — populated by `CA_REGISTER` and
    /// queried by `CA_REFRESH` / `CA_STATE` scalars. Same lazy shape
    /// as the other engine handles.
    pub(crate) continuous_aggregates: std::sync::OnceLock<
        Arc<crate::storage::timeseries::continuous_aggregate::ContinuousAggregateEngine>,
    >,
    /// Per-collection `vector.turbo` runtime state (issue #693, PRD
    /// #668). Lazily initialised: the entire map allocation is
    /// deferred until the first turbo collection is created. Each
    /// entry owns the in-memory `TurboQuantIndex` + optional
    /// `TurboExtent` for the collection.
    pub(crate) turbo_collections: std::sync::OnceLock<
        Arc<
            parking_lot::Mutex<
                std::collections::HashMap<
                    String,
                    Arc<crate::runtime::vector_turbo_kind::TurboCollectionState>,
                >,
            >,
        >,
    >,
    /// Join handles for `vector.turbo` background-rebuild workers
    /// (issue #673). Each call to `turbo_state` that materialises a
    /// fresh `TurboCollectionState` registers a handle here; the
    /// `Drop` impl below joins every handle before releasing
    /// `store`, so a runtime restart on the same database path is
    /// not racy with an in-flight rebuild holding the file lock.
    pub(crate) turbo_rebuild_workers: parking_lot::Mutex<Vec<std::thread::JoinHandle<()>>>,
    _ephemeral_cleanup: Option<EphemeralDataPathCleanup>,
}

pub(super) struct EphemeralDataPathCleanup {
    path: PathBuf,
}

impl EphemeralDataPathCleanup {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for EphemeralDataPathCleanup {
    fn drop(&mut self) {
        for path in ephemeral_data_artifacts(&self.path) {
            if path.is_dir() {
                let _ = fs::remove_dir_all(path);
            } else {
                let _ = fs::remove_file(path);
            }
        }
    }
}

pub(super) fn is_ephemeral_data_path(path: &Path) -> bool {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !file_name.starts_with("reddb-ephemeral-") || !file_name.ends_with(".rdb") {
        return false;
    }
    path.parent()
        .map(|parent| parent == std::env::temp_dir())
        .unwrap_or(false)
}

fn ephemeral_data_artifacts(data_path: &Path) -> Vec<PathBuf> {
    let logical_wal_path = reddb_file::layout::logical_wal_path(data_path);
    let result_cache_l2_path = reddb_file::layout::result_cache_l2_path(data_path);
    let legacy_logical_slots_path = reddb_file::layout::legacy_logical_slots_path(data_path);
    let mut operational_manifest_root = data_path.as_os_str().to_os_string();
    operational_manifest_root.push(".ops");
    let mut paths = vec![
        data_path.to_path_buf(),
        PathBuf::from(operational_manifest_root),
        reddb_file::layout::unified_wal_path(data_path),
        logical_wal_path.clone(),
        reddb_file::layout::logical_wal_temp_path(&logical_wal_path),
        reddb_file::layout::temp_path(data_path),
        reddb_file::layout::atomic_temp_path(data_path),
        result_cache_l2_path.clone(),
        reddb_file::layout::pager_legacy_wal_path(data_path),
        reddb_file::layout::engine_wal_path(data_path),
        reddb_file::layout::pager_header_path(data_path),
        reddb_file::layout::pager_meta_path(data_path),
        reddb_file::layout::pager_dwb_path(data_path),
        legacy_logical_slots_path.clone(),
        reddb_file::layout::legacy_logical_slots_temp_path(&legacy_logical_slots_path),
        reddb_file::layout::legacy_audit_log_path(data_path),
        reddb_file::layout::shm_path(data_path),
        reddb_file::layout::physical_metadata_json_path(data_path),
        reddb_file::layout::physical_metadata_binary_path(data_path),
        reddb_file::layout::rebootstrap_staging_root(data_path),
        reddb_file::layout::rebootstrap_pending_path(data_path),
        reddb_file::layout::rebootstrap_ready_marker_path(data_path),
        reddb_file::layout::rebootstrap_intent_log_path(data_path),
        reddb_file::layout::rebootstrap_previous_path(data_path),
        reddb_file::layout::primary_replica_root(data_path),
        reddb_file::layout::serverless_root(data_path),
    ];
    paths.extend(reddb_file::layout::pager_shadow_sidecar_paths(data_path));
    paths.extend(reddb_file::layout::pager_shadow_sidecar_paths(
        &result_cache_l2_path,
    ));
    if let Some(parent) = data_path.parent() {
        paths.push(reddb_file::layout::legacy_slow_query_log_path(parent));
    }
    paths.push(reddb_file::layout::support_dir_for(data_path));
    paths
}

impl Drop for RedDB {
    fn drop(&mut self) {
        if self.options.storage_profile.deploy_profile == crate::storage::DeployProfile::Embedded
            && self.options.storage_profile.packaging
                == crate::storage::StoragePackaging::SingleFile
            && !self.paged_mode
            && !self.options.read_only
        {
            if let Some(path) = &self.path {
                let snapshot = self.store.to_binary_dump_bytes();
                let _ = crate::storage::EmbeddedRdbArtifact::write_snapshot(path, &snapshot);
            }
        }

        // Issue #673 — wait for every `vector.turbo` background
        // rebuild worker to exit before our `Arc<UnifiedStore>` is
        // released. The worker holds a strong handle to the store
        // during its work phase, so without this join a fast
        // `RedDBRuntime` restart on the same path observes the
        // file lock as still held by the soon-to-exit worker.
        let handles: Vec<_> = self.turbo_rebuild_workers.lock().drain(..).collect();
        for h in handles {
            let _ = h.join();
        }
        // Issue #674 — also join any in-flight `.tv` snapshot dump
        // so the on-disk file is complete and renamed before a
        // restart observes it. Without this, a fast reopen can race
        // and see the `<path>.tv.tmp` before the atomic rename.
        if let Some(map) = self.turbo_collections.get() {
            let states: Vec<_> = map.lock().values().cloned().collect();
            for state in states {
                state.wait_snapshot();
            }
        }
    }
}

/// A cached HNSW index together with the entity count at build time.
/// When the live entity count diverges the cache is considered stale and
/// is rebuilt on the next query.
pub(crate) struct CachedVectorIndex {
    pub index: Arc<RwLock<HnswIndex>>,
    pub entity_count: usize,
}

#[derive(Debug, Clone)]
pub struct NativeHeaderMismatch {
    pub field: &'static str,
    pub native: String,
    pub expected: String,
}

#[derive(Debug, Clone)]
pub struct NativeHeaderInspection {
    pub native: PhysicalFileHeader,
    pub expected: PhysicalFileHeader,
    pub consistent: bool,
    pub mismatches: Vec<NativeHeaderMismatch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHeaderRepairPolicy {
    InSync,
    RepairNativeFromMetadata,
    NativeAheadOfMetadata,
}

#[derive(Debug, Clone)]
pub struct PhysicalAuthorityStatus {
    pub preference: String,
    pub sidecar_available: bool,
    pub native_state_available: bool,
    pub native_bootstrap_ready: bool,
    pub native_registry_complete: Option<bool>,
    pub native_recovery_complete: Option<bool>,
    pub native_catalog_complete: Option<bool>,
    pub sidecar_loaded_from: Option<String>,
    pub native_header_repair_policy: Option<String>,
    pub metadata_sequence: Option<u64>,
    pub native_sequence: Option<u64>,
    pub native_metadata_last_loaded_from: Option<String>,
    pub native_metadata_generated_at_unix_ms: Option<u128>,
}

#[derive(Debug, Clone)]
pub struct NativeVectorArtifactInspection {
    pub collection: String,
    pub artifact_kind: String,
    pub root_page: u32,
    pub page_count: u32,
    pub byte_len: u64,
    pub checksum: u64,
    pub node_count: u64,
    pub dimension: u32,
    pub max_layer: u32,
    pub total_connections: u64,
    pub avg_connections: f64,
    pub entry_point: Option<u64>,
    pub ivf_n_lists: Option<u32>,
    pub ivf_non_empty_lists: Option<u32>,
    pub ivf_trained: Option<bool>,
    pub graph_edge_count: Option<u64>,
    pub graph_node_count: Option<u64>,
    pub graph_label_count: Option<u32>,
    pub text_doc_count: Option<u64>,
    pub text_term_count: Option<u64>,
    pub text_posting_count: Option<u64>,
    pub document_doc_count: Option<u64>,
    pub document_path_count: Option<u64>,
    pub document_value_count: Option<u64>,
    pub document_unique_value_count: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct NativeVectorArtifactBatchInspection {
    pub inspected_count: usize,
    pub valid_count: usize,
    pub artifacts: Vec<NativeVectorArtifactInspection>,
    pub failures: Vec<(String, String, String)>,
}

mod impl_access;
mod impl_core_a;
mod impl_core_b;
mod impl_ec;
mod impl_metadata;
mod impl_registry;

impl Default for RedDB {
    fn default() -> Self {
        Self::new()
    }
}

fn infer_collection_index_kind(model: CollectionModel, index_name: &str) -> IndexKind {
    match index_name {
        "graph-adjacency" => IndexKind::GraphAdjacency,
        "vector-hnsw" => IndexKind::VectorHnsw,
        "vector-inverted" => IndexKind::VectorInverted,
        "vector-turbo" => IndexKind::VectorTurbo,
        "text-fulltext" => IndexKind::FullText,
        "document-pathvalue" => IndexKind::DocumentPathValue,
        "search-hybrid" => IndexKind::HybridSearch,
        _ => match model {
            CollectionModel::Graph => IndexKind::GraphAdjacency,
            CollectionModel::Vector => IndexKind::VectorHnsw,
            CollectionModel::Document => IndexKind::DocumentPathValue,
            CollectionModel::Kv | CollectionModel::Config | CollectionModel::Vault => {
                IndexKind::Hash
            }
            _ => IndexKind::BTree,
        },
    }
}

fn estimate_index_entries(collection: &CollectionDescriptor, kind: IndexKind) -> usize {
    match kind {
        IndexKind::BTree | IndexKind::Hash | IndexKind::Bitmap | IndexKind::Spatial => {
            collection.entities
        }
        IndexKind::GraphAdjacency => collection.cross_refs.max(collection.entities),
        IndexKind::VectorHnsw | IndexKind::VectorInverted | IndexKind::VectorTurbo => {
            collection.entities
        }
        IndexKind::FullText => collection.entities.saturating_mul(4),
        IndexKind::DocumentPathValue => collection.entities.saturating_mul(6),
        IndexKind::HybridSearch => collection.entities,
    }
}

fn estimate_index_memory(entries: usize, kind: IndexKind) -> u64 {
    let per_entry = match kind {
        IndexKind::BTree => 64,
        IndexKind::Hash => 48,
        IndexKind::Bitmap => 2,   // Roaring bitmaps are very compact
        IndexKind::Spatial => 40, // R-tree node: 2 floats + EntityId + overhead
        IndexKind::GraphAdjacency => 96,
        IndexKind::VectorHnsw => 256,
        IndexKind::VectorInverted => 128,
        IndexKind::VectorTurbo => 64,
        IndexKind::FullText => 80,
        IndexKind::DocumentPathValue => 104,
        IndexKind::HybridSearch => 144,
    };
    (entries as u64).saturating_mul(per_entry)
}

fn index_backend_name(kind: IndexKind) -> &'static str {
    match kind {
        IndexKind::BTree => "page-btree",
        IndexKind::Hash => "hash-map",
        IndexKind::Bitmap => "roaring-bitmap",
        IndexKind::Spatial => "rstar-rtree",
        IndexKind::GraphAdjacency => "adjacency-map",
        IndexKind::VectorHnsw => "vector-hnsw",
        IndexKind::VectorInverted => "vector-ivf",
        IndexKind::VectorTurbo => "vector-turbo",
        IndexKind::FullText => "inverted-text",
        IndexKind::DocumentPathValue => "document-pathvalue",
        IndexKind::HybridSearch => "hybrid-score",
    }
}

fn fnv1a_seed() -> u64 {
    0xcbf29ce484222325
}

fn fnv1a_hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= *byte as u64;
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

fn fnv1a_hash_value<T: Debug>(hash: &mut u64, value: &T) {
    let rendered = format!("{value:?}");
    fnv1a_hash_bytes(hash, rendered.as_bytes());
}
