//! Unified Storage Layer
//!
//! This module provides a unified abstraction over Tables, Graphs, and Vectors,
//! enabling queries that seamlessly combine all storage types.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────────┐
//! │                        UnifiedStore (Core API)                           │
//! ├──────────────────────────────────────────────────────────────────────────┤
//! │                        SegmentManager (Lifecycle)                        │
//! │   ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────┐  │
//! │   │ GrowingSegment  │  │ SealedSegment   │  │ Background Tasks        │  │
//! │   │ (In-memory)     │→→│ (Indexed)       │→→│ (Seal, Compact, Archive)│  │
//! │   └─────────────────┘  └─────────────────┘  └─────────────────────────┘  │
//! ├──────────────────────────────────────────────────────────────────────────┤
//! │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌─────────────┐   │
//! │  │ UnifiedEntity│  │ EntityData   │  │ Metadata     │  │ CrossRefs   │   │
//! │  │ (Core type)  │  │ (Variants)   │  │ (Type-aware) │  │ (Links)     │   │
//! │  └──────────────┘  └──────────────┘  └──────────────┘  └─────────────┘   │
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Entity Kinds
//!
//! - **TableRow**: A row in a structured table with schema
//! - **GraphNode**: A vertex in the graph with label and properties
//! - **GraphEdge**: A directed edge between nodes with weight
//! - **Vector**: A dense/sparse vector in a collection
//!
//! # Cross-References
//!
//! Entities can reference each other across storage types:
//! - TableRow → GraphNode (row represents a node)
//! - GraphNode → Vector (node has embeddings)
//! - Vector → TableRow (embedding source)

pub mod bitmap_index;
pub mod bloom_index;
pub mod context_index;
pub mod devx;
pub mod dsl;
pub mod entity;
pub mod hash_index;
pub mod index;
pub mod manager;
pub mod memtable;
pub mod metadata;
pub mod segment;
pub mod segment_codec;
pub mod skiplist;
pub mod spatial_index;
pub mod store;
pub mod tokenization;

pub use bitmap_index::{BitmapColumnIndex, BitmapIndexManager, BitmapIndexStats};
pub use bloom_index::{BloomFilterRegistry, BloomRegistryStats};
pub use context_index::{ContextIndex, ContextIndexStats, ContextPosting, ContextSearchHit};
pub use devx::{
    BatchBuilder, BatchResult, DevXError, EdgeBuilder, IndexConfig, LinkedEntity,
    MetadataFilter as DevXMetadataFilter, NativeHeaderRepairPolicy,
    NativeVectorArtifactBatchInspection, NativeVectorArtifactInspection, NodeBuilder, NodeRef,
    PhysicalAuthorityStatus, Preprocessor, PropertyFilter, QueryBuilder, QueryResult,
    QueryResultItem, RedDB, RowBuilder, SimilarResult, TableRef, VectorBuilder, VectorRef,
};
pub use entity::{
    CrossRef, EdgeData, EmbeddingSlot, EntityData, EntityId, EntityKind, GraphEdgeKind,
    GraphNodeKind, NodeData, RefType, RowData, SparseVector, TimeSeriesData, TimeSeriesPointKind,
    UnifiedEntity, VectorData,
};
pub use hash_index::{
    HashIndex, HashIndexConfig, HashIndexError, HashIndexManager, HashIndexStats,
};
pub use index::{
    AdjacencyEntry,
    EdgeDirection,
    // Graph adjacency index
    GraphAdjacencyIndex,
    IndexEvent,
    IndexEventKind,
    IndexStats,
    IndexStatus,
    // Index lifecycle management
    IndexType,
    IntegratedIndexConfig,
    IntegratedIndexManager,
    InvertedIndex,
    MetadataQueryFilter,
    TextSearchResult,
    VectorSearchResult,
};
pub use manager::{LifecycleEvent, ManagerConfig, ManagerStats, SegmentManager};
pub use metadata::{
    Metadata, MetadataFilter as UnifiedMetadataFilter, MetadataStorage, MetadataType,
    MetadataValue, RefTarget, TypedColumn,
};
pub use segment::{
    ColZone, SegmentConfig, SegmentError, SegmentId, SegmentState, SegmentStats, UnifiedSegment,
    ZoneColPred, ZoneColPredKind,
};
pub use spatial_index::{
    SpatialIndex, SpatialIndexManager, SpatialIndexStats, SpatialSearchResult,
};
pub use store::{StoreError, StoreStats, UnifiedStore, UnifiedStoreConfig};
// Query DSL for fluent multi-modal queries
pub use dsl::{
    Filter as DslFilter, FilterOp, FilterValue, GraphQueryBuilder, HybridQueryBuilder,
    MatchComponents, QueryResult as DslQueryResult, RefQueryBuilder, ScanQueryBuilder, ScoredMatch,
    SortOrder, TableQueryBuilder, TextSearchBuilder, TraversalDirection, VectorQueryBuilder,
    WhereClause, Q,
};
