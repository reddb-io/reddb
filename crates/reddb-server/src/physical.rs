//! Physical storage design primitives for RedDB's deterministic on-disk layout.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{CatalogSnapshot, CollectionStats, RedDBOptions, SchemaManifest, StorageMode};
use crate::index::IndexKind;
use crate::serde_json::{Map, Value as JsonValue};

pub const DEFAULT_GRID_BLOCK_SIZE: usize = 512 * 1024;
pub const DEFAULT_PAGE_SIZE: usize = 4096;
pub use reddb_file::layout::PHYSICAL_METADATA_BINARY_EXTENSION;
pub use reddb_file::{
    fold_dwb_into_wal_enabled, meta_json_sidecar_enabled, seqn_journal_enabled,
    seqn_journal_retention, set_fold_dwb_into_wal_enabled, set_meta_json_sidecar_enabled,
    set_seqn_journal_enabled, set_seqn_journal_retention, BlockReference, ExportDescriptor,
    ManifestEvent, ManifestEventKind, ManifestPointers, PhysicalAnalyticsJob,
    PhysicalGraphProjection, PhysicalTreeDefinition, SnapshotDescriptor, SuperblockHeader,
    DEFAULT_METADATA_JOURNAL_RETENTION, DEFAULT_SUPERBLOCK_COPIES,
    OPT_IN_METADATA_JOURNAL_RETENTION, PHYSICAL_METADATA_PROTOCOL_VERSION,
};
pub const DEFAULT_MANIFEST_EVENT_HISTORY: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalMetadataSource {
    Binary,
    BinaryJournal,
    Json,
}

impl PhysicalMetadataSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::BinaryJournal => "binary_journal",
            Self::Json => "json",
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionPolicy {
    Incremental,
    Manual,
}

#[derive(Debug, Clone)]
pub struct WalPolicy {
    pub auto_checkpoint_pages: u32,
    pub fsync_on_commit: bool,
    pub ring_buffer_bytes: u64,
}

