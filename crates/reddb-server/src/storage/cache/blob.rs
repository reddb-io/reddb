//! Byte-oriented Blob Cache.
//!
//! This is the first internal tracer for RedDB's exact-key blob cache. It is
//! intentionally L1-only: a sharded, byte-bounded, in-process cache with SIEVE
//! eviction, namespace caps, and opaque content metadata. Durable L2 storage,
//! dependency invalidation, and public APIs land in follow-up slices.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;

use super::compressor::{CompressOpts, Compressed, L2BlobCompressor};
use super::extended_ttl::{EffectiveExpiry, ExpiryDecision, ExtendedTtlPolicy};
use super::promotion_pool::{AsyncPromotionPool, PoolOpts, PromotionExecutor, PromotionRequest};

/// Test-only thread-local counter of how many times
/// `EffectiveExpiry::compute` is invoked from `Shard::get`. Thread-local
/// (rather than a global atomic) so the off-fast-path test does not race
/// with other tests in the harness's parallel executor.
#[cfg(test)]
thread_local! {
    static EFFECTIVE_EXPIRY_COMPUTE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

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
    l1_bytes_max: usize,
    l2_bytes_max: u64,
    l2_path: Option<PathBuf>,
    max_namespaces: usize,
    shard_count: usize,
    content_metadata_keys_max: usize,
    content_metadata_bytes_max: usize,
    l2_compression: L2Compression,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheError {
    BlobTooLarge {
        size: usize,
        max: usize,
    },
    MetadataTooLarge {
        keys: usize,
        bytes: usize,
        max_keys: usize,
        max_bytes: usize,
    },
    TooManyNamespaces {
        max: usize,
    },
    VersionMismatch {
        existing: u64,
        attempted: u64,
    },
    L2Full {
        size: u64,
        max: u64,
    },
    L2Io(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BlobCacheKey {
    namespace: String,
    key: String,
}

impl BlobCacheKey {
    fn new(namespace: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            key: key.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ScopedLabel {
    namespace: String,
    label: String,
}

impl ScopedLabel {
    fn new(namespace: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            label: label.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobCacheHit {
    bytes: Arc<[u8]>,
    content_metadata: BTreeMap<String, String>,
    version: Option<u64>,
    /// `Some(remaining_ms)` when the hit came from the stale-while-revalidate
    /// window of an `ExtendedTtlPolicy`; `None` when the entry was fresh.
    /// Boolean staleness is just `.is_some()`.
    stale_window_remaining_ms: Option<u64>,
}

impl BlobCacheHit {
    pub(crate) fn new(
        bytes: Arc<[u8]>,
        content_metadata: BTreeMap<String, String>,
        version: Option<u64>,
    ) -> Self {
        Self {
            bytes,
            content_metadata,
            version,
            stale_window_remaining_ms: None,
        }
    }

    pub(crate) fn new_stale(
        bytes: Arc<[u8]>,
        content_metadata: BTreeMap<String, String>,
        version: Option<u64>,
        window_remaining_ms: u64,
    ) -> Self {
        Self {
            bytes,
            content_metadata,
            version,
            stale_window_remaining_ms: Some(window_remaining_ms),
        }
    }

    /// Cached payload, refcounted so duplicate readers share the buffer.
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.bytes
    }

    /// Convenience accessor returning a `&[u8]` view into [`bytes`](Self::bytes).
    pub fn value(&self) -> &[u8] {
        &self.bytes
    }

    /// Opaque content metadata captured on `put`.
    pub fn content_metadata(&self) -> &BTreeMap<String, String> {
        &self.content_metadata
    }

    /// Optional CAS / freshness version stamped on `put`.
    pub fn version(&self) -> Option<u64> {
        self.version
    }

    /// `true` when the hit was served from the stale-while-revalidate window
    /// of an `ExtendedTtlPolicy`. Always `false` when the extended policy is
    /// `off()` or the entry was within its hard expiry.
    pub fn is_stale(&self) -> bool {
        self.stale_window_remaining_ms.is_some()
    }

    /// Remaining stale-window milliseconds when [`is_stale`](Self::is_stale)
    /// is `true`; `None` when the hit was fresh.
    pub fn stale_window_remaining_ms(&self) -> Option<u64> {
        self.stale_window_remaining_ms
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BlobCachePut {
    pub bytes: Vec<u8>,
    pub content_metadata: BTreeMap<String, String>,
    pub tags: BTreeSet<String>,
    pub dependencies: BTreeSet<String>,
    pub policy: BlobCachePolicy,
}

impl BlobCachePut {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            bytes: bytes.into(),
            content_metadata: BTreeMap::new(),
            tags: BTreeSet::new(),
            dependencies: BTreeSet::new(),
            policy: BlobCachePolicy::default(),
        }
    }

    pub fn with_content_metadata(mut self, content_metadata: BTreeMap<String, String>) -> Self {
        self.content_metadata = content_metadata;
        self
    }

    pub fn with_tags(mut self, tags: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.tags = tags.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_dependencies(
        mut self,
        dependencies: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.dependencies = dependencies.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_policy(mut self, policy: BlobCachePolicy) -> Self {
        self.policy = policy;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum L1Admission {
    Always,
    Auto,
    Never,
}

/// Three-valued answer for [`BlobCache::exists`].
///
/// Today the implementation always returns [`Present`](Self::Present) or
/// [`Absent`](Self::Absent) — it tracks the answer authoritatively. The
/// [`MaybePresent`](Self::MaybePresent) variant exists in the type so the
/// upcoming Bloom synopsis (#146) can answer "probably yes" without forcing
/// a metadata read, all without breaking the `exists` contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePresence {
    /// The cache holds a live entry for this key.
    Present,
    /// The cache definitely does not hold this key (negative cache hit).
    Absent,
    /// A probabilistic synopsis cannot rule the key out without a deeper
    /// lookup. Treat as a hit prospect: the caller should fetch.
    MaybePresent,
}

impl From<bool> for CachePresence {
    fn from(present: bool) -> Self {
        if present {
            CachePresence::Present
        } else {
            CachePresence::Absent
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobCachePolicy {
    ttl_ms: Option<u64>,
    expires_at_unix_ms: Option<u64>,
    max_blob_bytes: Option<usize>,
    l1_admission: L1Admission,
    priority: u8,
    version: Option<u64>,
    /// Extended TTL knobs (idle / stale-while-revalidate / jitter).
    /// Defaults to [`ExtendedTtlPolicy::off`] so existing call sites and
    /// stored entries continue to behave with hard-expiry-only semantics.
    /// Wired into [`BlobCache::get`] behind the
    /// `cache.blob.policy.extended` config knob (#194).
    extended: ExtendedTtlPolicy,
}

impl Default for BlobCachePolicy {
    fn default() -> Self {
        Self {
            ttl_ms: None,
            expires_at_unix_ms: None,
            max_blob_bytes: None,
            l1_admission: L1Admission::Auto,
            priority: 128,
            version: None,
            extended: ExtendedTtlPolicy::off(),
        }
    }
}

impl BlobCachePolicy {
    // ----- builder-style setters (consuming) -----------------------------

    pub fn ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.ttl_ms = Some(ttl_ms);
        self
    }

    pub fn expires_at_unix_ms(mut self, expires_at_unix_ms: u64) -> Self {
        self.expires_at_unix_ms = Some(expires_at_unix_ms);
        self
    }

    pub fn max_blob_bytes(mut self, max_blob_bytes: usize) -> Self {
        self.max_blob_bytes = Some(max_blob_bytes);
        self
    }

    pub fn l1_admission(mut self, l1_admission: L1Admission) -> Self {
        self.l1_admission = l1_admission;
        self
    }

    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    pub fn version(mut self, version: u64) -> Self {
        self.version = Some(version);
        self
    }

    /// Replace the extended TTL knobs in one chainable call. Defaults to
    /// [`ExtendedTtlPolicy::off`]; setting an active policy turns on the
    /// idle / stale-serve / jitter behaviours in [`BlobCache::get`] and
    /// [`BlobCache::put`] for entries written with this policy.
    pub fn extended(mut self, extended: ExtendedTtlPolicy) -> Self {
        self.extended = extended;
        self
    }

    // ----- read-back accessors -------------------------------------------
    //
    // Setter methods consume `self` and return `Self`, so they cannot share
    // a name with `&self` getters. The `*_value` suffix keeps both surfaces
    // available without renaming the public builder API.

    pub fn ttl_ms_value(&self) -> Option<u64> {
        self.ttl_ms
    }

    pub fn expires_at_unix_ms_value(&self) -> Option<u64> {
        self.expires_at_unix_ms
    }

    pub fn max_blob_bytes_value(&self) -> Option<usize> {
        self.max_blob_bytes
    }

    pub fn l1_admission_value(&self) -> L1Admission {
        self.l1_admission
    }

    pub fn priority_value(&self) -> u8 {
        self.priority
    }

    pub fn version_value(&self) -> Option<u64> {
        self.version
    }

    /// Read-back accessor for the extended TTL knobs. Mirrors the
    /// `*_value` getter pattern used by every other [`BlobCachePolicy`]
    /// field (#151 — fields are private; readers go through getters).
    pub fn extended_value(&self) -> ExtendedTtlPolicy {
        self.extended
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlobCacheStats {
    hits: u64,
    misses: u64,
    insertions: u64,
    evictions: u64,
    expirations: u64,
    invalidations: u64,
    namespace_flushes: u64,
    version_mismatches: u64,
    entries: usize,
    bytes_in_use: usize,
    l1_bytes_max: usize,
    l2_bytes_in_use: u64,
    l2_bytes_max: u64,
    l2_full_rejections: u64,
    l2_metadata_reads: u64,
    l2_negative_skips: u64,
    /// Times the per-namespace Bloom synopsis answered `MaybePresent` but the
    /// authoritative L2 metadata B+ tree said `Absent` (the false-positive
    /// cost of the probabilistic synopsis).
    synopsis_metadata_reads: u64,
    /// Total bytes used by all per-namespace Bloom synopsis filters.
    synopsis_bytes: u64,
    namespaces: usize,
    max_namespaces: usize,
    /// Async promotion pool counters (issue #193). All zero when the
    /// pool is not enabled (default — `cache.blob.async_promotion = "off"`).
    promotion_queued: u64,
    promotion_dropped: u64,
    promotion_completed: u64,
    promotion_queue_depth: usize,
    /// Numerator of the L2 compression ratio: sum of `original_len` over
    /// entries that actually compressed (#192). Stored as the ratio's
    /// component so [`BlobCacheStats`] stays `Eq` (avoids `f64` fields).
    l2_compression_original_bytes: u64,
    /// Denominator of the L2 compression ratio: sum of `stored_len` over
    /// entries that actually compressed.
    l2_compression_stored_bytes: u64,
    /// Counter of L2 entries the compressor returned as `Raw` (any reason).
    l2_compression_skipped_total: u64,
    /// Cumulative `(original_len - stored_len)` across compressed entries.
    l2_bytes_saved_total: u64,
    /// Counter — L1 hits that served a stale entry from the SWR window of
    /// an `ExtendedTtlPolicy` (#194). Stays 0 when extended is off.
    l1_stale_serves_total: u64,
    /// Counter — L1 entries evicted by the idle-TTL gate of an
    /// `ExtendedTtlPolicy` (#194). Stays 0 when extended is off.
    l1_idle_evicts_total: u64,
}

impl BlobCacheStats {
    /// Number of `get`/`exists` calls that resolved to `Present` /
    /// `MaybePresent`. Both count as hit prospects.
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Number of `get`/`exists` calls that resolved to `Absent`.
    pub fn misses(&self) -> u64 {
        self.misses
    }

    pub fn insertions(&self) -> u64 {
        self.insertions
    }

    pub fn evictions(&self) -> u64 {
        self.evictions
    }

    pub fn expirations(&self) -> u64 {
        self.expirations
    }

    pub fn invalidations(&self) -> u64 {
        self.invalidations
    }

    pub fn namespace_flushes(&self) -> u64 {
        self.namespace_flushes
    }

    pub fn version_mismatches(&self) -> u64 {
        self.version_mismatches
    }

    pub fn entries(&self) -> usize {
        self.entries
    }

    /// Bytes resident in L1. Returned as `u64` for symmetry with
    /// [`l2_bytes_in_use`](Self::l2_bytes_in_use); upcast is lossless.
    pub fn bytes_in_use(&self) -> u64 {
        self.bytes_in_use as u64
    }

    pub fn l1_bytes_max(&self) -> usize {
        self.l1_bytes_max
    }

    pub fn l2_bytes_in_use(&self) -> u64 {
        self.l2_bytes_in_use
    }

    pub fn l2_bytes_max(&self) -> u64 {
        self.l2_bytes_max
    }

    pub fn l2_full_rejections(&self) -> u64 {
        self.l2_full_rejections
    }

    pub fn l2_metadata_reads(&self) -> u64 {
        self.l2_metadata_reads
    }

    pub fn l2_negative_skips(&self) -> u64 {
        self.l2_negative_skips
    }

    /// Times the Bloom synopsis answered `MaybePresent` but the authoritative
    /// L2 metadata B+ tree said `Absent`. This is the cost of the
    /// probabilistic synopsis: a counter for the false-positive rate in
    /// production. Negative answers from the filter never trigger a metadata
    /// read (see [`l2_negative_skips`](Self::l2_negative_skips)).
    pub fn synopsis_metadata_reads(&self) -> u64 {
        self.synopsis_metadata_reads
    }

    /// Total bytes used by all per-namespace Bloom synopsis filters.
    pub fn synopsis_bytes(&self) -> u64 {
        self.synopsis_bytes
    }

    pub fn namespaces(&self) -> usize {
        self.namespaces
    }

    pub fn max_namespaces(&self) -> usize {
        self.max_namespaces
    }

    /// Total promotion requests successfully enqueued by `get` since boot.
    /// `0` when async promotion is disabled.
    pub fn promotion_queued(&self) -> u64 {
        self.promotion_queued
    }

    /// Total promotion requests dropped on queue saturation since boot.
    /// `0` when async promotion is disabled.
    pub fn promotion_dropped(&self) -> u64 {
        self.promotion_dropped
    }

    /// Total promotion requests executed by workers since boot.
    /// `0` when async promotion is disabled.
    pub fn promotion_completed(&self) -> u64 {
        self.promotion_completed
    }

    /// Snapshot of pending requests in the promotion queue.
    /// `0` when async promotion is disabled.
    pub fn promotion_queue_depth(&self) -> usize {
        self.promotion_queue_depth
    }

    /// Running average of `original_len / stored_len` for L2 entries that
    /// the compressor actually shrank (#192). Returns `1.0` when no
    /// compressed entry has been observed yet, regardless of how many
    /// `Raw` entries have passed through (callers should pair this with
    /// [`l2_compression_skipped_total`](Self::l2_compression_skipped_total)
    /// to interpret).
    pub fn l2_compression_ratio_observed(&self) -> f64 {
        if self.l2_compression_stored_bytes == 0 {
            return 1.0;
        }
        self.l2_compression_original_bytes as f64 / self.l2_compression_stored_bytes as f64
    }

    /// Number of L2 entries the compressor returned as `Raw` since boot —
    /// any reason: payload below `min_bytes`, content type already
    /// compressed, ratio gate fired, or `cache.blob.l2_compression = "off"`.
    pub fn l2_compression_skipped_total(&self) -> u64 {
        self.l2_compression_skipped_total
    }

    /// Cumulative `(original_len - stored_len)` across all L2 entries the
    /// compressor shrank. Operators read this to size the L2 budget
    /// multiplier from real workloads.
    pub fn l2_bytes_saved_total(&self) -> u64 {
        self.l2_bytes_saved_total
    }

    /// Counter — L1 hits served as stale by the SWR window of an
    /// `ExtendedTtlPolicy` (#194). `0` when no entry was written with an
    /// active extended policy.
    pub fn l1_stale_serves_total(&self) -> u64 {
        self.l1_stale_serves_total
    }

    /// Counter — L1 entries evicted by the idle-TTL gate of an
    /// `ExtendedTtlPolicy` (#194). `0` when no entry was written with an
    /// active extended policy.
    pub fn l1_idle_evicts_total(&self) -> u64 {
        self.l1_idle_evicts_total
    }
}

#[derive(Debug)]
struct Entry {
    bytes: Arc<[u8]>,
    content_metadata: BTreeMap<String, String>,
    tags: BTreeSet<String>,
    dependencies: BTreeSet<String>,
    size: usize,
    visited: bool,
    expires_at_unix_ms: Option<u64>,
    priority: u8,
    version: Option<u64>,
    namespace_generation: u64,
    /// Wall-clock time of the most recent access (`put` or successful
    /// `get`). Updated on hits to drive [`ExtendedTtlPolicy::idle_ttl_ms`].
    /// L1-only — never propagated to the L2 record (cache is the source of
    /// truth for access patterns).
    last_access_unix_ms: u64,
    /// Extended TTL knobs captured from the [`BlobCachePolicy`] at insert
    /// time, including any jitter expansion that was already applied to
    /// `expires_at_unix_ms`.
    extended: ExtendedTtlPolicy,
}

impl Entry {
    fn new(
        bytes: Vec<u8>,
        content_metadata: BTreeMap<String, String>,
        tags: BTreeSet<String>,
        dependencies: BTreeSet<String>,
        policy: BlobCachePolicy,
        namespace_generation: u64,
        now_ms: u64,
        namespace: &str,
        key: &str,
    ) -> Self {
        let size = bytes.len();
        Self {
            bytes: Arc::<[u8]>::from(bytes),
            content_metadata,
            tags,
            dependencies,
            size,
            visited: true,
            expires_at_unix_ms: effective_expires_at_unix_ms(policy, now_ms, namespace, key),
            priority: policy.priority_value(),
            version: policy.version_value(),
            namespace_generation,
            last_access_unix_ms: now_ms,
            extended: policy.extended_value(),
        }
    }

    fn hit(&self) -> BlobCacheHit {
        BlobCacheHit::new(
            Arc::clone(&self.bytes),
            self.content_metadata.clone(),
            self.version,
        )
    }

    fn hit_stale(&self, window_remaining_ms: u64) -> BlobCacheHit {
        BlobCacheHit::new_stale(
            Arc::clone(&self.bytes),
            self.content_metadata.clone(),
            self.version,
            window_remaining_ms,
        )
    }

    fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_unix_ms
            .is_some_and(|expires_at| now_ms >= expires_at)
    }
}

/// Stable seed for [`EffectiveExpiry::jittered_ttl_ms`] derived from the
/// (namespace, key, now_ms) triple. The same triple always yields the
/// same seed so jitter is deterministic per insert.
fn jitter_seed(namespace: &str, key: &str, now_ms: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    namespace.hash(&mut hasher);
    key.hash(&mut hasher);
    now_ms.hash(&mut hasher);
    hasher.finish()
}

fn effective_expires_at_unix_ms(
    policy: BlobCachePolicy,
    now_ms: u64,
    namespace: &str,
    key: &str,
) -> Option<u64> {
    let extended = policy.extended_value();
    // Jitter only applies to the relative `ttl_ms` knob; an absolute
    // `expires_at_unix_ms` is treated as a hard ceiling and is never
    // pushed out by jitter.
    let jittered_ttl = policy.ttl_ms_value().map(|base| {
        if extended.jitter_pct > 0 {
            EffectiveExpiry::jittered_ttl_ms(
                base,
                extended.jitter_pct,
                jitter_seed(namespace, key, now_ms),
            )
        } else {
            base
        }
    });
    match (jittered_ttl, policy.expires_at_unix_ms_value()) {
        (Some(ttl), Some(abs)) => Some(now_ms.saturating_add(ttl).min(abs)),
        (Some(ttl), None) => Some(now_ms.saturating_add(ttl)),
        (None, Some(abs)) => Some(abs),
        (None, None) => None,
    }
}

#[derive(Debug)]
struct Shard {
    entries: HashMap<BlobCacheKey, Entry>,
    order: Vec<BlobCacheKey>,
    hand: usize,
    bytes: usize,
}

impl Shard {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: Vec::new(),
            hand: 0,
            bytes: 0,
        }
    }

    fn get(&mut self, key: &BlobCacheKey, now_ms: u64, namespace_generation: u64) -> Lookup {
        let Some(entry) = self.entries.get_mut(key) else {
            return Lookup::Miss;
        };
        if entry.namespace_generation != namespace_generation {
            let removed = self.remove(key).expect("entry exists");
            return Lookup::Stale(removed);
        }
        // Fast path: extended is `off()` — bypass `EffectiveExpiry::compute`
        // entirely and use the legacy hard-only check. No idle/stale/jitter
        // semantics, no last_access bookkeeping for callers that opted out.
        if !entry.extended.is_active() {
            if entry.is_expired_at(now_ms) {
                let removed = self.remove(key).expect("entry exists");
                return Lookup::Expired(removed);
            }
            entry.visited = true;
            entry.last_access_unix_ms = now_ms;
            return Lookup::Hit(entry.hit());
        }

        // Extended path: route through `EffectiveExpiry::compute` for
        // idle / stale-while-revalidate decisions.
        #[cfg(test)]
        EFFECTIVE_EXPIRY_COMPUTE_CALLS.with(|c| c.set(c.get() + 1));
        let decision = EffectiveExpiry::compute(
            entry.expires_at_unix_ms,
            now_ms,
            entry.last_access_unix_ms,
            &entry.extended,
        );
        match decision {
            ExpiryDecision::Fresh => {
                entry.visited = true;
                entry.last_access_unix_ms = now_ms;
                Lookup::Hit(entry.hit())
            }
            ExpiryDecision::Stale {
                window_remaining_ms,
            } => {
                entry.visited = true;
                entry.last_access_unix_ms = now_ms;
                Lookup::Hit(entry.hit_stale(window_remaining_ms))
            }
            ExpiryDecision::Expired => {
                // Distinguish idle eviction from hard expiry so the caller
                // can bump the right counter. Hard expiry implies a
                // concrete `expires_at_unix_ms` already passed; otherwise
                // the kill came from the idle-TTL gate.
                let killed_by_idle = !entry.is_expired_at(now_ms);
                let removed = self.remove(key).expect("entry exists");
                if killed_by_idle {
                    Lookup::IdleEvicted(removed)
                } else {
                    Lookup::Expired(removed)
                }
            }
        }
    }

    fn contains(&mut self, key: &BlobCacheKey, now_ms: u64, namespace_generation: u64) -> Lookup {
        let Some(entry) = self.entries.get_mut(key) else {
            return Lookup::Miss;
        };
        if entry.namespace_generation != namespace_generation {
            let removed = self.remove(key).expect("entry exists");
            return Lookup::Stale(removed);
        }
        if entry.is_expired_at(now_ms) {
            let removed = self.remove(key).expect("entry exists");
            return Lookup::Expired(removed);
        }
        entry.visited = true;
        Lookup::Present
    }

    fn existing_version(&self, key: &BlobCacheKey, namespace_generation: u64) -> Option<u64> {
        self.entries.get(key).and_then(|entry| {
            if entry.namespace_generation == namespace_generation {
                entry.version
            } else {
                None
            }
        })
    }

    fn insert(&mut self, key: BlobCacheKey, entry: Entry) -> InsertOutcome {
        let old_entry = if let Some(old) = self.entries.remove(&key) {
            self.bytes = self.bytes.saturating_sub(old.size);
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
                if self.hand > pos {
                    self.hand -= 1;
                }
                if self.hand > self.order.len() {
                    self.hand = 0;
                }
            }
            Some(old)
        } else {
            None
        };

        self.bytes += entry.size;
        self.entries.insert(key.clone(), entry);
        self.order.push(key);
        InsertOutcome {
            old_entry,
            admitted: true,
        }
    }

    fn evict_one(&mut self) -> Option<(BlobCacheKey, Entry)> {
        if self.order.is_empty() {
            self.hand = 0;
            return None;
        }
        let max_sweeps = self.order.len().saturating_mul(2).max(1);
        for _ in 0..max_sweeps {
            if self.order.is_empty() {
                self.hand = 0;
                return None;
            }
            if self.hand >= self.order.len() {
                self.hand = 0;
            }
            let candidate = self.order[self.hand].clone();
            let Some(entry) = self.entries.get(&candidate) else {
                self.order.remove(self.hand);
                continue;
            };
            if entry.visited {
                if let Some(entry) = self.entries.get_mut(&candidate) {
                    entry.visited = false;
                }
                self.hand = (self.hand + 1) % self.order.len();
                continue;
            }

            if self.has_lower_priority_unvisited(entry.priority) {
                self.hand = (self.hand + 1) % self.order.len();
                continue;
            }

            let removed = self.entries.remove(&candidate).expect("candidate exists");
            self.bytes = self.bytes.saturating_sub(removed.size);
            self.order.remove(self.hand);
            if self.hand >= self.order.len() {
                self.hand = 0;
            }
            return Some((candidate, removed));
        }
        None
    }

    fn len(&self) -> usize {
        self.entries.len()
    }

    fn remove(&mut self, key: &BlobCacheKey) -> Option<Entry> {
        let removed = self.entries.remove(key)?;
        self.bytes = self.bytes.saturating_sub(removed.size);
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
            if self.hand > pos {
                self.hand -= 1;
            }
            if self.hand >= self.order.len() {
                self.hand = 0;
            }
        }
        Some(removed)
    }

    fn has_lower_priority_unvisited(&self, priority: u8) -> bool {
        self.entries
            .values()
            .any(|entry| !entry.visited && entry.priority < priority)
    }
}

enum Lookup {
    Hit(BlobCacheHit),
    Present,
    Expired(Entry),
    /// Entry was killed by the idle TTL gate of an `ExtendedTtlPolicy`.
    /// Distinguished from [`Expired`] so the cache-level counter for
    /// idle evictions stays separate from hard-TTL expirations.
    IdleEvicted(Entry),
    Stale(Entry),
    Miss,
}

struct InsertOutcome {
    old_entry: Option<Entry>,
    admitted: bool,
}

#[derive(Clone, Copy)]
enum IndexedKind {
    Tag,
    Dependency,
}

#[derive(Debug)]
struct AtomicStats {
    hits: AtomicU64,
    misses: AtomicU64,
    insertions: AtomicU64,
    evictions: AtomicU64,
    expirations: AtomicU64,
    invalidations: AtomicU64,
    namespace_flushes: AtomicU64,
    version_mismatches: AtomicU64,
    l2_full_rejections: AtomicU64,
    /// Counter incremented every time `BlobCache::get` returns a stale
    /// entry from the SWR window of an `ExtendedTtlPolicy`. Stays at 0
    /// when extended is `off()` for every entry.
    l1_stale_serves: AtomicU64,
    /// Counter incremented every time the idle-TTL gate of an
    /// `ExtendedTtlPolicy` evicts an L1 entry. Stays at 0 when extended
    /// is `off()` for every entry.
    l1_idle_evicts: AtomicU64,
}

impl AtomicStats {
    fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            insertions: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            expirations: AtomicU64::new(0),
            invalidations: AtomicU64::new(0),
            namespace_flushes: AtomicU64::new(0),
            version_mismatches: AtomicU64::new(0),
            l2_full_rejections: AtomicU64::new(0),
            l1_stale_serves: AtomicU64::new(0),
            l1_idle_evicts: AtomicU64::new(0),
        }
    }
}

const L2_CONTROL_MAGIC: &[u8; 4] = b"RDB2";
const L2_METADATA_MAGIC: &[u8; 4] = b"RDCM";
const L2_BLOB_MAGIC: &[u8; 4] = b"RDCB";

#[derive(Debug, Clone, Default)]
struct L2Control {
    metadata_root: u32,
    bytes_in_use: u64,
}

impl L2Control {
    fn read(path: &Path) -> Result<Self, CacheError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let mut file = File::open(path).map_err(|err| CacheError::L2Io(err.to_string()))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|err| CacheError::L2Io(err.to_string()))?;
        if bytes.len() < 16 || &bytes[0..4] != L2_CONTROL_MAGIC {
            return Err(CacheError::L2Io(
                "invalid blob-cache L2 control file".into(),
            ));
        }
        Ok(Self {
            metadata_root: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            bytes_in_use: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
        })
    }

    fn write(&self, path: &Path) -> Result<(), CacheError> {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(L2_CONTROL_MAGIC);
        bytes.extend_from_slice(&self.metadata_root.to_le_bytes());
        bytes.extend_from_slice(&self.bytes_in_use.to_le_bytes());
        let tmp = path.with_extension("ctl.tmp");
        {
            let mut file = File::create(&tmp).map_err(|err| CacheError::L2Io(err.to_string()))?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|err| CacheError::L2Io(err.to_string()))?;
        }
        std::fs::rename(&tmp, path).map_err(|err| CacheError::L2Io(err.to_string()))
    }
}

/// On-disk format marker for the bytes the L2 blob-chain holds.
///
/// `V1Raw` (= 0) is the legacy format: the chain bytes are the original
/// payload verbatim. `V2Framed` (= 1) is the post-#192 format: the chain
/// bytes are the [`Compressed`] disk encoding (1-byte tag, optional 4-byte
/// `original_len`, then the encoded payload).
///
/// New writes always emit `V2Framed`. Reads dispatch on this field so older
/// `V1Raw` entries on disk still decode correctly until they age out.
const L2_FORMAT_V1_RAW: u8 = 0;
const L2_FORMAT_V2_FRAMED: u8 = 1;

const L2_FRAME_TAG_RAW: u8 = 0;
const L2_FRAME_TAG_ZSTD: u8 = 1;

#[derive(Debug, Clone)]
struct L2Record {
    namespace: String,
    key: String,
    expires_at_unix_ms: Option<u64>,
    namespace_generation: u64,
    priority: u8,
    version: Option<u64>,
    root_page: u32,
    page_count: u32,
    byte_len: u64,
    checksum: u32,
    /// On-disk format tag for the blob chain. `0` means legacy raw bytes
    /// (entries written before #192); `1` means the post-#192 framed
    /// `Compressed` encoding. Forward-compat read: the field is parsed
    /// optionally so records persisted before this byte was reserved
    /// continue to deserialize as `V1Raw`.
    format_version: u8,
}

impl L2Record {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(96 + self.namespace.len() + self.key.len());
        out.extend_from_slice(L2_METADATA_MAGIC);
        write_l2_string(&mut out, &self.namespace);
        write_l2_string(&mut out, &self.key);
        out.extend_from_slice(&self.expires_at_unix_ms.unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&self.namespace_generation.to_le_bytes());
        out.push(self.priority);
        out.extend_from_slice(&self.version.unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&self.root_page.to_le_bytes());
        out.extend_from_slice(&self.page_count.to_le_bytes());
        out.extend_from_slice(&self.byte_len.to_le_bytes());
        out.extend_from_slice(&self.checksum.to_le_bytes());
        out.push(self.format_version);
        out
    }

    fn decode(mut bytes: &[u8]) -> Result<Self, CacheError> {
        if bytes.len() < 4 || &bytes[0..4] != L2_METADATA_MAGIC {
            return Err(CacheError::L2Io("invalid blob-cache L2 metadata".into()));
        }
        bytes = &bytes[4..];
        let namespace = read_l2_string(&mut bytes)?;
        let key = read_l2_string(&mut bytes)?;
        if bytes.len() < 41 {
            return Err(CacheError::L2Io("truncated blob-cache L2 metadata".into()));
        }
        let expires_at = u64::from_le_bytes(bytes[0..8].try_into().expect("len checked"));
        let namespace_generation =
            u64::from_le_bytes(bytes[8..16].try_into().expect("len checked"));
        let priority = bytes[16];
        let version = u64::from_le_bytes(bytes[17..25].try_into().expect("len checked"));
        let root_page = u32::from_le_bytes(bytes[25..29].try_into().expect("len checked"));
        let page_count = u32::from_le_bytes(bytes[29..33].try_into().expect("len checked"));
        let byte_len = u64::from_le_bytes(bytes[33..41].try_into().expect("len checked"));
        let checksum = if bytes.len() >= 45 {
            u32::from_le_bytes(bytes[41..45].try_into().expect("len checked"))
        } else {
            0
        };
        // Optional `format_version` byte (added in #192 lane 2/5). Records
        // written before this commit do not include it; they describe the
        // legacy `V1Raw` chain layout.
        let format_version = if bytes.len() >= 46 {
            bytes[45]
        } else {
            L2_FORMAT_V1_RAW
        };
        Ok(Self {
            namespace,
            key,
            expires_at_unix_ms: (expires_at != 0).then_some(expires_at),
            namespace_generation,
            priority,
            version: (version != 0).then_some(version),
            root_page,
            page_count,
            byte_len,
            checksum,
            format_version,
        })
    }

    fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_unix_ms
            .is_some_and(|expires_at| now_ms >= expires_at)
    }
}

/// Encode a [`Compressed`] payload into the V2 chain layout: `[tag]` for
/// `Raw`, or `[tag, original_len_le32, encoded_bytes...]` for `Zstd`.
///
/// The header overhead (1 byte for `Raw`, 5 bytes for `Zstd`) is intentional
/// — it lets the read path recover the original payload length without
/// trusting the [`L2Record::byte_len`] field, and lets `decode_v2_frame`
/// fail loudly on corruption rather than silently mis-slicing.
fn encode_v2_frame(c: &Compressed) -> Vec<u8> {
    match c {
        Compressed::Raw(bytes) => {
            let mut out = Vec::with_capacity(1 + bytes.len());
            out.push(L2_FRAME_TAG_RAW);
            out.extend_from_slice(bytes);
            out
        }
        Compressed::Zstd {
            bytes,
            original_len,
        } => {
            let mut out = Vec::with_capacity(5 + bytes.len());
            out.push(L2_FRAME_TAG_ZSTD);
            out.extend_from_slice(&original_len.to_le_bytes());
            out.extend_from_slice(bytes);
            out
        }
    }
}

/// Decode the V2 chain layout produced by [`encode_v2_frame`].
fn decode_v2_frame(bytes: &[u8]) -> Result<Compressed, CacheError> {
    if bytes.is_empty() {
        return Err(CacheError::L2Io("empty blob-cache L2 v2 frame".into()));
    }
    match bytes[0] {
        L2_FRAME_TAG_RAW => Ok(Compressed::Raw(bytes[1..].to_vec())),
        L2_FRAME_TAG_ZSTD => {
            if bytes.len() < 5 {
                return Err(CacheError::L2Io(
                    "truncated blob-cache L2 zstd frame".into(),
                ));
            }
            let original_len = u32::from_le_bytes(bytes[1..5].try_into().expect("len checked"));
            Ok(Compressed::Zstd {
                bytes: bytes[5..].to_vec(),
                original_len,
            })
        }
        other => Err(CacheError::L2Io(format!(
            "unknown blob-cache L2 frame tag {other}"
        ))),
    }
}

fn write_l2_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn read_l2_string(bytes: &mut &[u8]) -> Result<String, CacheError> {
    if bytes.len() < 2 {
        return Err(CacheError::L2Io("truncated blob-cache L2 string".into()));
    }
    let len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    *bytes = &bytes[2..];
    if bytes.len() < len {
        return Err(CacheError::L2Io("truncated blob-cache L2 string".into()));
    }
    let value = std::str::from_utf8(&bytes[..len])
        .map_err(|err| CacheError::L2Io(err.to_string()))?
        .to_string();
    *bytes = &bytes[len..];
    Ok(value)
}

/// Tiny in-tree Bloom filter for the L2 membership synopsis (#146).
///
/// # Sizing
///
/// For a target capacity `n` and false-positive rate `p`, the optimal
/// parameters are:
///
/// - bit array size `m = -n * ln(p) / ln(2)^2`
/// - hash count `k = (m / n) * ln(2)`
///
/// At the cache defaults (`n = 10_000`, `p = 0.01`) this yields
/// `m ≈ 95_851 bits ≈ 12 KB` and `k = 7`. With
/// [`DEFAULT_BLOB_MAX_NAMESPACES`] = 256 the worst-case synopsis state is
/// ~3 MB — acceptable next to a 256 MB L1 budget.
///
/// # Contract
///
/// - `contains(key)` returning `false` ALWAYS means absent (no
///   false-negatives).
/// - `contains(key)` returning `true` means MaybePresent — callers MUST verify
///   against the authoritative L2 metadata B+ tree.
/// - Bits cannot be cleared without losing the no-false-negatives guarantee,
///   so deletes / expirations leave stale bits behind. Stale bits cause extra
///   L2 metadata verifications, never spurious `Present` answers. A periodic
///   full rebuild from the metadata B+ tree (currently startup-only) reclaims
///   that space.
mod synopsis_filter {
    use std::hash::{Hash, Hasher};

    /// Per-namespace Bloom filter. Hashing uses double-hashing
    /// (`h_i(x) = h1(x) + i * h2(x)`) over two `DefaultHasher` seeds to avoid
    /// pulling in any new dependency. The filter is never persisted; it is
    /// rebuilt from the L2 metadata B+ tree at startup, so the per-process
    /// `RandomState` of `DefaultHasher` is irrelevant to correctness.
    #[derive(Debug)]
    pub(super) struct BloomFilter {
        bits: Vec<u64>,
        bit_count: usize,
        hash_count: u32,
    }

    impl BloomFilter {
        /// Sized for `capacity` insertions at `target_fpr` false-positive
        /// rate. `capacity` is clamped to ≥ 1 and `target_fpr` is clamped to
        /// `(0.0, 1.0)` to avoid undefined math at the edges.
        pub(super) fn with_capacity(capacity: usize, target_fpr: f64) -> Self {
            let n = capacity.max(1) as f64;
            let p = target_fpr.clamp(f64::MIN_POSITIVE, 0.999_999);
            // m = -n * ln(p) / ln(2)^2
            let ln2 = std::f64::consts::LN_2;
            let m_bits = (-(n * p.ln()) / (ln2 * ln2)).ceil() as usize;
            let bit_count = m_bits.max(64);
            // k = (m / n) * ln(2)
            let k = ((bit_count as f64 / n) * ln2).round() as u32;
            let hash_count = k.max(1);
            let words = bit_count.div_ceil(64);
            Self {
                bits: vec![0u64; words],
                bit_count,
                hash_count,
            }
        }

        pub(super) fn insert(&mut self, key: &str) {
            let (h1, h2) = double_hash(key);
            for i in 0..self.hash_count {
                let bit =
                    (h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.bit_count as u64) as usize;
                self.bits[bit / 64] |= 1u64 << (bit % 64);
            }
        }

        pub(super) fn contains(&self, key: &str) -> bool {
            let (h1, h2) = double_hash(key);
            for i in 0..self.hash_count {
                let bit =
                    (h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.bit_count as u64) as usize;
                if self.bits[bit / 64] & (1u64 << (bit % 64)) == 0 {
                    return false;
                }
            }
            true
        }

        /// Bytes consumed by the bit array (the heap allocation).
        pub(super) fn bytes(&self) -> u64 {
            (self.bits.len() * std::mem::size_of::<u64>()) as u64
        }

        #[cfg(test)]
        pub(super) fn bit_count(&self) -> usize {
            self.bit_count
        }

        #[cfg(test)]
        pub(super) fn hash_count(&self) -> u32 {
            self.hash_count
        }
    }

    /// Two independent 64-bit hashes via two seeded `DefaultHasher`s. Only
    /// used to derive `k` Bloom positions via double-hashing; correctness
    /// does not depend on the choice of hasher (only `contains` after
    /// `insert` returning `true` matters, which holds for any total hash).
    fn double_hash(key: &str) -> (u64, u64) {
        let mut h1 = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h1);
        let mut h2 = std::collections::hash_map::DefaultHasher::new();
        // Domain-separate the second hash so it is independent of `h1`
        // even though both seeds derive from the same `RandomState`.
        0xa5a5_a5a5_a5a5_a5a5u64.hash(&mut h2);
        key.hash(&mut h2);
        let v2 = h2.finish();
        // Ensure the increment is non-zero so all `k` probes are distinct.
        (h1.finish(), v2 | 1)
    }
}

