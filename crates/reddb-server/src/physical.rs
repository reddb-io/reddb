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
/// Retention applied when the seq-N catalog journal is enabled at the `Max`
/// tier. See [`seqn_journal_retention`].
pub const DEFAULT_METADATA_JOURNAL_RETENTION: usize = 32;
/// Retention applied when the seq-N catalog journal is opt-in enabled outside
/// of the `Max` tier — keeps forensics surface minimal on lower tiers.
pub const OPT_IN_METADATA_JOURNAL_RETENTION: usize = 4;

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

// JSON sidecar policy. 0 = unset (consult env, default off), 1 = enabled,
// 2 = disabled. Threaded as a process-global because the metadata save path
// is reached from many call sites that do not currently carry a layout
// handle. Tier wiring (#469/#471/#472) flips this on at startup for `Max`;
// minimal/standard/performance leave it off and emit only the binary
// `<data>.meta.rdbx` + journal entries.
static META_JSON_SIDECAR_POLICY: AtomicU8 = AtomicU8::new(0);

/// Process-wide opt-in for the legacy `<data>.meta.json` sidecar.
/// Call once at startup after resolving the active [`StorageLayout`].
pub fn set_meta_json_sidecar_enabled(enabled: bool) {
    META_JSON_SIDECAR_POLICY.store(if enabled { 1 } else { 2 }, Ordering::Relaxed);
}

/// Whether new metadata writes should additionally emit the JSON sidecar.
/// Defaults to `false`; opt-in via [`set_meta_json_sidecar_enabled`] or the
/// `REDDB_META_JSON_SIDECAR=1` env var (escape hatch for ad-hoc debugging
/// of a non-Max instance). Reads always tolerate either JSON or binary.
pub fn meta_json_sidecar_enabled() -> bool {
    match META_JSON_SIDECAR_POLICY.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => std::env::var("REDDB_META_JSON_SIDECAR")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
            .unwrap_or(false),
    }
}

// Seq-N catalog journal policy. 0 = unset (consult env, default off), 1 =
// enabled, 2 = disabled. Mirrors the meta-json sidecar toggle but governs the
// `<data>.meta.rdbx.seq-{N}` forensic trail emitted on every metadata save.
// Tier wiring (deferred) flips this on for `Max` with retention 32; opt-in
// elsewhere lands with retention 4. See `seqn_journal_retention`.
static SEQN_JOURNAL_POLICY: AtomicU8 = AtomicU8::new(0);
// Retention override. 0 = unset (consult env, default off-tier retention).
static SEQN_JOURNAL_RETENTION: AtomicUsize = AtomicUsize::new(0);

/// Process-wide opt-in for the seq-N catalog journal (`<data>.meta.rdbx.seq-N`
/// snapshot trail). Defaults off so non-`Max` tiers don't accumulate forensic
/// artifacts. Tier wiring should call this with `true` for `Max`. Escape
/// hatch: `REDDB_SEQN_JOURNAL=1`.
pub fn set_seqn_journal_enabled(enabled: bool) {
    SEQN_JOURNAL_POLICY.store(if enabled { 1 } else { 2 }, Ordering::Relaxed);
}

/// Whether new metadata saves should also emit a seq-N journal entry.
pub fn seqn_journal_enabled() -> bool {
    match SEQN_JOURNAL_POLICY.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => std::env::var("REDDB_SEQN_JOURNAL")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
            .unwrap_or(false),
    }
}

// Pager-meta sidecar policy (#477). 0 = unset (consult env, default off — keep
// `<data>-meta` shadow), 1 = enabled (fold meta into page 1 + overflow chain;
// no `-meta` sidecar), 2 = disabled (current behavior). Tier wiring (deferred)
// flips this on for tiers that prefer a single datafile artifact. Escape hatch:
// `REDDB_FOLD_PAGER_META=1`.
static FOLD_PAGER_META_POLICY: AtomicU8 = AtomicU8::new(0);

/// Process-wide opt-in for folding pager metadata (page 1) into the datafile
/// without an adjacent `<data>-meta` shadow. When enabled, the corruption-
/// recovery shadow at `<data>-meta` is not written; readers trust page 1
/// (plus its overflow chain) as the single source of truth. Defaults off.
pub fn set_fold_pager_meta_enabled(enabled: bool) {
    FOLD_PAGER_META_POLICY.store(if enabled { 1 } else { 2 }, Ordering::Relaxed);
}

