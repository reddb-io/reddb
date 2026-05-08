use std::path::{Path, PathBuf};

pub const DEFAULT_BLOB_L1_BYTES_MAX: usize = 256 * 1024 * 1024;
pub const DEFAULT_BLOB_L2_BYTES_MAX: u64 = 4 * 1024 * 1024 * 1024;
pub const DEFAULT_BLOB_MAX_NAMESPACES: usize = 256;
pub const DEFAULT_BLOB_SHARDS: usize = 64;
pub const DEFAULT_CONTENT_METADATA_KEYS_MAX: usize = 32;
pub const DEFAULT_CONTENT_METADATA_BYTES_MAX: usize = 4 * 1024;
pub const METRIC_CACHE_BLOB_L1_BYTES_IN_USE: &str = "cache_blob_l1_bytes_in_use";
pub const METRIC_CACHE_VERSION_MISMATCH_TOTAL: &str = "cache_version_mismatch_total";
pub const METRIC_CACHE_BLOB_L2_BYTES_IN_USE: &str = "reddb_cache_blob_l2_bytes_in_use";
pub const METRIC_CACHE_BLOB_L2_FULL_REJECTIONS_TOTAL: &str =
    "reddb_cache_blob_l2_full_rejections_total";
pub const METRIC_CACHE_BLOB_SYNOPSIS_METADATA_READS_TOTAL: &str =
    "cache_blob_synopsis_metadata_reads_total";
pub const METRIC_CACHE_BLOB_SYNOPSIS_BYTES: &str = "cache_blob_synopsis_bytes";

/// Default per-namespace Bloom synopsis sizing target. The filter is sized
/// for ~10K entries at ~1% false-positive rate.
pub const DEFAULT_BLOB_SYNOPSIS_CAPACITY: usize = 10_000;
pub const DEFAULT_BLOB_SYNOPSIS_FPR: f64 = 0.01;

/// Switch for L2 zstd compression (issue #192, lane 2/5).
///
/// `On` (default) routes every L2 spill through [`L2BlobCompressor`]; payloads
/// that fail the shrinkage gate or hit a precompressed-media content type are
/// still stored raw, but the L2 entry header carries the v2 framing. `Off`
/// skips the compress call entirely (CPU-saving), still emitting v2 framing
/// with `tag=0` so the on-disk format stays uniform across modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L2Compression {
    Off,
    On,
}

