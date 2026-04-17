//! Public API layer for the RedDB crate.
//!
//! This module is the first layer to consume from applications:
//! - stable options and contracts
//! - capability declarations
//! - typed errors and lightweight metadata snapshots
//! - cross-layer traits for catalog/operations observability

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::auth::AuthConfig;
use crate::replication::ReplicationConfig;

pub const DEFAULT_SNAPSHOT_RETENTION: usize = 16;
pub const DEFAULT_EXPORT_RETENTION: usize = 16;

pub const REDDB_PROTOCOL_VERSION: &str = "reddb-v2";
pub const REDDB_FORMAT_VERSION: u32 = 2;
pub const DEFAULT_GROUP_COMMIT_WINDOW_MS: u64 = 1;
pub const DEFAULT_GROUP_COMMIT_MAX_STATEMENTS: usize = 128;
pub const DEFAULT_GROUP_COMMIT_MAX_WAL_BYTES: u64 = 1024 * 1024;

pub type RedDBResult<T> = Result<T, RedDBError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum StorageMode {
    /// Durable, file-backed database with WAL + checkpointing.
    #[default]
    Persistent,
}

impl StorageMode {
    pub const fn is_persistent(self) -> bool {
        matches!(self, Self::Persistent)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum DurabilityMode {
    #[default]
    Strict,
    WalDurableGrouped,
}

impl DurabilityMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::WalDurableGrouped => "wal_durable_grouped",
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        match normalized.as_str() {
            // Legacy / opt-out form. Every commit pays its own fsync.
            "strict" => Some(Self::Strict),
            // Group-commit sync path — the perf-parity default. Matches
            // PostgreSQL's `synchronous_commit=on` behaviour: the
            // writer waits for durability, but fsyncs are batched
            // across concurrent writers so a burst of N commits pays
            // ~O(1) fsyncs instead of O(N).
            "sync"
            | "wal_durable_grouped"
            | "wal-durable-grouped"
            | "grouped"
            | "wal_grouped"
            | "wal-grouped" => Some(Self::WalDurableGrouped),
            // "async" aliases to the same grouped sync path today.
            // A true fire-and-forget async tier ships as a separate
            // variant in a later pass; accept the name now so the
            // config matrix / env overlay don't reject it outright.
            "async" => Some(Self::WalDurableGrouped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupCommitOptions {
    pub window_ms: u64,
    pub max_statements: usize,
    pub max_wal_bytes: u64,
}

impl Default for GroupCommitOptions {
    fn default() -> Self {
        Self {
            window_ms: DEFAULT_GROUP_COMMIT_WINDOW_MS,
            max_statements: DEFAULT_GROUP_COMMIT_MAX_STATEMENTS,
            max_wal_bytes: DEFAULT_GROUP_COMMIT_MAX_WAL_BYTES,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Structured row storage.
    Table,
    /// Graph nodes/edges.
    Graph,
    /// Vector collections and ANN search.
    Vector,
    /// Full-text / lexical search.
    FullText,
    /// Text/metadata security and enrichment modules.
    Security,
    /// Encryption at rest.
    Encryption,
}

impl Capability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Graph => "graph",
            Self::Vector => "vector",
            Self::FullText => "fulltext",
            Self::Security => "security",
            Self::Encryption => "encryption",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CapabilitySet {
    items: BTreeSet<Capability>,
}

impl CapabilitySet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, capability: Capability) -> Self {
        self.items.insert(capability);
        self
    }

    pub fn with_all(mut self, capabilities: &[Capability]) -> Self {
        capabilities.iter().copied().for_each(|capability| {
            self.items.insert(capability);
        });
        self
    }

    pub fn has(&self, capability: Capability) -> bool {
        self.items.contains(&capability)
    }

    pub fn as_slice(&self) -> Vec<Capability> {
        self.items.iter().copied().collect()
    }
}

pub struct RedDBOptions {
    pub mode: StorageMode,
    pub data_path: Option<PathBuf>,
    pub read_only: bool,
    pub create_if_missing: bool,
    pub verify_checksums: bool,
    pub durability_mode: DurabilityMode,
    pub group_commit: GroupCommitOptions,
    pub auto_checkpoint_pages: u32,
    pub cache_pages: usize,
    pub snapshot_retention: usize,
    pub export_retention: usize,
    pub feature_gates: CapabilitySet,
    pub force_create: bool,
    pub metadata: BTreeMap<String, String>,
    /// Optional remote storage backend for snapshot transport.
    pub remote_backend: Option<Arc<dyn crate::storage::backend::RemoteBackend>>,
    /// Remote object key used by the remote backend.
    pub remote_key: Option<String>,
    /// Replication configuration.
    pub replication: ReplicationConfig,
    /// Authentication & authorization configuration.
    pub auth: AuthConfig,
}

impl fmt::Debug for RedDBOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let backend_name = self.remote_backend.as_ref().map(|b| b.name().to_string());
        f.debug_struct("RedDBOptions")
            .field("mode", &self.mode)
            .field("data_path", &self.data_path)
            .field("read_only", &self.read_only)
            .field("create_if_missing", &self.create_if_missing)
            .field("verify_checksums", &self.verify_checksums)
            .field("durability_mode", &self.durability_mode)
            .field("group_commit", &self.group_commit)
            .field("auto_checkpoint_pages", &self.auto_checkpoint_pages)
            .field("cache_pages", &self.cache_pages)
            .field("snapshot_retention", &self.snapshot_retention)
            .field("export_retention", &self.export_retention)
            .field("feature_gates", &self.feature_gates)
            .field("force_create", &self.force_create)
            .field("metadata", &self.metadata)
            .field("remote_backend", &backend_name)
            .field("remote_key", &self.remote_key)
            .field("replication", &self.replication)
            .field("auth", &self.auth)
            .finish()
    }
}

impl Clone for RedDBOptions {
    fn clone(&self) -> Self {
        Self {
            mode: self.mode,
            data_path: self.data_path.clone(),
            read_only: self.read_only,
            create_if_missing: self.create_if_missing,
            verify_checksums: self.verify_checksums,
            durability_mode: self.durability_mode,
            group_commit: self.group_commit,
            auto_checkpoint_pages: self.auto_checkpoint_pages,
            cache_pages: self.cache_pages,
            snapshot_retention: self.snapshot_retention,
            export_retention: self.export_retention,
            feature_gates: self.feature_gates.clone(),
            force_create: self.force_create,
            metadata: self.metadata.clone(),
            remote_backend: self.remote_backend.clone(),
            remote_key: self.remote_key.clone(),
            replication: self.replication.clone(),
            auth: self.auth.clone(),
        }
    }
}

impl Default for RedDBOptions {
    fn default() -> Self {
        Self {
            mode: StorageMode::Persistent,
            data_path: None,
            read_only: false,
            create_if_missing: true,
            verify_checksums: true,
            // Perf-parity default — `WalDurableGrouped` matches
            // PostgreSQL's `synchronous_commit=on` behaviour while
            // amortising fsync cost across concurrent writers. The
            // legacy `Strict` tier (per-commit fsync) stays available
            // via `durability.mode = "strict"` / `REDDB_DURABILITY=strict`.
            durability_mode: DurabilityMode::WalDurableGrouped,
            group_commit: GroupCommitOptions::default(),
            auto_checkpoint_pages: 1000,
            cache_pages: 10_000,
            snapshot_retention: DEFAULT_SNAPSHOT_RETENTION,
            export_retention: DEFAULT_EXPORT_RETENTION,
            feature_gates: CapabilitySet::new()
                .with(Capability::Table)
                .with(Capability::Graph)
                .with(Capability::Vector),
            force_create: true,
            metadata: BTreeMap::new(),
            remote_backend: None,
            remote_key: None,
            replication: ReplicationConfig::standalone(),
            auth: AuthConfig::default(),
        }
    }
}

impl RedDBOptions {
    pub fn persistent<P: Into<PathBuf>>(path: P) -> Self {
        Self {
            mode: StorageMode::Persistent,
            data_path: Some(path.into()),
            ..Default::default()
        }
    }

    /// Ephemeral, tempfile-backed database.
    ///
    /// The underlying storage is a real persistent file placed under the system
    /// temp directory with a unique name — there is no longer a true in-memory
    /// execution mode. Prefer [`RedDBOptions::persistent`] when the data should
    /// outlive the process.
    pub fn in_memory() -> Self {
        let now_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "reddb-ephemeral-{}-{}.rdb",
            std::process::id(),
            now_nanos
        ));
        let _ = std::fs::remove_file(&path);
        Self {
            mode: StorageMode::Persistent,
            data_path: Some(path),
            auto_checkpoint_pages: 0,
            cache_pages: 2_000,
            snapshot_retention: DEFAULT_SNAPSHOT_RETENTION,
            export_retention: DEFAULT_EXPORT_RETENTION,
            read_only: false,
            force_create: true,
            ..Default::default()
        }
    }

