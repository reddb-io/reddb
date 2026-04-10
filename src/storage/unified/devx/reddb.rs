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
use super::preprocessors::{IndexConfig, Preprocessor};
use super::query::QueryBuilder;
use super::refs::{NodeRef, TableRef, VectorRef};
use super::types::{LinkedEntity, SimilarResult};
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
    preprocessors: Vec<Box<dyn Preprocessor>>,
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
    /// Optional remote storage backend for snapshot transport.
    pub(crate) remote_backend: Option<Box<dyn crate::storage::backend::RemoteBackend>>,
    /// Remote object key used by the remote backend.
    pub(crate) remote_key: Option<String>,
    /// Primary replication state (only present when role is Primary).
    pub(crate) replication: Option<Arc<PrimaryReplication>>,
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
        "text-fulltext" => IndexKind::FullText,
        "document-pathvalue" => IndexKind::DocumentPathValue,
        "search-hybrid" => IndexKind::HybridSearch,
        _ => match model {
            CollectionModel::Graph => IndexKind::GraphAdjacency,
            CollectionModel::Vector => IndexKind::VectorHnsw,
            CollectionModel::Document => IndexKind::DocumentPathValue,
            _ => IndexKind::BTree,
        },
    }
}

fn estimate_index_entries(collection: &CollectionDescriptor, kind: IndexKind) -> usize {
    match kind {
        IndexKind::BTree | IndexKind::Hash | IndexKind::Bitmap => collection.entities,
        IndexKind::GraphAdjacency => collection.cross_refs.max(collection.entities),
        IndexKind::VectorHnsw | IndexKind::VectorInverted => collection.entities,
        IndexKind::FullText => collection.entities.saturating_mul(4),
        IndexKind::DocumentPathValue => collection.entities.saturating_mul(6),
        IndexKind::HybridSearch => collection.entities,
    }
}

fn estimate_index_memory(entries: usize, kind: IndexKind) -> u64 {
    let per_entry = match kind {
        IndexKind::BTree => 64,
        IndexKind::Hash => 48,
        IndexKind::Bitmap => 2, // Roaring bitmaps are very compact
        IndexKind::GraphAdjacency => 96,
        IndexKind::VectorHnsw => 256,
        IndexKind::VectorInverted => 128,
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
        IndexKind::GraphAdjacency => "adjacency-map",
        IndexKind::VectorHnsw => "vector-hnsw",
        IndexKind::VectorInverted => "vector-ivf",
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