use synopsis_filter::BloomFilter;

struct BlobCacheL2 {
    pager: Arc<crate::storage::engine::Pager>,
    metadata: RwLock<crate::storage::engine::BTree>,
    synopsis: RwLock<HashMap<String, BloomFilter>>,
    control: RwLock<L2Control>,
    control_path: PathBuf,
    bytes_in_use: AtomicU64,
    metadata_reads: AtomicU64,
    negative_skips: AtomicU64,
    synopsis_metadata_reads: AtomicU64,
    bytes_max: u64,
    /// Number of L2 entries written through the `Zstd` variant.
    compression_compressed_count: AtomicU64,
    /// Sum of `original_len` over compressed entries (numerator of the
    /// observed-ratio metric).
    compression_original_bytes_sum: AtomicU64,
    /// Sum of `stored_len` over compressed entries (denominator of the
    /// observed-ratio metric).
    compression_stored_bytes_sum: AtomicU64,
    /// Cumulative bytes saved by the compressor across all L2 puts.
    compression_bytes_saved: AtomicU64,
    /// Number of L2 entries the compressor returned as `Raw` (any reason:
    /// too small, precompressed media, ratio gate, or `compression="off"`).
    compression_skipped_count: AtomicU64,
    #[cfg(test)]
    fault_after_blob_write: std::sync::atomic::AtomicBool,
}

