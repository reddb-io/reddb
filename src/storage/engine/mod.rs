//! RedDB Storage Engine
//!
//! A page-based storage engine inspired by SQLite/Turso architecture.
//! Implements 4KB aligned pages for efficient disk I/O with SIEVE caching.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                       Database API                          │
//! ├─────────────────────────────────────────────────────────────┤
//! │                       B-Tree Engine                         │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Page Cache (SIEVE)  │     Pager (I/O)     │   Free List   │
//! ├─────────────────────────────────────────────────────────────┤
//! │                     Page Structure                          │
//! ├─────────────────────────────────────────────────────────────┤
//! │                   File System / WAL                         │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! # References
//!
//! - Turso `core/storage/pager.rs` - Page I/O management
//! - Turso `core/storage/page_cache.rs` - SIEVE eviction algorithm
//! - Turso `core/storage/btree.rs` - B-tree page layout

pub mod algorithms;
pub mod btree;
pub mod bulk_writer;
pub mod clustering;
pub mod crc32;
pub mod database;
pub mod distance;
pub mod emitter;
pub mod freelist;
pub mod graph_store;
pub mod graph_table_index;
pub mod hnsw;
pub mod hot_update;
pub mod hybrid;
pub mod ivf;
pub mod page;
pub mod page_cache;
pub mod pager;
pub mod pathfinding;
pub mod pq;
pub mod projection;
pub mod simd_distance;
pub mod store_strategy;

// Quantization modules for tiered vector search
pub mod binary_quantize;
pub mod int8_quantize;
pub mod tiered_search;
pub mod unified_index;
pub mod vector_metadata;
pub mod vector_store;

#[path = "encrypted-pager.rs"]
pub mod encrypted_pager;

pub use btree::{BTree, BTreeCursor, BTreeError};
pub use crc32::crc32;
pub use database::{Database, DatabaseConfig, DatabaseError};
#[allow(deprecated)]
pub use encrypted_pager::{EncryptedPager, EncryptedPagerConfig, EncryptedPagerError};
pub use freelist::FreeList;
pub use graph_store::{GraphEdgeType, GraphNodeType, GraphStore, StoredEdge, StoredNode, TableRef};
pub use graph_table_index::{GraphTableIndex, GraphTableIndexStats, RowKey};
pub use page::{Page, PageHeader, PageType, HEADER_SIZE, PAGE_SIZE};
pub use page_cache::PageCache;
pub use pager::{Pager, PagerConfig, PhysicalFileHeader};

// Graph algorithms
pub use algorithms::{
    BetweennessCentrality,
    BetweennessResult,
    ClosenessCentrality,
    ClosenessResult,
    ClusteringCoefficient,
    ClusteringResult,
    CommunitiesResult,
    Community,
    Component,
    ComponentsResult,
    ConnectedComponents,
    Cycle,
    CycleDetector,
    CyclesResult,
    // Additional centrality algorithms
    DegreeCentrality,
    DegreeCentralityResult,
    EigenvectorCentrality,
    EigenvectorResult,
    HITSResult,
    LabelPropagation,
    Louvain,
    LouvainResult,
    // Core algorithms
    PageRank,
    PageRankResult,
    PersonalizedPageRank,
    SCCResult,
    // Community detection
    StronglyConnectedComponents,
    TriangleCounting,
    TriangleResult,
    WCCResult,
    WeaklyConnectedComponents,
    HITS,
};

// Path finding algorithms
pub use pathfinding::{
    AStar, AllPathsResult, AllShortestPaths, BellmanFord, BellmanFordResult, Dijkstra,
    KShortestPaths, Path, ShortestPathResult, BFS, DFS,
};

// Graph emitter for module integration
pub use emitter::{EmitterStats, GraphEmitter, ScanResult, ServiceResult};

// Vector storage
pub use distance::{
    cosine_distance, distance, dot_product, inner_product_distance, l2, l2_norm, l2_squared,
    normalize, normalized, Distance, DistanceMetric, DistanceResult,
};
pub use hnsw::{Bitset, HnswConfig, HnswIndex, HnswStats, NodeId};
pub use vector_metadata::{MetadataEntry, MetadataFilter, MetadataStore, MetadataValue};
pub use vector_store::{
    SearchResult, SegmentConfig, SegmentId, SegmentState, VectorCollection, VectorId,
    VectorSegment, VectorStore, VectorStoreError,
};

// Unified cross-storage index
pub use unified_index::{
    CrossRef, RowKey as TableRowKey, StorageRef, UnifiedIndex, UnifiedIndexStats, VectorKey,
};

// Graph projections
pub use projection::{
    AggregationStrategy, EdgeFilter, GraphProjection, NodeFilter, ProjectedNode, ProjectionBuilder,
    ProjectionStats, PropertyProjection,
};

// Hybrid search (dense + sparse)
pub use hybrid::{
    dbsf_fusion, linear_fusion, reciprocal_rank_fusion, BM25Config, ExactMatchReranker,
    FusionMethod, HybridQueryBuilder, HybridResult, HybridSearch, Reranker, RerankerPipeline,
    SparseIndex, SparseResult,
};

// IVF (Inverted File Index) for approximate search
pub use ivf::{IvfConfig, IvfIndex, IvfStats};

// Product Quantization for vector compression
pub use pq::{PQCode, PQConfig, PQIndex, ProductQuantizer};

// Binary quantization for ultra-fast similarity search
pub use binary_quantize::{hamming_distance_simd, BinaryIndex, BinarySearchResult, BinaryVector};

// int8 quantization for efficient rescoring
pub use int8_quantize::{dot_product_i8_f32_simd, dot_product_i8_simd, Int8Index, Int8Vector};

// Tiered search pipeline (binary → int8 → fp32)
pub use tiered_search::{
    MemoryConstraint, MemoryLimitError, TieredIndex, TieredIndexBuilder, TieredMemoryStats,
    TieredSearchConfig, TieredSearchResult,
};