/// Whether the pager should fold metadata into page 1 only and skip the
/// `<data>-meta` sidecar shadow. Reads still tolerate the sidecar so existing
/// databases keep working through the flag flip.
pub fn fold_pager_meta_enabled() -> bool {
    match FOLD_PAGER_META_POLICY.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => std::env::var("REDDB_FOLD_PAGER_META")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
            .unwrap_or(false),
    }
}

// Fold-DWB-into-WAL policy (#478). 0 = unset (consult env, default off — keep
// `-dwb` sidecar), 1 = enabled (emit FullPageImage WAL records before first
// page modification per checkpoint cycle; no `-dwb` sidecar), 2 = disabled.
// Tier wiring (deferred) flips this on for tiers that prefer a single WAL-
// rooted recovery path. Escape hatch: `REDDB_FOLD_DWB_INTO_WAL=1`.
static FOLD_DWB_INTO_WAL_POLICY: AtomicU8 = AtomicU8::new(0);

/// Process-wide opt-in for folding the double-write buffer into the WAL via
/// full-page-image (FPI) records. When enabled, the pager does not open or
/// write `<data>-dwb`; recovery rebuilds torn pages from FPI records replayed
/// before normal redo. Defaults off.
pub fn set_fold_dwb_into_wal_enabled(enabled: bool) {
    FOLD_DWB_INTO_WAL_POLICY.store(if enabled { 1 } else { 2 }, Ordering::Relaxed);
}

/// Whether the pager should fold DWB into WAL (no `<data>-dwb` sidecar).
/// Reads still tolerate the legacy sidecar so existing databases keep
/// working through the flag flip.
pub fn fold_dwb_into_wal_enabled() -> bool {
    match FOLD_DWB_INTO_WAL_POLICY.load(Ordering::Relaxed) {
        1 => true,
        2 => false,
        _ => std::env::var("REDDB_FOLD_DWB_INTO_WAL")
            .ok()
            .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
            .unwrap_or(false),
    }
}

/// Process-wide retention for the seq-N catalog journal. Tier wiring should
/// call this with `DEFAULT_METADATA_JOURNAL_RETENTION` (32) for `Max` and
/// `OPT_IN_METADATA_JOURNAL_RETENTION` (4) for opt-in non-`Max` use.
/// `0` resets to defaults (env or off-tier baseline).
pub fn set_seqn_journal_retention(retention: usize) {
    SEQN_JOURNAL_RETENTION.store(retention, Ordering::Relaxed);
}

