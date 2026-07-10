//! Byte-oriented Blob Cache.
//!
//! This is the first internal tracer for RedDB's exact-key blob cache. It is
//! intentionally L1-only: a sharded, byte-bounded, in-process cache with SIEVE
//! eviction, namespace caps, and opaque content metadata. Durable L2 storage,
//! dependency invalidation, and public APIs land in follow-up slices.

pub mod cache;
pub mod config;
pub mod entry;
pub mod l2;
pub mod shard;

pub use cache::{
    BlobCache, BlobCacheHit, BlobCachePolicy, BlobCachePut, BlobCacheStats, CacheError,
    CachePresence, L1Admission,
};
pub use config::{
    BlobCacheConfig, BlobCacheConfigBuilder, BlobCacheConfigError, L2Compression,
    L2PromotionPolicy, DEFAULT_BLOB_L1_BYTES_MAX, DEFAULT_BLOB_L2_BYTES_MAX,
    DEFAULT_BLOB_MAX_NAMESPACES, DEFAULT_BLOB_SHARDS, DEFAULT_BLOB_SYNOPSIS_CAPACITY,
    DEFAULT_CONTENT_METADATA_BYTES_MAX, DEFAULT_CONTENT_METADATA_KEYS_MAX,
    METRIC_CACHE_BLOB_L1_BYTES_IN_USE, METRIC_CACHE_BLOB_L2_BYTES_IN_USE,
    METRIC_CACHE_BLOB_L2_FULL_REJECTIONS_TOTAL, METRIC_CACHE_BLOB_SYNOPSIS_BYTES,
    METRIC_CACHE_BLOB_SYNOPSIS_METADATA_READS_TOTAL, METRIC_CACHE_VERSION_MISMATCH_TOTAL,
};