impl Default for WalPolicy {
    fn default() -> Self {
        Self {
            auto_checkpoint_pages: 1000,
            fsync_on_commit: true,
            ring_buffer_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GridLayout {
    pub block_size: usize,
    pub page_size: usize,
    pub superblock_copies: u8,
}

impl Default for GridLayout {
    fn default() -> Self {
        Self {
            block_size: DEFAULT_GRID_BLOCK_SIZE,
            page_size: DEFAULT_PAGE_SIZE,
            superblock_copies: DEFAULT_SUPERBLOCK_COPIES,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PhysicalLayout {
    pub mode: StorageMode,
    pub grid: GridLayout,
    pub wal: WalPolicy,
    pub compaction: CompactionPolicy,
}

impl PhysicalLayout {
    pub fn from_options(options: &RedDBOptions) -> Self {
        Self {
            mode: options.mode,
            grid: GridLayout::default(),
            wal: WalPolicy {
                auto_checkpoint_pages: options.auto_checkpoint_pages,
                ..WalPolicy::default()
            },
            compaction: CompactionPolicy::Incremental,
        }
    }

    pub fn is_persistent(&self) -> bool {
        self.mode == StorageMode::Persistent
    }
}

pub(crate) use reddb_engine::catalog::{
    is_collection_contract_decode_error, CollectionContractDecodeError,
};
pub use reddb_engine::catalog::{CollectionContract, ContractOrigin, DeclaredColumnContract};

/// Canonical artifact lifecycle states.
///
/// State machine transitions:
/// ```text
///   Declared ──► Building ──► Ready ──► Stale ──► RequiresRebuild
///       │            │          │                       │
///       │            ▼          ▼                       │
///       │         Failed    Disabled                    │
///       │            │                                  │
///       └────────────┴──────────────────────────────────┘
///                    (rebuild restarts from Building)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactState {
    /// Index declared but never materialized.
    Declared,
    /// Artifact is being built or rebuilt.
    Building,
    /// Artifact is materialized and queryable.
    Ready,
    /// Artifact is explicitly disabled by the operator.
    Disabled,
    /// Underlying data changed; artifact is out of date.
    Stale,
    /// Build or warmup failed; manual intervention may be needed.
    Failed,
    /// Artifact must be rebuilt before it can serve reads.
    RequiresRebuild,
}

impl ArtifactState {
    /// Parse from the legacy string representation stored in physical metadata.
    pub fn from_build_state(s: &str, enabled: bool) -> Self {
        if !enabled {
            return Self::Disabled;
        }
        match s {
            "ready" => Self::Ready,
            "building" | "catalog-derived" | "metadata-only" | "artifact-published"
            | "registry-loaded" => Self::Building,
            "stale" => Self::Stale,
            "failed" => Self::Failed,
            "requires_rebuild" | "requires-rebuild" => Self::RequiresRebuild,
            _ => Self::Declared,
        }
    }

    /// Canonical string representation for storage and API surfaces.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Building => "building",
            Self::Ready => "ready",
            Self::Disabled => "disabled",
            Self::Stale => "stale",
            Self::Failed => "failed",
            Self::RequiresRebuild => "requires_rebuild",
        }
    }

    /// Whether this artifact is safe for query reads.
    pub fn is_queryable(&self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Whether a rebuild operation is valid from this state.
    pub fn can_rebuild(&self) -> bool {
        matches!(
            self,
            Self::Declared | Self::Stale | Self::Failed | Self::RequiresRebuild
        )
    }

    /// Whether this state indicates the artifact needs attention.
    pub fn needs_attention(&self) -> bool {
        matches!(self, Self::Failed | Self::RequiresRebuild | Self::Stale)
    }
}

impl std::fmt::Display for ArtifactState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone)]
pub struct PhysicalIndexState {
    pub name: String,
    pub kind: IndexKind,
    pub collection: Option<String>,
    pub enabled: bool,
    pub entries: usize,
    pub estimated_memory_bytes: u64,
    pub last_refresh_ms: Option<u128>,
    pub backend: String,
    pub artifact_kind: Option<String>,
    pub artifact_root_page: Option<u32>,
    pub artifact_checksum: Option<u64>,
    pub build_state: String,
}

impl PhysicalIndexState {
    /// Canonical artifact lifecycle state derived from physical state.
    pub fn artifact_state(&self) -> ArtifactState {
        ArtifactState::from_build_state(&self.build_state, self.enabled)
    }
}

/// A single persisted hypertable chunk. Mirror of
/// `storage::timeseries::ChunkMeta`, flattened for the metadata
/// sidecar so the registry's routing spine survives a restart
/// (issue #866). `start_ns` plus the owning hypertable name is the
/// chunk's stable identity.
#[derive(Debug, Clone)]
pub struct PhysicalHypertableChunk {
    pub start_ns: u64,
    pub end_ns_exclusive: u64,
    pub row_count: u64,
    pub min_ts_ns: u64,
    pub max_ts_ns: u64,
    pub sealed: bool,
    pub ttl_override_ns: Option<u64>,
    /// Columnar-vs-row migration discriminant — mirror of
    /// `ChunkMeta.columnar_page` (PRD #850, Phase 1). `Some` → the chunk's
    /// `RDCC` `ColumnBlock` location; `None` → legacy row-stored. Absent on
    /// sidecars written before the feature, decoding to `None`.
    pub columnar_page: Option<crate::storage::engine::PageLocation>,
    /// Columnar blocks are derived, rebuildable artifacts (ADR 0069), not
    /// source-of-truth row data. Backup/restore may skip or rebuild them.
    pub columnar_derived: bool,
}

/// A persisted hypertable spec plus all of its chunks. Stored in the
/// physical metadata sidecar alongside collection contracts so chunk
/// bounds / routing / TTL are recovered identically after a restart
/// — the same durability path the rest of the catalog already uses,
/// not a parallel one (issue #866).
#[derive(Debug, Clone)]
pub struct PhysicalHypertable {
    pub name: String,
    pub time_column: String,
    pub chunk_interval_ns: u64,
    pub default_ttl_ns: Option<u64>,
    pub chunks: Vec<PhysicalHypertableChunk>,
}

#[derive(Debug, Clone)]
pub struct PhysicalMetadataFile {
    pub protocol_version: String,
    pub generated_at_unix_ms: u128,
    pub last_loaded_from: Option<String>,
    pub last_healed_at_unix_ms: Option<u128>,
    pub manifest: SchemaManifest,
    pub catalog: CatalogSnapshot,
    pub manifest_events: Vec<ManifestEvent>,
    pub indexes: Vec<PhysicalIndexState>,
    pub graph_projections: Vec<PhysicalGraphProjection>,
    pub analytics_jobs: Vec<PhysicalAnalyticsJob>,
    pub tree_definitions: Vec<PhysicalTreeDefinition>,
    pub collection_ttl_defaults_ms: BTreeMap<String, u64>,
    pub collection_contracts: Vec<CollectionContract>,
    /// Persisted hypertable chunk spine (issue #866). Empty on legacy
    /// sidecars written before the feature and for non-hypertable
    /// databases.
    pub hypertables: Vec<PhysicalHypertable>,
    pub exports: Vec<ExportDescriptor>,
    pub superblock: SuperblockHeader,
    pub snapshots: Vec<SnapshotDescriptor>,
}

mod helpers;
mod json_codec;
mod metadata_file;
pub mod shm;

pub(crate) use self::json_codec::{
    deserialize_collection_contracts, serialize_collection_contracts,
};

pub use self::shm::{
    provision_shm, read_shm_header, set_shm_provisioning_enabled, shm_path_for,
    shm_provisioning_enabled, ShmHandle, ShmHeader, ShmProvisionState, SHM_FILE_SIZE,
    SHM_HEADER_SIZE, SHM_MAGIC, SHM_VERSION,
};

use self::helpers::*;
use self::json_codec::*;