impl BlobCacheL2 {
    fn open(path: PathBuf, bytes_max: u64) -> Result<Self, CacheError> {
        let control_path = path.with_extension("blob-cache.ctl");
        let control = L2Control::read(&control_path)?;
        let pager = Arc::new(
            crate::storage::engine::Pager::open(
                &path,
                crate::storage::engine::PagerConfig::default(),
            )
            .map_err(|err| CacheError::L2Io(err.to_string()))?,
        );
        let metadata = if control.metadata_root == 0 {
            crate::storage::engine::BTree::new(Arc::clone(&pager))
        } else {
            crate::storage::engine::BTree::with_root(Arc::clone(&pager), control.metadata_root)
        };
        let synopsis = rebuild_l2_synopsis(&metadata);
        Ok(Self {
            pager,
            metadata: RwLock::new(metadata),
            synopsis: RwLock::new(synopsis),
            bytes_in_use: AtomicU64::new(control.bytes_in_use),
            control: RwLock::new(control),
            control_path,
            metadata_reads: AtomicU64::new(0),
            negative_skips: AtomicU64::new(0),
            synopsis_metadata_reads: AtomicU64::new(0),
            bytes_max,
            compression_compressed_count: AtomicU64::new(0),
            compression_original_bytes_sum: AtomicU64::new(0),
            compression_stored_bytes_sum: AtomicU64::new(0),
            compression_bytes_saved: AtomicU64::new(0),
            compression_skipped_count: AtomicU64::new(0),
            #[cfg(test)]
            fault_after_blob_write: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn get(&self, key: &BlobCacheKey, now_ms: u64, generation: u64) -> Option<Entry> {
        if !self.synopsis_may_contain(&key.namespace, &key.key) {
            self.negative_skips.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let encoded_key = encode_l2_key(&key.namespace, &key.key);
        self.metadata_reads.fetch_add(1, Ordering::Relaxed);
        let record = match self
            .metadata
            .read()
            .get(&encoded_key)
            .ok()
            .flatten()
            .and_then(|bytes| L2Record::decode(&bytes).ok())
        {
            Some(record) => record,
            None => {
                // Bloom synopsis said MaybePresent but the authoritative
                // metadata B+ tree disagrees: a false positive (or stale
                // bit). Count it for FPR observability.
                self.synopsis_metadata_reads.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        };
        if record.namespace_generation != generation || record.is_expired_at(now_ms) {
            let _ = self.delete_key(key);
            return None;
        }
        let chain_bytes = self.read_blob_chain(record.root_page).ok()?;
        if crate::storage::engine::crc32(&chain_bytes) != record.checksum {
            return None;
        }
        // Forward-compat: legacy `V1Raw` entries (written before #192 lane
        // 2/5) keep their original bytes verbatim in the chain. New entries
        // wrap a `Compressed` payload via `encode_v2_frame`. Dispatch on the
        // record's format tag rather than guessing from the byte stream.
        let payload = match record.format_version {
            L2_FORMAT_V1_RAW => chain_bytes,
            L2_FORMAT_V2_FRAMED => {
                let framed = decode_v2_frame(&chain_bytes).ok()?;
                L2BlobCompressor::decompress(&framed).ok()?
            }
            _ => return None,
        };
        Some(Entry {
            size: payload.len(),
            bytes: Arc::<[u8]>::from(payload),
            content_metadata: BTreeMap::new(),
            tags: BTreeSet::new(),
            dependencies: BTreeSet::new(),
            visited: true,
            expires_at_unix_ms: record.expires_at_unix_ms,
            priority: record.priority,
            version: record.version,
            namespace_generation: record.namespace_generation,
            // L2 records do not persist last_access (cache-source-of-truth)
            // — seed with `now_ms` so an entry rehydrated from L2 starts
            // its idle window fresh.
            last_access_unix_ms: now_ms,
            // L2 records do not persist the extended policy. Rehydrated
            // entries start with `off()`, matching the historical
            // hard-only semantics until they are overwritten by a fresh
            // `put` carrying an explicit extended policy.
            extended: ExtendedTtlPolicy::off(),
        })
    }

    fn put(
        &self,
        key: &BlobCacheKey,
        entry: &Entry,
        old_entry_size: u64,
        compressed: Compressed,
    ) -> Result<(), CacheError> {
        let original_len = entry.size as u64;
        let stored_len = compressed.stored_len() as u64;
        let was_compressed = compressed.is_compressed();
        // Account against the *stored* length — this is the disk-capacity
        // multiplier the L2 budget is meant to amplify (#192).
        let current = self.bytes_in_use.load(Ordering::Relaxed);
        let projected = current
            .saturating_sub(old_entry_size)
            .saturating_add(stored_len);
        if projected > self.bytes_max {
            return Err(CacheError::L2Full {
                size: projected,
                max: self.bytes_max,
            });
        }

        let framed = encode_v2_frame(&compressed);
        let (root_page, page_count, checksum) = self.write_blob_chain(&framed)?;
        #[cfg(test)]
        if self
            .fault_after_blob_write
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            panic!("blob-cache L2 fault after blob write");
        }

        let record = L2Record {
            namespace: key.namespace.clone(),
            key: key.key.clone(),
            expires_at_unix_ms: entry.expires_at_unix_ms,
            namespace_generation: entry.namespace_generation,
            priority: entry.priority,
            version: entry.version,
            root_page,
            page_count,
            byte_len: stored_len,
            checksum,
            format_version: L2_FORMAT_V2_FRAMED,
        };
        let encoded_key = encode_l2_key(&key.namespace, &key.key);
        let metadata = self.metadata.write();
        let _ = metadata.delete(&encoded_key);
        metadata
            .insert(&encoded_key, &record.encode())
            .map_err(|err| CacheError::L2Io(err.to_string()))?;
        let new_root = metadata.root_page_id();
        drop(metadata);

        self.bytes_in_use.store(projected, Ordering::Relaxed);
        let mut control = self.control.write();
        control.metadata_root = new_root;
        control.bytes_in_use = projected;
        control.write(&self.control_path)?;
        self.add_synopsis_key(&key.namespace, &key.key);
        // Compression observability counters (#192). Only `Zstd`-variant
        // entries contribute to the saved-bytes / ratio aggregates; `Raw`
        // entries (skip rules: too small, precompressed media, ratio gate)
        // bump the `skipped` counter so operators can tell *why* the L2
        // multiplier is not climbing.
        if was_compressed {
            self.compression_compressed_count
                .fetch_add(1, Ordering::Relaxed);
            self.compression_original_bytes_sum
                .fetch_add(original_len, Ordering::Relaxed);
            self.compression_stored_bytes_sum
                .fetch_add(stored_len, Ordering::Relaxed);
            // `original >= stored` always holds by the `max_ratio` gate.
            self.compression_bytes_saved
                .fetch_add(original_len.saturating_sub(stored_len), Ordering::Relaxed);
        } else {
            self.compression_skipped_count
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    fn record_size(&self, key: &BlobCacheKey) -> u64 {
        let encoded_key = encode_l2_key(&key.namespace, &key.key);
        self.metadata
            .read()
            .get(&encoded_key)
            .ok()
            .flatten()
            .and_then(|bytes| L2Record::decode(&bytes).ok())
            .map_or(0, |record| record.byte_len)
    }

    fn delete_key(&self, key: &BlobCacheKey) -> Option<u64> {
        let encoded_key = encode_l2_key(&key.namespace, &key.key);
        let metadata = self.metadata.write();
        let old = metadata
            .get(&encoded_key)
            .ok()
            .flatten()
            .and_then(|bytes| L2Record::decode(&bytes).ok());
        let removed = metadata.delete(&encoded_key).ok().unwrap_or(false);
        let new_root = metadata.root_page_id();
        drop(metadata);
        if !removed {
            return None;
        }
        let old_size = old.as_ref().map_or(0, |record| record.byte_len);
        let new_bytes = self
            .bytes_in_use
            .fetch_sub(old_size, Ordering::Relaxed)
            .saturating_sub(old_size);
        let mut control = self.control.write();
        control.metadata_root = new_root;
        control.bytes_in_use = new_bytes;
        let _ = control.write(&self.control_path);
        Some(old_size)
    }

    fn delete_namespace(&self, namespace: &str) -> usize {
        self.delete_where(|record| record.namespace == namespace)
    }

    fn has_namespace(&self, namespace: &str) -> bool {
        let metadata = self.metadata.read();
        let mut cursor = match metadata.cursor_first() {
            Ok(cursor) => cursor,
            Err(_) => return false,
        };
        while let Ok(Some((_, value))) = cursor.next() {
            if L2Record::decode(&value).is_ok_and(|record| record.namespace == namespace) {
                return true;
            }
        }
        false
    }

    fn delete_prefix(&self, namespace: &str, prefix: &str) -> usize {
        self.delete_where(|record| record.namespace == namespace && record.key.starts_with(prefix))
    }

    fn delete_where(&self, predicate: impl Fn(&L2Record) -> bool) -> usize {
        let keys = {
            let metadata = self.metadata.read();
            let mut cursor = match metadata.cursor_first() {
                Ok(cursor) => cursor,
                Err(_) => return 0,
            };
            let mut keys = Vec::new();
            while let Ok(Some((key, value))) = cursor.next() {
                if L2Record::decode(&value).is_ok_and(|record| predicate(&record)) {
                    keys.push(key);
                }
            }
            keys
        };

        let mut removed = 0;
        for encoded in keys {
            let metadata = self.metadata.write();
            let old = metadata
                .get(&encoded)
                .ok()
                .flatten()
                .and_then(|bytes| L2Record::decode(&bytes).ok());
            if metadata.delete(&encoded).ok().unwrap_or(false) {
                removed += 1;
                if let Some(old) = old {
                    self.bytes_in_use.fetch_sub(old.byte_len, Ordering::Relaxed);
                }
            }
        }
        self.persist_control();
        removed
    }

    fn persist_control(&self) {
        let metadata_root = self.metadata.read().root_page_id();
        let bytes_in_use = self.bytes_in_use.load(Ordering::Relaxed);
        let mut control = self.control.write();
        control.metadata_root = metadata_root;
        control.bytes_in_use = bytes_in_use;
        let _ = control.write(&self.control_path);
    }

    fn stats_bytes_in_use(&self) -> u64 {
        self.bytes_in_use.load(Ordering::Relaxed)
    }

    fn stats_metadata_reads(&self) -> u64 {
        self.metadata_reads.load(Ordering::Relaxed)
    }

    fn stats_negative_skips(&self) -> u64 {
        self.negative_skips.load(Ordering::Relaxed)
    }

    fn stats_synopsis_metadata_reads(&self) -> u64 {
        self.synopsis_metadata_reads.load(Ordering::Relaxed)
    }

    fn stats_synopsis_bytes(&self) -> u64 {
        self.synopsis
            .read()
            .values()
            .map(|filter| filter.bytes())
            .sum()
    }

    fn stats_compression_original_bytes(&self) -> u64 {
        self.compression_original_bytes_sum.load(Ordering::Relaxed)
    }

    fn stats_compression_stored_bytes(&self) -> u64 {
        self.compression_stored_bytes_sum.load(Ordering::Relaxed)
    }

    fn stats_compression_skipped_total(&self) -> u64 {
        self.compression_skipped_count.load(Ordering::Relaxed)
    }

    fn stats_bytes_saved_total(&self) -> u64 {
        self.compression_bytes_saved.load(Ordering::Relaxed)
    }

    fn synopsis_may_contain(&self, namespace: &str, key: &str) -> bool {
        self.synopsis
            .read()
            .get(namespace)
            .is_some_and(|filter| filter.contains(key))
    }

    fn add_synopsis_key(&self, namespace: &str, key: &str) {
        self.synopsis
            .write()
            .entry(namespace.to_string())
            .or_insert_with(|| {
                BloomFilter::with_capacity(
                    DEFAULT_BLOB_SYNOPSIS_CAPACITY,
                    DEFAULT_BLOB_SYNOPSIS_FPR,
                )
            })
            .insert(key);
    }

    #[cfg(test)]
    fn inject_synopsis_maybe_present(&self, namespace: &str, key: &str) {
        self.add_synopsis_key(namespace, key);
    }

    #[cfg(test)]
    fn inject_fault_after_blob_write_once(&self) {
        self.fault_after_blob_write
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Test-only escape hatch that synthesises a legacy `V1Raw` L2 entry —
    /// raw payload bytes in the chain, `format_version = 0` in the
    /// metadata. Used to verify forward-compat reads of entries written
    /// before #192 lane 2/5 landed.
    #[cfg(test)]
    fn inject_v1_entry(&self, key: &BlobCacheKey, payload: &[u8]) -> Result<(), CacheError> {
        let (root_page, page_count, checksum) = self.write_blob_chain(payload)?;
        let record = L2Record {
            namespace: key.namespace.clone(),
            key: key.key.clone(),
            expires_at_unix_ms: None,
            namespace_generation: 0,
            priority: 128,
            version: None,
            root_page,
            page_count,
            byte_len: payload.len() as u64,
            checksum,
            format_version: L2_FORMAT_V1_RAW,
        };
        let encoded_key = encode_l2_key(&key.namespace, &key.key);
        let metadata = self.metadata.write();
        let _ = metadata.delete(&encoded_key);
        metadata
            .insert(&encoded_key, &record.encode())
            .map_err(|err| CacheError::L2Io(err.to_string()))?;
        let new_root = metadata.root_page_id();
        drop(metadata);
        let new_bytes = self
            .bytes_in_use
            .fetch_add(payload.len() as u64, Ordering::Relaxed)
            .saturating_add(payload.len() as u64);
        let mut control = self.control.write();
        control.metadata_root = new_root;
        control.bytes_in_use = new_bytes;
        control.write(&self.control_path)?;
        self.add_synopsis_key(&key.namespace, &key.key);
        Ok(())
    }

    fn write_blob_chain(&self, payload: &[u8]) -> Result<(u32, u32, u32), CacheError> {
        if payload.is_empty() {
            return Ok((0, 0, 0));
        }
        let chunk_capacity =
            crate::storage::engine::PAGE_SIZE - crate::storage::engine::HEADER_SIZE - 12;
        let mut page_ids = Vec::new();
        for _ in payload.chunks(chunk_capacity) {
            page_ids.push(
                self.pager
                    .allocate_page(crate::storage::engine::PageType::NativeMeta)
                    .map_err(|err| CacheError::L2Io(err.to_string()))?
                    .page_id(),
            );
        }
        for (index, chunk) in payload.chunks(chunk_capacity).enumerate() {
            let page_id = page_ids[index];
            let next_page = page_ids.get(index + 1).copied().unwrap_or(0);
            let mut page = crate::storage::engine::Page::new(
                crate::storage::engine::PageType::NativeMeta,
                page_id,
            );
            let bytes = page.as_bytes_mut();
            let start = crate::storage::engine::HEADER_SIZE;
            bytes[start..start + 4].copy_from_slice(L2_BLOB_MAGIC);
            bytes[start + 4..start + 8].copy_from_slice(&next_page.to_le_bytes());
            bytes[start + 8..start + 12].copy_from_slice(&(chunk.len() as u32).to_le_bytes());
            bytes[start + 12..start + 12 + chunk.len()].copy_from_slice(chunk);
            self.pager
                .write_page(page_id, page)
                .map_err(|err| CacheError::L2Io(err.to_string()))?;
        }
        self.pager
            .flush()
            .map_err(|err| CacheError::L2Io(err.to_string()))?;
        Ok((
            page_ids[0],
            page_ids.len() as u32,
            crate::storage::engine::crc32(payload),
        ))
    }

    fn read_blob_chain(&self, root_page: u32) -> Result<Vec<u8>, CacheError> {
        if root_page == 0 {
            return Ok(Vec::new());
        }
        let mut current = root_page;
        let mut payload = Vec::new();
        while current != 0 {
            let page = self
                .pager
                .read_page(current)
                .map_err(|err| CacheError::L2Io(err.to_string()))?;
            let bytes = page.as_bytes();
            let start = crate::storage::engine::HEADER_SIZE;
            if bytes.len() < start + 12 || &bytes[start..start + 4] != L2_BLOB_MAGIC {
                return Err(CacheError::L2Io("invalid blob-cache L2 blob page".into()));
            }
            let next_page = u32::from_le_bytes(bytes[start + 4..start + 8].try_into().unwrap());
            let chunk_len =
                u32::from_le_bytes(bytes[start + 8..start + 12].try_into().unwrap()) as usize;
            if start + 12 + chunk_len > bytes.len() {
                return Err(CacheError::L2Io("truncated blob-cache L2 blob page".into()));
            }
            payload.extend_from_slice(&bytes[start + 12..start + 12 + chunk_len]);
            current = next_page;
        }
        Ok(payload)
    }
}

fn encode_l2_key(namespace: &str, key: &str) -> Vec<u8> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    namespace.hash(&mut hasher);
    let namespace_hash = hasher.finish();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    let key_hash = hasher.finish();
    let mut out = Vec::with_capacity(20 + namespace.len() + key.len());
    out.extend_from_slice(&namespace_hash.to_be_bytes());
    out.extend_from_slice(&key_hash.to_be_bytes());
    write_l2_string(&mut out, namespace);
    write_l2_string(&mut out, key);
    out
}

fn rebuild_l2_synopsis(metadata: &crate::storage::engine::BTree) -> HashMap<String, BloomFilter> {
    let mut synopsis: HashMap<String, BloomFilter> = HashMap::new();
    let Ok(mut cursor) = metadata.cursor_first() else {
        return synopsis;
    };
    while let Ok(Some((_, value))) = cursor.next() {
        if let Ok(record) = L2Record::decode(&value) {
            synopsis
                .entry(record.namespace)
                .or_insert_with(|| {
                    BloomFilter::with_capacity(
                        DEFAULT_BLOB_SYNOPSIS_CAPACITY,
                        DEFAULT_BLOB_SYNOPSIS_FPR,
                    )
                })
                .insert(&record.key);
        }
    }
    synopsis
}

/// Sharded, byte-bounded blob cache with optional durable L2 backing.
///
/// # Concurrency
///
/// `BlobCache` is `Send + Sync`. All public methods are safe to call from
/// multiple threads concurrently. Internal sharding ensures disjoint-key
/// contention does not serialize: independent keys land on independent
/// `RwLock<Shard>` instances, and the global indexes (namespace set, tag /
/// dependency maps) are read-mostly behind their own `RwLock`s.
///
/// `BlobCache` is **not** `Clone` — share ownership via `Arc<BlobCache>`.
///
/// # Blocking
///
/// All methods are synchronous. `put` may perform L2 disk I/O on the
/// calling thread when an L2 path is configured; tokio callers should wrap
/// `put` in `spawn_blocking`. `get`, `exists`, and the `invalidate_*`
/// family touch L2 only on rehydrate / delete paths.
pub struct BlobCache {
    config: BlobCacheConfig,
    shards: Vec<RwLock<Shard>>,
    namespaces: RwLock<HashSet<String>>,
    namespace_generations: RwLock<HashMap<String, u64>>,
    tag_index: RwLock<HashMap<ScopedLabel, HashSet<BlobCacheKey>>>,
    dependency_index: RwLock<HashMap<ScopedLabel, HashSet<BlobCacheKey>>>,
    l2: Option<Arc<BlobCacheL2>>,
    bytes_in_use: AtomicUsize,
    stats: AtomicStats,
    /// Optional async L2->L1 promotion pool (issue #193). When `None`,
    /// `get` performs the L1 promotion synchronously on the read path.
    /// When set via `enable_async_promotion`, L2 hits return bytes to
    /// the caller immediately and the L1 install runs on a worker.
    promotion_pool: OnceLock<Arc<AsyncPromotionPool>>,
}

// Compile-time guarantee that the documented `Send + Sync` contract above
// stays in lockstep with the struct's interior. If this ever fails to
// compile, the docstring is lying — fix the field that broke it, do not
// remove this assertion.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<BlobCache>();
};

impl BlobCache {
    /// Infallible constructor. Panics if `config.l2_path` is set and the L2
    /// file cannot be opened — use [`BlobCache::open_with_l2`] instead for
    /// configs that include an L2 path so boot errors are handled gracefully.
    pub fn new(config: BlobCacheConfig) -> Self {
        Self::try_new(config).expect("open blob-cache L2")
    }

    /// Fallible constructor for configs that include an L2 path.
    /// Returns `Err(CacheError::L2Io(...))` on invalid path, corrupt control
    /// sidecar, or any other recoverable I/O failure — the process stays alive.
    pub fn open_with_l2(config: BlobCacheConfig) -> Result<Self, CacheError> {
        Self::try_new(config)
    }

    fn try_new(config: BlobCacheConfig) -> Result<Self, CacheError> {
        let config = BlobCacheConfig {
            shard_count: config.shard_count.max(1),
            ..config
        };
        let l2 = config
            .l2_path
            .clone()
            .map(|path| BlobCacheL2::open(path, config.l2_bytes_max))
            .transpose()?;
        let shards = (0..config.shard_count)
            .map(|_| RwLock::new(Shard::new()))
            .collect();
        Ok(Self {
            config,
            shards,
            namespaces: RwLock::new(HashSet::new()),
            namespace_generations: RwLock::new(HashMap::new()),
            tag_index: RwLock::new(HashMap::new()),
            dependency_index: RwLock::new(HashMap::new()),
            l2: l2.map(Arc::new),
            bytes_in_use: AtomicUsize::new(0),
            stats: AtomicStats::new(),
            promotion_pool: OnceLock::new(),
        })
    }

    pub fn with_defaults() -> Self {
        Self::new(BlobCacheConfig::default())
    }

    /// Path to the L2 metadata B+ tree directory, when L2 is enabled.
    ///
    /// Used by the backup orchestrator (`include_blob_cache=true`) so it
    /// can locate the on-disk L2 tree for tarball / per-file upload, and
    /// by the runbook procedures in
    /// `docs/operations/blob-cache-backup-restore.md` §2 / §3 to confirm
    /// where on disk the cache lives.
    pub fn l2_path(&self) -> Option<&std::path::Path> {
        self.config.l2_path.as_deref()
    }

    pub fn put(
        &self,
        namespace: impl Into<String>,
        key: impl Into<String>,
        input: BlobCachePut,
    ) -> Result<(), CacheError> {
        self.put_at(namespace, key, input, unix_now_ms())
    }

    fn put_at(
        &self,
        namespace: impl Into<String>,
        key: impl Into<String>,
        input: BlobCachePut,
        now_ms: u64,
    ) -> Result<(), CacheError> {
        let namespace = namespace.into();
        let key = BlobCacheKey::new(namespace.clone(), key);
        self.validate_blob_size(input.bytes.len(), input.policy)?;
        self.validate_metadata(&input.content_metadata)?;
        self.ensure_namespace(&namespace)?;
        let namespace_generation = self.current_generation(&namespace);
        let tags = input.tags.clone();
        let dependencies = input.dependencies.clone();

        let shard_idx = self.shard_index(&key);
        let mut shard = self.shards[shard_idx].write();
        self.check_version(
            &shard,
            &key,
            input.policy.version_value(),
            namespace_generation,
        )?;
        let entry = Entry::new(
            input.bytes,
            input.content_metadata,
            input.tags,
            input.dependencies,
            input.policy,
            namespace_generation,
            now_ms,
            &namespace,
            &key.key,
        );
        let entry_size = entry.size;
        if let Some(l2) = &self.l2 {
            let old_l2_size = l2.record_size(&key);
            // Compression decision happens in the foreground put — the
            // outcome (`Compressed::Raw` or `Compressed::Zstd`) is what
            // gets framed and written to the chain (#192). When the knob
            // is `Off`, skip the compressor entirely (CPU savings) and
            // emit a `Raw` variant directly so the on-disk format stays
            // uniform.
            let compressed = match self.config.l2_compression {
                L2Compression::Off => Compressed::Raw(entry.bytes.as_ref().to_vec()),
                L2Compression::On => {
                    let content_type = entry
                        .content_metadata
                        .get("content-type")
                        .map(String::as_str);
                    L2BlobCompressor::compress(
                        entry.bytes.as_ref(),
                        content_type,
                        &CompressOpts::default(),
                    )
                    .map_err(|err| CacheError::L2Io(err.to_string()))?
                }
            };
            match l2.put(&key, &entry, old_l2_size, compressed) {
                Ok(()) => {}
                Err(err @ CacheError::L2Full { .. }) => {
                    self.stats
                        .l2_full_rejections
                        .fetch_add(1, Ordering::Relaxed);
                    return Err(err);
                }
                Err(err) => return Err(err),
            }
        }
        let outcome = if matches!(input.policy.l1_admission_value(), L1Admission::Never) {
            let old_entry = shard.remove(&key);
            InsertOutcome {
                old_entry,
                admitted: false,
            }
        } else {
            shard.insert(key.clone(), entry)
        };
        drop(shard);

        if let Some(old_entry) = outcome.old_entry.as_ref() {
            self.deindex_entry(&key, old_entry);
        }
        if outcome.admitted {
            self.index_entry(&key, &tags, &dependencies);
        }

        let old_size = outcome.old_entry.as_ref().map_or(0, |entry| entry.size);
        let new_size = if outcome.admitted { entry_size } else { 0 };
        if new_size >= old_size {
            self.bytes_in_use
                .fetch_add(new_size - old_size, Ordering::Relaxed);
        } else {
            self.bytes_in_use
                .fetch_sub(old_size - new_size, Ordering::Relaxed);
        }
        self.stats.insertions.fetch_add(1, Ordering::Relaxed);
        if outcome.admitted {
            self.evict_until_within_budget(shard_idx);
        }
        Ok(())
    }

    pub fn get(&self, namespace: &str, key: &str) -> Option<BlobCacheHit> {
        self.get_at(namespace, key, unix_now_ms())
    }

    fn get_at(&self, namespace: &str, key: &str, now_ms: u64) -> Option<BlobCacheHit> {
        let cache_key = BlobCacheKey::new(namespace, key);
        let namespace_generation = self.current_generation(namespace);
        let shard_idx = self.shard_index(&cache_key);
        let mut shard = self.shards[shard_idx].write();
        match shard.get(&cache_key, now_ms, namespace_generation) {
            Lookup::Hit(hit) => {
                self.stats.hits.fetch_add(1, Ordering::Relaxed);
                if hit.is_stale() {
                    self.stats.l1_stale_serves.fetch_add(1, Ordering::Relaxed);
                }
                Some(hit)
            }
            Lookup::Expired(entry) => {
                drop(shard);
                self.record_removed_entry(&cache_key, &entry);
                if let Some(l2) = &self.l2 {
                    l2.delete_key(&cache_key);
                }
                self.stats.expirations.fetch_add(1, Ordering::Relaxed);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Lookup::IdleEvicted(entry) => {
                drop(shard);
                self.record_removed_entry(&cache_key, &entry);
                if let Some(l2) = &self.l2 {
                    l2.delete_key(&cache_key);
                }
                self.stats.expirations.fetch_add(1, Ordering::Relaxed);
                self.stats.l1_idle_evicts.fetch_add(1, Ordering::Relaxed);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Lookup::Stale(entry) => {
                drop(shard);
                self.record_removed_entry(&cache_key, &entry);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Lookup::Miss => {
                drop(shard);
                if let Some(pool) = self.promotion_pool.get() {
                    // Async path: do the L2 read (we owe the bytes to the
                    // caller right now) but defer the L1 install onto the
                    // worker pool. Caller does not pay promotion bookkeeping.
                    if let Some(l2) = self.l2.as_ref() {
                        if let Some(entry) = l2.get(&cache_key, now_ms, namespace_generation) {
                            let hit = entry.hit();
                            // Drop the freshly-fetched Entry — the worker will
                            // re-fetch it. Cost: one extra L2 metadata read +
                            // blob read per L2 hit while async mode is on.
                            // Acceptable trade-off for opt-in mode; documented
                            // in the PR.
                            drop(entry);
                            let request = PromotionRequest {
                                namespace: cache_key.namespace.clone(),
                                key: cache_key.key.clone(),
                                bytes: Arc::clone(hit.bytes()),
                                policy: BlobCachePolicy::default(),
                            };
                            let _ = pool.schedule(request);
                            self.stats.hits.fetch_add(1, Ordering::Relaxed);
                            return Some(hit);
                        }
                    }
                    self.stats.misses.fetch_add(1, Ordering::Relaxed);
                    return None;
                }
                if let Some(hit) =
                    self.rehydrate_l2_entry(&cache_key, now_ms, namespace_generation, shard_idx)
                {
                    self.stats.hits.fetch_add(1, Ordering::Relaxed);
                    return Some(hit);
                }
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Lookup::Present => unreachable!("get cannot return presence-only lookup"),
        }
    }

    /// Probe whether `(namespace, key)` is cached.
    ///
    /// Returns a three-valued [`CachePresence`]:
    ///
    /// - `Present` when an L1-resident entry is held for the key.
    /// - `Absent` when the cache can authoritatively rule the key out: either
    ///   no L2 is configured, or the per-namespace Bloom synopsis
    ///   (no-false-negatives) says the key was never inserted into L2.
    /// - `MaybePresent` when L1 missed but the Bloom synopsis cannot rule the
    ///   key out. Callers that need an exact answer must follow up with
    ///   [`get`](Self::get), which performs the authoritative metadata read
    ///   and either rehydrates a hit or surfaces a genuine miss.
    ///
    /// `exists` deliberately does NOT touch the L2 metadata B+ tree on a
    /// `MaybePresent` answer — that is the whole reason the synopsis exists
    /// (#146). The probabilistic answer is the cheap fast path; pay the
    /// metadata-read cost only when you actually need the bytes.
    pub fn exists(&self, namespace: &str, key: &str) -> CachePresence {
        self.exists_at(namespace, key, unix_now_ms())
    }

    fn exists_at(&self, namespace: &str, key: &str, now_ms: u64) -> CachePresence {
        let cache_key = BlobCacheKey::new(namespace, key);
        let namespace_generation = self.current_generation(namespace);
        let shard_idx = self.shard_index(&cache_key);
        let mut shard = self.shards[shard_idx].write();
        match shard.contains(&cache_key, now_ms, namespace_generation) {
            Lookup::Present => {
                self.stats.hits.fetch_add(1, Ordering::Relaxed);
                CachePresence::Present
            }
            Lookup::Expired(entry) => {
                drop(shard);
                self.record_removed_entry(&cache_key, &entry);
                if let Some(l2) = &self.l2 {
                    l2.delete_key(&cache_key);
                }
                self.stats.expirations.fetch_add(1, Ordering::Relaxed);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                CachePresence::Absent
            }
            Lookup::IdleEvicted(entry) => {
                drop(shard);
                self.record_removed_entry(&cache_key, &entry);
                if let Some(l2) = &self.l2 {
                    l2.delete_key(&cache_key);
                }
                self.stats.expirations.fetch_add(1, Ordering::Relaxed);
                self.stats.l1_idle_evicts.fetch_add(1, Ordering::Relaxed);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                CachePresence::Absent
            }
            Lookup::Stale(entry) => {
                drop(shard);
                self.record_removed_entry(&cache_key, &entry);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                CachePresence::Absent
            }
            Lookup::Miss => {
                drop(shard);
                let Some(l2) = self.l2.as_ref() else {
                    self.stats.misses.fetch_add(1, Ordering::Relaxed);
                    return CachePresence::Absent;
                };
                if l2.synopsis_may_contain(namespace, key) {
                    // Filter says maybe — the cheap fast path defers the
                    // authoritative read to `get`. Count as a hit prospect.
                    self.stats.hits.fetch_add(1, Ordering::Relaxed);
                    CachePresence::MaybePresent
                } else {
                    // Filter says no — definitively absent (no
                    // false-negatives).
                    self.stats.misses.fetch_add(1, Ordering::Relaxed);
                    CachePresence::Absent
                }
            }
            Lookup::Hit(_) => unreachable!("exists cannot return a hit payload"),
        }
    }

    /// Node-local invalidation for one exact cache key.
    ///
    /// This does not propagate to replicas. Cluster-wide invalidation is a
    /// future contract; callers that need cross-node coherence must rely on the
    /// underlying write reaching each node and triggering local eviction there.
    pub fn invalidate_key(&self, namespace: &str, key: &str) -> usize {
        if !self.namespace_exists(namespace) {
            return 0;
        }
        let cache_key = BlobCacheKey::new(namespace, key);
        let shard_idx = self.shard_index(&cache_key);
        let mut shard = self.shards[shard_idx].write();
        let removed = shard.remove(&cache_key);
        drop(shard);

        if let Some(entry) = removed {
            self.record_invalidated_entry(&cache_key, &entry);
            1
        } else {
            self.l2
                .as_ref()
                .and_then(|l2| l2.delete_key(&cache_key))
                .map(|_| {
                    self.stats.invalidations.fetch_add(1, Ordering::Relaxed);
                    1
                })
                .unwrap_or(0)
        }
    }

    /// Node-local invalidation for keys with a namespace-local prefix.
    pub fn invalidate_prefix(&self, namespace: &str, prefix: &str) -> usize {
        if !self.namespace_exists(namespace) {
            return 0;
        }

        let mut removed = Vec::new();
        for shard in &self.shards {
            let mut shard = shard.write();
            let keys = shard
                .entries
                .keys()
                .filter(|key| key.namespace == namespace && key.key.starts_with(prefix))
                .cloned()
                .collect::<Vec<_>>();
            for key in keys {
                if let Some(entry) = shard.remove(&key) {
                    removed.push((key, entry));
                }
            }
        }

        let count = removed.len();
        for (key, entry) in removed {
            self.record_invalidated_entry(&key, &entry);
        }
        let l2_count = self
            .l2
            .as_ref()
            .map_or(0, |l2| l2.delete_prefix(namespace, prefix));
        if l2_count > count {
            self.stats
                .invalidations
                .fetch_add((l2_count - count) as u64, Ordering::Relaxed);
        }
        count.max(l2_count)
    }

    /// Node-local batched invalidation for all entries carrying any of `tags`.
    ///
    /// Locks each affected shard once per call, so a batched invalidation
    /// from a downstream adapter (#143) does not multiply lock acquisitions
    /// the way N singular calls would.
    pub fn invalidate_tags(&self, namespace: &str, tags: &[&str]) -> usize {
        self.invalidate_indexed_many(namespace, tags, IndexedKind::Tag)
    }

    /// Node-local batched invalidation for all entries carrying any of `dependencies`.
    pub fn invalidate_dependencies(&self, namespace: &str, dependencies: &[&str]) -> usize {
        self.invalidate_indexed_many(namespace, dependencies, IndexedKind::Dependency)
    }

    /// Node-local invalidation for all entries carrying `tag`.
    #[deprecated(
        since = "0.1.0",
        note = "use `invalidate_tags(namespace, &[tag])` for batched callers"
    )]
    pub fn invalidate_tag(&self, namespace: &str, tag: &str) -> usize {
        self.invalidate_indexed_many(namespace, &[tag], IndexedKind::Tag)
    }

    /// Node-local invalidation for all entries carrying `dependency`.
    #[deprecated(
        since = "0.1.0",
        note = "use `invalidate_dependencies(namespace, &[dependency])` for batched callers"
    )]
    pub fn invalidate_dependency(&self, namespace: &str, dependency: &str) -> usize {
        self.invalidate_indexed_many(namespace, &[dependency], IndexedKind::Dependency)
    }

    /// O(1) foreground namespace flush.
    ///
    /// The foreground path only bumps a namespace generation. Old entries become
    /// invisible immediately and are physically removed by later cache access or
    /// a future sweeper.
    pub fn invalidate_namespace(&self, namespace: &str) -> bool {
        if !self.namespace_exists(namespace) {
            return false;
        }
        let mut generations = self.namespace_generations.write();
        let generation = generations.entry(namespace.to_string()).or_insert(0);
        *generation = generation.saturating_add(1);
        if let Some(l2) = &self.l2 {
            l2.delete_namespace(namespace);
        }
        self.stats.namespace_flushes.fetch_add(1, Ordering::Relaxed);
        true
    }

    pub fn stats(&self) -> BlobCacheStats {
        BlobCacheStats {
            hits: self.stats.hits.load(Ordering::Relaxed),
            misses: self.stats.misses.load(Ordering::Relaxed),
            insertions: self.stats.insertions.load(Ordering::Relaxed),
            evictions: self.stats.evictions.load(Ordering::Relaxed),
            expirations: self.stats.expirations.load(Ordering::Relaxed),
            invalidations: self.stats.invalidations.load(Ordering::Relaxed),
            namespace_flushes: self.stats.namespace_flushes.load(Ordering::Relaxed),
            version_mismatches: self.stats.version_mismatches.load(Ordering::Relaxed),
            entries: self.shards.iter().map(|shard| shard.read().len()).sum(),
            bytes_in_use: self.bytes_in_use.load(Ordering::Relaxed),
            l1_bytes_max: self.config.l1_bytes_max,
            l2_bytes_in_use: self.l2.as_ref().map_or(0, |l2| l2.stats_bytes_in_use()),
            l2_bytes_max: self.config.l2_bytes_max,
            l2_full_rejections: self.stats.l2_full_rejections.load(Ordering::Relaxed),
            l2_metadata_reads: self.l2.as_ref().map_or(0, |l2| l2.stats_metadata_reads()),
            l2_negative_skips: self.l2.as_ref().map_or(0, |l2| l2.stats_negative_skips()),
            synopsis_metadata_reads: self
                .l2
                .as_ref()
                .map_or(0, |l2| l2.stats_synopsis_metadata_reads()),
            synopsis_bytes: self.l2.as_ref().map_or(0, |l2| l2.stats_synopsis_bytes()),
            namespaces: self.namespaces.read().len(),
            max_namespaces: self.config.max_namespaces,
            promotion_queued: self
                .promotion_pool
                .get()
                .map_or(0, |p| p.metrics().queued_total),
            promotion_dropped: self
                .promotion_pool
                .get()
                .map_or(0, |p| p.metrics().dropped_total),
            promotion_completed: self
                .promotion_pool
                .get()
                .map_or(0, |p| p.metrics().completed_total),
            promotion_queue_depth: self
                .promotion_pool
                .get()
                .map_or(0, |p| p.metrics().queue_depth),
            l2_compression_original_bytes: self
                .l2
                .as_ref()
                .map_or(0, |l2| l2.stats_compression_original_bytes()),
            l2_compression_stored_bytes: self
                .l2
                .as_ref()
                .map_or(0, |l2| l2.stats_compression_stored_bytes()),
            l2_compression_skipped_total: self
                .l2
                .as_ref()
                .map_or(0, |l2| l2.stats_compression_skipped_total()),
            l2_bytes_saved_total: self
                .l2
                .as_ref()
                .map_or(0, |l2| l2.stats_bytes_saved_total()),
            l1_stale_serves_total: self.stats.l1_stale_serves.load(Ordering::Relaxed),
            l1_idle_evicts_total: self.stats.l1_idle_evicts.load(Ordering::Relaxed),
        }
    }

    // -- Async promotion (issue #193) ---------------------------------------

    /// Initialize the async L2->L1 promotion pool. Must be called on an
    /// `Arc<Self>` so the executor closure can hold a `Weak<Self>` (no
    /// reference cycle).
    ///
    /// Idempotent on first call only — `OnceLock` semantics: a second call
    /// returns the previously-installed pool unchanged. The returned `Arc`
    /// can be used by callers that want to inspect metrics directly; most
    /// callers should ignore it and read metrics via `stats()`.
    pub fn enable_async_promotion(self: &Arc<Self>, opts: PoolOpts) -> Arc<AsyncPromotionPool> {
        let weak: Weak<Self> = Arc::downgrade(self);
        let executor: PromotionExecutor = Arc::new(move |req| {
            // Upgrade only at execution time. If the cache has been
            // dropped, the worker silently no-ops (executor never holds
            // a strong ref between calls).
            let Some(cache) = weak.upgrade() else {
                return Ok(());
            };
            cache.promote_from_l2(&req)
        });
        let pool = AsyncPromotionPool::new_with_executor(opts, executor);
        match self.promotion_pool.set(Arc::clone(&pool)) {
            Ok(()) => pool,
            // Race: another caller already initialized. Drain ours and
            // return the winner. The losing pool's workers are spawned;
            // shutdown drains them out gracefully.
            Err(losing_pool) => {
                losing_pool.shutdown();
                Arc::clone(
                    self.promotion_pool
                        .get()
                        .expect("OnceLock set+get inconsistency"),
                )
            }
        }
    }

    /// Drain and stop the async promotion pool, if enabled. Safe to call
    /// from `Drop` impls / test teardown — no-op when the pool was never
    /// initialized.
    pub fn shutdown_async_promotion(&self) {
        if let Some(pool) = self.promotion_pool.get() {
            Arc::clone(pool).shutdown();
        }
    }

    /// Test-only escape hatch: schedule outcome of the most recent attempt
    /// is internal; tests assert on `stats()` counters instead.
    #[cfg(test)]
    fn promotion_pool_handle(&self) -> Option<Arc<AsyncPromotionPool>> {
        self.promotion_pool.get().cloned()
    }

    /// Test-only: install a custom executor (e.g. one that sleeps to
    /// expose the hot-path / worker-path latency split). Used by the
    /// async-promotion wiring tests in this file.
    #[cfg(test)]
    fn enable_async_promotion_with_executor(
        self: &Arc<Self>,
        opts: PoolOpts,
        executor: PromotionExecutor,
    ) -> Arc<AsyncPromotionPool> {
        let pool = AsyncPromotionPool::new_with_executor(opts, executor);
        let _ = self.promotion_pool.set(Arc::clone(&pool));
        pool
    }

    pub fn config(&self) -> &BlobCacheConfig {
        &self.config
    }

    #[cfg(test)]
    fn inject_l2_fault_after_blob_write_once(&self) {
        self.l2
            .as_ref()
            .expect("L2 enabled")
            .inject_fault_after_blob_write_once();
    }

    #[cfg(test)]
    fn inject_l2_synopsis_maybe_present(&self, namespace: &str, key: &str) {
        self.l2
            .as_ref()
            .expect("L2 enabled")
            .inject_synopsis_maybe_present(namespace, key);
    }

    /// Test-only escape hatch (#192 lane 2/5): synthesise a legacy
    /// `V1Raw` L2 entry on disk so the forward-compat read test can
    /// verify pre-compression entries still rehydrate.
    #[cfg(test)]
    fn inject_l2_v1_entry(
        &self,
        namespace: &str,
        key: &str,
        payload: &[u8],
    ) -> Result<(), CacheError> {
        let l2 = self.l2.as_ref().expect("L2 enabled");
        let cache_key = BlobCacheKey::new(namespace, key);
        l2.inject_v1_entry(&cache_key, payload)
    }

    fn validate_blob_size(&self, size: usize, policy: BlobCachePolicy) -> Result<(), CacheError> {
        let max = policy
            .max_blob_bytes_value()
            .unwrap_or(self.config.l1_bytes_max);
        if size > max {
            Err(CacheError::BlobTooLarge { size, max })
        } else {
            Ok(())
        }
    }

    fn validate_metadata(&self, metadata: &BTreeMap<String, String>) -> Result<(), CacheError> {
        let keys = metadata.len();
        let bytes = metadata
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>();
        if keys > self.config.content_metadata_keys_max
            || bytes > self.config.content_metadata_bytes_max
        {
            Err(CacheError::MetadataTooLarge {
                keys,
                bytes,
                max_keys: self.config.content_metadata_keys_max,
                max_bytes: self.config.content_metadata_bytes_max,
            })
        } else {
            Ok(())
        }
    }

    fn rehydrate_l2_entry(
        &self,
        key: &BlobCacheKey,
        now_ms: u64,
        namespace_generation: u64,
        shard_idx: usize,
    ) -> Option<BlobCacheHit> {
        let l2 = self.l2.as_ref()?;
        let entry = l2.get(key, now_ms, namespace_generation)?;
        let hit = entry.hit();
        self.do_l1_promotion_sync(key, entry, shard_idx);
        Some(hit)
    }

    /// Pure L1 install bookkeeping: shard write-lock, byte accounting,
    /// eviction loop. Extracted so the async promotion pool can call it
    /// from a worker (issue #193, lane 1/5).
    ///
    /// This is intentionally side-effect-only — it does not touch hit/miss
    /// stats (the caller already counted the hit) and does not return the
    /// `BlobCacheHit` (the caller already handed bytes to the user).
    fn do_l1_promotion_sync(&self, key: &BlobCacheKey, entry: Entry, shard_idx: usize) {
        let entry_size = entry.size;
        let mut shard = self.shards[shard_idx].write();
        let outcome = shard.insert(key.clone(), entry);
        drop(shard);
        let old_size = outcome.old_entry.as_ref().map_or(0, |entry| entry.size);
        if entry_size >= old_size {
            self.bytes_in_use
                .fetch_add(entry_size - old_size, Ordering::Relaxed);
        } else {
            self.bytes_in_use
                .fetch_sub(old_size - entry_size, Ordering::Relaxed);
        }
        self.evict_until_within_budget(shard_idx);
    }

    /// Worker-side promotion path: re-fetch the entry from L2 and run the
    /// L1 install bookkeeping. Idempotent — re-promoting a key that the
    /// hot path already promoted (race with another reader) is harmless.
    /// Returns `Err` only when L2 is unavailable or the key is no longer
    /// present at L2 (silently treated as a no-op upstream).
    fn promote_from_l2(&self, req: &PromotionRequest) -> Result<(), String> {
        let l2 = self
            .l2
            .as_ref()
            .ok_or_else(|| "promotion executor invoked without L2 configured".to_string())?;
        let cache_key = BlobCacheKey::new(req.namespace.as_str(), req.key.as_str());
        let now_ms = unix_now_ms();
        let namespace_generation = self.current_generation(req.namespace.as_str());
        if let Some(entry) = l2.get(&cache_key, now_ms, namespace_generation) {
            let shard_idx = self.shard_index(&cache_key);
            self.do_l1_promotion_sync(&cache_key, entry, shard_idx);
        }
        Ok(())
    }

    fn ensure_namespace(&self, namespace: &str) -> Result<(), CacheError> {
        {
            let namespaces = self.namespaces.read();
            if namespaces.contains(namespace) {
                return Ok(());
            }
        }
        let mut namespaces = self.namespaces.write();
        if namespaces.contains(namespace) {
            return Ok(());
        }
        if namespaces.len() >= self.config.max_namespaces {
            return Err(CacheError::TooManyNamespaces {
                max: self.config.max_namespaces,
            });
        }
        namespaces.insert(namespace.to_string());
        self.namespace_generations
            .write()
            .entry(namespace.to_string())
            .or_insert(0);
        Ok(())
    }

    fn namespace_exists(&self, namespace: &str) -> bool {
        self.namespaces.read().contains(namespace)
            || self
                .l2
                .as_ref()
                .is_some_and(|l2| l2.has_namespace(namespace))
    }

    fn current_generation(&self, namespace: &str) -> u64 {
        self.namespace_generations
            .read()
            .get(namespace)
            .copied()
            .unwrap_or(0)
    }

    fn index_entry(
        &self,
        key: &BlobCacheKey,
        tags: &BTreeSet<String>,
        dependencies: &BTreeSet<String>,
    ) {
        if !tags.is_empty() {
            let mut index = self.tag_index.write();
            for tag in tags {
                index
                    .entry(ScopedLabel::new(key.namespace.as_str(), tag.as_str()))
                    .or_default()
                    .insert(key.clone());
            }
        }
        if !dependencies.is_empty() {
            let mut index = self.dependency_index.write();
            for dependency in dependencies {
                index
                    .entry(ScopedLabel::new(
                        key.namespace.as_str(),
                        dependency.as_str(),
                    ))
                    .or_default()
                    .insert(key.clone());
            }
        }
    }

    fn deindex_entry(&self, key: &BlobCacheKey, entry: &Entry) {
        Self::remove_indexed_labels(&self.tag_index, key, &entry.tags);
        Self::remove_indexed_labels(&self.dependency_index, key, &entry.dependencies);
    }

    fn remove_indexed_labels(
        index: &RwLock<HashMap<ScopedLabel, HashSet<BlobCacheKey>>>,
        key: &BlobCacheKey,
        labels: &BTreeSet<String>,
    ) {
        if labels.is_empty() {
            return;
        }
        let mut index = index.write();
        for label in labels {
            let scoped = ScopedLabel::new(key.namespace.as_str(), label.as_str());
            let should_remove = if let Some(keys) = index.get_mut(&scoped) {
                keys.remove(key);
                keys.is_empty()
            } else {
                false
            };
            if should_remove {
                index.remove(&scoped);
            }
        }
    }

    fn record_removed_entry(&self, key: &BlobCacheKey, entry: &Entry) {
        self.bytes_in_use.fetch_sub(entry.size, Ordering::Relaxed);
        self.deindex_entry(key, entry);
    }

    fn record_invalidated_entry(&self, key: &BlobCacheKey, entry: &Entry) {
        self.record_removed_entry(key, entry);
        if let Some(l2) = &self.l2 {
            l2.delete_key(key);
        }
        self.stats.invalidations.fetch_add(1, Ordering::Relaxed);
    }

    fn invalidate_indexed_many(
        &self,
        namespace: &str,
        labels: &[&str],
        kind: IndexedKind,
    ) -> usize {
        if labels.is_empty() || !self.namespace_exists(namespace) {
            return 0;
        }

        // Snapshot the candidate keys for every label up front so the
        // shard-locking pass below sees a stable set. We deduplicate by
        // BlobCacheKey so a key tagged with multiple invalidated labels is
        // still removed (and counted) exactly once.
        let mut candidates: HashMap<BlobCacheKey, HashSet<String>> = HashMap::new();
        {
            let index = match kind {
                IndexedKind::Tag => self.tag_index.read(),
                IndexedKind::Dependency => self.dependency_index.read(),
            };
            for label in labels {
                let scoped = ScopedLabel::new(namespace, *label);
                if let Some(keys) = index.get(&scoped) {
                    for key in keys {
                        candidates
                            .entry(key.clone())
                            .or_default()
                            .insert((*label).to_string());
                    }
                }
            }
        }

        if candidates.is_empty() {
            return 0;
        }

        // Group candidates by shard so each shard lock is taken at most
        // once per call.
        let mut by_shard: HashMap<usize, Vec<(BlobCacheKey, HashSet<String>)>> = HashMap::new();
        for (key, matched_labels) in candidates {
            let shard_idx = self.shard_index(&key);
            by_shard
                .entry(shard_idx)
                .or_default()
                .push((key, matched_labels));
        }

        let mut removed = Vec::new();
        for (shard_idx, keys) in by_shard {
            let mut shard = self.shards[shard_idx].write();
            for (key, matched_labels) in keys {
                let still_matches = shard.entries.get(&key).is_some_and(|entry| match kind {
                    IndexedKind::Tag => matched_labels.iter().any(|l| entry.tags.contains(l)),
                    IndexedKind::Dependency => matched_labels
                        .iter()
                        .any(|l| entry.dependencies.contains(l)),
                });
                if still_matches {
                    if let Some(entry) = shard.remove(&key) {
                        removed.push((key, entry));
                    }
                }
            }
        }

        let count = removed.len();
        for (key, entry) in removed {
            self.record_invalidated_entry(&key, &entry);
        }
        count
    }

    fn shard_index(&self, key: &BlobCacheKey) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        (hasher.finish() as usize) % self.shards.len()
    }

    fn check_version(
        &self,
        shard: &Shard,
        key: &BlobCacheKey,
        attempted: Option<u64>,
        namespace_generation: u64,
    ) -> Result<(), CacheError> {
        let Some(attempted) = attempted else {
            return Ok(());
        };
        let Some(existing) = shard.existing_version(key, namespace_generation) else {
            return Ok(());
        };
        if existing >= attempted {
            self.stats
                .version_mismatches
                .fetch_add(1, Ordering::Relaxed);
            Err(CacheError::VersionMismatch {
                existing,
                attempted,
            })
        } else {
            Ok(())
        }
    }

    fn evict_until_within_budget(&self, preferred_start: usize) {
        while self.bytes_in_use.load(Ordering::Relaxed) > self.config.l1_bytes_max {
            let mut evicted = false;
            for offset in 0..self.shards.len() {
                let idx = (preferred_start + offset) % self.shards.len();
                let mut shard = self.shards[idx].write();
                if let Some((key, entry)) = shard.evict_one() {
                    self.bytes_in_use.fetch_sub(entry.size, Ordering::Relaxed);
                    self.stats.evictions.fetch_add(1, Ordering::Relaxed);
                    evicted = true;
                    drop(shard);
                    self.deindex_entry(&key, &entry);
                    break;
                }
            }
            if !evicted {
                break;
            }
        }
    }
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

impl Default for BlobCache {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_cache(bytes: usize) -> BlobCache {
        BlobCache::new(
            BlobCacheConfig::default()
                .with_l1_bytes_max(bytes)
                .with_shard_count(1)
                .with_max_namespaces(4),
        )
    }

    fn l2_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "reddb-blob-cache-{name}-{}-{nanos}.rdb",
            std::process::id()
        ))
    }

    fn l2_cache(path: &Path) -> BlobCache {
        BlobCache::open_with_l2(
            BlobCacheConfig::default()
                .with_l1_bytes_max(128)
                .with_shard_count(1)
                .with_max_namespaces(4)
                .with_l2_path(path),
        )
        .expect("l2_cache test helper")
    }

    #[test]
    fn put_get_and_exists_round_trip_blob() {
        let cache = small_cache(128);
        cache
            .put("images", "hero", BlobCachePut::new(vec![1, 2, 3]))
            .expect("put");

        assert_eq!(cache.exists("images", "hero"), CachePresence::Present);
        let hit = cache.get("images", "hero").expect("hit");
        assert_eq!(&*hit.bytes, &[1, 2, 3]);

        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 0);
        assert_eq!(stats.insertions, 1);
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.bytes_in_use, 3);
        assert_eq!(stats.l1_bytes_max, 128);
    }

