use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

use super::cache::{BlobCacheKey, CacheError};
use super::config::DEFAULT_BLOB_SYNOPSIS_CAPACITY;
use super::entry::Entry;
use crate::storage::cache::compressor::{Compressed, L2BlobCompressor};
use crate::storage::cache::extended_ttl::ExtendedTtlPolicy;
use crate::storage::primitives::split_block_bloom::SplitBlockBloom;
use reddb_file::{
    blob_cache_control_path, decode_l2_v2_frame, encode_l2_key, encode_l2_v2_frame, L2BlobFrame,
    L2Control, L2Record, L2_BLOB_MAGIC, L2_FORMAT_V1_RAW, L2_FORMAT_V2_FRAMED,
};

/// Split-block Bloom filter for the L2 membership synopsis (#146).
///
/// # Sizing
///
/// The canonical split-block Bloom primitive sizes itself at about 10 bits
/// per entry for an approximately 1% false-positive rate. At the cache default
/// (`n = 10_000`) this allocates 512 split blocks, or 16 KB per namespace.
/// With
/// [`super::config::DEFAULT_BLOB_MAX_NAMESPACES`] = 256 the worst-case
/// synopsis state is ~4 MB — acceptable next to a 256 MB L1 budget.
///
/// # Contract
///
/// - `probe_bytes(key)` returning `false` ALWAYS means absent (no
///   false-negatives).
/// - `probe_bytes(key)` returning `true` means MaybePresent — callers MUST
///   verify against the authoritative L2 metadata B+ tree.
/// - Bits cannot be cleared without losing the no-false-negatives guarantee,
///   so deletes / expirations leave stale bits behind. Stale bits cause extra
///   L2 metadata verifications, never spurious `Present` answers. A periodic
///   full rebuild from the metadata B+ tree (currently startup-only) reclaims
///   that space.
/// - The synopsis is never persisted; it is rebuilt from the L2 metadata B+
///   tree at startup. The canonical byte-key hasher is stable across processes
///   anyway, so rebuild and probe use the same bit mapping.
type L2Synopsis = SplitBlockBloom;

pub(super) struct BlobCacheL2 {
    pager: Arc<crate::storage::engine::Pager>,
    metadata: RwLock<crate::storage::engine::BTree>,
    synopsis: RwLock<HashMap<String, L2Synopsis>>,
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
    /// Number of L2 entries the compressor returned as `Raw`.
    compression_skipped_count: AtomicU64,
    #[cfg(test)]
    fault_after_blob_write: std::sync::atomic::AtomicBool,
}

