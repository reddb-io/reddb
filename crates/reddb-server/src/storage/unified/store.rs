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
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use super::context_index::ContextIndex;
use super::entity::{
    CrossRef, EdgeData, EmbeddingSlot, EntityData, EntityId, EntityKind, GraphEdgeKind,
    GraphNodeKind, NodeData, RefType, RowData, TimeSeriesPointKind, UnifiedEntity, VectorData,
};
use super::entity_cache::EntityCache;
use super::manager::{ManagerConfig, ManagerStats, SegmentManager};
use super::metadata::{Metadata, MetadataFilter, MetadataValue};
use super::segment::SegmentError;
use crate::api::{DurabilityMode, GroupCommitOptions};
use crate::physical::{ManifestEvent, ManifestEventKind};
use crate::storage::engine::pager::PagerError;
use crate::storage::engine::{BTree, BTreeError, Pager, PagerConfig, PhysicalFileHeader};
use crate::storage::primitives::encoding::{read_varu32, read_varu64, write_varu32, write_varu64};
use crate::storage::schema::types::Value;

pub use reddb_file::{
    is_supported_store_version, NativeCatalogCollectionSummary, NativeCatalogSummary,
    NativeExportSummary, NativeManifestEntrySummary, NativeManifestSummary,
    NativeMetadataStateSummary, NativeRecoverySummary, NativeRegistryIndexSummary,
    NativeRegistryJobSummary, NativeRegistryProjectionSummary, NativeRegistrySummary,
    NativeSnapshotSummary, NativeVectorArtifactPageSummary, NativeVectorArtifactSummary,
    ENTITY_RECORD_MAGIC, METADATA_MAGIC, METADATA_OVERFLOW_MAGIC, NATIVE_BLOB_MAGIC,
    NATIVE_CATALOG_MAGIC, NATIVE_COLLECTION_ROOTS_MAGIC, NATIVE_MANIFEST_MAGIC,
    NATIVE_MANIFEST_SAMPLE_LIMIT, NATIVE_METADATA_STATE_MAGIC, NATIVE_RECOVERY_MAGIC,
    NATIVE_REGISTRY_MAGIC, NATIVE_VECTOR_ARTIFACT_MAGIC, STORE_MAGIC, STORE_VERSION_CURRENT,
    STORE_VERSION_V1, STORE_VERSION_V11, STORE_VERSION_V12, STORE_VERSION_V2, STORE_VERSION_V3,
    STORE_VERSION_V4, STORE_VERSION_V5, STORE_VERSION_V6, STORE_VERSION_V7, STORE_VERSION_V8,
    STORE_VERSION_V9,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MvccVacuumStats {
    pub scanned_versions: u64,
    pub retained_versions: u64,
    pub reclaimed_versions: u64,
    pub retained_history_versions: u64,
    pub reclaimed_history_versions: u64,
    pub retained_tombstones: u64,
    pub reclaimed_tombstones: u64,
}

impl MvccVacuumStats {
    pub fn add(&mut self, other: &Self) {
        self.scanned_versions += other.scanned_versions;
        self.retained_versions += other.retained_versions;
        self.reclaimed_versions += other.reclaimed_versions;
        self.retained_history_versions += other.retained_history_versions;
        self.reclaimed_history_versions += other.reclaimed_history_versions;
        self.retained_tombstones += other.retained_tombstones;
        self.reclaimed_tombstones += other.reclaimed_tombstones;
    }
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
    /// Automatically build a HASH index on a user `id` column the first
    /// time a row carrying that column is inserted into a collection.
    /// Mirrors PostgreSQL's implicit primary-key index and Mongo's `_id`
    /// default index — without it, `WHERE id = N` falls through to a
    /// full segment scan because RedDB has no concept of an automatic
    /// primary-key index on user-declared columns. See `docs/perf/
    /// delete-sequential-2026-05-06.md` for the perf rationale.
    /// Defaults to `true`; set to `false` to opt out per workload.
    pub auto_index_id: bool,
    /// Maximum cross-references per entity
    pub max_cross_refs: usize,
    /// Enable write-ahead logging
    pub enable_wal: bool,
    /// Durability profile for paged writes.
    pub durability_mode: DurabilityMode,
    /// Group-commit batching knobs when using grouped durability.
    pub group_commit: GroupCommitOptions,
    /// Data directory path
    pub data_dir: Option<std::path::PathBuf>,
    /// Embedded single-file artifact used for the internal WAL stream.
    pub embedded_wal_path: Option<std::path::PathBuf>,
    /// Preallocated page-cache slot count (ADR 0073 §2). `None` leaves the
    /// pager on its own structural default; the promoted boot path always
    /// supplies a count derived from the page-cache budget share, so the
    /// default is reached only by direct library callers.
    pub page_cache_slots: Option<usize>,
}

impl Default for UnifiedStoreConfig {
    fn default() -> Self {
        Self {
            manager_config: ManagerConfig::default(),
            auto_index_refs: true,
            auto_index_id: true,
            max_cross_refs: 1000,
            enable_wal: false,
            // Mirrors `RedDBOptions::default().durability_mode` — see
            // `src/api.rs` for the rationale.
            durability_mode: DurabilityMode::WalDurableGrouped,
            group_commit: GroupCommitOptions::default(),
            data_dir: None,
            embedded_wal_path: None,
            page_cache_slots: None,
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

    pub fn with_durability_mode(mut self, mode: DurabilityMode) -> Self {
        self.durability_mode = mode;
        self
    }

    pub fn with_group_commit(mut self, options: GroupCommitOptions) -> Self {
        self.group_commit = options;
        self
    }

    pub fn with_embedded_wal_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
        self.embedded_wal_path = Some(path.into());
        self
    }

    /// Pre-size the page cache from its budget share (ADR 0073 §2).
    pub fn with_page_cache_slots(mut self, slots: usize) -> Self {
        self.page_cache_slots = Some(slots);
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
    /// Checksummed storage bytes failed validation.
    StorageIntegrity(crate::api::StorageIntegrityError),
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
            Self::StorageIntegrity(e) => write!(f, "{e}"),
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
    /// B-tree indices for O(log n) entity lookups by ID (per collection).
    /// Stored as `Arc<BTree>` so hot-path callers can clone the handle out
    /// under a read lock and release the map-level lock before doing the
    /// actual insert — previously the outer RwLock was held for the whole
    /// btree mutation, serialising every concurrent insert across every
    /// collection into one global write lock.
    btree_indices: RwLock<HashMap<String, Arc<BTree>>>,
    /// Cross-structure context index for unified search
    context_index: ContextIndex,
    /// Hot entity cache — sharded bounded LRU for `get_any` lookups.
    /// See `entity_cache.rs` for the rationale; this replaced a single
    /// `RwLock<HashMap>` that serialised every `delete_batch` invalidation.
    entity_cache: EntityCache,
    /// Graph node label index: (collection, label) → Vec<EntityId>.
    /// O(1) lookup for MATCH (n:Label) graph patterns — avoids full collection scan.
    graph_label_index: RwLock<HashMap<(String, String), Vec<EntityId>>>,
    /// Whether the paged registry on page 1 must be rewritten before the next flush.
    paged_registry_dirty: AtomicBool,
    /// Logical store WAL / grouped durability coordinator for paged mode.
    commit: Option<Arc<StoreCommitCoordinator>>,
    /// Counts how often `unindex_cross_refs_batch` took the read-only fast
    /// path (no inbound refs, no outbound refs for any deleted id) and so
    /// avoided acquiring the `cross_refs` / `reverse_refs` write locks.
    /// Used by tests to pin the early-exit; cheap relaxed counter otherwise.
    unindex_cross_refs_fast_path: AtomicU64,
    /// WAL-replayed `VectorInsert` records, captured at open time and
    /// drained per-collection on first `vector.turbo` access (issue
    /// #694). Boot-time recovery: the in-memory TurboQuant index is
    /// rebuilt by replaying these FP32 vectors in WAL order, so the
    /// rebuilt state is byte-deterministic against the pre-restart
    /// state under a fixed codec seed.
    pub(crate) replayed_turbo_inserts: parking_lot::Mutex<HashMap<String, Vec<(u64, Vec<f32>)>>>,
    /// WAL-replayed probabilistic mutation deltas captured at open time.
    /// Runtime recovery drains these after loading the latest full
    /// probabilistic snapshot from store state.
    pub(crate) replayed_probabilistic_deltas:
        parking_lot::Mutex<Vec<(u8, u8, String, Vec<Vec<u8>>)>>,
    /// Opaque store-level auxiliary metadata persisted inside the binary dump
    /// (store format V10+). RedDB uses this to carry collection contracts
    /// through the single-file artifact so a collection's `declared_model`
    /// (e.g. `kv`) survives a restart instead of being re-inferred as a table.
    /// The store treats the bytes as opaque; only RedDB interprets them.
    pub(crate) aux_metadata: RwLock<Vec<u8>>,
}

mod builder;
mod commit;
mod impl_entities;
mod impl_file;
mod impl_native_a;
mod impl_native_b;
mod impl_native_c;
mod impl_pages;
mod native_helpers;

pub use self::builder::EntityBuilder;
pub(crate) use self::commit::DeferredStoreWalActions;
use self::commit::{StoreCommitCoordinator, StoreWalAction};
use self::native_helpers::*;
