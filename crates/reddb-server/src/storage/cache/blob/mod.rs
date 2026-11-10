//! Byte-oriented Blob Cache.
//!
//! L1 is a sharded, byte-bounded in-process cache with SIEVE eviction. L2 can
//! use either the legacy standalone pager or RedDB's internal single-file
//! collection backend.

pub mod cache;
pub mod config;
pub mod entry;
pub mod l2;
mod l2_backend;
mod l2_store;
pub mod shard;

pub use cache::{
    BlobCache, BlobCacheHit, BlobCachePolicy, BlobCachePut, BlobCacheStats, CacheError,
    CachePresence, L1Admission,
};
pub use config::{
    BlobCacheConfig, BlobCacheConfigBuilder, BlobCacheConfigError, L2Compression,
    L2PromotionPolicy, DEFAULT_BLOB_L1_BYTES_MAX, DEFAULT_BLOB_L2_BYTES_MAX,
    DEFAULT_BLOB_MAX_NAMESPACES, DEFAULT_BLOB_NAMESPACE_BYTES_MAX, DEFAULT_BLOB_SHARDS,
    DEFAULT_BLOB_SYNOPSIS_CAPACITY, DEFAULT_CONTENT_METADATA_BYTES_MAX,
    DEFAULT_CONTENT_METADATA_KEYS_MAX, METRIC_CACHE_BLOB_L1_BYTES_IN_USE,
    METRIC_CACHE_BLOB_L2_BYTES_IN_USE, METRIC_CACHE_BLOB_L2_FULL_REJECTIONS_TOTAL,
    METRIC_CACHE_BLOB_SYNOPSIS_BYTES, METRIC_CACHE_BLOB_SYNOPSIS_METADATA_READS_TOTAL,
    METRIC_CACHE_VERSION_MISMATCH_TOTAL,
};
