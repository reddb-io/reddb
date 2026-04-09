//! Physical storage design primitives for RedDB's deterministic on-disk layout.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{
    CatalogSnapshot, CollectionStats, RedDBOptions, SchemaManifest, StorageMode,
    REDDB_FORMAT_VERSION,
};
use crate::index::IndexKind;
use crate::json::{from_slice, parse_json, to_vec};
use crate::serde_json::{Map, Value as JsonValue};

pub const DEFAULT_GRID_BLOCK_SIZE: usize = 512 * 1024;
pub const DEFAULT_PAGE_SIZE: usize = 4096;
pub const DEFAULT_SUPERBLOCK_COPIES: u8 = 4;
pub const PHYSICAL_METADATA_PROTOCOL_VERSION: &str = "reddb-physical-v1";
pub const PHYSICAL_METADATA_BINARY_EXTENSION: &str = "meta.rdbx";
pub const DEFAULT_MANIFEST_EVENT_HISTORY: usize = 256;
pub const DEFAULT_METADATA_JOURNAL_RETENTION: usize = 32;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockReference {
    pub index: u64,
    pub checksum: u128,
}

#[derive(Debug, Clone, Default)]
pub struct ManifestPointers {
    pub oldest: BlockReference,
    pub newest: BlockReference,
}

#[derive(Debug, Clone)]
pub struct SuperblockHeader {
    pub format_version: u32,
    pub sequence: u64,
    pub copies: u8,
    pub manifest: ManifestPointers,
    pub free_set: BlockReference,
    pub collection_roots: BTreeMap<String, u64>,
}

impl Default for SuperblockHeader {
    fn default() -> Self {
        Self {
            format_version: crate::api::REDDB_FORMAT_VERSION,
            sequence: 0,
            copies: DEFAULT_SUPERBLOCK_COPIES,
            manifest: ManifestPointers::default(),
            free_set: BlockReference::default(),
            collection_roots: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestEventKind {
    Insert,
    Update,
    Remove,
    Checkpoint,
}

#[derive(Debug, Clone)]
pub struct ManifestEvent {
    pub collection: String,
    pub object_key: String,
    pub kind: ManifestEventKind,
    pub block: BlockReference,
    pub snapshot_min: u64,
    pub snapshot_max: Option<u64>,
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

#[derive(Debug, Clone, Default)]
pub struct SnapshotDescriptor {
    pub snapshot_id: u64,
    pub created_at_unix_ms: u128,
    pub superblock_sequence: u64,
    pub collection_count: usize,
    pub total_entities: usize,
}

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

#[derive(Debug, Clone)]
pub struct ExportDescriptor {
    pub name: String,
    pub created_at_unix_ms: u128,
    pub snapshot_id: Option<u64>,
    pub superblock_sequence: u64,
    pub data_path: String,
    pub metadata_path: String,
    pub collection_count: usize,
    pub total_entities: usize,
}

#[derive(Debug, Clone)]
pub struct PhysicalGraphProjection {
    pub name: String,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub state: String,
    pub source: String,
    pub node_labels: Vec<String>,
    pub node_types: Vec<String>,
    pub edge_labels: Vec<String>,
    pub last_materialized_sequence: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct PhysicalAnalyticsJob {
    pub id: String,
    pub kind: String,
    pub state: String,
    pub projection: Option<String>,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub last_run_sequence: Option<u64>,
    pub metadata: BTreeMap<String, String>,
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
    pub exports: Vec<ExportDescriptor>,
    pub superblock: SuperblockHeader,
    pub snapshots: Vec<SnapshotDescriptor>,
}

mod helpers;
mod json_codec;
mod metadata_file;

use self::helpers::*;
use self::json_codec::*;
