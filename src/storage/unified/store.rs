//! Unified Store
//!
//! High-level API for the unified storage layer that combines tables, graphs,
//! and vectors into a single coherent interface.
//!
//! # Features
//!
//! - Multi-collection management
//! - Cross-collection queries
//! - Unified entity access
//! - Automatic ID generation
//! - Cross-reference management
//! - **Binary persistence** with pages, indices, and efficient encoding
//! - **Page-based storage** via Pager for ACID durability
//!
//! # Persistence Modes
//!
//! 1. **File Mode** (`save_to_file`/`load_from_file`): Simple binary dump
//! 2. **Paged Mode** (`open`/`persist`): Full page-based storage with B-tree indices

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use super::context_index::ContextIndex;
use super::entity::{
    CrossRef, EdgeData, EmbeddingSlot, EntityData, EntityId, EntityKind, GraphEdgeKind,
    GraphNodeKind, NodeData, RefType, RowData, TimeSeriesPointKind, UnifiedEntity, VectorData,
};
use super::manager::{ManagerConfig, ManagerStats, SegmentManager};
use super::metadata::{Metadata, MetadataFilter, MetadataValue};
use super::segment::SegmentError;
use crate::physical::{ManifestEvent, ManifestEventKind};
use crate::storage::engine::pager::PagerError;
use crate::storage::engine::{BTree, BTreeError, Pager, PagerConfig, PhysicalFileHeader};
use crate::storage::primitives::encoding::{read_varu32, read_varu64, write_varu32, write_varu64};
use crate::storage::schema::types::Value;

const STORE_MAGIC: &[u8; 4] = b"RDST";
const STORE_VERSION_V1: u32 = 1;
const STORE_VERSION_V2: u32 = 2;
const STORE_VERSION_V3: u32 = 3;
const METADATA_MAGIC: &[u8; 4] = b"RDM2";
const NATIVE_COLLECTION_ROOTS_MAGIC: &[u8; 4] = b"RDRT";
const NATIVE_MANIFEST_MAGIC: &[u8; 4] = b"RDMF";
const NATIVE_REGISTRY_MAGIC: &[u8; 4] = b"RDRG";
const NATIVE_RECOVERY_MAGIC: &[u8; 4] = b"RDRV";
const NATIVE_CATALOG_MAGIC: &[u8; 4] = b"RDCL";
const NATIVE_METADATA_STATE_MAGIC: &[u8; 4] = b"RDMS";
const NATIVE_VECTOR_ARTIFACT_MAGIC: &[u8; 4] = b"RDVA";
const NATIVE_BLOB_MAGIC: &[u8; 4] = b"RDBL";
const NATIVE_MANIFEST_SAMPLE_LIMIT: usize = 16;

