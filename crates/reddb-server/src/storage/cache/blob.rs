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
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobCacheConfig {
    pub l1_bytes_max: usize,
    pub l2_bytes_max: u64,
    pub l2_path: Option<PathBuf>,
    pub max_namespaces: usize,
    pub shard_count: usize,
    pub content_metadata_keys_max: usize,
    pub content_metadata_bytes_max: usize,
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
        }
    }
}

impl BlobCacheConfig {
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
    pub bytes: Arc<[u8]>,
    pub content_metadata: BTreeMap<String, String>,
    pub version: Option<u64>,
}

impl BlobCacheHit {
    fn new(
        bytes: Arc<[u8]>,
        content_metadata: BTreeMap<String, String>,
        version: Option<u64>,
    ) -> Self {
        Self {
            bytes,
            content_metadata,
            version,
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlobCachePolicy {
    pub ttl_ms: Option<u64>,
    pub expires_at_unix_ms: Option<u64>,
    pub max_blob_bytes: Option<usize>,
    pub l1_admission: L1Admission,
    pub priority: u8,
    pub version: Option<u64>,
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
        }
    }
}

impl BlobCachePolicy {
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
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlobCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub insertions: u64,
    pub evictions: u64,
    pub expirations: u64,
    pub invalidations: u64,
    pub namespace_flushes: u64,
    pub version_mismatches: u64,
    pub entries: usize,
    pub bytes_in_use: usize,
    pub l1_bytes_max: usize,
    pub l2_bytes_in_use: u64,
    pub l2_bytes_max: u64,
    pub l2_full_rejections: u64,
    pub l2_metadata_reads: u64,
    pub l2_negative_skips: u64,
    pub namespaces: usize,
    pub max_namespaces: usize,
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
    ) -> Self {
        let size = bytes.len();
        Self {
            bytes: Arc::<[u8]>::from(bytes),
            content_metadata,
            tags,
            dependencies,
            size,
            visited: true,
            expires_at_unix_ms: effective_expires_at_unix_ms(policy, now_ms),
            priority: policy.priority,
            version: policy.version,
            namespace_generation,
        }
    }

    fn hit(&self) -> BlobCacheHit {
        BlobCacheHit::new(
            Arc::clone(&self.bytes),
            self.content_metadata.clone(),
            self.version,
        )
    }

    fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_unix_ms
            .is_some_and(|expires_at| now_ms >= expires_at)
    }
}

fn effective_expires_at_unix_ms(policy: BlobCachePolicy, now_ms: u64) -> Option<u64> {
    match (policy.ttl_ms, policy.expires_at_unix_ms) {
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
        if entry.is_expired_at(now_ms) {
            let removed = self.remove(key).expect("entry exists");
            return Lookup::Expired(removed);
        }
        entry.visited = true;
        Lookup::Hit(entry.hit())
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
        })
    }

    fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_unix_ms
            .is_some_and(|expires_at| now_ms >= expires_at)
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