impl Default for L2Compression {
    fn default() -> Self {
        Self::On
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobCacheConfig {
    pub(super) l1_bytes_max: usize,
    pub(super) l2_bytes_max: u64,
    pub(super) l2_path: Option<PathBuf>,
    pub(super) max_namespaces: usize,
    pub(super) shard_count: usize,
    pub(super) content_metadata_keys_max: usize,
    pub(super) content_metadata_bytes_max: usize,
    pub(super) l2_compression: L2Compression,
}

impl Default for BlobCacheConfig {
    fn default() -> Self {
        Self {
            l1_bytes_max: DEFAULT_BLOB_L1_BYTES_MAX,
            l2_bytes_max: DEFAULT_BLOB_L2_BYTES_MAX,
            l2_path: None,
            max_namespaces: DEFAULT_BLOB_MAX_NAMESPACES,
            shard_count: DEFAULT_BLOB_SHARDS,
            content_metadata_keys_max: DEFAULT_CONTENT_METADATA_KEYS_MAX,
            content_metadata_bytes_max: DEFAULT_CONTENT_METADATA_BYTES_MAX,
            l2_compression: L2Compression::default(),
        }
    }
}

impl BlobCacheConfig {
    /// Returns a fresh builder primed with the cache defaults.
    ///
    /// Prefer this over field literals — fields are private so future
    /// additions (PRD stories #8–#10) do not break callers.
    pub fn builder() -> BlobCacheConfigBuilder {
        BlobCacheConfigBuilder::new()
    }

    pub fn with_l1_bytes_max(mut self, l1_bytes_max: usize) -> Self {
        self.l1_bytes_max = l1_bytes_max;
        self
    }

    pub fn with_l2_bytes_max(mut self, l2_bytes_max: u64) -> Self {
        self.l2_bytes_max = l2_bytes_max;
        self
    }

    pub fn with_l2_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.l2_path = Some(path.into());
        self
    }

    pub fn with_max_namespaces(mut self, max_namespaces: usize) -> Self {
        self.max_namespaces = max_namespaces;
        self
    }

    pub fn with_shard_count(mut self, shard_count: usize) -> Self {
        self.shard_count = shard_count.max(1);
        self
    }

    pub fn with_content_metadata_limits(mut self, keys_max: usize, bytes_max: usize) -> Self {
        self.content_metadata_keys_max = keys_max;
        self.content_metadata_bytes_max = bytes_max;
        self
    }

    pub fn with_l2_compression(mut self, compression: L2Compression) -> Self {
        self.l2_compression = compression;
        self
    }

    pub fn l1_bytes_max(&self) -> usize {
        self.l1_bytes_max
    }

    pub fn l2_bytes_max(&self) -> u64 {
        self.l2_bytes_max
    }

    pub fn l2_path(&self) -> Option<&Path> {
        self.l2_path.as_deref()
    }

    pub fn max_namespaces(&self) -> usize {
        self.max_namespaces
    }

    pub fn shard_count(&self) -> usize {
        self.shard_count
    }

    pub fn content_metadata_keys_max(&self) -> usize {
        self.content_metadata_keys_max
    }

    pub fn content_metadata_bytes_max(&self) -> usize {
        self.content_metadata_bytes_max
    }

    pub fn l2_compression(&self) -> L2Compression {
        self.l2_compression
    }
}

/// Builder for [`BlobCacheConfig`].
///
/// Created via [`BlobCacheConfig::builder`]. Each setter validates its
/// argument; invalid configurations are rejected at [`build`](Self::build).
#[derive(Debug, Clone)]
pub struct BlobCacheConfigBuilder {
    inner: BlobCacheConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobCacheConfigError {
    /// `shard_count` must be at least 1.
    ZeroShardCount,
    /// `max_namespaces` must be at least 1.
    ZeroMaxNamespaces,
}

impl BlobCacheConfigBuilder {
    fn new() -> Self {
        Self {
            inner: BlobCacheConfig::default(),
        }
    }

    pub fn l1_bytes_max(mut self, value: usize) -> Self {
        self.inner.l1_bytes_max = value;
        self
    }

    pub fn l2_bytes_max(mut self, value: u64) -> Self {
        self.inner.l2_bytes_max = value;
        self
    }

    pub fn l2_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.inner.l2_path = Some(path.into());
        self
    }

    pub fn max_namespaces(mut self, value: usize) -> Self {
        self.inner.max_namespaces = value;
        self
    }

    pub fn shard_count(mut self, value: usize) -> Self {
        self.inner.shard_count = value;
        self
    }

    pub fn content_metadata_keys_max(mut self, value: usize) -> Self {
        self.inner.content_metadata_keys_max = value;
        self
    }

    pub fn content_metadata_bytes_max(mut self, value: usize) -> Self {
        self.inner.content_metadata_bytes_max = value;
        self
    }

    pub fn l2_compression(mut self, value: L2Compression) -> Self {
        self.inner.l2_compression = value;
        self
    }

    pub fn try_build(self) -> Result<BlobCacheConfig, BlobCacheConfigError> {
        if self.inner.shard_count == 0 {
            return Err(BlobCacheConfigError::ZeroShardCount);
        }
        if self.inner.max_namespaces == 0 {
            return Err(BlobCacheConfigError::ZeroMaxNamespaces);
        }
        Ok(self.inner)
    }

    /// Convenience wrapper around [`try_build`](Self::try_build) that
    /// panics on invalid input. Tests and bootstrap code should prefer this.
    pub fn build(self) -> BlobCacheConfig {
        self.try_build().expect("blob cache config")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_cache_config_builder_rejects_zero_shard_count() {
        let err = BlobCacheConfig::builder()
            .shard_count(0)
            .try_build()
            .expect_err("zero shard count must be rejected");
        assert_eq!(err, BlobCacheConfigError::ZeroShardCount);
    }

    #[test]
    fn blob_cache_config_builder_rejects_zero_max_namespaces() {
        let err = BlobCacheConfig::builder()
            .max_namespaces(0)
            .try_build()
            .expect_err("zero max_namespaces must be rejected");
        assert_eq!(err, BlobCacheConfigError::ZeroMaxNamespaces);
    }
}
