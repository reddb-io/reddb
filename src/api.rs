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
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_SNAPSHOT_RETENTION: usize = 16;
pub const DEFAULT_EXPORT_RETENTION: usize = 16;

pub const REDDB_PROTOCOL_VERSION: &str = "reddb-v2";
pub const REDDB_FORMAT_VERSION: u32 = 2;

pub type RedDBResult<T> = Result<T, RedDBError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StorageMode {
    /// Durable, file-backed database with WAL + checkpointing.
    Persistent,
    /// Ephemeral, process-memory execution (no WAL or checkpoints).
    InMemory,
}

impl StorageMode {
    pub const fn is_persistent(self) -> bool {
        matches!(self, Self::Persistent)
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

#[derive(Debug, Clone)]
pub struct RedDBOptions {
    pub mode: StorageMode,
    pub data_path: Option<PathBuf>,
    pub read_only: bool,
    pub create_if_missing: bool,
    pub verify_checksums: bool,
    pub auto_checkpoint_pages: u32,
    pub cache_pages: usize,
    pub snapshot_retention: usize,
    pub export_retention: usize,
    pub feature_gates: CapabilitySet,
    pub force_create: bool,
    pub metadata: BTreeMap<String, String>,
}

impl Default for RedDBOptions {
    fn default() -> Self {
        Self {
            mode: StorageMode::Persistent,
            data_path: None,
            read_only: false,
            create_if_missing: true,
            verify_checksums: true,
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

    pub fn in_memory() -> Self {
        Self {
            mode: StorageMode::InMemory,
            data_path: None,
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

    pub fn resolved_path(&self, fallback: impl AsRef<Path>) -> PathBuf {
        self.data_path
            .clone()
            .unwrap_or_else(|| fallback.as_ref().to_path_buf())
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
                write!(f, "schema version mismatch: expected {expected}, found {found}")
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
        CatalogService, CatalogSnapshot, Capability, CapabilitySet, CollectionStats, DataOps,
        QueryPlanner, RedDBError, RedDBOptions, RedDBResult, REDDB_FORMAT_VERSION,
        REDDB_PROTOCOL_VERSION, SchemaManifest, StorageMode,
    };
}