impl BlobCacheL2 {
    pub(super) fn open(path: PathBuf, bytes_max: u64) -> Result<Self, CacheError> {
        let control_path = blob_cache_control_path(&path);
        let control =
            L2Control::read(&control_path).map_err(|err| CacheError::L2Io(err.to_string()))?;
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

    pub(super) fn get(&self, key: &BlobCacheKey, now_ms: u64, generation: u64) -> Option<Entry> {
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
        let payload = match record.format_version {
            L2_FORMAT_V1_RAW => chain_bytes,
            L2_FORMAT_V2_FRAMED => {
                let framed = decode_compressed_l2_frame(&chain_bytes).ok()?;
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
            slot_index: 0,
            last_access_unix_ms: now_ms,
            extended: ExtendedTtlPolicy::off(),
        })
    }

    pub(super) fn put(
        &self,
        key: &BlobCacheKey,
        entry: &Entry,
        old_entry_size: u64,
        compressed: Compressed,
    ) -> Result<(), CacheError> {
        let original_len = entry.size as u64;
        let stored_len = compressed.stored_len() as u64;
        let was_compressed = compressed.is_compressed();
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

        let framed = encode_compressed_l2_frame(&compressed);
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
        control
            .write(&self.control_path)
            .map_err(|err| CacheError::L2Io(err.to_string()))?;
        self.add_synopsis_key(&key.namespace, &key.key);
        if was_compressed {
            self.compression_compressed_count
                .fetch_add(1, Ordering::Relaxed);
            self.compression_original_bytes_sum
                .fetch_add(original_len, Ordering::Relaxed);
            self.compression_stored_bytes_sum
                .fetch_add(stored_len, Ordering::Relaxed);
            self.compression_bytes_saved
                .fetch_add(original_len.saturating_sub(stored_len), Ordering::Relaxed);
        } else {
            self.compression_skipped_count
                .fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }

    pub(super) fn record_size(&self, key: &BlobCacheKey) -> u64 {
        let encoded_key = encode_l2_key(&key.namespace, &key.key);
        self.metadata
            .read()
            .get(&encoded_key)
            .ok()
            .flatten()
            .and_then(|bytes| L2Record::decode(&bytes).ok())
            .map_or(0, |record| record.byte_len)
    }

    pub(super) fn delete_key(&self, key: &BlobCacheKey) -> Option<u64> {
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

    pub(super) fn delete_namespace(&self, namespace: &str) -> usize {
        self.delete_where(|record| record.namespace == namespace)
    }

    pub(super) fn has_namespace(&self, namespace: &str) -> bool {
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

    pub(super) fn delete_prefix(&self, namespace: &str, prefix: &str) -> usize {
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

    pub(super) fn stats_bytes_in_use(&self) -> u64 {
        self.bytes_in_use.load(Ordering::Relaxed)
    }

    pub(super) fn stats_metadata_reads(&self) -> u64 {
        self.metadata_reads.load(Ordering::Relaxed)
    }

    pub(super) fn stats_negative_skips(&self) -> u64 {
        self.negative_skips.load(Ordering::Relaxed)
    }

    pub(super) fn stats_synopsis_metadata_reads(&self) -> u64 {
        self.synopsis_metadata_reads.load(Ordering::Relaxed)
    }

    pub(super) fn stats_synopsis_bytes(&self) -> u64 {
        self.synopsis.read().values().map(synopsis_heap_bytes).sum()
    }

    /// RAM held by L2 (ADR 0073 §2): the resident pages of the metadata B+
    /// tree plus the per-namespace Bloom synopsis filters. The disk extent is
    /// not memory and is deliberately excluded — `stats_bytes_in_use` reports
    /// that separately.
    pub(super) fn resident_metadata_bytes(&self) -> u64 {
        let metadata_pages = self.pager.cache_len() as u64;
        metadata_pages
            .saturating_mul(crate::storage::memory_pools::PAGE_CACHE_PAGE_SIZE_BYTES)
            .saturating_add(self.stats_synopsis_bytes())
    }

    pub(super) fn stats_compression_original_bytes(&self) -> u64 {
        self.compression_original_bytes_sum.load(Ordering::Relaxed)
    }

    pub(super) fn stats_compression_stored_bytes(&self) -> u64 {
        self.compression_stored_bytes_sum.load(Ordering::Relaxed)
    }

    pub(super) fn stats_compression_skipped_total(&self) -> u64 {
        self.compression_skipped_count.load(Ordering::Relaxed)
    }

    pub(super) fn stats_bytes_saved_total(&self) -> u64 {
        self.compression_bytes_saved.load(Ordering::Relaxed)
    }

    pub(super) fn synopsis_may_contain(&self, namespace: &str, key: &str) -> bool {
        self.synopsis
            .read()
            .get(namespace)
            .is_some_and(|filter| filter.probe_bytes(key.as_bytes()))
    }

    fn add_synopsis_key(&self, namespace: &str, key: &str) {
        self.synopsis
            .write()
            .entry(namespace.to_string())
            .or_insert_with(|| L2Synopsis::with_capacity(DEFAULT_BLOB_SYNOPSIS_CAPACITY))
            .insert_bytes(key.as_bytes());
    }

    #[cfg(test)]
    pub(super) fn inject_synopsis_maybe_present(&self, namespace: &str, key: &str) {
        self.add_synopsis_key(namespace, key);
    }

    #[cfg(test)]
    pub(super) fn inject_fault_after_blob_write_once(&self) {
        self.fault_after_blob_write
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Test-only escape hatch that synthesises a legacy `V1Raw` L2 entry.
    #[cfg(test)]
    pub(super) fn inject_v1_entry(
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
        control
            .write(&self.control_path)
            .map_err(|err| CacheError::L2Io(err.to_string()))?;
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

fn synopsis_heap_bytes(filter: &L2Synopsis) -> u64 {
    (filter.num_blocks() * 32) as u64
}

fn rebuild_l2_synopsis(metadata: &crate::storage::engine::BTree) -> HashMap<String, L2Synopsis> {
    let mut synopsis: HashMap<String, L2Synopsis> = HashMap::new();
    let Ok(mut cursor) = metadata.cursor_first() else {
        return synopsis;
    };
    while let Ok(Some((_, value))) = cursor.next() {
        if let Ok(record) = L2Record::decode(&value) {
            synopsis
                .entry(record.namespace)
                .or_insert_with(|| L2Synopsis::with_capacity(DEFAULT_BLOB_SYNOPSIS_CAPACITY))
                .insert_bytes(record.key.as_bytes());
        }
    }
    synopsis
}

fn encode_compressed_l2_frame(compressed: &Compressed) -> Vec<u8> {
    let frame = match compressed {
        Compressed::Raw(bytes) => L2BlobFrame::Raw(bytes.clone()),
        Compressed::Zstd {
            bytes,
            original_len,
        } => L2BlobFrame::Zstd {
            bytes: bytes.clone(),
            original_len: *original_len,
        },
    };
    encode_l2_v2_frame(&frame)
}

fn decode_compressed_l2_frame(bytes: &[u8]) -> Result<Compressed, CacheError> {
    match decode_l2_v2_frame(bytes).map_err(|err| CacheError::L2Io(err.to_string()))? {
        L2BlobFrame::Raw(bytes) => Ok(Compressed::Raw(bytes)),
        L2BlobFrame::Zstd {
            bytes,
            original_len,
        } => Ok(Compressed::Zstd {
            bytes,
            original_len,
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::super::{
        BlobCache, BlobCacheConfig, BlobCachePolicy, BlobCachePut, CachePresence, L1Admission,
        L2Compression,
    };
    use super::DEFAULT_BLOB_SYNOPSIS_CAPACITY;
    use crate::storage::primitives::split_block_bloom::SplitBlockBloom;

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

    fn cleanup_l2(path: &Path) {
        // Remove the L2 `.rdb` AND every sidecar so nothing is left for the leak
        // guard (scripts/check-temp-residue.sh). The blob-cache L2 file format is
        // owned by reddb-file, so derive each sidecar path through its
        // authoritative helpers rather than re-declaring suffix literals here
        // (enforced by `server_does_not_redeclare_blob_cache_l2_file_format`).
        // Callers must drop the cache BEFORE calling this so the pager's
        // drop-time flush cannot re-create a sidecar after removal.
        let control = reddb_file::blob_cache_control_path(path);
        for sidecar in [
            path.to_path_buf(),
            reddb_file::layout::pager_dwb_path(path),
            reddb_file::blob_cache_double_write_path(path),
            reddb_file::blob_cache_control_temp_path(&control),
            control,
        ] {
            let _ = std::fs::remove_file(sidecar);
        }
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

    fn l2_cache_with_compression(path: &Path, mode: L2Compression) -> BlobCache {
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

    #[test]
    fn l2_round_trip_rehydrates_after_reopen_without_json_rows() {
        let path = l2_path("reopen-l2-module");
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
        cleanup_l2(&path);
    }

    #[test]
    fn l2_synopsis_reports_split_block_heap_bytes() {
        let path = l2_path("synopsis-split-block-bytes-l2-module");
        let cache = l2_cache(&path);
        cache
            .put(
                "n",
                "known",
                BlobCachePut::new(b"known".to_vec())
                    .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never)),
            )
            .unwrap();

        let expected =
            SplitBlockBloom::with_capacity(DEFAULT_BLOB_SYNOPSIS_CAPACITY).num_blocks() as u64 * 32;
        assert_eq!(expected, 16 * 1024);
        assert_eq!(cache.stats().synopsis_bytes, expected);

        drop(cache);
        cleanup_l2(&path);
    }

    #[test]
    fn l2_synopsis_rebuilds_from_metadata_on_reopen() {
        let path = l2_path("synopsis-rebuild-l2-module");
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
        cleanup_l2(&path);
    }

    #[test]
    fn l2_synopsis_rebuilds_filter_with_same_hit_rate_after_reopen() {
        let path = l2_path("synopsis-bloom-rebuild-l2-module");
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
        for key in &live {
            assert!(matches!(
                cache.exists("n", key),
                CachePresence::Present | CachePresence::MaybePresent
            ));
        }
        let negatives: Vec<String> = (0..5_000).map(|i| format!("never-{i}")).collect();
        let mut maybe_or_present = 0usize;
        for key in &negatives {
            if !matches!(cache.exists("n", key), CachePresence::Absent) {
                maybe_or_present += 1;
            }
        }
        assert_eq!(maybe_or_present, 0);

        drop(cache);
        cleanup_l2(&path);
    }

    #[test]
    fn l2_round_trip_compresses_text_payload_and_returns_original_bytes() {
        let path = l2_path("compression-text-l2-module");
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

        let hit = cache.get("n", "doc").expect("L2 hit");
        assert_eq!(&*hit.bytes, &payload[..]);

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

        drop(cache);
        cleanup_l2(&path);
    }

    #[test]
    fn l2_round_trip_with_compression_off_stores_raw_bytes() {
        let path = l2_path("compression-off-l2-module");
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
        assert_eq!(stats.l2_bytes_in_use, payload.len() as u64);
        assert_eq!(stats.l2_compression_skipped_total(), 1);
        assert_eq!(stats.l2_bytes_saved_total(), 0);
        assert_eq!(stats.l2_compression_ratio_observed(), 1.0);

        drop(cache);
        cleanup_l2(&path);
    }

    #[test]
    fn l2_round_trip_with_image_content_type_stores_raw() {
        let path = l2_path("compression-image-ct-l2-module");
        let cache = l2_cache_with_compression(&path, L2Compression::On);
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

        drop(cache);
        cleanup_l2(&path);
    }

    #[test]
    fn l2_round_trip_with_high_entropy_payload_falls_back_to_raw_via_ratio_gate() {
        let path = l2_path("compression-entropy-l2-module");
        let cache = l2_cache_with_compression(&path, L2Compression::On);
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

        drop(cache);
        cleanup_l2(&path);
    }
}