    pub fn with_mode(mut self, mode: StorageMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_data_path<P: Into<PathBuf>>(mut self, path: P) -> Self {
        self.data_path = Some(path.into());
        self
    }

    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn with_auto_checkpoint(mut self, pages: u32) -> Self {
        self.auto_checkpoint_pages = pages;
        self
    }

    pub fn with_durability_mode(mut self, mode: DurabilityMode) -> Self {
        self.durability_mode = mode;
        self
    }

    pub fn with_group_commit_window_ms(mut self, window_ms: u64) -> Self {
        self.group_commit.window_ms = window_ms.max(1);
        self
    }

    pub fn with_group_commit_max_statements(mut self, max_statements: usize) -> Self {
        self.group_commit.max_statements = max_statements.max(1);
        self
    }

    pub fn with_group_commit_max_wal_bytes(mut self, max_wal_bytes: u64) -> Self {
        self.group_commit.max_wal_bytes = max_wal_bytes.max(1);
        self
    }

    pub fn with_cache_pages(mut self, pages: usize) -> Self {
        self.cache_pages = pages.max(2);
        self
    }

    pub fn with_snapshot_retention(mut self, limit: usize) -> Self {
        self.snapshot_retention = limit.max(1);
        self
    }

    pub fn with_export_retention(mut self, limit: usize) -> Self {
        self.export_retention = limit.max(1);
        self
    }

    pub fn with_metadata<K: Into<String>, V: Into<String>>(mut self, key: K, value: V) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    pub fn with_capability(mut self, capability: Capability) -> Self {
        self.feature_gates = self.feature_gates.with(capability);
        self
    }

    /// Attach a remote storage backend for snapshot transport.
    ///
    /// On open, the database snapshot is downloaded from the remote `key`
    /// to the local data path. On flush, the local file is uploaded back
    /// to the remote backend under the same key.
    pub fn with_remote_backend(
        mut self,
        backend: Arc<dyn crate::storage::backend::RemoteBackend>,
        key: impl Into<String>,
    ) -> Self {
        self.remote_backend = Some(backend);
        self.remote_key = Some(key.into());
        self
    }

    pub fn with_replication(mut self, config: ReplicationConfig) -> Self {
        self.replication = config;
        self
    }

    pub fn with_auth(mut self, config: AuthConfig) -> Self {
        self.auth = config;
        self
    }

    pub fn resolved_path(&self, fallback: impl AsRef<Path>) -> PathBuf {
        self.data_path
            .clone()
            .unwrap_or_else(|| fallback.as_ref().to_path_buf())
    }

    pub fn remote_namespace_prefix(&self) -> String {
        let Some(remote_key) = &self.remote_key else {
            return String::new();
        };
        let normalized = remote_key.trim_matches('/');
        if normalized.is_empty() {
            return String::new();
        }
        match normalized.rsplit_once('/') {
            Some((parent, _)) if !parent.is_empty() => format!("{parent}/"),
            _ => String::new(),
        }
    }

    pub fn default_backup_head_key(&self) -> String {
        if let Some(value) = self.metadata.get("red.config.backup.head_key") {
            return value.clone();
        }
        format!("{}manifests/head.json", self.remote_namespace_prefix())
    }

    pub fn default_snapshot_prefix(&self) -> String {
        if let Some(value) = self.metadata.get("red.config.backup.snapshot_prefix") {
            return value.clone();
        }
        format!("{}snapshots/", self.remote_namespace_prefix())
    }

    pub fn default_wal_archive_prefix(&self) -> String {
        if let Some(value) = self.metadata.get("red.config.wal.archive.prefix") {
            return value.clone();
        }
        format!("{}wal/", self.remote_namespace_prefix())
    }

    pub fn has_capability(&self, capability: Capability) -> bool {
        self.feature_gates.has(capability)
    }
}

#[derive(Debug, Clone, Default)]
pub struct CollectionStats {
    pub entities: usize,
    pub cross_refs: usize,
    pub segments: usize,
}

#[derive(Debug, Clone)]
pub struct CatalogSnapshot {
    pub name: String,
    pub total_entities: usize,
    pub total_collections: usize,
    pub stats_by_collection: BTreeMap<String, CollectionStats>,
    pub updated_at: SystemTime,
}

impl Default for CatalogSnapshot {
    fn default() -> Self {
        Self {
            name: String::new(),
            total_entities: 0,
            total_collections: 0,
            stats_by_collection: BTreeMap::new(),
            updated_at: UNIX_EPOCH,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SchemaManifest {
    pub format_version: u32,
    pub created_at_unix_ms: u128,
    pub updated_at_unix_ms: u128,
    pub options: RedDBOptions,
    pub collection_count: usize,
}

impl SchemaManifest {
    pub fn now(options: RedDBOptions, collection_count: usize) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Self {
            format_version: REDDB_FORMAT_VERSION,
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            options,
            collection_count,
        }
    }
}

#[derive(Debug)]
pub enum RedDBError {
    InvalidConfig(String),
    SchemaVersionMismatch { expected: u32, found: u32 },
    FeatureNotEnabled(String),
    NotFound(String),
    ReadOnly(String),
    Engine(String),
    Catalog(String),
    Query(String),
    Io(io::Error),
    VersionUnavailable,
    Internal(String),
}

impl fmt::Display for RedDBError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(msg) => write!(f, "invalid config: {msg}"),
            Self::SchemaVersionMismatch { expected, found } => {
                write!(
                    f,
                    "schema version mismatch: expected {expected}, found {found}"
                )
            }
            Self::FeatureNotEnabled(msg) => write!(f, "feature disabled: {msg}"),
            Self::NotFound(msg) => write!(f, "not found: {msg}"),
            Self::ReadOnly(msg) => write!(f, "read-only violation: {msg}"),
            Self::Engine(msg) => write!(f, "engine error: {msg}"),
            Self::Catalog(msg) => write!(f, "catalog error: {msg}"),
            Self::Query(msg) => write!(f, "query error: {msg}"),
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::VersionUnavailable => write!(f, "version information unavailable"),
            Self::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl std::error::Error for RedDBError {}

impl From<io::Error> for RedDBError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<crate::storage::engine::DatabaseError> for RedDBError {
    fn from(err: crate::storage::engine::DatabaseError) -> Self {
        Self::Engine(err.to_string())
    }
}

impl From<crate::storage::wal::TxError> for RedDBError {
    fn from(err: crate::storage::wal::TxError) -> Self {
        Self::Engine(err.to_string())
    }
}

impl From<crate::storage::StoreError> for RedDBError {
    fn from(err: crate::storage::StoreError) -> Self {
        Self::Catalog(err.to_string())
    }
}

impl From<crate::storage::unified::devx::DevXError> for RedDBError {
    fn from(err: crate::storage::unified::devx::DevXError) -> Self {
        match err {
            crate::storage::unified::devx::DevXError::Validation(msg) => Self::InvalidConfig(msg),
            crate::storage::unified::devx::DevXError::Storage(msg) => Self::Engine(msg),
            crate::storage::unified::devx::DevXError::NotFound(msg) => Self::NotFound(msg),
        }
    }
}

pub trait CatalogService {
    fn list_collections(&self) -> Vec<String>;
    fn collection_stats(&self, collection: &str) -> Option<CollectionStats>;
    fn catalog_snapshot(&self) -> CatalogSnapshot;
}

pub trait QueryPlanner {
    fn plan_cost(&self, query: &str) -> Option<f64>;
}

pub trait DataOps {
    fn execute_query(&self, query: &str) -> RedDBResult<()>;
}

pub mod prelude {
    pub use super::{
        Capability, CapabilitySet, CatalogService, CatalogSnapshot, CollectionStats, DataOps,
        QueryPlanner, RedDBError, RedDBOptions, RedDBResult, SchemaManifest, StorageMode,
        REDDB_FORMAT_VERSION, REDDB_PROTOCOL_VERSION,
    };
}
