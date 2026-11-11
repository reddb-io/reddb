//! Byte-oriented Blob Cache.
//!
//! L1 is a sharded, byte-bounded in-process cache with SIEVE eviction. L2 can
//! use either the legacy standalone pager or RedDB's internal single-file
//! collection backend.

use super::config::{BlobCacheConfig, L2Compression, L2PromotionPolicy};
use super::entry::{effective_expires_at_unix_ms, jitter_seed, Entry};
use super::l2::BlobCacheL2;
use super::l2_backend::BlobCacheL2Backend;
use super::l2_store::StoreBlobCacheL2;
use super::shard::{InsertOutcome, Lookup, Shard, ShardSweep};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
#[cfg(test)]
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use indexmap::Equivalent;
use parking_lot::RwLock;

use super::super::compressor::{CompressOpts, Compressed, L2BlobCompressor};
use super::super::extended_ttl::ExtendedTtlPolicy;
use super::super::promotion_pool::{
    AsyncPromotionPool, PoolOpts, PromotionExecutor, PromotionRequest,
};

// Test-only thread-local counter of how many times
// `EffectiveExpiry::compute` is invoked from `Shard::get`. Thread-local
// (rather than a global atomic) so the off-fast-path test does not race
// with other tests in the harness's parallel executor.
#[cfg(test)]
thread_local! {
    static PUT_PREPARED_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
    pub(super) static EFFECTIVE_EXPIRY_COMPUTE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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
pub(super) struct BlobCacheKey {
    pub(super) namespace: String,
    pub(super) key: String,
}

impl BlobCacheKey {
    pub(super) fn new(namespace: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            key: key.into(),
        }
    }

    pub(super) fn borrowed<'a>(namespace: &'a str, key: &'a str) -> BlobCacheKeyRef<'a> {
        BlobCacheKeyRef { namespace, key }
    }
}

pub(super) struct BlobCacheKeyRef<'a> {
    pub(super) namespace: &'a str,
    pub(super) key: &'a str,
}

impl Hash for BlobCacheKeyRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.namespace.hash(state);
        self.key.hash(state);
    }
}

impl Equivalent<BlobCacheKey> for BlobCacheKeyRef<'_> {
    fn equivalent(&self, key: &BlobCacheKey) -> bool {
        self.namespace == key.namespace && self.key == key.key
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
    pub(super) bytes: Arc<[u8]>,
    pub(super) content_metadata: Option<Arc<BTreeMap<String, String>>>,
    pub(super) version: Option<u64>,
    pub(super) expires_at_unix_ms: Option<u64>,
    /// `Some(remaining_ms)` when the hit came from the stale-while-revalidate
    /// window of an `ExtendedTtlPolicy`; `None` when the entry was fresh.
    /// Boolean staleness is just `.is_some()`.
    pub(super) stale_window_remaining_ms: Option<u64>,
}

impl BlobCacheHit {
    pub(crate) fn new(
        bytes: Arc<[u8]>,
        content_metadata: Option<Arc<BTreeMap<String, String>>>,
        version: Option<u64>,
        expires_at_unix_ms: Option<u64>,
    ) -> Self {
        Self {
            bytes,
            content_metadata,
            version,
            expires_at_unix_ms,
            stale_window_remaining_ms: None,
        }
    }