/// Resolved retention bound for the seq-N journal. Falls back to env
/// `REDDB_SEQN_JOURNAL_RETENTION`, then to `OPT_IN_METADATA_JOURNAL_RETENTION`
/// (4) — the conservative off-tier baseline.
pub fn seqn_journal_retention() -> usize {
    let stored = SEQN_JOURNAL_RETENTION.load(Ordering::Relaxed);
    if stored > 0 {
        return stored;
    }
    std::env::var("REDDB_SEQN_JOURNAL_RETENTION")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(OPT_IN_METADATA_JOURNAL_RETENTION)
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractOrigin {
    Explicit,
    Implicit,
    Migrated,
}

impl ContractOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Implicit => "implicit",
            Self::Migrated => "migrated",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeclaredColumnContract {
    pub name: String,
    pub data_type: String,
    pub sql_type: Option<crate::storage::schema::SqlTypeName>,
    pub not_null: bool,
    pub default: Option<String>,
    pub compress: Option<u8>,
    pub unique: bool,
    pub primary_key: bool,
    pub enum_variants: Vec<String>,
    pub array_element: Option<String>,
    pub decimal_precision: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct CollectionContract {
    pub name: String,
    pub declared_model: crate::catalog::CollectionModel,
    pub schema_mode: crate::catalog::SchemaMode,
    pub origin: ContractOrigin,
    pub version: u32,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub default_ttl_ms: Option<u64>,
    pub vector_dimension: Option<usize>,
    pub vector_metric: Option<crate::storage::engine::distance::DistanceMetric>,
    pub context_index_fields: Vec<String>,
    pub declared_columns: Vec<DeclaredColumnContract>,
    pub table_def: Option<crate::storage::schema::TableDef>,
    /// Enabled by `CREATE TABLE ... WITH timestamps = true`. When true,
    /// the runtime auto-populates two user-visible columns
    /// `created_at` + `updated_at` (BIGINT unix-ms) sourced from the
    /// `UnifiedEntity::created_at/updated_at` fields. `created_at` is
    /// immutable after insert; `updated_at` is bumped on every mutation.
    pub timestamps_enabled: bool,
    /// Enabled by `CREATE TABLE ... WITH context_index = true` (or by
    /// naming specific `context_index_fields`). When true, every INSERT
    /// tokenises the row's text fields and populates the global context
    /// index that backs `SEARCH CONTEXT` / `SEARCH SIMILAR TEXT` / `ASK`
    /// (RAG). When false (default), inserts skip the tokenisation +
    /// 3-way RwLock write storm entirely — ~800 ns faster per insert,
    /// and SEARCH returns empty for this collection.
    ///
    /// Opt-in by design: pure OLTP tables (accounts, orders, events)
    /// pay zero indexing tax; search-oriented tables (articles, docs)
    /// flip the switch at CREATE time.
    pub context_index_enabled: bool,
    /// Metrics collections are backed by time-series storage but carry a
    /// metrics-specific raw sample retention contract.
    pub metrics_raw_retention_ms: Option<u64>,
    /// Metrics rollup tiers declared by `CREATE METRICS ... DOWNSAMPLE`.
    pub metrics_rollup_policies: Vec<String>,
    /// Metrics tenant identity source. Defaults to current tenant context and
    /// can be declared as a stable identity path for future ingestion slices.
    pub metrics_tenant_identity: Option<String>,
    /// Metrics namespace identity. v0 starts with a default namespace so
    /// series identity is namespace-aware before Prometheus ingestion exists.
    pub metrics_namespace: Option<String>,
    /// Enabled by `CREATE TABLE ... APPEND ONLY` or `WITH
    /// (append_only = true)`. When true, the runtime rejects
    /// `UPDATE` and `DELETE` against this collection at parse time
    /// with a clear error — the operator's immutability intent
    /// becomes a first-class catalog fact rather than an RLS-shaped
    /// approximation. Default `false` so legacy DDL keeps its
    /// mutable semantics.
    pub append_only: bool,
    /// Declarative subscriptions created by `WITH EVENTS`. This is
    /// metadata only in #291; event emission is wired by the outbox slice.
    pub subscriptions: Vec<crate::catalog::SubscriptionDescriptor>,
    /// `CREATE TIMESERIES ... WITH SESSION_KEY <col>` — the column the
    /// `SESSIONIZE` operator partitions by when no key is supplied at
    /// query-time. `None` for non-timeseries collections and for
    /// timeseries created without the clause. Issue #576 slice 1.
    pub session_key: Option<String>,
    /// `CREATE TIMESERIES ... SESSION_GAP <duration>` — the default
    /// inactivity gap (milliseconds) the `SESSIONIZE` operator uses to
    /// close a session when no gap is supplied at query-time. `None`
    /// for non-timeseries collections and for timeseries created
    /// without the clause. Issue #576 slice 1.
    pub session_gap_ms: Option<u64>,
    /// `ALTER COLLECTION ... SET RETENTION <duration>` — declarative
    /// retention policy in milliseconds. `None` means retention is
    /// not enforced. Reads filter out rows older than `now -
    /// retention_duration_ms` by the collection's timestamp column.
    /// Issue #580 — DeclarativeRetention slice 1.
    pub retention_duration_ms: Option<u64>,
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
pub struct PhysicalTreeDefinition {
    pub collection: String,
    pub name: String,
    pub root_id: u64,
    pub default_max_children: usize,
    pub ordered_children: bool,
    pub ownership: String,
    pub auto_fix_mode: String,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
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
    pub exports: Vec<ExportDescriptor>,
    pub superblock: SuperblockHeader,
    pub snapshots: Vec<SnapshotDescriptor>,
}

mod helpers;
mod json_codec;
mod metadata_file;
pub mod shm;

pub use self::shm::{
    provision_shm, read_shm_header, set_shm_provisioning_enabled, shm_path_for,
    shm_provisioning_enabled, ShmHandle, ShmHeader, ShmProvisionState, SHM_FILE_SIZE,
    SHM_HEADER_SIZE, SHM_MAGIC, SHM_VERSION,
};

use self::helpers::*;
use self::json_codec::*;
