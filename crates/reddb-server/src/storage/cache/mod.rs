//! Cache Module
//!
//! High-performance caching infrastructure for RedDB.
//!
//! # Components
//!
//! - **sieve**: SIEVE page cache for database pages (O(1) operations)
//! - **result**: Query result cache with dependency-based invalidation
//! - **aggregates**: Precomputed aggregations (COUNT, SUM, AVG, etc.)
//! - **spill**: Graph spill-to-disk for memory-limited environments
//!
//! # Architecture (inspired by Turso/Milvus/Neo4j)
//!
//! ```text
//! ┌────────────────────────────────────────────────────────┐
//! │                    Query Layer                         │
//! ├────────────────────────────────────────────────────────┤
//! │  Result Cache   │  Materialized Views  │  Plan Cache   │
//! ├────────────────────────────────────────────────────────┤
//! │           Aggregation Cache (COUNT/SUM/AVG)            │
//! ├────────────────────────────────────────────────────────┤
//! │   SIEVE Page Cache    │     Spill Manager              │
//! ├────────────────────────────────────────────────────────┤
//! │                   Storage Engine                       │
//! └────────────────────────────────────────────────────────┘
//! ```

pub mod aggregates;
pub mod bgwriter;
pub mod result;
pub mod ring;
pub mod sieve;
pub mod spill;
pub mod strategy;

pub use aggregates::{AggCacheStats, AggValue, AggregationCache, CardinalityEstimate, NumericAgg};
pub use result::{
    CacheKey, CachePolicy, MaterializedViewCache, MaterializedViewDef, RefreshPolicy, ResultCache,
    ResultCacheStats,
};
pub use ring::BufferRing;
pub use sieve::{CacheConfig, CacheStats, PageCache, PageId};
pub use spill::{SpillConfig, SpillError, SpillManager, SpillStats, SpillableGraph};
pub use strategy::BufferAccessStrategy;