struct BlobCacheL2 {
    pager: Arc<crate::storage::engine::Pager>,
    metadata: RwLock<crate::storage::engine::BTree>,
    synopsis: RwLock<HashMap<String, HashSet<String>>>,
    control: RwLock<L2Control>,
    control_path: PathBuf,
    bytes_in_use: AtomicU64,
    metadata_reads: AtomicU64,
    negative_skips: AtomicU64,
    bytes_max: u64,
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
            bytes_max,
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
        let record = self
            .metadata
            .read()
            .get(&encoded_key)
            .ok()
            .flatten()
            .and_then(|bytes| L2Record::decode(&bytes).ok())?;
        if record.namespace_generation != generation || record.is_expired_at(now_ms) {
            let _ = self.delete_key(key);
            return None;
        }
        let bytes = self.read_blob_chain(record.root_page).ok()?;
        if crate::storage::engine::crc32(&bytes) != record.checksum {
            return None;
        }
        Some(Entry {
            size: bytes.len(),
            bytes: Arc::<[u8]>::from(bytes),
            content_metadata: BTreeMap::new(),
            tags: BTreeSet::new(),
            dependencies: BTreeSet::new(),
            visited: true,
            expires_at_unix_ms: record.expires_at_unix_ms,
            priority: record.priority,
            version: record.version,
            namespace_generation: record.namespace_generation,
        })
    }

    fn put(
        &self,
        key: &BlobCacheKey,
        entry: &Entry,
        old_entry_size: u64,
    ) -> Result<(), CacheError> {
        let new_size = entry.size as u64;
        let current = self.bytes_in_use.load(Ordering::Relaxed);
        let projected = current
            .saturating_sub(old_entry_size)
            .saturating_add(new_size);
        if projected > self.bytes_max {
            return Err(CacheError::L2Full {
                size: projected,
                max: self.bytes_max,
            });
        }

        let (root_page, page_count, checksum) = self.write_blob_chain(&entry.bytes)?;
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
            byte_len: new_size,
            checksum,
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

    fn synopsis_may_contain(&self, namespace: &str, key: &str) -> bool {
        self.synopsis
            .read()
            .get(namespace)
            .is_some_and(|keys| keys.contains(key))
    }

    fn add_synopsis_key(&self, namespace: &str, key: &str) {
        self.synopsis
            .write()
            .entry(namespace.to_string())
            .or_default()
            .insert(key.to_string());
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

fn rebuild_l2_synopsis(
    metadata: &crate::storage::engine::BTree,
) -> HashMap<String, HashSet<String>> {
    let mut synopsis: HashMap<String, HashSet<String>> = HashMap::new();
    let Ok(mut cursor) = metadata.cursor_first() else {
        return synopsis;
    };
    while let Ok(Some((_, value))) = cursor.next() {
        if let Ok(record) = L2Record::decode(&value) {
            synopsis
                .entry(record.namespace)
                .or_default()
                .insert(record.key);
        }
    }
    synopsis
}

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
}

impl BlobCache {
    pub fn new(config: BlobCacheConfig) -> Self {
        let config = BlobCacheConfig {
            shard_count: config.shard_count.max(1),
            ..config
        };
        let l2 = config
            .l2_path
            .clone()
            .map(|path| BlobCacheL2::open(path, config.l2_bytes_max))
            .transpose()
            .expect("open blob-cache L2");
        let shards = (0..config.shard_count)
            .map(|_| RwLock::new(Shard::new()))
            .collect();
        Self {
            config,
            shards,
            namespaces: RwLock::new(HashSet::new()),
            namespace_generations: RwLock::new(HashMap::new()),
            tag_index: RwLock::new(HashMap::new()),
            dependency_index: RwLock::new(HashMap::new()),
            l2: l2.map(Arc::new),
            bytes_in_use: AtomicUsize::new(0),
            stats: AtomicStats::new(),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(BlobCacheConfig::default())
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
        self.check_version(&shard, &key, input.policy.version, namespace_generation)?;
        let entry = Entry::new(
            input.bytes,
            input.content_metadata,
            input.tags,
            input.dependencies,
            input.policy,
            namespace_generation,
            now_ms,
        );
        let entry_size = entry.size;
        if let Some(l2) = &self.l2 {
            let old_l2_size = l2.record_size(&key);
            match l2.put(&key, &entry, old_l2_size) {
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
        let outcome = if matches!(input.policy.l1_admission, L1Admission::Never) {
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
            Lookup::Stale(entry) => {
                drop(shard);
                self.record_removed_entry(&cache_key, &entry);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
            Lookup::Miss => {
                drop(shard);
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

    pub fn exists(&self, namespace: &str, key: &str) -> bool {
        self.exists_at(namespace, key, unix_now_ms())
    }

    fn exists_at(&self, namespace: &str, key: &str, now_ms: u64) -> bool {
        let cache_key = BlobCacheKey::new(namespace, key);
        let namespace_generation = self.current_generation(namespace);
        let shard_idx = self.shard_index(&cache_key);
        let mut shard = self.shards[shard_idx].write();
        match shard.contains(&cache_key, now_ms, namespace_generation) {
            Lookup::Present => {
                self.stats.hits.fetch_add(1, Ordering::Relaxed);
                true
            }
            Lookup::Expired(entry) => {
                drop(shard);
                self.record_removed_entry(&cache_key, &entry);
                if let Some(l2) = &self.l2 {
                    l2.delete_key(&cache_key);
                }
                self.stats.expirations.fetch_add(1, Ordering::Relaxed);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                false
            }
            Lookup::Stale(entry) => {
                drop(shard);
                self.record_removed_entry(&cache_key, &entry);
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                false
            }
            Lookup::Miss => {
                drop(shard);
                if self
                    .rehydrate_l2_entry(&cache_key, now_ms, namespace_generation, shard_idx)
                    .is_some()
                {
                    self.stats.hits.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                false
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

    /// Node-local invalidation for all entries carrying `tag`.
    pub fn invalidate_tag(&self, namespace: &str, tag: &str) -> usize {
        self.invalidate_indexed(namespace, tag, IndexedKind::Tag)
    }

    /// Node-local invalidation for all entries carrying `dependency`.
    pub fn invalidate_dependency(&self, namespace: &str, dependency: &str) -> usize {
        self.invalidate_indexed(namespace, dependency, IndexedKind::Dependency)
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
            namespaces: self.namespaces.read().len(),
            max_namespaces: self.config.max_namespaces,
        }
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

    fn validate_blob_size(&self, size: usize, policy: BlobCachePolicy) -> Result<(), CacheError> {
        let max = policy.max_blob_bytes.unwrap_or(self.config.l1_bytes_max);
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
        Some(hit)
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

    fn invalidate_indexed(&self, namespace: &str, label: &str, kind: IndexedKind) -> usize {
        if !self.namespace_exists(namespace) {
            return 0;
        }
        let scoped = ScopedLabel::new(namespace, label);
        let candidates = match kind {
            IndexedKind::Tag => self.tag_index.read().get(&scoped).cloned(),
            IndexedKind::Dependency => self.dependency_index.read().get(&scoped).cloned(),
        };
        let Some(candidates) = candidates else {
            return 0;
        };

        let mut removed = Vec::new();
        for key in candidates {
            let shard_idx = self.shard_index(&key);
            let mut shard = self.shards[shard_idx].write();
            let matches_label = shard.entries.get(&key).is_some_and(|entry| match kind {
                IndexedKind::Tag => entry.tags.contains(label),
                IndexedKind::Dependency => entry.dependencies.contains(label),
            });
            if matches_label {
                if let Some(entry) = shard.remove(&key) {
                    removed.push((key, entry));
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
        BlobCache::new(
            BlobCacheConfig::default()
                .with_l1_bytes_max(128)
                .with_shard_count(1)
                .with_max_namespaces(4)
                .with_l2_path(path),
        )
    }

    #[test]
    fn put_get_and_exists_round_trip_blob() {
        let cache = small_cache(128);
        cache
            .put("images", "hero", BlobCachePut::new(vec![1, 2, 3]))
            .expect("put");

        assert!(cache.exists("images", "hero"));
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
        assert!(!cache.exists("images", "missing"));
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
        assert!(!cache.exists_at("n", "ttl", 1_011));

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

        assert_eq!(cache.invalidate_tag("n", "hot"), 1);
        assert!(cache.get("n", "tagged").is_none());
        assert_eq!(cache.invalidate_dependency("n", "row:42"), 1);
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
        assert_eq!(cache.invalidate_tag("n", "cold"), 0);
        assert_eq!(cache.invalidate_dependency("n", "row:missing"), 0);
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
        assert!(!cache.exists("n", "b"));
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
        let cache = BlobCache::new(
            BlobCacheConfig::default()
                .with_l1_bytes_max(128)
                .with_shard_count(1)
                .with_l2_bytes_max(2)
                .with_l2_path(&path),
        );
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

        assert!(!cache.exists("n", "deleted"));
        let stats = cache.stats();
        assert_eq!(stats.l2_metadata_reads, 1);

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

        assert!(!cache.exists_at("n", "expired", 1_010));
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
            assert!(!cache.exists("n", &key));
        }
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
    }
}