#[derive(Debug, Clone)]
pub struct NativeManifestEntrySummary {
    pub collection: String,
    pub object_key: String,
    pub kind: String,
    pub block_index: u64,
    pub block_checksum: u128,
    pub snapshot_min: u64,
    pub snapshot_max: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct NativeManifestSummary {
    pub sequence: u64,
    pub event_count: u32,
    pub events_complete: bool,
    pub omitted_event_count: u32,
    pub recent_events: Vec<NativeManifestEntrySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRegistryIndexSummary {
    pub name: String,
    pub kind: String,
    pub collection: Option<String>,
    pub enabled: bool,
    pub entries: u64,
    pub estimated_memory_bytes: u64,
    pub last_refresh_ms: Option<u128>,
    pub backend: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRegistryProjectionSummary {
    pub name: String,
    pub source: String,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub node_labels: Vec<String>,
    pub node_types: Vec<String>,
    pub edge_labels: Vec<String>,
    pub last_materialized_sequence: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRegistryJobSummary {
    pub id: String,
    pub kind: String,
    pub projection: Option<String>,
    pub state: String,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub last_run_sequence: Option<u64>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeVectorArtifactSummary {
    pub collection: String,
    pub artifact_kind: String,
    pub vector_count: u64,
    pub dimension: u32,
    pub max_layer: u32,
    pub serialized_bytes: u64,
    pub checksum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeVectorArtifactPageSummary {
    pub collection: String,
    pub artifact_kind: String,
    pub root_page: u32,
    pub page_count: u32,
    pub byte_len: u64,
    pub checksum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRegistrySummary {
    pub collection_count: u32,
    pub index_count: u32,
    pub graph_projection_count: u32,
    pub analytics_job_count: u32,
    pub vector_artifact_count: u32,
    pub collections_complete: bool,
    pub indexes_complete: bool,
    pub graph_projections_complete: bool,
    pub analytics_jobs_complete: bool,
    pub vector_artifacts_complete: bool,
    pub omitted_collection_count: u32,
    pub omitted_index_count: u32,
    pub omitted_graph_projection_count: u32,
    pub omitted_analytics_job_count: u32,
    pub omitted_vector_artifact_count: u32,
    pub collection_names: Vec<String>,
    pub indexes: Vec<NativeRegistryIndexSummary>,
    pub graph_projections: Vec<NativeRegistryProjectionSummary>,
    pub analytics_jobs: Vec<NativeRegistryJobSummary>,
    pub vector_artifacts: Vec<NativeVectorArtifactSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeSnapshotSummary {
    pub snapshot_id: u64,
    pub created_at_unix_ms: u128,
    pub superblock_sequence: u64,
    pub collection_count: u32,
    pub total_entities: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeExportSummary {
    pub name: String,
    pub created_at_unix_ms: u128,
    pub snapshot_id: Option<u64>,
    pub superblock_sequence: u64,
    pub collection_count: u32,
    pub total_entities: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRecoverySummary {
    pub snapshot_count: u32,
    pub export_count: u32,
    pub snapshots_complete: bool,
    pub exports_complete: bool,
    pub omitted_snapshot_count: u32,
    pub omitted_export_count: u32,
    pub snapshots: Vec<NativeSnapshotSummary>,
    pub exports: Vec<NativeExportSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCatalogCollectionSummary {
    pub name: String,
    pub entities: u64,
    pub cross_refs: u64,
    pub segments: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeCatalogSummary {
    pub collection_count: u32,
    pub total_entities: u64,
    pub collections_complete: bool,
    pub omitted_collection_count: u32,
    pub collections: Vec<NativeCatalogCollectionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeMetadataStateSummary {
    pub protocol_version: String,
    pub generated_at_unix_ms: u128,
    pub last_loaded_from: Option<String>,
    pub last_healed_at_unix_ms: Option<u128>,
}

#[derive(Debug, Clone)]
pub struct NativePhysicalState {
    pub header: PhysicalFileHeader,
    pub collection_roots: BTreeMap<String, u64>,
    pub manifest: Option<NativeManifestSummary>,
    pub registry: Option<NativeRegistrySummary>,
    pub recovery: Option<NativeRecoverySummary>,
    pub catalog: Option<NativeCatalogSummary>,
    pub metadata_state: Option<NativeMetadataStateSummary>,
    pub vector_artifact_pages: Option<Vec<NativeVectorArtifactPageSummary>>,
}

// ============================================================================
// Configuration
// ============================================================================

/// Configuration for UnifiedStore
#[derive(Debug, Clone)]
pub struct UnifiedStoreConfig {
    /// Configuration for segment managers
    pub manager_config: ManagerConfig,
    /// Automatically index cross-references on insert
    pub auto_index_refs: bool,
    /// Maximum cross-references per entity
    pub max_cross_refs: usize,
    /// Enable write-ahead logging
    pub enable_wal: bool,
    /// Data directory path
    pub data_dir: Option<std::path::PathBuf>,
}

impl Default for UnifiedStoreConfig {
    fn default() -> Self {
        Self {
            manager_config: ManagerConfig::default(),
            auto_index_refs: true,
            max_cross_refs: 1000,
            enable_wal: false,
            data_dir: None,
        }
    }
}

impl UnifiedStoreConfig {
    /// Create config with data directory
    pub fn with_data_dir(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.data_dir = Some(path.into());
        self
    }

    /// Enable WAL
    pub fn with_wal(mut self) -> Self {
        self.enable_wal = true;
        self
    }

    /// Set max cross-references
    pub fn with_max_refs(mut self, max: usize) -> Self {
        self.max_cross_refs = max;
        self
    }
}

// ============================================================================
// Error Types
// ============================================================================

/// Errors from UnifiedStore operations
#[derive(Debug)]
pub enum StoreError {
    /// Collection already exists
    CollectionExists(String),
    /// Collection not found
    CollectionNotFound(String),
    /// Entity not found
    EntityNotFound(EntityId),
    /// Too many cross-references
    TooManyRefs(EntityId),
    /// Segment error
    Segment(SegmentError),
    /// I/O error
    Io(std::io::Error),
    /// Serialization error
    Serialization(String),
    /// Internal error (lock poisoning, invariant violation)
    Internal(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CollectionExists(name) => write!(f, "Collection already exists: {}", name),
            Self::CollectionNotFound(name) => write!(f, "Collection not found: {}", name),
            Self::EntityNotFound(id) => write!(f, "Entity not found: {}", id),
            Self::TooManyRefs(id) => write!(f, "Too many cross-references for entity: {}", id),
            Self::Segment(e) => write!(f, "Segment error: {:?}", e),
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Serialization(msg) => write!(f, "Serialization error: {}", msg),
            Self::Internal(msg) => write!(f, "Internal error: {}", msg),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<SegmentError> for StoreError {
    fn from(e: SegmentError) -> Self {
        Self::Segment(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ============================================================================
// Statistics
// ============================================================================

/// Statistics for UnifiedStore
#[derive(Debug, Clone, Default)]
pub struct StoreStats {
    /// Number of collections
    pub collection_count: usize,
    /// Total entities across all collections
    pub total_entities: usize,
    /// Total memory usage in bytes
    pub total_memory_bytes: usize,
    /// Per-collection statistics
    pub collections: HashMap<String, ManagerStats>,
    /// Total cross-references
    pub cross_ref_count: usize,
}

impl StoreStats {
    /// Get average entities per collection
    pub fn avg_entities_per_collection(&self) -> f64 {
        if self.collection_count == 0 {
            0.0
        } else {
            self.total_entities as f64 / self.collection_count as f64
        }
    }

    /// Get memory in MB
    pub fn memory_mb(&self) -> f64 {
        self.total_memory_bytes as f64 / (1024.0 * 1024.0)
    }
}

// ============================================================================
// UnifiedStore - The Main API
// ============================================================================

/// Unified storage for tables, graphs, and vectors
///
/// UnifiedStore provides a single coherent interface for all data types:
/// - **Tables**: Row-based data with columns
/// - **Graphs**: Nodes and edges with labels
/// - **Vectors**: Embeddings for similarity search
///
/// # Features
///
/// - Multi-collection management
/// - Cross-collection queries
/// - Cross-reference tracking between entities
/// - Automatic ID generation
/// - Segment-based storage with growing/sealed lifecycle
///
/// # Example
///
/// ```ignore
/// use reddb::storage::{Entity, Store};
///
/// let store = Store::new();
///
/// // Create a collection
/// store.create_collection("hosts")?;
///
/// // Insert an entity
/// let entity = Entity::table_row(1, "hosts", 1, vec![]);
/// let id = store.insert("hosts", entity)?;
///
/// // Query
/// let found = store.get("hosts", id);
/// ```
pub struct UnifiedStore {
    /// Store configuration
    config: UnifiedStoreConfig,
    /// File format version for serialization
    format_version: AtomicU32,
    /// Global entity ID counter
    next_entity_id: AtomicU64,
    /// Collections by name
    collections: RwLock<HashMap<String, Arc<SegmentManager>>>,
    /// Forward cross-references: source_id → [(target_id, ref_type, target_collection)]
    cross_refs: RwLock<HashMap<EntityId, Vec<(EntityId, RefType, String)>>>,
    /// Reverse cross-references: target_id → [(source_id, ref_type, source_collection)]
    reverse_refs: RwLock<HashMap<EntityId, Vec<(EntityId, RefType, String)>>>,
    /// Optional page-based storage via Pager
    pager: Option<Arc<Pager>>,
    /// Database file path (for paged mode)
    db_path: Option<PathBuf>,
    /// B-tree indices for O(log n) entity lookups by ID (per collection)
    btree_indices: RwLock<HashMap<String, BTree>>,
    /// Cross-structure context index for unified search
    context_index: ContextIndex,
    /// Hot entity cache — LRU for frequently accessed entities by ID
    entity_cache: RwLock<HashMap<u64, (String, UnifiedEntity)>>,
    /// Graph node label index: (collection, label) → Vec<EntityId>.
    /// O(1) lookup for MATCH (n:Label) graph patterns — avoids full collection scan.
    graph_label_index: RwLock<HashMap<(String, String), Vec<EntityId>>>,
}

mod builder;
mod impl_entities;
mod impl_file;
mod impl_native_a;
mod impl_native_b;
mod impl_native_c;
mod impl_pages;
mod native_helpers;

pub use self::builder::EntityBuilder;
use self::native_helpers::*;
