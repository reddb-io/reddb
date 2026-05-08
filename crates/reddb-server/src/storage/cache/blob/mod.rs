//! Byte-oriented Blob Cache.
//!
//! This is the first internal tracer for RedDB's exact-key blob cache. It is
//! intentionally L1-only: a sharded, byte-bounded, in-process cache with SIEVE
//! eviction, namespace caps, and opaque content metadata. Durable L2 storage,
//! dependency invalidation, and public APIs land in follow-up slices.

pub mod config;
pub mod entry;

pub use config::*;
use entry::{
    Entry, L2Control, L2Record,
    encode_l2_key, encode_v2_frame, decode_v2_frame,
    L2_BLOB_MAGIC, L2_FORMAT_V1_RAW, L2_FORMAT_V2_FRAMED,
};

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;

use super::compressor::{Compressed, CompressOpts, L2BlobCompressor};
use super::extended_ttl::{EffectiveExpiry, ExpiryDecision, ExtendedTtlPolicy};
use super::promotion_pool::{
    AsyncPromotionPool, PoolOpts, PromotionExecutor, PromotionRequest,
};

/// Test-only thread-local counter of how many times
/// `EffectiveExpiry::compute` is invoked from `Shard::get`. Thread-local
/// (rather than a global atomic) so the off-fast-path test does not race
/// with other tests in the harness's parallel executor.
#[cfg(test)]
thread_local! {
    static EFFECTIVE_EXPIRY_COMPUTE_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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
        self.l2_compression_original_bytes as f64
            / self.l2_compression_stored_bytes as f64
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
            ExpiryDecision::Stale { window_remaining_ms } => {
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
                let bit = (h1.wrapping_add((i as u64).wrapping_mul(h2))
                    % self.bit_count as u64) as usize;
                self.bits[bit / 64] |= 1u64 << (bit % 64);
            }
        }

        pub(super) fn contains(&self, key: &str) -> bool {
            let (h1, h2) = double_hash(key);
            for i in 0..self.hash_count {
                let bit = (h1.wrapping_add((i as u64).wrapping_mul(h2))
                    % self.bit_count as u64) as usize;
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
                self.synopsis_metadata_reads
                    .fetch_add(1, Ordering::Relaxed);
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
    fn inject_v1_entry(
        &self,
        key: &BlobCacheKey,
        payload: &[u8],
    ) -> Result<(), CacheError> {
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


fn rebuild_l2_synopsis(
    metadata: &crate::storage::engine::BTree,
) -> HashMap<String, BloomFilter> {
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
        self.check_version(&shard, &key, input.policy.version_value(), namespace_generation)?;
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
                Arc::clone(self.promotion_pool.get().expect("OnceLock set+get inconsistency"))
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
        let max = policy.max_blob_bytes_value().unwrap_or(self.config.l1_bytes_max);
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
mod tests;

