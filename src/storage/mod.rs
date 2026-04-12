// reddb persistent storage core
//
// This module exposes the unified RedDB storage engine for tables, documents,
// graphs, and vectors with a single API surface.

// Low-level primitives (bloom filters, encoding, mmap, serialization)
pub mod primitives;

pub mod client;

pub mod layout;
pub mod segments;
pub mod service;

pub mod records;
pub mod session;

// RedDB Storage Engine (page-based, B-tree indexed)
pub mod engine;

// B+ Tree with MVCC (Concurrent Storage)
pub mod btree;

// Transaction Management (ACID)
pub mod transaction;

// Page Cache (SIEVE Algorithm)
pub mod cache;

// SQLite Import/Compatibility Layer
pub mod import;

// Write-ahead log - serializer is now integrated into storage primitives

// Write-Ahead Log (Durability)
pub mod wal;

// Encryption Layer (Security)
pub mod encryption;

// Remote Storage Backend Abstraction (S3, R2, GCS, Turso, D1)
pub mod backend;

// Keyring integration for secure password storage
pub mod keyring;

// Schema System (Types, Tables, Registry)
pub mod schema;

// Time-Series Storage
pub mod timeseries;

// Queue / Deque Storage
pub mod queue;

// Query Engine (Filters, Sorting, Similarity Search)
pub mod query;

// Unified Storage Layer (Tables + Graphs + Vectors)
pub(crate) mod unified;

// Public surface re-used by the rest of the codebase.
pub use backend::{BackendError, LocalBackend, RemoteBackend};
pub use client::{
    ActionConfig, ActionRecorder, PasswordSource, PersistenceConfig, PersistenceManager,
    QueryManager,
};
pub use keyring::{clear_keyring, has_keyring_password, resolve_password, save_to_keyring};
pub use service::{PartitionKey, PartitionMetadata, StorageService};
pub use session::{SessionFile, SessionMetadata};
pub use unified::RedDB;

// Unified intelligence layer exports
pub use segments::actions::{
    ActionOutcome, ActionRecord, ActionSource, ActionTrace, ActionType, IntoActionRecord,
    RecordPayload, Target,
};
pub use segments::convert::{
    DnsResults, FingerprintResults, HttpResults, PingResults, PortScanResults, TlsAuditResults,
    VulnResults, WhoisResults,
};

// =============================================================================
// UNIFIED STORAGE INTERFACE (PRIMARY API)
// =============================================================================
//
// The unified storage layer is THE primary interface for all storage operations.
// Use `storage::Store` and `storage::Query` for all new code.
//
// Use `storage::Store` and `storage::Query` for all new code.

pub use unified::{
    AdjacencyEntry,
    CrossRef,
    DslFilter,
    DslQueryResult as QueryResult,
    EdgeData,
    EdgeDirection,
    EmbeddingSlot,
    EntityData,
    // Entity types - Universal data model
    EntityId,
    EntityKind,
    FilterOp,
    FilterValue,
    // Graph adjacency index
    GraphAdjacencyIndex,
    GraphQueryBuilder,
    HybridQueryBuilder,
    IndexEvent,
    IndexEventKind,

    IndexStats,
    IndexStatus,
    // Index lifecycle management
    IndexType,
    IntegratedIndexConfig as IndexConfig,
    IntegratedIndexConfig,

    // Index Manager - Unified indexing (HNSW + Inverted + B-tree + Graph)
    IntegratedIndexManager as IndexManager,
    IntegratedIndexManager,
    InvertedIndex,
    LifecycleEvent,

    ManagerConfig,
    ManagerStats,
    MatchComponents,

    // Metadata
    Metadata,
    MetadataQueryFilter,
    MetadataStorage,
    MetadataType,

    MetadataValue,
    // =========================================================================
    // PRIMARY INTERFACE - Use these for all new code
    // =========================================================================
    NativeHeaderRepairPolicy,
    NodeData,
    QueryResultItem,
    RefQueryBuilder,
    RefType,

    RowData,
    ScanQueryBuilder,
    ScoredMatch,
    SegmentConfig as UnifiedSegmentConfig,
    SegmentError,

    SegmentId as UnifiedSegmentId,
    // Manager
    SegmentManager,
    SegmentState,
    SegmentStats,
    SimilarResult,
    SortOrder,
    SparseVector,
    StoreError,

    StoreStats,
    TableQueryBuilder,
    TextSearchBuilder,
    TextSearchResult,
    TraversalDirection,
    UnifiedEntity,
    UnifiedEntity as Entity,
    UnifiedMetadataFilter,
    // Segments
    UnifiedSegment,
    // Store - THE primary storage interface
    UnifiedStore,
    UnifiedStore as Store,
    VectorData,
    // Query builders (for advanced use)
    VectorQueryBuilder,
    VectorSearchResult,
    WhereClause,
    // Query DSL - Entry point for all queries
    Q as Query,
};