    pub(crate) fn new_stale(
        bytes: Arc<[u8]>,
        content_metadata: Option<Arc<BTreeMap<String, String>>>,
        version: Option<u64>,
        expires_at_unix_ms: Option<u64>,
        window_remaining_ms: u64,
    ) -> Self {
        Self {
            bytes,
            content_metadata,
            version,
            expires_at_unix_ms,
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
        static EMPTY: BTreeMap<String, String> = BTreeMap::new();
        self.content_metadata.as_deref().unwrap_or(&EMPTY)
    }

    /// Optional CAS / freshness version stamped on `put`.
    pub fn version(&self) -> Option<u64> {
        self.version
    }

    /// Absolute hard-expiry deadline attached to this value, if any.
    pub fn expires_at_unix_ms(&self) -> Option<u64> {
        self.expires_at_unix_ms
    }

    /// Remaining hard TTL at `now_ms`. `None` means the entry does not expire.
    pub fn remaining_ttl_ms_at(&self, now_ms: u64) -> Option<u64> {
        self.expires_at_unix_ms
            .map(|expires_at| expires_at.saturating_sub(now_ms))
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
    pub(super) hits: u64,
    pub(super) misses: u64,
    pub(super) insertions: u64,
    pub(super) evictions: u64,
    pub(super) expirations: u64,
    pub(super) invalidations: u64,
    pub(super) namespace_flushes: u64,
    pub(super) version_mismatches: u64,
    pub(super) entries: usize,
    pub(super) bytes_in_use: usize,
    pub(super) l1_bytes_max: usize,
    pub(super) namespace_bytes_max: usize,
    pub(super) l2_bytes_in_use: u64,
    pub(super) l2_bytes_max: u64,
    pub(super) l2_full_rejections: u64,
    pub(super) l2_metadata_reads: u64,
    pub(super) l2_negative_skips: u64,
    /// Times the per-namespace Bloom synopsis answered `MaybePresent` but the
    /// authoritative L2 metadata B+ tree said `Absent` (the false-positive
    /// cost of the probabilistic synopsis).
    pub(super) synopsis_metadata_reads: u64,
    /// Total bytes used by all per-namespace Bloom synopsis filters.
    pub(super) synopsis_bytes: u64,
    pub(super) namespaces: usize,
    pub(super) max_namespaces: usize,
    /// Async promotion pool counters (issue #193). All zero when the
    /// pool is not enabled (default — `cache.blob.async_promotion = "off"`).
    pub(super) promotion_queued: u64,
    pub(super) promotion_dropped: u64,
    pub(super) promotion_completed: u64,
    pub(super) promotion_queue_depth: usize,
    /// Numerator of the L2 compression ratio: sum of `original_len` over
    /// entries that actually compressed (#192). Stored as the ratio's
    /// component so [`BlobCacheStats`] stays `Eq` (avoids `f64` fields).
    pub(super) l2_compression_original_bytes: u64,
    /// Denominator of the L2 compression ratio: sum of `stored_len` over
    /// entries that actually compressed.
    pub(super) l2_compression_stored_bytes: u64,
    /// Counter of L2 entries the compressor returned as `Raw` (any reason).
    pub(super) l2_compression_skipped_total: u64,
    /// Cumulative `(original_len - stored_len)` across compressed entries.
    pub(super) l2_bytes_saved_total: u64,
    /// Counter — L1 hits that served a stale entry from the SWR window of
    /// an `ExtendedTtlPolicy` (#194). Stays 0 when extended is off.
    pub(super) l1_stale_serves_total: u64,
    /// Counter — L1 entries evicted by the idle-TTL gate of an
    /// `ExtendedTtlPolicy` (#194). Stays 0 when extended is off.
    pub(super) l1_idle_evicts_total: u64,
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

    pub fn namespace_bytes_max(&self) -> usize {
        self.namespace_bytes_max
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
    l2: Option<Arc<dyn BlobCacheL2Backend>>,
    bytes_in_use: AtomicUsize,
    namespace_bytes_in_use: RwLock<HashMap<String, usize>>,
    sweep_shard_cursor: AtomicUsize,
    namespace_flush_epoch: AtomicU64,
    namespace_reclaimed_epoch: AtomicU64,
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

    /// Open L2 inside the owning RedDB store. The cache rows, byte counter,
    /// WAL records, and checkpoints remain in the same `.rdb` artifact.
    pub fn open_with_internal_l2(
        config: BlobCacheConfig,
        store: Arc<crate::storage::UnifiedStore>,
    ) -> Result<Self, CacheError> {
        let l2 = (config.l2_bytes_max > 0)
            .then(|| StoreBlobCacheL2::open(store, config.l2_bytes_max))
            .transpose()?
            .map(|backend| Arc::new(backend) as Arc<dyn BlobCacheL2Backend>);
        Self::with_backend(config, l2)
    }

    fn try_new(config: BlobCacheConfig) -> Result<Self, CacheError> {
        let l2 = config
            .l2_path
            .clone()
            .map(|path| BlobCacheL2::open(path, config.l2_bytes_max))
            .transpose()?
            .map(|backend| Arc::new(backend) as Arc<dyn BlobCacheL2Backend>);
        Self::with_backend(config, l2)
    }

    fn with_backend(
        config: BlobCacheConfig,
        l2: Option<Arc<dyn BlobCacheL2Backend>>,
    ) -> Result<Self, CacheError> {
        let config = BlobCacheConfig {
            shard_count: config.shard_count.max(1),
            ..config
        };
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
            l2,
            bytes_in_use: AtomicUsize::new(0),
            namespace_bytes_in_use: RwLock::new(HashMap::new()),
            sweep_shard_cursor: AtomicUsize::new(0),
            namespace_flush_epoch: AtomicU64::new(0),
            namespace_reclaimed_epoch: AtomicU64::new(0),
            stats: AtomicStats::new(),
            promotion_pool: OnceLock::new(),
        })
    }

    pub fn with_defaults() -> Self {
        Self::new(BlobCacheConfig::default())
    }

    /// Path to the legacy standalone L2 pager, when that backend is enabled.
    ///
    /// The server's internal L2 is part of the owning `.rdb` and therefore
    /// returns `None` here. The backup orchestrator only needs this path for
    /// callers that explicitly selected the compatibility file backend.
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
        if self.l2.is_some() {
            self.check_version(
                &self.shards[shard_idx].read(),
                &key,
                input.policy.version_value(),
                namespace_generation,
            )?;
        }
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
        let entry_charge = entry.l1_charge_bytes;
        let compressed = if self.l2.is_some() {
            // Compression decision happens in the foreground put — the
            // outcome (`Compressed::Raw` or `Compressed::Zstd`) is what
            // gets framed and written to the chain (#192). When the knob
            // is `Off`, skip the compressor entirely (CPU savings) and
            // emit a `Raw` variant directly so the on-disk format stays
            // uniform.
            Some(match self.config.l2_compression {
                L2Compression::Off => Compressed::Raw(entry.bytes.as_ref().to_vec()),
                L2Compression::On => {
                    let content_type = entry
                        .content_metadata
                        .as_ref()
                        .and_then(|metadata| metadata.get("content-type"))
                        .map(String::as_str);
                    L2BlobCompressor::compress(
                        entry.bytes.as_ref(),
                        content_type,
                        &CompressOpts::default(),
                    )
                    .map_err(|err| CacheError::L2Io(err.to_string()))?
                }
            })
        } else {
            None
        };
        #[cfg(test)]
        if let Some(hook) = PUT_PREPARED_HOOK.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
        let mut shard = self.shards[shard_idx].write();
        // Another writer may have published while compression ran. Recheck
        // under the publication guard before changing either cache tier.
        self.check_version(
            &shard,
            &key,
            input.policy.version_value(),
            namespace_generation,
        )?;
        shard.clear_l2_hit_marker(&key);
        if let Some(l2) = &self.l2 {
            let old_l2_size = l2.record_size(&key);
            let compressed = compressed.expect("L2 payload prepared before publication");
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
        let old_charge = outcome
            .old_entry
            .as_ref()
            .map_or(0, |entry| entry.l1_charge_bytes);
        let new_charge = if outcome.admitted { entry_charge } else { 0 };
        self.adjust_namespace_bytes(&namespace, old_charge, new_charge);
        if new_charge >= old_charge {
            self.bytes_in_use
                .fetch_add(new_charge - old_charge, Ordering::Relaxed);
        } else {
            self.bytes_in_use
                .fetch_sub(old_charge - new_charge, Ordering::Relaxed);
        }
        if let Some(old_entry) = outcome.old_entry.as_ref() {
            self.deindex_entry(&key, old_entry);
        }
        if outcome.admitted {
            self.index_entry(&key, &tags, &dependencies);
        }
        drop(shard);

        self.stats.insertions.fetch_add(1, Ordering::Relaxed);
        if outcome.admitted {
            self.evict_namespace_until_within_budget(&namespace, shard_idx);
            self.evict_until_within_budget(shard_idx);
        }
        Ok(())
    }

    pub fn get(&self, namespace: &str, key: &str) -> Option<BlobCacheHit> {
        self.get_at(namespace, key, unix_now_ms())
    }

    fn get_at(&self, namespace: &str, key: &str, now_ms: u64) -> Option<BlobCacheHit> {
        let namespace_generation = self.current_generation(namespace);
        let shard_idx = self.shard_index_parts(namespace, key);
        if let Some(hit) =
            self.shards[shard_idx]
                .read()
                .shared_hit(namespace, key, now_ms, namespace_generation)
        {
            self.stats.hits.fetch_add(1, Ordering::Relaxed);
            return Some(hit);
        }
        let mut shard = self.shards[shard_idx].write();
        match shard.get_by_parts(namespace, key, now_ms, namespace_generation) {
            Lookup::Hit(hit) => {
                self.stats.hits.fetch_add(1, Ordering::Relaxed);
                if hit.is_stale() {
                    self.stats.l1_stale_serves.fetch_add(1, Ordering::Relaxed);
                }
                Some(hit)
            }
            Lookup::Expired(entry) => {
                let cache_key = BlobCacheKey::new(namespace, key);
                self.record_removed_entry(&cache_key, &entry);
                if let Some(l2) = &self.l2 {
                    l2.delete_key(&cache_key);
                }
                drop(shard);
                self.stats.expirations.fetch_add(1, Ordering::Relaxed);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Lookup::IdleEvicted(entry) => {
                let cache_key = BlobCacheKey::new(namespace, key);
                self.record_removed_entry(&cache_key, &entry);
                if let Some(l2) = &self.l2 {
                    l2.delete_key(&cache_key);
                }
                drop(shard);
                self.stats.expirations.fetch_add(1, Ordering::Relaxed);
                self.stats.l1_idle_evicts.fetch_add(1, Ordering::Relaxed);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Lookup::Stale(entry) => {
                let cache_key = BlobCacheKey::new(namespace, key);
                self.record_removed_entry(&cache_key, &entry);
                drop(shard);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Lookup::Miss => {
                drop(shard);
                let cache_key = BlobCacheKey::new(namespace, key);
                if let Some(pool) = self.promotion_pool.get() {
                    // Async path: do the L2 read (we owe the bytes to the
                    // caller right now) but defer the L1 install onto the
                    // worker pool. Caller does not pay promotion bookkeeping.
                    if let Some(l2) = self.l2.as_ref() {
                        if let Some(entry) = l2.get(&cache_key, now_ms, namespace_generation) {
                            let hit = entry.hit();
                            if self.should_promote_l2_hit(&cache_key, shard_idx) {
                                // Drop the freshly-fetched Entry — the worker will
                                // re-fetch it. Cost: one extra L2 metadata read +
                                // blob read per promoted L2 hit while async mode is on.
                                // Acceptable trade-off for opt-in mode.
                                drop(entry);
                                let request = PromotionRequest {
                                    namespace: cache_key.namespace.clone(),
                                    key: cache_key.key.clone(),
                                    bytes: Arc::clone(hit.bytes()),
                                    policy: BlobCachePolicy::default(),
                                };
                                let _ = pool.schedule(request);
                            }
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

    /// Replace a live entry's expiry with a relative TTL.
    pub fn expire(&self, namespace: &str, key: &str, ttl_ms: u64) -> Result<bool, CacheError> {
        let now_ms = unix_now_ms();
        self.update_expiry(namespace, key, Some(now_ms.saturating_add(ttl_ms)), now_ms)
    }

    /// Replace a live entry's expiry with an absolute Unix-millisecond deadline.
    pub fn expire_at(
        &self,
        namespace: &str,
        key: &str,
        expires_at_unix_ms: u64,
    ) -> Result<bool, CacheError> {
        self.update_expiry(namespace, key, Some(expires_at_unix_ms), unix_now_ms())
    }

    /// Remove expiry from a live entry without rewriting its value or tags.
    pub fn persist(&self, namespace: &str, key: &str) -> Result<bool, CacheError> {
        self.update_expiry(namespace, key, None, unix_now_ms())
    }

    fn update_expiry(
        &self,
        namespace: &str,
        key: &str,
        expires_at_unix_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<bool, CacheError> {
        if !self.namespace_exists(namespace) {
            return Ok(false);
        }
        let cache_key = BlobCacheKey::new(namespace, key);
        let namespace_generation = self.current_generation(namespace);
        let shard_index = self.shard_index(&cache_key);
        let mut shard = self.shards[shard_index].write();
        let l1_live = shard.can_update_expiry(&cache_key, now_ms, namespace_generation);
        let l2_updated = match &self.l2 {
            Some(l2) => {
                l2.update_expiry(&cache_key, namespace_generation, now_ms, expires_at_unix_ms)?
            }
            None => false,
        };
        if l1_live {
            shard.update_expiry(&cache_key, expires_at_unix_ms, now_ms);
        }
        Ok(l1_live || l2_updated)
    }

    fn exists_at(&self, namespace: &str, key: &str, now_ms: u64) -> CachePresence {
        let namespace_generation = self.current_generation(namespace);
        let shard_idx = self.shard_index_parts(namespace, key);
        let mut shard = self.shards[shard_idx].write();
        match shard.contains_by_parts(namespace, key, now_ms, namespace_generation) {
            Lookup::Present => {
                self.stats.hits.fetch_add(1, Ordering::Relaxed);
                CachePresence::Present
            }
            Lookup::Expired(entry) => {
                let cache_key = BlobCacheKey::new(namespace, key);
                self.record_removed_entry(&cache_key, &entry);
                if let Some(l2) = &self.l2 {
                    l2.delete_key(&cache_key);
                }
                drop(shard);
                self.stats.expirations.fetch_add(1, Ordering::Relaxed);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                CachePresence::Absent
            }
            Lookup::IdleEvicted(entry) => {
                let cache_key = BlobCacheKey::new(namespace, key);
                self.record_removed_entry(&cache_key, &entry);
                if let Some(l2) = &self.l2 {
                    l2.delete_key(&cache_key);
                }
                drop(shard);
                self.stats.expirations.fetch_add(1, Ordering::Relaxed);
                self.stats.l1_idle_evicts.fetch_add(1, Ordering::Relaxed);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                CachePresence::Absent
            }
            Lookup::Stale(entry) => {
                let cache_key = BlobCacheKey::new(namespace, key);
                self.record_removed_entry(&cache_key, &entry);
                drop(shard);
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

        if let Some(entry) = removed {
            self.record_invalidated_entry(&cache_key, &entry);
            drop(shard);
            1
        } else {
            drop(shard);
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

        let mut count = 0usize;
        for shard in &self.shards {
            let mut shard = shard.write();
            let keys = shard
                .keys_matching(|key| key.namespace == namespace && key.key.starts_with(prefix));
            for key in keys {
                if let Some(entry) = shard.remove(&key) {
                    self.record_invalidated_entry(&key, &entry);
                    count += 1;
                }
            }
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
        self.namespace_flush_epoch.fetch_add(1, Ordering::Release);
        if let Some(l2) = &self.l2 {
            l2.delete_namespace(namespace);
        }
        self.stats.namespace_flushes.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// RAM this cache occupies right now (ADR 0073 §2, the `blob_cache_l1`
    /// pool): L1 entry payloads plus L2's resident metadata — its B+ tree
    /// pages and Bloom synopsis filters. L2's *disk* extent is not memory and
    /// is excluded; `BlobCacheStats::l2_bytes_in_use` reports it separately.
    pub fn ram_bytes_in_use(&self) -> u64 {
        let l1 = self.bytes_in_use.load(Ordering::Relaxed) as u64;
        let l2 = self
            .l2
            .as_ref()
            .map_or(0, |l2| l2.resident_metadata_bytes());
        l1.saturating_add(l2)
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
            namespace_bytes_max: self.config.namespace_bytes_max,
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

    /// Remove a bounded set of due L1 entries.
    ///
    /// This is the storage primitive behind the cache sweeper. It never
    /// locks more than one shard at a time. TTL work comes from each shard's
    /// deadline index; a rotating scan runs only while a namespace-generation
    /// flush is awaiting physical reclamation.
    pub(crate) fn sweep_expired_l1(
        &self,
        entries_limit: Option<usize>,
        millis_limit: Option<u32>,
    ) -> BlobCacheSweepOutcome {
        let started = Instant::now();
        let time_limit = millis_limit.map(|millis| Duration::from_millis(u64::from(millis)));
        let now_ms = unix_now_ms();
        let hard_entries_limit = entries_limit.unwrap_or(usize::MAX);
        let shard_count = self.shards.len();
        let first_shard = self.sweep_shard_cursor.fetch_add(1, Ordering::Relaxed) % shard_count;
        let flush_epoch = self.namespace_flush_epoch.load(Ordering::Acquire);
        let stale_cleanup_pending =
            self.namespace_reclaimed_epoch.load(Ordering::Acquire) < flush_epoch;
        let mut outcome = BlobCacheSweepOutcome::default();
        let mut completed_all_shards = true;

        for offset in 0..shard_count {
            if outcome.entries_scanned >= hard_entries_limit
                || time_limit.is_some_and(|limit| started.elapsed() >= limit)
            {
                completed_all_shards = false;
                break;
            }
            let shard_index = (first_shard + offset) % shard_count;
            let mut shard = self.shards[shard_index].write();
            let sweep = shard.sweep_expired(
                now_ms,
                hard_entries_limit - outcome.entries_scanned,
                started,
                time_limit,
            );
            // Finish removal bookkeeping under the shard publication lock.
            // Otherwise a concurrent put could publish a fresh value for the
            // same key before the old tag/dependency indexes or L2 record are
            // removed, making the cleanup erase the fresh entry's metadata.
            completed_all_shards &= sweep.completed_cycle;
            self.record_sweep(sweep, &mut outcome);

            if stale_cleanup_pending
                && outcome.entries_scanned < hard_entries_limit
                && time_limit.is_none_or(|limit| started.elapsed() < limit)
            {
                // Keep generations stable while this shard is inspected. A
                // flush cannot then make a fresh generation look stale.
                let namespace_generations = self.namespace_generations.read();
                let stale_sweep = shard.sweep_stale_generations(
                    &namespace_generations,
                    flush_epoch,
                    hard_entries_limit - outcome.entries_scanned,
                    started,
                    time_limit,
                );
                completed_all_shards &= stale_sweep.completed_cycle;
                self.record_sweep(stale_sweep, &mut outcome);
            }
            drop(shard);
        }

        if stale_cleanup_pending
            && self
                .shards
                .iter()
                .all(|shard| shard.read().stale_cleanup_completed(flush_epoch))
        {
            self.namespace_reclaimed_epoch
                .fetch_max(flush_epoch, Ordering::Release);
        }

        if outcome.expirations > 0 {
            let expirations = u64::try_from(outcome.expirations).unwrap_or(u64::MAX);
            self.stats
                .expirations
                .fetch_add(expirations, Ordering::Relaxed);
        }
        outcome.completed = completed_all_shards;
        outcome.elapsed_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        outcome
    }

    /// Remove a bounded set of due L2-only entries from the persistent
    /// deadline projection. The internal backend resolves due ids without a
    /// collection scan; the compatibility file backend reports no work.
    pub(crate) fn sweep_expired_l2(
        &self,
        entries_limit: usize,
        millis_limit: Option<u32>,
    ) -> BlobCacheSweepOutcome {
        let Some(l2) = &self.l2 else {
            return BlobCacheSweepOutcome {
                completed: true,
                ..BlobCacheSweepOutcome::default()
            };
        };
        let outcome = l2.sweep_expired(unix_now_ms(), entries_limit, millis_limit);
        if outcome.expirations > 0 {
            self.stats.expirations.fetch_add(
                u64::try_from(outcome.expirations).unwrap_or(u64::MAX),
                Ordering::Relaxed,
            );
        }
        outcome
    }

    fn record_sweep(&self, sweep: ShardSweep, outcome: &mut BlobCacheSweepOutcome) {
        for (key, entry, _) in &sweep.removed {
            self.record_removed_entry(key, entry);
            if let Some(l2) = &self.l2 {
                l2.delete_key(key);
            }
        }
        outcome.entries_scanned += sweep.entries_scanned;
        for (namespace, scanned) in sweep.scanned_by_namespace {
            outcome
                .by_namespace
                .entry(namespace)
                .or_default()
                .entries_scanned += scanned;
        }
        for (key, entry, expired) in sweep.removed {
            let entry_charge_bytes = u64::try_from(entry.l1_charge_bytes).unwrap_or(u64::MAX);
            let namespace_stats = outcome
                .by_namespace
                .entry(key.namespace.clone())
                .or_default();
            namespace_stats.entries_evicted += 1;
            namespace_stats.bytes_reclaimed = namespace_stats
                .bytes_reclaimed
                .saturating_add(entry_charge_bytes);
            outcome.entries_evicted += 1;
            outcome.expirations += usize::from(expired);
            outcome.bytes_reclaimed = outcome.bytes_reclaimed.saturating_add(entry_charge_bytes);
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
        if self.should_promote_l2_hit(key, shard_idx) {
            self.do_l1_promotion_sync(key, entry, shard_idx);
        }
        Some(hit)
    }

    fn should_promote_l2_hit(&self, key: &BlobCacheKey, shard_idx: usize) -> bool {
        match self.config.l2_promotion_policy {
            L2PromotionPolicy::Immediate => true,
            L2PromotionPolicy::OnSecondHit => self.shards[shard_idx]
                .write()
                .l2_hit_should_promote_on_second_hit(key),
        }
    }

    /// Pure L1 install bookkeeping: shard write-lock, byte accounting,
    /// eviction loop. Extracted so the async promotion pool can call it
    /// from a worker (issue #193, lane 1/5).
    ///
    /// This is intentionally side-effect-only — it does not touch hit/miss
    /// stats (the caller already counted the hit) and does not return the
    /// `BlobCacheHit` (the caller already handed bytes to the user).
    fn do_l1_promotion_sync(&self, key: &BlobCacheKey, entry: Entry, shard_idx: usize) {
        let entry_charge = entry.l1_charge_bytes;
        let mut shard = self.shards[shard_idx].write();
        let outcome = shard.insert(key.clone(), entry);
        let old_charge = outcome
            .old_entry
            .as_ref()
            .map_or(0, |entry| entry.l1_charge_bytes);
        self.adjust_namespace_bytes(&key.namespace, old_charge, entry_charge);
        if entry_charge >= old_charge {
            self.bytes_in_use
                .fetch_add(entry_charge - old_charge, Ordering::Relaxed);
        } else {
            self.bytes_in_use
                .fetch_sub(old_charge - entry_charge, Ordering::Relaxed);
        }
        drop(shard);
        self.evict_namespace_until_within_budget(&key.namespace, shard_idx);
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
        self.namespace_bytes_in_use
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

    pub(crate) fn current_generation(&self, namespace: &str) -> u64 {
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
        self.bytes_in_use
            .fetch_sub(entry.l1_charge_bytes, Ordering::Relaxed);
        self.adjust_namespace_bytes(&key.namespace, entry.l1_charge_bytes, 0);
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

        let mut count = 0usize;
        for (shard_idx, keys) in by_shard {
            let mut shard = self.shards[shard_idx].write();
            for (key, matched_labels) in keys {
                let still_matches = match kind {
                    IndexedKind::Tag => shard.entry_has_any_tag(&key, &matched_labels),
                    IndexedKind::Dependency => {
                        shard.entry_has_any_dependency(&key, &matched_labels)
                    }
                };
                if still_matches {
                    if let Some(entry) = shard.remove(&key) {
                        self.record_invalidated_entry(&key, &entry);
                        count += 1;
                    }
                }
            }
        }
        // L2's stable record format predates tag/dependency metadata. Never
        // allow an L2-only entry to survive a label invalidation and later
        // resurrect. Until that format grows label indexes, conservatively
        // flush the remaining L2 namespace while retaining unrelated L1 hits.
        let l2_count = self
            .l2
            .as_ref()
            .map_or(0, |l2| l2.delete_namespace(namespace));
        if l2_count > 0 {
            self.stats
                .invalidations
                .fetch_add(l2_count as u64, Ordering::Relaxed);
        }
        count.saturating_add(l2_count)
    }

    fn shard_index(&self, key: &BlobCacheKey) -> usize {
        self.shard_index_parts(&key.namespace, &key.key)
    }

    fn shard_index_parts(&self, namespace: &str, key: &str) -> usize {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        namespace.hash(&mut hasher);
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
                    self.stats.evictions.fetch_add(1, Ordering::Relaxed);
                    self.record_removed_entry(&key, &entry);
                    drop(shard);
                    evicted = true;
                    break;
                }
            }
            if !evicted {
                break;
            }
        }
    }

    fn evict_namespace_until_within_budget(&self, namespace: &str, preferred_start: usize) {
        while self.namespace_bytes(namespace) > self.config.namespace_bytes_max {
            let mut evicted = false;
            for offset in 0..self.shards.len() {
                let idx = (preferred_start + offset) % self.shards.len();
                let mut shard = self.shards[idx].write();
                if let Some((key, entry)) = shard.remove_one_from_namespace(namespace) {
                    self.stats.evictions.fetch_add(1, Ordering::Relaxed);
                    self.record_removed_entry(&key, &entry);
                    drop(shard);
                    evicted = true;
                    break;
                }
            }
            if !evicted {
                break;
            }
        }
    }

    fn namespace_bytes(&self, namespace: &str) -> usize {
        self.namespace_bytes_in_use
            .read()
            .get(namespace)
            .copied()
            .unwrap_or(0)
    }

    fn adjust_namespace_bytes(&self, namespace: &str, old_charge: usize, new_charge: usize) {
        let mut usage = self.namespace_bytes_in_use.write();
        let bytes = usage.entry(namespace.to_string()).or_insert(0);
        *bytes = bytes.saturating_sub(old_charge).saturating_add(new_charge);
    }
}

#[derive(Debug, Default)]
pub(crate) struct BlobCacheSweepOutcome {
    pub(crate) entries_scanned: usize,
    pub(crate) entries_evicted: usize,
    pub(crate) expirations: usize,
    pub(crate) bytes_reclaimed: u64,
    pub(crate) elapsed_ms: u32,
    pub(crate) completed: bool,
    pub(crate) by_namespace: BTreeMap<String, BlobCacheNamespaceSweep>,
}

#[derive(Debug, Default)]
pub(crate) struct BlobCacheNamespaceSweep {
    pub(crate) entries_scanned: usize,
    pub(crate) entries_evicted: usize,
    pub(crate) bytes_reclaimed: u64,
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
mod tests;