    #[test]
    fn missing_key_updates_miss_counter() {
        let cache = small_cache(128);
        assert!(cache.get("images", "missing").is_none());
        assert_eq!(cache.exists("images", "missing"), CachePresence::Absent);
        let stats = cache.stats();
        assert_eq!(stats.hits, 0);
        assert_eq!(stats.misses, 2);
    }

    #[test]
    fn namespace_isolation_keeps_same_key_separate() {
        let cache = small_cache(128);
        cache
            .put("a", "same", BlobCachePut::new(b"a".to_vec()))
            .unwrap();
        cache
            .put("b", "same", BlobCachePut::new(b"b".to_vec()))
            .unwrap();

        assert_eq!(&*cache.get("a", "same").unwrap().bytes, b"a");
        assert_eq!(&*cache.get("b", "same").unwrap().bytes, b"b");
        assert_eq!(cache.stats().namespaces, 2);
    }

    #[test]
    fn byte_capacity_evicts_with_sieve() {
        let cache = small_cache(6);
        cache
            .put("n", "a", BlobCachePut::new(vec![1, 1, 1]))
            .unwrap();
        cache
            .put("n", "b", BlobCachePut::new(vec![2, 2, 2]))
            .unwrap();
        let _ = cache.get("n", "a");
        cache
            .put("n", "c", BlobCachePut::new(vec![3, 3, 3]))
            .unwrap();

        assert!(cache.get("n", "c").is_some(), "new entry remains cached");
        let stats = cache.stats();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.bytes_in_use, 6);
        assert!(stats.evictions >= 1);
    }

    #[test]
    fn namespace_cap_rejects_new_namespace() {
        let cache = BlobCache::new(
            BlobCacheConfig::default()
                .with_l1_bytes_max(128)
                .with_shard_count(1)
                .with_max_namespaces(1),
        );
        cache.put("a", "k", BlobCachePut::new(vec![1])).unwrap();
        let err = cache
            .put("b", "k", BlobCachePut::new(vec![1]))
            .expect_err("second namespace rejected");
        assert_eq!(err, CacheError::TooManyNamespaces { max: 1 });
    }

    #[test]
    fn content_metadata_round_trips_and_is_capped() {
        let cache = BlobCache::new(
            BlobCacheConfig::default()
                .with_l1_bytes_max(128)
                .with_shard_count(1)
                .with_content_metadata_limits(2, 64),
        );
        let metadata = BTreeMap::from([
            ("content-type".to_string(), "text/plain".to_string()),
            ("etag".to_string(), "v1".to_string()),
        ]);
        cache
            .put(
                "http",
                "home",
                BlobCachePut::new(b"ok".to_vec()).with_content_metadata(metadata.clone()),
            )
            .unwrap();
        assert_eq!(
            cache.get("http", "home").unwrap().content_metadata,
            metadata
        );

        let too_many = BTreeMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
            ("c".to_string(), "3".to_string()),
        ]);
        let err = cache
            .put(
                "http",
                "too_many",
                BlobCachePut::new(b"ok".to_vec()).with_content_metadata(too_many),
            )
            .expect_err("too many metadata keys");
        assert!(matches!(err, CacheError::MetadataTooLarge { .. }));

        let too_large = BTreeMap::from([("long".to_string(), "x".repeat(64))]);
        let err = cache
            .put(
                "http",
                "too_large",
                BlobCachePut::new(b"ok".to_vec()).with_content_metadata(too_large),
            )
            .expect_err("metadata bytes too large");
        assert!(matches!(err, CacheError::MetadataTooLarge { .. }));
    }

    #[test]
    fn blob_larger_than_l1_budget_is_rejected() {
        let cache = small_cache(4);
        let err = cache
            .put("n", "large", BlobCachePut::new(vec![0; 5]))
            .expect_err("blob too large");
        assert_eq!(err, CacheError::BlobTooLarge { size: 5, max: 4 });
    }

    #[test]
    fn hard_ttl_expires_entries_on_get_and_exists() {
        let cache = small_cache(128);
        let policy = BlobCachePolicy::default().ttl_ms(10);
        cache
            .put_at(
                "n",
                "ttl",
                BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
                1_000,
            )
            .unwrap();

        assert!(cache.get_at("n", "ttl", 1_009).is_some());
        assert!(cache.get_at("n", "ttl", 1_010).is_none());
        assert_eq!(cache.exists_at("n", "ttl", 1_011), CachePresence::Absent);

        let stats = cache.stats();
        assert_eq!(stats.expirations, 1);
        assert_eq!(stats.misses, 2);
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.bytes_in_use, 0);
    }

    #[test]
    fn absolute_expiry_is_hard_boundary() {
        let cache = small_cache(128);
        let policy = BlobCachePolicy::default().expires_at_unix_ms(500);
        cache
            .put_at(
                "n",
                "abs",
                BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
                100,
            )
            .unwrap();

        assert!(cache.get_at("n", "abs", 499).is_some());
        assert!(cache.get_at("n", "abs", 500).is_none());
        assert_eq!(cache.stats().expirations, 1);
    }

    #[test]
    fn ttl_and_absolute_expiry_use_earliest_deadline() {
        let cache = small_cache(128);
        let policy = BlobCachePolicy::default()
            .ttl_ms(100)
            .expires_at_unix_ms(1_050);
        cache
            .put_at(
                "n",
                "earliest",
                BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
                1_000,
            )
            .unwrap();

        assert!(cache.get_at("n", "earliest", 1_049).is_some());
        assert!(cache.get_at("n", "earliest", 1_050).is_none());
    }

    #[test]
    fn per_entry_max_blob_bytes_rejects_large_blob() {
        let cache = small_cache(128);
        let policy = BlobCachePolicy::default().max_blob_bytes(2);
        let err = cache
            .put(
                "n",
                "large",
                BlobCachePut::new(vec![1, 2, 3]).with_policy(policy),
            )
            .expect_err("per-entry cap rejects blob");

        assert_eq!(err, CacheError::BlobTooLarge { size: 3, max: 2 });
        assert_eq!(cache.stats().insertions, 0);
    }

    #[test]
    fn l1_admission_never_accepts_put_without_storing_l1_entry() {
        let cache = small_cache(128);
        let policy = BlobCachePolicy::default().l1_admission(L1Admission::Never);
        cache
            .put(
                "n",
                "skip",
                BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
            )
            .unwrap();

        assert!(cache.get("n", "skip").is_none());
        let stats = cache.stats();
        assert_eq!(stats.insertions, 1);
        assert_eq!(stats.entries, 0);
        assert_eq!(stats.bytes_in_use, 0);
    }

    #[test]
    fn l1_admission_always_and_auto_store_entries() {
        let cache = small_cache(128);
        cache
            .put(
                "n",
                "always",
                BlobCachePut::new(b"a".to_vec())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Always)),
            )
            .unwrap();
        cache
            .put(
                "n",
                "auto",
                BlobCachePut::new(b"b".to_vec())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Auto)),
            )
            .unwrap();

        assert_eq!(&*cache.get("n", "always").unwrap().bytes, b"a");
        assert_eq!(&*cache.get("n", "auto").unwrap().bytes, b"b");
    }

    #[test]
    fn priority_biases_sieve_eviction_toward_lower_priority_entries() {
        let cache = small_cache(6);
        cache
            .put(
                "n",
                "low",
                BlobCachePut::new(vec![1, 1, 1])
                    .with_policy(BlobCachePolicy::default().priority(1)),
            )
            .unwrap();
        cache
            .put(
                "n",
                "high",
                BlobCachePut::new(vec![2, 2, 2])
                    .with_policy(BlobCachePolicy::default().priority(250)),
            )
            .unwrap();
        cache
            .put("n", "new", BlobCachePut::new(vec![3, 3, 3]))
            .unwrap();

        assert!(cache.get("n", "high").is_some());
        assert!(cache.get("n", "low").is_none());
        let stats = cache.stats();
        assert_eq!(stats.entries, 2);
        assert_eq!(stats.bytes_in_use, 6);
        assert!(stats.evictions >= 1);
    }

    #[test]
    fn cas_version_must_increase_to_mutate_existing_entry() {
        let cache = small_cache(128);
        cache
            .put(
                "n",
                "cas",
                BlobCachePut::new(b"v1".to_vec())
                    .with_policy(BlobCachePolicy::default().version(1)),
            )
            .unwrap();
        cache
            .put(
                "n",
                "cas",
                BlobCachePut::new(b"v2".to_vec())
                    .with_policy(BlobCachePolicy::default().version(2)),
            )
            .unwrap();

        let hit = cache.get("n", "cas").unwrap();
        assert_eq!(&*hit.bytes, b"v2");
        assert_eq!(hit.version, Some(2));
    }

    #[test]
    fn cas_equal_or_lower_version_rejects_without_mutating_or_counting_insert() {
        let cache = small_cache(128);
        cache
            .put(
                "n",
                "cas",
                BlobCachePut::new(b"v2".to_vec())
                    .with_policy(BlobCachePolicy::default().version(2)),
            )
            .unwrap();

        let equal = cache
            .put(
                "n",
                "cas",
                BlobCachePut::new(b"equal".to_vec())
                    .with_policy(BlobCachePolicy::default().version(2)),
            )
            .expect_err("equal version rejected");
        assert_eq!(
            equal,
            CacheError::VersionMismatch {
                existing: 2,
                attempted: 2,
            }
        );

        let lower = cache
            .put(
                "n",
                "cas",
                BlobCachePut::new(b"lower".to_vec())
                    .with_policy(BlobCachePolicy::default().version(1)),
            )
            .expect_err("lower version rejected");
        assert_eq!(
            lower,
            CacheError::VersionMismatch {
                existing: 2,
                attempted: 1,
            }
        );

        let hit = cache.get("n", "cas").unwrap();
        assert_eq!(&*hit.bytes, b"v2");
        assert_eq!(hit.version, Some(2));
        let stats = cache.stats();
        assert_eq!(stats.insertions, 1);
        assert_eq!(stats.version_mismatches, 2);
    }

    #[test]
    fn cas_missing_key_with_version_succeeds() {
        let cache = small_cache(128);
        cache
            .put(
                "n",
                "missing",
                BlobCachePut::new(b"v7".to_vec())
                    .with_policy(BlobCachePolicy::default().version(7)),
            )
            .unwrap();

        let hit = cache.get("n", "missing").unwrap();
        assert_eq!(&*hit.bytes, b"v7");
        assert_eq!(hit.version, Some(7));
    }

    #[test]
    fn put_without_version_overwrites_unconditionally() {
        let cache = small_cache(128);
        cache
            .put(
                "n",
                "cas",
                BlobCachePut::new(b"v9".to_vec())
                    .with_policy(BlobCachePolicy::default().version(9)),
            )
            .unwrap();
        cache
            .put("n", "cas", BlobCachePut::new(b"plain".to_vec()))
            .unwrap();

        let hit = cache.get("n", "cas").unwrap();
        assert_eq!(&*hit.bytes, b"plain");
        assert_eq!(hit.version, None);
    }

    #[test]
    fn invalidate_key_removes_one_entry_and_is_idempotent() {
        let cache = small_cache(128);
        cache
            .put("n", "a", BlobCachePut::new(b"a".to_vec()))
            .unwrap();
        cache
            .put("n", "b", BlobCachePut::new(b"b".to_vec()))
            .unwrap();

        assert_eq!(cache.invalidate_key("n", "a"), 1);
        assert_eq!(cache.invalidate_key("n", "a"), 0);
        assert!(cache.get("n", "a").is_none());
        assert_eq!(&*cache.get("n", "b").unwrap().bytes, b"b");

        let stats = cache.stats();
        assert_eq!(stats.invalidations, 1);
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.bytes_in_use, 1);
    }

    #[test]
    fn invalidate_prefix_removes_matching_namespace_keys_only() {
        let cache = small_cache(128);
        cache
            .put("n", "user:1", BlobCachePut::new(b"1".to_vec()))
            .unwrap();
        cache
            .put("n", "user:2", BlobCachePut::new(b"2".to_vec()))
            .unwrap();
        cache
            .put("n", "post:1", BlobCachePut::new(b"3".to_vec()))
            .unwrap();
        cache
            .put("other", "user:1", BlobCachePut::new(b"4".to_vec()))
            .unwrap();

        assert_eq!(cache.invalidate_prefix("n", "user:"), 2);
        assert!(cache.get("n", "user:1").is_none());
        assert!(cache.get("n", "user:2").is_none());
        assert!(cache.get("n", "post:1").is_some());
        assert!(cache.get("other", "user:1").is_some());
        assert_eq!(cache.stats().invalidations, 2);
    }

    #[test]
    fn invalidate_tag_and_dependency_use_indexes() {
        let cache = small_cache(128);
        cache
            .put(
                "n",
                "tagged",
                BlobCachePut::new(b"a".to_vec()).with_tags(["hot", "tenant:1"]),
            )
            .unwrap();
        cache
            .put(
                "n",
                "dependent",
                BlobCachePut::new(b"b".to_vec()).with_dependencies(["row:42"]),
            )
            .unwrap();
        cache
            .put("n", "plain", BlobCachePut::new(b"c".to_vec()))
            .unwrap();

        assert_eq!(cache.invalidate_tags("n", &["hot"]), 1);
        assert!(cache.get("n", "tagged").is_none());
        assert_eq!(cache.invalidate_dependencies("n", &["row:42"]), 1);
        assert!(cache.get("n", "dependent").is_none());
        assert!(cache.get("n", "plain").is_some());
        assert_eq!(cache.stats().invalidations, 2);
    }

    #[test]
    fn cold_invalidation_returns_without_stats_changes_when_no_namespace_or_label_can_match() {
        let cache = small_cache(128);
        cache
            .put(
                "n",
                "tagged",
                BlobCachePut::new(b"a".to_vec()).with_tags(["warm"]),
            )
            .unwrap();
        let before = cache.stats();

        assert_eq!(cache.invalidate_prefix("missing", "x"), 0);
        assert_eq!(cache.invalidate_tags("n", &["cold"]), 0);
        assert_eq!(cache.invalidate_dependencies("n", &["row:missing"]), 0);
        assert_eq!(cache.stats(), before);
    }

    #[test]
    fn namespace_flush_bumps_generation_and_old_entries_are_immediately_absent() {
        let cache = small_cache(128);
        cache
            .put("n", "a", BlobCachePut::new(b"a".to_vec()))
            .unwrap();
        cache
            .put("n", "b", BlobCachePut::new(b"b".to_vec()))
            .unwrap();
        assert_eq!(cache.stats().entries, 2);

        assert!(cache.invalidate_namespace("n"));
        let after_flush = cache.stats();
        assert_eq!(after_flush.namespace_flushes, 1);
        assert_eq!(after_flush.entries, 2, "foreground path does not sweep");

        assert!(cache.get("n", "a").is_none());
        assert_eq!(cache.exists("n", "b"), CachePresence::Absent);
        cache
            .put("n", "c", BlobCachePut::new(b"c".to_vec()))
            .unwrap();
        assert_eq!(&*cache.get("n", "c").unwrap().bytes, b"c");
    }

    #[test]
    fn namespace_flush_makes_prior_versions_irrelevant_for_subsequent_put() {
        let cache = small_cache(128);
        cache
            .put(
                "n",
                "cas",
                BlobCachePut::new(b"old".to_vec())
                    .with_policy(BlobCachePolicy::default().version(9)),
            )
            .unwrap();

        assert!(cache.invalidate_namespace("n"));
        cache
            .put(
                "n",
                "cas",
                BlobCachePut::new(b"new".to_vec())
                    .with_policy(BlobCachePolicy::default().version(1)),
            )
            .unwrap();

        let hit = cache.get("n", "cas").unwrap();
        assert_eq!(&*hit.bytes, b"new");
        assert_eq!(hit.version, Some(1));
        assert_eq!(cache.stats().version_mismatches, 0);
    }

    #[test]
    fn invalidation_is_node_local_for_mvp() {
        let primary = small_cache(128);
        let replica = small_cache(128);
        primary
            .put("n", "k", BlobCachePut::new(b"primary".to_vec()))
            .unwrap();
        replica
            .put("n", "k", BlobCachePut::new(b"replica".to_vec()))
            .unwrap();

        assert_eq!(primary.invalidate_key("n", "k"), 1);
        assert!(primary.get("n", "k").is_none());
        assert_eq!(&*replica.get("n", "k").unwrap().bytes, b"replica");
    }

    #[test]
    fn l2_rehydrates_after_reopen_without_json_rows() {
        let path = l2_path("reopen");
        {
            let cache = l2_cache(&path);
            cache
                .put(
                    "n",
                    "k",
                    BlobCachePut::new(b"durable".to_vec())
                        .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
                )
                .unwrap();
            assert!(cache.get("n", "k").is_some());
        }
        {
            let cache = l2_cache(&path);
            let hit = cache.get("n", "k").expect("rehydrates from L2");
            assert_eq!(&*hit.bytes, b"durable");
            assert_eq!(cache.stats().l2_bytes_in_use, 7);
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_expired_entry_does_not_rehydrate_on_reopen() {
        let path = l2_path("expired");
        {
            let cache = l2_cache(&path);
            cache
                .put_at(
                    "n",
                    "ttl",
                    BlobCachePut::new(b"old".to_vec())
                        .with_policy(BlobCachePolicy::default().ttl_ms(10)),
                    1_000,
                )
                .unwrap();
        }
        {
            let cache = l2_cache(&path);
            assert!(cache.get_at("n", "ttl", 1_010).is_none());
            assert_eq!(cache.stats().l2_bytes_in_use, 0);
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_invalidated_entry_does_not_resurrect_after_reopen() {
        let path = l2_path("invalidated");
        {
            let cache = l2_cache(&path);
            cache
                .put("n", "k", BlobCachePut::new(b"gone".to_vec()))
                .unwrap();
            assert_eq!(cache.invalidate_key("n", "k"), 1);
        }
        {
            let cache = l2_cache(&path);
            assert!(cache.get("n", "k").is_none());
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_rejects_put_when_hard_byte_cap_is_exceeded() {
        let path = l2_path("full");
        let cache = BlobCache::open_with_l2(
            BlobCacheConfig::default()
                .with_l1_bytes_max(128)
                .with_shard_count(1)
                .with_l2_bytes_max(2)
                .with_l2_path(&path),
        )
        .expect("open l2");
        let err = cache
            .put("n", "large", BlobCachePut::new(vec![1, 2, 3]))
            .expect_err("L2 cap rejects");
        assert_eq!(err, CacheError::L2Full { size: 3, max: 2 });
        assert_eq!(cache.stats().l2_full_rejections, 1);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_metadata_last_hides_partial_blob_after_fault() {
        let path = l2_path("fault");
        {
            let cache = l2_cache(&path);
            cache.inject_l2_fault_after_blob_write_once();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                cache
                    .put("n", "partial", BlobCachePut::new(b"partial".to_vec()))
                    .unwrap();
            }));
            assert!(result.is_err(), "fault hook should panic mid-write");
        }
        {
            let cache = l2_cache(&path);
            assert!(cache.get("n", "partial").is_none());
            assert_eq!(cache.stats().l2_bytes_in_use, 0);
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_synopsis_negative_skip_avoids_metadata_read() {
        let path = l2_path("synopsis-negative");
        let cache = l2_cache(&path);

        assert!(cache.get("n", "missing").is_none());
        let stats = cache.stats();
        assert_eq!(stats.l2_negative_skips, 1);
        assert_eq!(stats.l2_metadata_reads, 0);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_synopsis_maybe_present_verifies_authoritative_metadata() {
        let path = l2_path("synopsis-maybe");
        let cache = l2_cache(&path);
        cache.inject_l2_synopsis_maybe_present("n", "ghost");

        assert!(cache.get("n", "ghost").is_none());
        let stats = cache.stats();
        assert_eq!(stats.l2_negative_skips, 0);
        assert_eq!(stats.l2_metadata_reads, 1);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn stale_synopsis_bits_after_delete_cannot_produce_present() {
        let path = l2_path("synopsis-delete");
        let cache = l2_cache(&path);
        cache
            .put(
                "n",
                "deleted",
                BlobCachePut::new(b"gone".to_vec())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .unwrap();
        assert_eq!(cache.invalidate_key("n", "deleted"), 1);

        // Bloom filter cannot clear bits, so the key still hashes positive
        // — but `exists` must surface that ambiguity as `MaybePresent`, not a
        // false `Present`. The authoritative `get` then returns None and
        // bumps the synopsis-false-positive counter.
        assert_eq!(cache.exists("n", "deleted"), CachePresence::MaybePresent);
        assert!(cache.get("n", "deleted").is_none());
        let stats = cache.stats();
        assert_eq!(stats.l2_metadata_reads, 1);
        assert_eq!(stats.synopsis_metadata_reads, 1);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn stale_synopsis_bits_after_expiry_cannot_produce_present() {
        let path = l2_path("synopsis-expiry");
        let cache = l2_cache(&path);
        cache
            .put_at(
                "n",
                "expired",
                BlobCachePut::new(b"old".to_vec()).with_policy(
                    BlobCachePolicy::default()
                        .ttl_ms(10)
                        .l1_admission(L1Admission::Never),
                ),
                1_000,
            )
            .unwrap();

        // Filter still says maybe (bits cannot be cleared), so `exists`
        // returns MaybePresent. The authoritative `get` walks the metadata,
        // observes the expiry, and returns None.
        assert_eq!(
            cache.exists_at("n", "expired", 1_010),
            CachePresence::MaybePresent
        );
        assert!(cache.get_at("n", "expired", 1_010).is_none());
        let stats = cache.stats();
        assert_eq!(stats.l2_metadata_reads, 1);
        assert_eq!(stats.l2_bytes_in_use, 0);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_synopsis_rebuilds_from_metadata_on_reopen() {
        let path = l2_path("synopsis-rebuild");
        {
            let cache = l2_cache(&path);
            cache
                .put(
                    "n",
                    "known",
                    BlobCachePut::new(b"known".to_vec())
                        .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
                )
                .unwrap();
        }
        {
            let cache = l2_cache(&path);
            assert_eq!(&*cache.get("n", "known").unwrap().bytes, b"known");
            let stats = cache.stats();
            assert_eq!(stats.l2_negative_skips, 0);
            assert_eq!(stats.l2_metadata_reads, 1);
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn deleted_l2_entries_never_return_present_under_repeated_stale_synopsis() {
        let path = l2_path("synopsis-deleted-many");
        let cache = l2_cache(&path);
        for i in 0..1_000 {
            let key = format!("k{i}");
            cache
                .put(
                    "n",
                    &key,
                    BlobCachePut::new(vec![1])
                        .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
                )
                .unwrap();
            assert_eq!(cache.invalidate_key("n", &key), 1);
            // After delete the Bloom filter still has stale bits — exists
            // can answer MaybePresent or Absent depending on whether the
            // hash collides with a still-live key. The strict invariant is
            // that `get` (the authoritative path) NEVER returns Some for a
            // deleted key.
            assert!(matches!(
                cache.exists("n", &key),
                CachePresence::MaybePresent | CachePresence::Absent
            ));
            assert!(cache.get("n", &key).is_none());
        }
        // Each `get` of a deleted key with positive Bloom bits walks the
        // metadata and finds nothing; that's the false-positive cost. Filter
        // sizing (10K capacity / 1% FPR) means most lookups hit fast.
        assert_eq!(cache.stats().l2_metadata_reads, 1_000);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn metric_name_is_stable_for_observability_adapter() {
        assert_eq!(
            METRIC_CACHE_BLOB_L1_BYTES_IN_USE,
            "cache_blob_l1_bytes_in_use"
        );
        assert_eq!(
            METRIC_CACHE_VERSION_MISMATCH_TOTAL,
            "cache_version_mismatch_total"
        );
        assert_eq!(
            METRIC_CACHE_BLOB_L2_BYTES_IN_USE,
            "reddb_cache_blob_l2_bytes_in_use"
        );
        assert_eq!(
            METRIC_CACHE_BLOB_L2_FULL_REJECTIONS_TOTAL,
            "reddb_cache_blob_l2_full_rejections_total"
        );
        assert_eq!(
            METRIC_CACHE_BLOB_SYNOPSIS_METADATA_READS_TOTAL,
            "cache_blob_synopsis_metadata_reads_total"
        );
        assert_eq!(
            METRIC_CACHE_BLOB_SYNOPSIS_BYTES,
            "cache_blob_synopsis_bytes"
        );
    }

    // -- API review #151 follow-ups -----------------------------------------

    #[test]
    fn cache_presence_from_bool_round_trips_present_and_absent() {
        assert_eq!(CachePresence::from(true), CachePresence::Present);
        assert_eq!(CachePresence::from(false), CachePresence::Absent);
        // The `MaybePresent` variant is emitted by the L2 Bloom synopsis
        // (#146); the `From<bool>` adapter still maps the binary case
        // exactly so callers that have a definitive answer can lift it
        // without going through the synopsis.
        let _ = CachePresence::MaybePresent;
    }

    #[test]
    fn exists_returns_present_or_absent_today() {
        let cache = small_cache(128);
        cache
            .put("n", "k", BlobCachePut::new(b"v".to_vec()))
            .unwrap();

        assert_eq!(cache.exists("n", "k"), CachePresence::Present);
        assert_eq!(cache.exists("n", "missing"), CachePresence::Absent);
        assert_eq!(cache.exists("missing", "k"), CachePresence::Absent);
    }

    #[test]
    fn invalidate_tags_batched_call_removes_keys_from_multiple_labels() {
        let cache = small_cache(256);
        cache
            .put(
                "n",
                "a",
                BlobCachePut::new(b"a".to_vec()).with_tags(["red"]),
            )
            .unwrap();
        cache
            .put(
                "n",
                "b",
                BlobCachePut::new(b"b".to_vec()).with_tags(["green"]),
            )
            .unwrap();
        cache
            .put(
                "n",
                "c",
                BlobCachePut::new(b"c".to_vec()).with_tags(["blue"]),
            )
            .unwrap();

        // One batched call removes the two named tags but leaves "blue".
        assert_eq!(cache.invalidate_tags("n", &["red", "green"]), 2);
        assert!(cache.get("n", "a").is_none());
        assert!(cache.get("n", "b").is_none());
        assert!(cache.get("n", "c").is_some());
        assert_eq!(cache.stats().invalidations(), 2);
    }

    #[test]
    fn invalidate_dependencies_batched_call_dedups_multi_label_keys() {
        let cache = small_cache(256);
        cache
            .put(
                "n",
                "shared",
                BlobCachePut::new(b"x".to_vec()).with_dependencies(["row:1", "row:2"]),
            )
            .unwrap();

        // The same key matches both dependencies; the batched form must
        // count it once, not twice.
        assert_eq!(cache.invalidate_dependencies("n", &["row:1", "row:2"]), 1);
        assert!(cache.get("n", "shared").is_none());
    }

    #[test]
    fn invalidate_tags_with_empty_slice_is_a_no_op() {
        let cache = small_cache(128);
        cache
            .put("n", "a", BlobCachePut::new(b"a".to_vec()).with_tags(["x"]))
            .unwrap();
        assert_eq!(cache.invalidate_tags("n", &[]), 0);
        assert_eq!(cache.invalidate_dependencies("n", &[]), 0);
        assert!(cache.get("n", "a").is_some());
    }

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

    #[test]
    fn blob_cache_config_builder_constructs_cache_end_to_end() {
        let config = BlobCacheConfig::builder()
            .l1_bytes_max(64)
            .max_namespaces(2)
            .shard_count(1)
            .build();
        assert_eq!(config.l1_bytes_max(), 64);
        assert_eq!(config.max_namespaces(), 2);
        assert_eq!(config.shard_count(), 1);

        let cache = BlobCache::new(config);
        cache
            .put("n", "k", BlobCachePut::new(b"v".to_vec()))
            .unwrap();
        assert_eq!(cache.exists("n", "k"), CachePresence::Present);
    }

    #[test]
    fn blob_cache_stats_getters_match_internal_field_state() {
        let cache = small_cache(128);
        cache
            .put("n", "k", BlobCachePut::new(b"abc".to_vec()))
            .unwrap();
        let _ = cache.get("n", "k");
        let _ = cache.get("n", "missing");

        let stats = cache.stats();
        // Each getter must mirror the internal field that backs it.
        assert_eq!(stats.hits(), stats.hits);
        assert_eq!(stats.misses(), stats.misses);
        assert_eq!(stats.insertions(), stats.insertions);
        assert_eq!(stats.evictions(), stats.evictions);
        assert_eq!(stats.expirations(), stats.expirations);
        assert_eq!(stats.invalidations(), stats.invalidations);
        assert_eq!(stats.namespace_flushes(), stats.namespace_flushes);
        assert_eq!(stats.version_mismatches(), stats.version_mismatches);
        assert_eq!(stats.entries(), stats.entries);
        assert_eq!(stats.bytes_in_use(), stats.bytes_in_use as u64);
        assert_eq!(stats.l1_bytes_max(), stats.l1_bytes_max);
        assert_eq!(stats.l2_bytes_in_use(), stats.l2_bytes_in_use);
        assert_eq!(stats.l2_bytes_max(), stats.l2_bytes_max);
        assert_eq!(stats.l2_full_rejections(), stats.l2_full_rejections);
        assert_eq!(stats.l2_metadata_reads(), stats.l2_metadata_reads);
        assert_eq!(stats.l2_negative_skips(), stats.l2_negative_skips);
        assert_eq!(
            stats.synopsis_metadata_reads(),
            stats.synopsis_metadata_reads
        );
        assert_eq!(stats.synopsis_bytes(), stats.synopsis_bytes);
        assert_eq!(stats.namespaces(), stats.namespaces);
        assert_eq!(stats.max_namespaces(), stats.max_namespaces);
        assert_eq!(stats.promotion_queued(), stats.promotion_queued);
        assert_eq!(stats.promotion_dropped(), stats.promotion_dropped);
        assert_eq!(stats.promotion_completed(), stats.promotion_completed);
        assert_eq!(stats.promotion_queue_depth(), stats.promotion_queue_depth);
    }

    #[test]
    fn blob_cache_hit_getters_expose_payload_and_metadata() {
        let cache = small_cache(128);
        let metadata = BTreeMap::from([("ct".to_string(), "t".to_string())]);
        cache
            .put(
                "n",
                "k",
                BlobCachePut::new(b"hello".to_vec())
                    .with_content_metadata(metadata.clone())
                    .with_policy(BlobCachePolicy::default().version(7)),
            )
            .unwrap();
        let hit = cache.get("n", "k").expect("hit");
        assert_eq!(hit.value(), b"hello");
        assert_eq!(&**hit.bytes(), b"hello");
        assert_eq!(hit.content_metadata(), &metadata);
        assert_eq!(hit.version(), Some(7));
    }

    #[test]
    fn blob_cache_policy_setter_then_getter_round_trips() {
        let policy = BlobCachePolicy::default()
            .ttl_ms(60)
            .expires_at_unix_ms(1_000)
            .max_blob_bytes(512)
            .l1_admission(L1Admission::Always)
            .priority(7)
            .version(42);
        assert_eq!(policy.ttl_ms_value(), Some(60));
        assert_eq!(policy.expires_at_unix_ms_value(), Some(1_000));
        assert_eq!(policy.max_blob_bytes_value(), Some(512));
        assert_eq!(policy.l1_admission_value(), L1Admission::Always);
        assert_eq!(policy.priority_value(), 7);
        assert_eq!(policy.version_value(), Some(42));
    }

    #[test]
    fn blob_cache_is_send_and_sync_across_thread_boundary() {
        // Belt and braces alongside the file-level `assert_send_sync` const:
        // actually exercise the contract by sharing an `Arc<BlobCache>` with
        // a worker thread.
        use std::thread;
        let cache = Arc::new(small_cache(128));
        cache
            .put("n", "k", BlobCachePut::new(b"v".to_vec()))
            .unwrap();
        let worker = {
            let cache = Arc::clone(&cache);
            thread::spawn(move || {
                assert_eq!(cache.exists("n", "k"), CachePresence::Present);
                cache.get("n", "k").map(|hit| hit.value().to_vec())
            })
        };
        let observed = worker.join().expect("worker thread");
        assert_eq!(observed.as_deref(), Some(b"v".as_slice()));
    }

    // -- #146 Bloom synopsis ------------------------------------------------

    /// Tiny shared helper for the Bloom-filter property tests below.
    fn fpr_for(filter: &super::synopsis_filter::BloomFilter, negatives: &[String]) -> f64 {
        let positives = negatives.iter().filter(|key| filter.contains(key)).count() as f64;
        positives / negatives.len().max(1) as f64
    }

    #[test]
    fn bloom_synopsis_filter_no_false_negatives_and_fpr_within_target() {
        // Insert N keys, assert all are reported as `contains == true`
        // (no false-negatives). Then probe a 10*N disjoint negative set and
        // check the empirical FPR is within ±2% of the configured target.
        use super::synopsis_filter::BloomFilter;
        let n = DEFAULT_BLOB_SYNOPSIS_CAPACITY;
        let p = DEFAULT_BLOB_SYNOPSIS_FPR;
        let mut filter = BloomFilter::with_capacity(n, p);

        let inserted: Vec<String> = (0..n).map(|i| format!("present-{i}")).collect();
        for key in &inserted {
            filter.insert(key);
        }
        for key in &inserted {
            assert!(
                filter.contains(key),
                "Bloom filter must never report false-negatives ({key} missing)"
            );
        }

        let negatives: Vec<String> = (0..n * 10).map(|i| format!("absent-{i}")).collect();
        let observed_fpr = fpr_for(&filter, &negatives);
        let tolerance = 0.02;
        assert!(
            (observed_fpr - p).abs() <= tolerance,
            "observed FPR {observed_fpr:.4} not within ±{tolerance} of target {p}"
        );
    }

    #[test]
    fn bloom_synopsis_filter_default_sizing_is_about_twelve_kilobytes() {
        // Documented in the module comment: at the cache defaults the per-
        // namespace filter is ~12 KB. Lock that in so an accidental sizing
        // change shows up in review.
        use super::synopsis_filter::BloomFilter;
        let filter =
            BloomFilter::with_capacity(DEFAULT_BLOB_SYNOPSIS_CAPACITY, DEFAULT_BLOB_SYNOPSIS_FPR);
        // 95_851 bits round up to 1499 * 64 = 95_936 bits = 11_992 bytes.
        assert!(filter.bit_count() >= 95_000 && filter.bit_count() <= 100_000);
        assert!(filter.bytes() >= 11_500 && filter.bytes() <= 12_500);
        assert_eq!(filter.hash_count(), 7);
    }

    #[test]
    fn l2_synopsis_get_after_invalidate_returns_none_via_metadata_check() {
        // Stale-bits-after-delete: insert key, invalidate, `get` must still
        // return None because the authoritative metadata read says so.
        let path = l2_path("synopsis-bloom-delete");
        let cache = l2_cache(&path);
        cache
            .put(
                "n",
                "deleted",
                BlobCachePut::new(b"gone".to_vec())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .unwrap();
        assert_eq!(cache.invalidate_key("n", "deleted"), 1);
        assert!(cache.get("n", "deleted").is_none());
        assert_eq!(cache.stats().synopsis_metadata_reads(), 1);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_synopsis_get_after_expiry_returns_none_via_metadata_check() {
        // Stale-bits-after-expiry: TTL elapses, `get` must return None even
        // though the Bloom bits still hash positive.
        let path = l2_path("synopsis-bloom-expiry");
        let cache = l2_cache(&path);
        cache
            .put_at(
                "n",
                "expired",
                BlobCachePut::new(b"old".to_vec()).with_policy(
                    BlobCachePolicy::default()
                        .ttl_ms(10)
                        .l1_admission(L1Admission::Never),
                ),
                1_000,
            )
            .unwrap();
        assert!(cache.get_at("n", "expired", 1_010).is_none());

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_synopsis_rebuilds_filter_with_same_hit_rate_after_reopen() {
        // Startup rebuild: write a non-trivial corpus, close, reopen, and
        // assert the rebuilt filter produces the same negative-skip behaviour
        // as the in-memory one would have.
        let path = l2_path("synopsis-bloom-rebuild");
        let live: Vec<String> = (0..512).map(|i| format!("live-{i}")).collect();
        {
            let cache = l2_cache(&path);
            for key in &live {
                cache
                    .put(
                        "n",
                        key,
                        BlobCachePut::new(b"x".to_vec()).with_policy(
                            BlobCachePolicy::default().l1_admission(L1Admission::Never),
                        ),
                    )
                    .unwrap();
            }
        }
        let cache = l2_cache(&path);
        // All live keys must still be reported MaybePresent (no false-
        // negatives survived the rebuild).
        for key in &live {
            assert!(matches!(
                cache.exists("n", key),
                CachePresence::Present | CachePresence::MaybePresent
            ));
        }
        // Negative probes hit the rebuilt Bloom path; almost all of them
        // should be Absent. We tolerate the same FPR window as the
        // dedicated filter test (within 2% of target).
        let negatives: Vec<String> = (0..5_000).map(|i| format!("never-{i}")).collect();
        let mut maybe_or_present = 0usize;
        for key in &negatives {
            if !matches!(cache.exists("n", key), CachePresence::Absent) {
                maybe_or_present += 1;
            }
        }
        let observed_fpr = maybe_or_present as f64 / negatives.len() as f64;
        // Filter is well under capacity (512 of 10K), so empirical FPR is
        // far below the 1% target; just assert it stays inside the
        // documented ±2% envelope.
        assert!(
            observed_fpr <= DEFAULT_BLOB_SYNOPSIS_FPR + 0.02,
            "rebuilt filter FPR {observed_fpr:.4} exceeded target+tolerance"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_synopsis_present_answer_is_never_a_false_hit_under_random_workload() {
        // Negative-only property test: drive a random workload and assert
        // every `Present` answer is backed by an authoritative metadata hit.
        let path = l2_path("synopsis-bloom-property");
        let cache = l2_cache(&path);
        // Lightweight deterministic LCG so the test does not pull in `rand`.
        let mut state: u64 = 0xc001_d00d_dead_beef;
        let mut next = || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            state
        };

        for _ in 0..1_000 {
            let key = format!("k{}", next() % 200);
            match next() % 3 {
                0 => {
                    let _ = cache.put(
                        "n",
                        &key,
                        BlobCachePut::new(b"v".to_vec()).with_policy(
                            BlobCachePolicy::default().l1_admission(L1Admission::Never),
                        ),
                    );
                }
                1 => {
                    let _ = cache.invalidate_key("n", &key);
                }
                _ => {
                    if cache.exists("n", &key) == CachePresence::Present {
                        // Any `Present` answer must round-trip via `get`.
                        // A false hit would surface as `None` here.
                        assert!(
                            cache.get("n", &key).is_some(),
                            "exists reported Present for {key} but get returned None"
                        );
                    }
                }
            }
        }

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_synopsis_metadata_reads_counter_increments_on_filter_false_positive() {
        // Inject a bit (no L2 record), then `get` — the metadata read finds
        // nothing, which must bump `synopsis_metadata_reads_total`.
        let path = l2_path("synopsis-bloom-stats");
        let cache = l2_cache(&path);
        cache.inject_l2_synopsis_maybe_present("n", "phantom");

        assert!(cache.get("n", "phantom").is_none());
        let stats = cache.stats();
        assert_eq!(stats.synopsis_metadata_reads(), 1);
        assert!(stats.synopsis_bytes() > 0);

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    #[test]
    fn l2_synopsis_concurrent_readers_never_block_each_other() {
        // 8 reader threads call `exists` while 1 writer thread inserts. The
        // synopsis is RwLock-read-heavy, so readers should never deadlock or
        // see torn state. The strict invariant: every reader makes forward
        // progress and observes a legal `CachePresence` answer.
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc as StdArc;
        use std::thread;
        let path = l2_path("synopsis-bloom-concurrent");
        let cache = StdArc::new(l2_cache(&path));
        let stop = StdArc::new(AtomicBool::new(false));

        let writer = {
            let cache = StdArc::clone(&cache);
            let stop = StdArc::clone(&stop);
            thread::spawn(move || {
                let mut i = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    let key = format!("w{}", i % 256);
                    let _ = cache.put(
                        "n",
                        &key,
                        BlobCachePut::new(b"x".to_vec()).with_policy(
                            BlobCachePolicy::default().l1_admission(L1Admission::Never),
                        ),
                    );
                    i += 1;
                }
            })
        };

        let readers: Vec<_> = (0..8)
            .map(|tid| {
                let cache = StdArc::clone(&cache);
                thread::spawn(move || {
                    let mut probes = 0u64;
                    for i in 0..2_000 {
                        let key = format!("w{}", (i + tid) % 256);
                        let answer = cache.exists("n", &key);
                        // Any of the three legal answers is fine; the test
                        // is that we did not deadlock and got a value.
                        let _ = matches!(
                            answer,
                            CachePresence::Present
                                | CachePresence::Absent
                                | CachePresence::MaybePresent
                        );
                        probes += 1;
                    }
                    probes
                })
            })
            .collect();

        for r in readers {
            assert_eq!(r.join().unwrap(), 2_000);
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    // -- Async promotion wiring (issue #193, lane 1/5) ----------------------

    fn cleanup_l2(path: &Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(path.with_extension("blob-cache.ctl"));
        let _ = std::fs::remove_file(path.with_extension("dwb"));
    }

    /// Slow executor that sleeps `delay` then increments a counter.
    /// Used to make the hot-path / worker-path latency split observable
    /// without relying on real L2 read time.
    fn slow_executor(
        delay: std::time::Duration,
        counter: Arc<std::sync::atomic::AtomicUsize>,
    ) -> PromotionExecutor {
        Arc::new(move |_req| {
            std::thread::sleep(delay);
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
    }

    /// Hot-path latency on an L2-hit drops to near-zero when async
    /// promotion is enabled — the slow executor only blocks the worker.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn l2_hit_with_async_on_returns_immediately() {
        let path = l2_path("async-on");
        let cache = Arc::new(l2_cache(&path));
        // Seed L2: put then evict from L1 by namespace flush so next get
        // misses L1 and re-reads L2.
        cache
            .put("ns", "k", BlobCachePut::new(b"hello".to_vec()))
            .expect("put");
        // Force L1 eviction of "k" by overflowing the byte cap with fillers.
        for i in 0..40 {
            cache
                .put(
                    "ns",
                    &format!("filler{i}"),
                    BlobCachePut::new(vec![0u8; 16]),
                )
                .expect("filler");
        }

        let executed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        cache.enable_async_promotion_with_executor(
            PoolOpts {
                queue_capacity: 16,
                worker_count: 1,
            },
            slow_executor(std::time::Duration::from_millis(50), Arc::clone(&executed)),
        );

        let start = std::time::Instant::now();
        let hit = cache.get("ns", "k").expect("L2 hit");
        let elapsed = start.elapsed();
        assert_eq!(&*hit.bytes, b"hello");
        eprintln!("async-on hot-path latency: {elapsed:?}");
        assert!(
            elapsed < std::time::Duration::from_millis(20),
            "hot path should not block on slow executor; elapsed={elapsed:?}"
        );

        // Wait for the worker to drain so cleanup is sound.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while executed.load(Ordering::Relaxed) == 0 && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(executed.load(Ordering::Relaxed) >= 1, "worker did not run");

        cache.shutdown_async_promotion();
        cleanup_l2(&path);
    }

    /// Same slow executor, but with async OFF (default). The hot path
    /// pays the full sync promotion cost — but with no executor in the
    /// loop it should still be fast. Sanity: opt-in didn't break legacy.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn l2_hit_with_async_off_uses_legacy_sync_path() {
        let path = l2_path("async-off");
        let cache = Arc::new(l2_cache(&path));
        cache
            .put("ns", "k", BlobCachePut::new(b"hello".to_vec()))
            .expect("put");
        for i in 0..40 {
            cache
                .put(
                    "ns",
                    &format!("filler{i}"),
                    BlobCachePut::new(vec![0u8; 16]),
                )
                .expect("filler");
        }
        // Async NOT enabled — pool is None.
        assert!(cache.promotion_pool_handle().is_none());

        let start = std::time::Instant::now();
        let hit = cache.get("ns", "k").expect("L2 hit");
        let elapsed = start.elapsed();
        eprintln!("async-off (legacy sync) hot-path latency: {elapsed:?}");
        assert_eq!(&*hit.bytes, b"hello");
        // Stats show zero promotion activity in legacy mode.
        let s = cache.stats();
        assert_eq!(s.promotion_queued(), 0);
        assert_eq!(s.promotion_completed(), 0);
        assert_eq!(s.promotion_dropped(), 0);
        assert_eq!(s.promotion_queue_depth(), 0);

        cleanup_l2(&path);
    }

    /// Saturating the promotion queue does not corrupt `get`'s response —
    /// the L2 read still happens on the hot path, so callers always see
    /// the correct bytes even when the pool drops the promotion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drop_on_saturation_never_loses_correctness() {
        let path = l2_path("async-saturate");
        let cache = Arc::new(l2_cache(&path));
        // Seed many distinct keys in L2.
        for i in 0..32 {
            cache
                .put("ns", &format!("k{i}"), BlobCachePut::new(vec![i as u8; 4]))
                .expect("put");
        }
        // Evict L1.
        for i in 0..40 {
            cache
                .put(
                    "ns",
                    &format!("filler{i}"),
                    BlobCachePut::new(vec![0u8; 16]),
                )
                .expect("filler");
        }
        // Tiny queue, sleep-forever-ish executor — first request blocks
        // the worker, queue saturates almost instantly.
        let blocked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        cache.enable_async_promotion_with_executor(
            PoolOpts {
                queue_capacity: 1,
                worker_count: 1,
            },
            slow_executor(std::time::Duration::from_millis(500), Arc::clone(&blocked)),
        );

        // Hammer the cache; bytes must always come back unchanged.
        for i in 0..32 {
            let hit = cache.get("ns", &format!("k{i}")).expect("L2 hit");
            assert_eq!(&*hit.bytes, &vec![i as u8; 4][..]);
        }

        let s = cache.stats();
        assert!(
            s.promotion_dropped() > 0,
            "expected at least one drop under saturation; got {s:?}"
        );

        cache.shutdown_async_promotion();
        cleanup_l2(&path);
    }

    /// Shutdown drains queued requests within a bounded budget.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_drains_pool_within_budget() {
        let path = l2_path("async-shutdown");
        let cache = Arc::new(l2_cache(&path));
        for i in 0..20 {
            cache
                .put("ns", &format!("k{i}"), BlobCachePut::new(vec![i as u8; 4]))
                .expect("put");
        }
        for i in 0..40 {
            cache
                .put(
                    "ns",
                    &format!("filler{i}"),
                    BlobCachePut::new(vec![0u8; 16]),
                )
                .expect("filler");
        }
        let executed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        cache.enable_async_promotion_with_executor(
            PoolOpts {
                queue_capacity: 64,
                worker_count: 2,
            },
            slow_executor(std::time::Duration::from_millis(1), Arc::clone(&executed)),
        );

        let mut scheduled = 0u64;
        for i in 0..20 {
            let _ = cache.get("ns", &format!("k{i}"));
            // Each L2 hit schedules at most one promotion.
            scheduled += 1;
        }
        cache.shutdown_async_promotion();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let s = cache.stats();
            if s.promotion_completed() + s.promotion_dropped() >= scheduled {
                break;
            }
            if std::time::Instant::now() > deadline {
                panic!("shutdown did not drain: {:?}", cache.stats());
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        cleanup_l2(&path);
    }

    /// The executor closure holds only a `Weak<BlobCache>` — dropping the
    /// `Arc<BlobCache>` releases the cache even while the pool is alive.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_arc_cycle_executor_holds_only_weak_reference() {
        let path = l2_path("async-noarccycle");
        let cache = Arc::new(l2_cache(&path));
        let pool = cache.enable_async_promotion(PoolOpts {
            queue_capacity: 4,
            worker_count: 1,
        });

        // Construct the canary weak BEFORE we drop the strong arc.
        let canary: Weak<BlobCache> = Arc::downgrade(&cache);
        assert!(canary.upgrade().is_some());

        // Drop the user-held strong arc. The pool itself may still hold
        // refs to its own internal queue/executor, but the executor
        // closure was built on a `Weak<BlobCache>`, so the cache should
        // be deallocatable.
        drop(cache);

        // Pool is still alive (its workers are running), but the cache
        // is gone — the canary cannot be upgraded.
        assert!(
            canary.upgrade().is_none(),
            "BlobCache leaked: executor closure still holds a strong reference"
        );

        // Cleanup: tell the pool to stop. We can't call shutdown via the
        // cache (it's dropped) but we have the pool handle.
        Arc::clone(&pool).shutdown();
        cleanup_l2(&path);
    }

    // -- L2 compression wiring (#192 lane 2/5) -----------------------------

    /// Build a 4 KB payload of repetitive Lorem text — guaranteed to
    /// compress well under the default zstd settings.
    fn lorem_4kb() -> Vec<u8> {
        let unit = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                     Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. \
                     Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris \
                     nisi ut aliquip ex ea commodo consequat. ";
        let mut out = Vec::with_capacity(4096 + unit.len());
        while out.len() < 4096 {
            out.extend_from_slice(unit);
        }
        out.truncate(4096);
        out
    }

    /// Linear congruential generator — deterministic high-entropy bytes
    /// without pulling in `rand`.
    fn pseudo_random(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push((state >> 33) as u8);
        }
        out
    }

    fn l2_cache_with_compression(path: &Path, mode: L2Compression) -> BlobCache {
        // L1 is sized large enough to admit the blob (`validate_blob_size`
        // checks against `l1_bytes_max`), but every put uses
        // `L1Admission::Never` so the L2 path is what we exercise.
        BlobCache::open_with_l2(
            BlobCacheConfig::default()
                .with_l1_bytes_max(64 * 1024)
                .with_shard_count(1)
                .with_max_namespaces(4)
                .with_l2_path(path)
                .with_l2_compression(mode),
        )
        .expect("l2_cache_with_compression test helper")
    }

    #[test]
    fn l2_round_trip_compresses_text_payload_and_returns_original_bytes() {
        let path = l2_path("compression-text");
        let cache = l2_cache_with_compression(&path, L2Compression::On);
        let payload = lorem_4kb();

        cache
            .put(
                "n",
                "doc",
                BlobCachePut::new(payload.clone())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .expect("put");

        // Round-trip through L2: bytes returned must match the original.
        let hit = cache.get("n", "doc").expect("L2 hit");
        assert_eq!(&*hit.bytes, &payload[..]);

        // L2 budget accounting must reflect the *compressed* size, well
        // below the original 4 KB.
        let stats = cache.stats();
        assert!(
            stats.l2_bytes_in_use < payload.len() as u64,
            "expected stored bytes < {}, got {}",
            payload.len(),
            stats.l2_bytes_in_use
        );
        assert_eq!(stats.l2_compression_skipped_total(), 0);
        assert!(stats.l2_compression_ratio_observed() > 1.0);
        assert!(stats.l2_bytes_saved_total() > 0);

        cleanup_l2(&path);
    }

    #[test]
    fn l2_round_trip_with_compression_off_stores_raw_bytes() {
        let path = l2_path("compression-off");
        let cache = l2_cache_with_compression(&path, L2Compression::Off);
        let payload = lorem_4kb();

        cache
            .put(
                "n",
                "doc",
                BlobCachePut::new(payload.clone())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .expect("put");

        let hit = cache.get("n", "doc").expect("L2 hit");
        assert_eq!(&*hit.bytes, &payload[..]);

        let stats = cache.stats();
        // `Off` skips the compress call entirely → entry counted as
        // skipped, stored bytes equal original size.
        assert_eq!(stats.l2_bytes_in_use, payload.len() as u64);
        assert_eq!(stats.l2_compression_skipped_total(), 1);
        assert_eq!(stats.l2_bytes_saved_total(), 0);
        assert_eq!(stats.l2_compression_ratio_observed(), 1.0);

        cleanup_l2(&path);
    }

    #[test]
    fn l2_round_trip_with_image_content_type_stores_raw() {
        let path = l2_path("compression-image-ct");
        let cache = l2_cache_with_compression(&path, L2Compression::On);
        // 4 KB of zero bytes would otherwise compress superbly; the
        // content-type rule must short-circuit that.
        let payload = vec![0u8; 4096];
        let metadata = BTreeMap::from([("content-type".to_string(), "image/png".to_string())]);

        cache
            .put(
                "n",
                "img",
                BlobCachePut::new(payload.clone())
                    .with_content_metadata(metadata)
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .expect("put");

        let hit = cache.get("n", "img").expect("L2 hit");
        assert_eq!(&*hit.bytes, &payload[..]);

        let stats = cache.stats();
        assert_eq!(stats.l2_bytes_in_use, payload.len() as u64);
        assert_eq!(stats.l2_compression_skipped_total(), 1);
        assert_eq!(stats.l2_bytes_saved_total(), 0);

        cleanup_l2(&path);
    }

    #[test]
    fn l2_round_trip_with_high_entropy_payload_falls_back_to_raw_via_ratio_gate() {
        let path = l2_path("compression-entropy");
        let cache = l2_cache_with_compression(&path, L2Compression::On);
        // 8 KB of LCG output — zstd cannot meaningfully shrink it, so
        // the `max_ratio` gate fires and the entry stores raw.
        let payload = pseudo_random(0xCAFE_F00D, 8 * 1024);

        cache
            .put(
                "n",
                "noise",
                BlobCachePut::new(payload.clone())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .expect("put");

        let hit = cache.get("n", "noise").expect("L2 hit");
        assert_eq!(&*hit.bytes, &payload[..]);

        let stats = cache.stats();
        assert_eq!(stats.l2_bytes_in_use, payload.len() as u64);
        assert_eq!(stats.l2_compression_skipped_total(), 1);

        cleanup_l2(&path);
    }

    #[test]
    fn l2_forward_compat_reads_legacy_v1_entry_written_before_compression() {
        let path = l2_path("compression-v1-compat");
        let cache = l2_cache_with_compression(&path, L2Compression::On);
        // Synthesise a legacy entry on disk: raw bytes, no v2 framing.
        let payload = b"legacy-payload-pre-issue-192".to_vec();
        cache
            .inject_l2_v1_entry("n", "legacy", &payload)
            .expect("inject v1");

        // Subsequent `get` must dispatch on the record's `format_version`
        // and return the raw bytes verbatim — no decompress, no framing.
        let hit = cache.get("n", "legacy").expect("L2 hit");
        assert_eq!(&*hit.bytes, &payload[..]);

        cleanup_l2(&path);
    }

    #[test]
    fn l2_budget_amplifies_when_entries_compress() {
        // Original L2 budget that fits ~10 raw entries of 4 KB (40 KB
        // total). With compression on, the *stored* bytes are far smaller,
        // so all 10 entries must be admitted without `L2Full`.
        let path = l2_path("compression-budget");
        let payload = lorem_4kb();
        let raw_total = (payload.len() * 10) as u64;
        // Pick a budget below `raw_total` but above the expected
        // compressed total. zstd typically shrinks Lorem to <30% so 25%
        // of `raw_total` is a comfortable headroom.
        let budget = raw_total / 4;
        let cache = BlobCache::open_with_l2(
            BlobCacheConfig::default()
                .with_l1_bytes_max(64 * 1024)
                .with_shard_count(1)
                .with_max_namespaces(4)
                .with_l2_bytes_max(budget)
                .with_l2_path(&path)
                .with_l2_compression(L2Compression::On),
        )
        .expect("open l2");

        for i in 0..10 {
            cache
                .put(
                    "n",
                    &format!("doc{i}"),
                    BlobCachePut::new(payload.clone())
                        .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
                )
                .expect("put admitted under compressed budget");
        }

        let stats = cache.stats();
        assert_eq!(stats.l2_full_rejections(), 0, "no rejections expected");
        assert!(
            stats.l2_bytes_in_use <= budget,
            "stored bytes {} exceed budget {}",
            stats.l2_bytes_in_use,
            budget
        );
        // Sanity: would have blown past the budget at raw sizing.
        assert!(stats.l2_bytes_in_use < raw_total / 2);

        cleanup_l2(&path);
    }

    #[test]
    fn l2_compression_metrics_partition_compressible_and_skipped_entries() {
        let path = l2_path("compression-metrics");
        let cache = l2_cache_with_compression(&path, L2Compression::On);

        // 10 compressible Lorem entries.
        let payload = lorem_4kb();
        for i in 0..10 {
            cache
                .put(
                    "n",
                    &format!("text{i}"),
                    BlobCachePut::new(payload.clone())
                        .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
                )
                .expect("put");
        }
        // 5 high-entropy entries — these must hit the `max_ratio` gate
        // and land in the `skipped` counter.
        for i in 0..5 {
            let bin = pseudo_random(0x1234_5678 ^ i as u64, 4 * 1024);
            cache
                .put(
                    "n",
                    &format!("bin{i}"),
                    BlobCachePut::new(bin)
                        .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
                )
                .expect("put");
        }

        let stats = cache.stats();
        assert_eq!(stats.l2_compression_skipped_total(), 5);
        assert!(
            stats.l2_compression_ratio_observed() > 1.0,
            "compressed entries did not contribute to ratio"
        );
        assert!(stats.l2_bytes_saved_total() > 0);

        cleanup_l2(&path);
    }

    // ----------------------------------------------------------------------
    // Extended TTL wiring (issue #194 lane 3/5)
    // ----------------------------------------------------------------------

    /// Backwards compat — when extended is `off()`, the cache must behave
    /// exactly like the legacy hard-TTL path: past hard TTL → None, no
    /// stale serve, no idle bookkeeping leaking.
    #[test]
    fn extended_off_preserves_legacy_hard_ttl_behavior() {
        let cache = small_cache(128);
        let policy = BlobCachePolicy::default().ttl_ms(50);
        cache
            .put_at(
                "n",
                "k",
                BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
                1_000,
            )
            .unwrap();
        // Past hard TTL → None.
        assert!(cache.get_at("n", "k", 1_051).is_none());
        let stats = cache.stats();
        assert_eq!(stats.expirations(), 1);
        assert_eq!(stats.l1_idle_evicts_total(), 0);
        assert_eq!(stats.l1_stale_serves_total(), 0);
    }

    /// Idle TTL evicts an entry that has not been accessed within
    /// `idle_ttl_ms`, even when its hard TTL is far in the future.
    #[test]
    fn extended_idle_ttl_evicts_dormant_entry() {
        let cache = small_cache(128);
        let extended = ExtendedTtlPolicy {
            idle_ttl_ms: Some(100),
            stale_serve_ms: None,
            jitter_pct: 0,
        };
        let policy = BlobCachePolicy::default().ttl_ms(10_000).extended(extended);
        cache
            .put_at(
                "n",
                "k",
                BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
                1_000,
            )
            .unwrap();
        // 200ms after put, no intervening access → idle window blown.
        assert!(cache.get_at("n", "k", 1_200).is_none());
        let stats = cache.stats();
        assert_eq!(stats.l1_idle_evicts_total(), 1);
        assert_eq!(stats.expirations(), 1);
    }

    /// Idle TTL must reset on every successful `get`. Two accesses spaced
    /// 150ms apart with `idle_ttl_ms = 200ms` keep the entry alive across
    /// 250ms of wall clock.
    #[test]
    fn extended_idle_ttl_resets_on_access() {
        let cache = small_cache(128);
        let extended = ExtendedTtlPolicy {
            idle_ttl_ms: Some(200),
            stale_serve_ms: None,
            jitter_pct: 0,
        };
        let policy = BlobCachePolicy::default().ttl_ms(10_000).extended(extended);
        cache
            .put_at(
                "n",
                "k",
                BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
                1_000,
            )
            .unwrap();
        // First access at +100ms — still within idle window, last_access
        // bumps to 1_100.
        assert!(cache.get_at("n", "k", 1_100).is_some());
        // Second access at +250ms (= 150ms past the previous access):
        // because last_access was reset, idle = 150 ≤ 200 → still Fresh.
        assert!(cache.get_at("n", "k", 1_250).is_some());
        let stats = cache.stats();
        assert_eq!(stats.l1_idle_evicts_total(), 0);
        assert_eq!(stats.hits(), 2);
    }

    /// SWR window — past hard TTL but inside `stale_serve_ms` returns a
    /// `BlobCacheHit` flagged stale with the remaining window.
    #[test]
    fn extended_stale_serve_returns_stale_hit() {
        let cache = small_cache(128);
        let extended = ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: Some(100),
            jitter_pct: 0,
        };
        let policy = BlobCachePolicy::default().ttl_ms(50).extended(extended);
        cache
            .put_at(
                "n",
                "k",
                BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
                1_000,
            )
            .unwrap();
        // hard expires at 1_050, stale window runs to 1_150.
        // get at 1_060 → Stale with ~90ms remaining.
        let hit = cache.get_at("n", "k", 1_060).expect("stale hit");
        assert!(hit.is_stale());
        assert_eq!(hit.stale_window_remaining_ms(), Some(90));
        let stats = cache.stats();
        assert_eq!(stats.l1_stale_serves_total(), 1);
    }

    /// Past the cumulative `hard + stale_serve_ms` window, the entry is
    /// hard-expired regardless of how big the stale window was.
    #[test]
    fn extended_stale_serve_expires_after_window_closes() {
        let cache = small_cache(128);
        let extended = ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: Some(100),
            jitter_pct: 0,
        };
        let policy = BlobCachePolicy::default().ttl_ms(50).extended(extended);
        cache
            .put_at(
                "n",
                "k",
                BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
                1_000,
            )
            .unwrap();
        // get at 1_200 → past 1_150 stale deadline → Expired.
        assert!(cache.get_at("n", "k", 1_200).is_none());
        let stats = cache.stats();
        assert_eq!(stats.l1_stale_serves_total(), 0);
        assert_eq!(stats.expirations(), 1);
    }

    /// Jitter at insert time spreads `expires_at_unix_ms` deterministically
    /// inside `[base_ttl, base_ttl * (1 + pct/100)]` for unique keys.
    #[test]
    fn extended_jitter_spreads_expires_at_within_bound() {
        let cache = BlobCache::new(
            BlobCacheConfig::default()
                .with_l1_bytes_max(1_024 * 1024)
                .with_shard_count(1)
                .with_max_namespaces(4),
        );
        let extended = ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: None,
            jitter_pct: 20,
        };
        let policy = BlobCachePolicy::default().ttl_ms(1_000).extended(extended);
        let now = 10_000u64;
        // Probe 1000 entries; each must remain Fresh at +1000ms (the base
        // TTL floor) and Expired by +1200ms (the jittered ceiling, +20%).
        for i in 0..1_000u32 {
            let key = format!("k{i}");
            cache
                .put_at(
                    "n",
                    &key,
                    BlobCachePut::new(vec![i as u8]).with_policy(policy),
                    now,
                )
                .expect("put");
            // At now + base_ttl - 1 → must still be Fresh (jitter only
            // ever pushes expiry later, never earlier).
            assert!(
                cache.get_at("n", &key, now + 999).is_some(),
                "entry {key} should be Fresh at base_ttl - 1",
            );
            // At now + base_ttl * (1 + pct/100) + 1 → must be Expired
            // (jitter ceiling crossed).
            assert!(
                cache.get_at("n", &key, now + 1_201).is_none(),
                "entry {key} should be Expired beyond jitter ceiling",
            );
        }
    }

    /// Jitter must be deterministic — same `(namespace, key, now_ms)`
    /// triple must produce the same expires_at across independent caches.
    #[test]
    fn extended_jitter_is_deterministic_per_triple() {
        let extended = ExtendedTtlPolicy {
            idle_ttl_ms: None,
            stale_serve_ms: None,
            jitter_pct: 50,
        };
        let policy = BlobCachePolicy::default().ttl_ms(1_000).extended(extended);
        let now = 42_000u64;

        let cache_a = small_cache(1_024);
        let cache_b = small_cache(1_024);
        for key in ["alpha", "beta", "gamma", "delta", "epsilon"] {
            cache_a
                .put_at(
                    "n",
                    key,
                    BlobCachePut::new(b"x".to_vec()).with_policy(policy),
                    now,
                )
                .unwrap();
            cache_b
                .put_at(
                    "n",
                    key,
                    BlobCachePut::new(b"x".to_vec()).with_policy(policy),
                    now,
                )
                .unwrap();
            // The two caches will have computed identical expires_at_unix_ms.
            // Probe the boundary: any time `t` where one cache returns Some
            // and the other returns None proves they diverge.
            for t_offset in [999u64, 1_000, 1_100, 1_250, 1_499, 1_500, 1_501] {
                let a = cache_a.get_at("n", key, now + t_offset).is_some();
                let b = cache_b.get_at("n", key, now + t_offset).is_some();
                assert_eq!(
                    a, b,
                    "jitter diverged for key={key} t_offset={t_offset}: a={a} b={b}",
                );
            }
        }
    }

    /// Performance contract — when extended is `off()`,
    /// `EffectiveExpiry::compute` must NEVER be called from the hot path.
    /// Verified via a process-global counter incremented inside the
    /// extended branch of `Shard::get`.
    #[test]
    fn extended_off_skips_effective_expiry_compute() {
        // Thread-local counter — no cross-test race. Reset to 0 at the
        // start so the absolute value below is the count contributed by
        // this test alone.
        EFFECTIVE_EXPIRY_COMPUTE_CALLS.with(|c| c.set(0));
        let cache = small_cache(128);
        let policy = BlobCachePolicy::default().ttl_ms(10_000); // extended defaults to off()
        cache
            .put_at(
                "n",
                "k",
                BlobCachePut::new(b"ok".to_vec()).with_policy(policy),
                1_000,
            )
            .unwrap();
        for t in [1_001u64, 1_500, 2_000, 5_000, 9_999] {
            let _ = cache.get_at("n", "k", t);
        }
        let calls = EFFECTIVE_EXPIRY_COMPUTE_CALLS.with(|c| c.get());
        assert_eq!(
            calls, 0,
            "EffectiveExpiry::compute was invoked {calls} times despite extended=off()",
        );
    }

    // -------------------------------------------------------------------------
    // open_with_l2 error-path tests (#220)
    // -------------------------------------------------------------------------

    #[test]
    fn open_with_l2_returns_err_on_corrupt_control_sidecar() {
        let path = l2_path("corrupt-ctl");
        // Write garbage to the control sidecar so L2Control::read returns Err.
        let ctl = path.with_extension("blob-cache.ctl");
        std::fs::create_dir_all(path.parent().unwrap()).ok();
        std::fs::write(&ctl, b"not-a-valid-control-file").unwrap();

        let result = BlobCache::open_with_l2(BlobCacheConfig::default().with_l2_path(&path));
        match &result {
            Err(CacheError::L2Io(_)) => {}
            Err(other) => panic!("expected L2Io error, got: {other:?}"),
            Ok(_) => panic!("expected L2Io error, got Ok(BlobCache)"),
        }
        // Process is still alive — test reaches here.
        let _ = std::fs::remove_file(&ctl);
    }

    #[test]
    fn open_with_l2_returns_err_on_readonly_path() {
        // Create the pager file's parent as a file (so opening the pager path
        // as a file underneath it fails with an I/O error).
        let path = l2_path("readonly");
        // Write a regular file at the path so Pager::open gets an I/O error
        // when it tries to create/open the pager file (or the control sidecar
        // can't be created because the path itself is a directory with no write
        // permission — use a read-only directory instead).
        std::fs::create_dir_all(path.parent().unwrap()).ok();
        // Create the pager path as a directory so opening it as a file fails.
        std::fs::create_dir_all(&path).unwrap();

        let result = BlobCache::open_with_l2(BlobCacheConfig::default().with_l2_path(&path));
        assert!(
            result.is_err(),
            "expected Err when l2_path is a directory, got Ok",
        );
        let _ = std::fs::remove_dir_all(&path);
    }
}
