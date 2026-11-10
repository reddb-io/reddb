use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use reddb_file::{
    decode_l2_v2_frame, encode_l2_key, encode_l2_v2_frame, L2BlobFrame, L2Record,
    L2_FORMAT_V2_FRAMED,
};
use reddb_types::Value;

use super::cache::{BlobCacheKey, CacheError};
use super::config::DEFAULT_BLOB_SYNOPSIS_CAPACITY;
use super::entry::Entry;
use super::l2_backend::BlobCacheL2Backend;
use crate::storage::cache::compressor::{Compressed, L2BlobCompressor};
use crate::storage::cache::extended_ttl::ExtendedTtlPolicy;
use crate::storage::primitives::split_block_bloom::SplitBlockBloom;
use crate::storage::unified::store::RowPublication;
use crate::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity, UnifiedStore};

const COLLECTION: &str = "red.cache.blob_l2";
const CONTROL_KEY: &str = "__control";
const RECORD_MAGIC: &[u8; 4] = b"RLC1";
const RECORD_HEADER_BYTES: usize = 8;
const DELETE_BATCH: usize = 256;

type L2Synopsis = SplitBlockBloom;

struct StoredRecord {
    id: EntityId,
    record: L2Record,
    framed: Vec<u8>,
}

/// Single-file L2 backend stored as an internal collection in the owning
/// UnifiedStore. Record replacement and the byte counter are published in one
/// WAL batch, so restart never trusts a counter older than the cache rows.
pub(super) struct StoreBlobCacheL2 {
    store: Arc<UnifiedStore>,
    operation: Mutex<()>,
    synopsis: RwLock<HashMap<String, L2Synopsis>>,
    synopsis_complete: AtomicBool,
    bytes_in_use: AtomicU64,
    bytes_max: u64,
    metadata_reads: AtomicU64,
    negative_skips: AtomicU64,
    synopsis_metadata_reads: AtomicU64,
    compression_original_bytes_sum: AtomicU64,
    compression_stored_bytes_sum: AtomicU64,
    compression_bytes_saved: AtomicU64,
    compression_skipped_count: AtomicU64,
}

impl StoreBlobCacheL2 {
    pub(super) fn open(store: Arc<UnifiedStore>, bytes_max: u64) -> Result<Self, CacheError> {
        let manager = store.get_collection(COLLECTION);
        let row_count = manager.as_ref().map_or(0, |manager| manager.count());
        let control = find_value(&store, CONTROL_KEY).and_then(|(_, value)| match value {
            Value::UnsignedInteger(bytes) => Some(bytes),
            _ => None,
        });
        // Missing control beside data indicates an interrupted pre-transactional
        // prototype or manual damage. This is a rare recovery path; scan once
        // rather than undercounting and escaping the configured quota.
        let bytes_in_use = control.unwrap_or_else(|| {
            manager.as_ref().map_or(0, |manager| {
                let mut total = 0u64;
                manager.for_each_entity(|entity| {
                    if let Some((record, _)) = decode_entity_record(entity) {
                        total = total.saturating_add(record.byte_len);
                    }
                    true
                });
                total
            })
        });
        let data_rows = row_count.saturating_sub(usize::from(control.is_some()));
        Ok(Self {
            store,
            operation: Mutex::new(()),
            synopsis: RwLock::new(HashMap::new()),
            // A non-empty reopened cache skips an O(N) cold-start rebuild.
            // Until a future background rebuild completes, `may_contain`
            // conservatively returns true and exact key lookup remains correct.
            synopsis_complete: AtomicBool::new(data_rows == 0),
            bytes_in_use: AtomicU64::new(bytes_in_use),
            bytes_max,
            metadata_reads: AtomicU64::new(0),
            negative_skips: AtomicU64::new(0),
            synopsis_metadata_reads: AtomicU64::new(0),
            compression_original_bytes_sum: AtomicU64::new(0),
            compression_stored_bytes_sum: AtomicU64::new(0),
            compression_bytes_saved: AtomicU64::new(0),
            compression_skipped_count: AtomicU64::new(0),
        })
    }

    fn find_record(&self, key: &BlobCacheKey) -> Option<StoredRecord> {
        let storage_key = storage_key(key);
        let (id, value) = find_value(&self.store, &storage_key)?;
        let Value::Blob(encoded) = value else {
            return None;
        };
        let (record, framed) = decode_store_record(&encoded).ok()?;
        Some(StoredRecord {
            id,
            record,
            framed: framed.to_vec(),
        })
    }

    fn publish_record(
        &self,
        key: &BlobCacheKey,
        encoded: Vec<u8>,
        previous: Option<EntityId>,
        bytes_in_use: u64,
    ) -> Result<(), CacheError> {
        let control = find_value(&self.store, CONTROL_KEY).map(|(id, _)| id);
        let mut deletes = Vec::with_capacity(2);
        deletes.extend(previous.map(|id| (COLLECTION.to_string(), id)));
        deletes.extend(control.map(|id| (COLLECTION.to_string(), id)));
        self.store
            .publish_rows(RowPublication {
                inserts: vec![
                    (
                        COLLECTION.to_string(),
                        cache_row(&storage_key(key), Value::Blob(encoded)),
                        None,
                    ),
                    (
                        COLLECTION.to_string(),
                        cache_row(CONTROL_KEY, Value::UnsignedInteger(bytes_in_use)),
                        None,
                    ),
                ],
                deletes,
            })
            .map_err(store_error)
    }

    fn publish_record_without_counter(
        &self,
        key: &BlobCacheKey,
        encoded: Vec<u8>,
        previous: EntityId,
    ) -> Result<(), CacheError> {
        self.store
            .publish_rows(RowPublication {
                inserts: vec![(
                    COLLECTION.to_string(),
                    cache_row(&storage_key(key), Value::Blob(encoded)),
                    None,
                )],
                deletes: vec![(COLLECTION.to_string(), previous)],
            })
            .map_err(store_error)
    }

    fn publish_deletions(&self, ids: Vec<EntityId>, bytes_in_use: u64) -> Result<(), CacheError> {
        let control = find_value(&self.store, CONTROL_KEY).map(|(id, _)| id);
        let mut deletes: Vec<_> = ids
            .into_iter()
            .map(|id| (COLLECTION.to_string(), id))
            .collect();
        deletes.extend(control.map(|id| (COLLECTION.to_string(), id)));
        self.store
            .publish_rows(RowPublication {
                inserts: vec![(
                    COLLECTION.to_string(),
                    cache_row(CONTROL_KEY, Value::UnsignedInteger(bytes_in_use)),
                    None,
                )],
                deletes,
            })
            .map_err(store_error)
    }

    fn delete_key_locked(&self, key: &BlobCacheKey) -> Result<Option<u64>, CacheError> {
        let Some(stored) = self.find_record(key) else {
            return Ok(None);
        };
        let new_bytes = self
            .bytes_in_use
            .load(Ordering::Relaxed)
            .saturating_sub(stored.record.byte_len);
        self.publish_deletions(vec![stored.id], new_bytes)?;
        self.bytes_in_use.store(new_bytes, Ordering::Relaxed);
        Ok(Some(stored.record.byte_len))
    }

    fn evict_until_fits(
        &self,
        protected_key: &BlobCacheKey,
        old_size: u64,
        stored_len: u64,
    ) -> Result<(), CacheError> {
        loop {
            let current = self.bytes_in_use.load(Ordering::Relaxed);
            if current.saturating_sub(old_size).saturating_add(stored_len) <= self.bytes_max {
                return Ok(());
            }
            let mut ids = Vec::new();
            let mut reclaimed = 0u64;
            self.visit_records(|id, record| {
                if record.namespace == protected_key.namespace && record.key == protected_key.key {
                    return true;
                }
                ids.push(id);
                reclaimed = reclaimed.saturating_add(record.byte_len);
                ids.len() < DELETE_BATCH
                    && current
                        .saturating_sub(old_size)
                        .saturating_sub(reclaimed)
                        .saturating_add(stored_len)
                        > self.bytes_max
            });
            if ids.is_empty() {
                return Ok(());
            }
            let new_bytes = current.saturating_sub(reclaimed);
            self.publish_deletions(ids, new_bytes)?;
            self.bytes_in_use.store(new_bytes, Ordering::Relaxed);
        }
    }

    fn delete_where(&self, predicate: impl Fn(&L2Record) -> bool) -> usize {
        let _operation = self.operation.lock();
        let mut removed = 0usize;
        loop {
            let mut ids = Vec::new();
            let mut reclaimed = 0u64;
            self.visit_records(|id, record| {
                if predicate(&record) {
                    ids.push(id);
                    reclaimed = reclaimed.saturating_add(record.byte_len);
                }
                ids.len() < DELETE_BATCH
            });
            if ids.is_empty() {
                break;
            }
            let batch_len = ids.len();
            let current = self.bytes_in_use.load(Ordering::Relaxed);
            let new_bytes = current.saturating_sub(reclaimed);
            if self.publish_deletions(ids, new_bytes).is_err() {
                break;
            }
            self.bytes_in_use.store(new_bytes, Ordering::Relaxed);
            removed = removed.saturating_add(batch_len);
        }
        removed
    }

    fn visit_records(&self, mut visit: impl FnMut(EntityId, L2Record) -> bool) {
        let Some(manager) = self.store.get_collection(COLLECTION) else {
            return;
        };
        manager.for_each_entity(|entity| {
            let Some((record, _)) = decode_entity_record(entity) else {
                return true;
            };
            visit(entity.id, record)
        });
    }

    fn add_synopsis_key(&self, namespace: &str, key: &str) {
        self.synopsis
            .write()
            .entry(namespace.to_string())
            .or_insert_with(|| L2Synopsis::with_capacity(DEFAULT_BLOB_SYNOPSIS_CAPACITY))
            .insert_bytes(key.as_bytes());
    }
}

impl BlobCacheL2Backend for StoreBlobCacheL2 {
    fn get(&self, key: &BlobCacheKey, now_ms: u64, generation: u64) -> Option<Entry> {
        if !self.synopsis_may_contain(&key.namespace, &key.key) {
            self.negative_skips.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let _operation = self.operation.lock();
        self.metadata_reads.fetch_add(1, Ordering::Relaxed);
        let Some(stored) = self.find_record(key) else {
            self.synopsis_metadata_reads.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        if stored.record.namespace_generation != generation || stored.record.is_expired_at(now_ms) {
            let _ = self.delete_key_locked(key);
            return None;
        }
        if crate::storage::engine::crc32(&stored.framed) != stored.record.checksum {
            return None;
        }
        let compressed = decode_compressed_frame(&stored.framed).ok()?;
        let payload = L2BlobCompressor::decompress(&compressed).ok()?;
        let size = payload.len();
        Some(Entry {
            l1_charge_bytes: Entry::estimate_l1_charge_bytes(
                size,
                &key.namespace,
                &key.key,
                &BTreeMap::new(),
                &BTreeSet::new(),
                &BTreeSet::new(),
            ),
            size,
            bytes: Arc::<[u8]>::from(payload),
            content_metadata: None,
            tags: BTreeSet::new(),
            dependencies: BTreeSet::new(),
            visited: AtomicBool::new(true),
            expires_at_unix_ms: stored.record.expires_at_unix_ms,
            priority: stored.record.priority,
            version: stored.record.version,
            namespace_generation: stored.record.namespace_generation,
            slot_index: 0,
            last_access_unix_ms: now_ms,
            extended: ExtendedTtlPolicy::off(),
        })
    }

    fn put(
        &self,
        key: &BlobCacheKey,
        entry: &Entry,
        _old_entry_size: u64,
        compressed: Compressed,
    ) -> Result<(), CacheError> {
        let _operation = self.operation.lock();
        let old = self.find_record(key);
        let old_size = old.as_ref().map_or(0, |stored| stored.record.byte_len);
        let stored_len = compressed.stored_len() as u64;
        if stored_len > self.bytes_max {
            return Err(CacheError::L2Full {
                size: stored_len,
                max: self.bytes_max,
            });
        }
        self.evict_until_fits(key, old_size, stored_len)?;
        let current = self.bytes_in_use.load(Ordering::Relaxed);
        let projected = current.saturating_sub(old_size).saturating_add(stored_len);
        if projected > self.bytes_max {
            return Err(CacheError::L2Full {
                size: projected,
                max: self.bytes_max,
            });
        }

        let framed = encode_compressed_frame(&compressed);
        let record = L2Record {
            namespace: key.namespace.clone(),
            key: key.key.clone(),
            expires_at_unix_ms: entry.expires_at_unix_ms,
            namespace_generation: entry.namespace_generation,
            priority: entry.priority,
            version: entry.version,
            root_page: 0,
            page_count: 0,
            byte_len: stored_len,
            checksum: crate::storage::engine::crc32(&framed),
            format_version: L2_FORMAT_V2_FRAMED,
        };
        self.publish_record(
            key,
            encode_store_record(&record, &framed),
            old.as_ref().map(|stored| stored.id),
            projected,
        )?;
        self.bytes_in_use.store(projected, Ordering::Relaxed);
        self.add_synopsis_key(&key.namespace, &key.key);

        let original_len = entry.size as u64;
        if compressed.is_compressed() {
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

    fn record_size(&self, key: &BlobCacheKey) -> u64 {
        let _operation = self.operation.lock();
        self.find_record(key)
            .map_or(0, |stored| stored.record.byte_len)
    }

    fn update_expiry(
        &self,
        key: &BlobCacheKey,
        namespace_generation: u64,
        now_ms: u64,
        expires_at_unix_ms: Option<u64>,
    ) -> Result<bool, CacheError> {
        let _operation = self.operation.lock();
        let Some(mut stored) = self.find_record(key) else {
            return Ok(false);
        };
        if stored.record.namespace_generation != namespace_generation
            || stored.record.is_expired_at(now_ms)
        {
            return Ok(false);
        }
        stored.record.expires_at_unix_ms = expires_at_unix_ms;
        self.publish_record_without_counter(
            key,
            encode_store_record(&stored.record, &stored.framed),
            stored.id,
        )?;
        Ok(true)
    }

    fn delete_key(&self, key: &BlobCacheKey) -> Option<u64> {
        let _operation = self.operation.lock();
        self.delete_key_locked(key).ok().flatten()
    }

    fn delete_namespace(&self, namespace: &str) -> usize {
        self.delete_where(|record| record.namespace == namespace)
    }

    fn has_namespace(&self, namespace: &str) -> bool {
        let _operation = self.operation.lock();
        let mut found = false;
        self.visit_records(|_, record| {
            found = record.namespace == namespace;
            !found
        });
        found
    }

    fn delete_prefix(&self, namespace: &str, prefix: &str) -> usize {
        self.delete_where(|record| record.namespace == namespace && record.key.starts_with(prefix))
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
            .map(|filter| (filter.num_blocks() * 32) as u64)
            .sum()
    }

    fn resident_metadata_bytes(&self) -> u64 {
        // Store pages/segments are already charged by the owning UnifiedStore.
        self.stats_synopsis_bytes()
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
        if !self.synopsis_complete.load(Ordering::Acquire) {
            return true;
        }
        self.synopsis
            .read()
            .get(namespace)
            .is_some_and(|filter| filter.probe_bytes(key.as_bytes()))
    }

    #[cfg(test)]
    fn inject_synopsis_maybe_present(&self, namespace: &str, key: &str) {
        self.add_synopsis_key(namespace, key);
    }

    #[cfg(test)]
    fn inject_fault_after_blob_write_once(&self) {
        panic!("file-backed L2 fault injection is unavailable for single-file L2");
    }

    #[cfg(test)]
    fn inject_v1_entry(&self, _key: &BlobCacheKey, _payload: &[u8]) -> Result<(), CacheError> {
        Err(CacheError::L2Io(
            "legacy file-backed L2 injection is unavailable for single-file L2".into(),
        ))
    }
}

fn cache_row(key: &str, value: Value) -> UnifiedEntity {
    UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::TableRow {
            table: Arc::from(COLLECTION),
            row_id: 0,
        },
        EntityData::Row(RowData::with_names(
            vec![Value::text(key.to_string()), value],
            vec!["key".into(), "value".into()],
        )),
    )
}

fn find_value(store: &UnifiedStore, key: &str) -> Option<(EntityId, Value)> {
    store
        .get_collection(COLLECTION)?
        .project_key_entity(key, None, false, |entity| {
            let value = entity
                .data
                .as_row()
                .and_then(|row| row.get_field("value"))
                .cloned()
                .unwrap_or(Value::Null);
            (entity.id, value)
        })
}

fn storage_key(key: &BlobCacheKey) -> String {
    hex::encode(encode_l2_key(&key.namespace, &key.key))
}

fn encode_store_record(record: &L2Record, framed: &[u8]) -> Vec<u8> {
    let metadata = record.encode();
    let metadata_len = u32::try_from(metadata.len()).unwrap_or(u32::MAX);
    let mut encoded = Vec::with_capacity(RECORD_HEADER_BYTES + metadata.len() + framed.len());
    encoded.extend_from_slice(RECORD_MAGIC);
    encoded.extend_from_slice(&metadata_len.to_le_bytes());
    encoded.extend_from_slice(&metadata);
    encoded.extend_from_slice(framed);
    encoded
}

fn decode_store_record(encoded: &[u8]) -> Result<(L2Record, &[u8]), CacheError> {
    if encoded.len() < RECORD_HEADER_BYTES || &encoded[..4] != RECORD_MAGIC {
        return Err(CacheError::L2Io("invalid single-file L2 record".into()));
    }
    let metadata_len = u32::from_le_bytes(
        encoded[4..8]
            .try_into()
            .map_err(|_| CacheError::L2Io("invalid single-file L2 header".into()))?,
    ) as usize;
    let metadata_end = RECORD_HEADER_BYTES
        .checked_add(metadata_len)
        .filter(|end| *end <= encoded.len())
        .ok_or_else(|| CacheError::L2Io("truncated single-file L2 metadata".into()))?;
    let record = L2Record::decode(&encoded[RECORD_HEADER_BYTES..metadata_end])
        .map_err(|error| CacheError::L2Io(error.to_string()))?;
    Ok((record, &encoded[metadata_end..]))
}

fn decode_entity_record(entity: &UnifiedEntity) -> Option<(L2Record, &[u8])> {
    let Value::Blob(encoded) = entity.data.as_row()?.get_field("value")? else {
        return None;
    };
    decode_store_record(encoded).ok()
}

fn encode_compressed_frame(compressed: &Compressed) -> Vec<u8> {
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

fn decode_compressed_frame(bytes: &[u8]) -> Result<Compressed, CacheError> {
    match decode_l2_v2_frame(bytes).map_err(|error| CacheError::L2Io(error.to_string()))? {
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

fn store_error(error: crate::storage::StoreError) -> CacheError {
    CacheError::L2Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::cache::{
        BlobCache, BlobCacheConfig, BlobCachePolicy, BlobCachePut, L1Admission, L2Compression,
    };

    fn internal_cache(store: Arc<UnifiedStore>, l2_bytes_max: u64) -> BlobCache {
        BlobCache::open_with_internal_l2(
            BlobCacheConfig::default()
                .with_l1_bytes_max(64 * 1024)
                .with_l2_bytes_max(l2_bytes_max)
                .with_l2_compression(L2Compression::Off)
                .with_shard_count(1),
            store,
        )
        .expect("single-file L2")
    }

    fn l2_only(bytes: &[u8]) -> BlobCachePut {
        BlobCachePut::new(bytes.to_vec())
            .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never))
    }

    #[test]
    fn internal_l2_reopens_from_the_same_store() {
        let store = Arc::new(UnifiedStore::new());
        let cache = internal_cache(Arc::clone(&store), 1024);
        cache.put("tenant", "key", l2_only(b"durable")).unwrap();
        assert_eq!(cache.stats().entries(), 0);
        assert!(cache.stats().l2_bytes_in_use() > 0);
        drop(cache);

        let reopened = internal_cache(store, 1024);
        assert_eq!(
            reopened.get("tenant", "key").unwrap().bytes().as_ref(),
            b"durable"
        );
    }

    #[test]
    fn internal_l2_invalidation_and_expiry_survive_reopen() {
        let store = Arc::new(UnifiedStore::new());
        let cache = internal_cache(Arc::clone(&store), 1024);
        cache.put("tenant", "deleted", l2_only(b"old")).unwrap();
        cache
            .put(
                "tenant",
                "expired",
                BlobCachePut::new(b"old".to_vec()).with_policy(
                    BlobCachePolicy::default()
                        .expires_at_unix_ms(1)
                        .l1_admission(L1Admission::Never),
                ),
            )
            .unwrap();
        assert_eq!(cache.invalidate_key("tenant", "deleted"), 1);
        drop(cache);

        let reopened = internal_cache(store, 1024);
        assert!(reopened.get("tenant", "deleted").is_none());
        assert!(reopened.get("tenant", "expired").is_none());
        assert_eq!(reopened.stats().l2_bytes_in_use(), 0);
    }

    #[test]
    fn label_invalidation_cannot_resurrect_an_l2_only_entry() {
        let store = Arc::new(UnifiedStore::new());
        let cache = internal_cache(Arc::clone(&store), 1024);
        cache
            .put(
                "tenant",
                "tagged",
                l2_only(b"old").with_tags(["customer:42"]),
            )
            .unwrap();

        assert_eq!(cache.invalidate_tags("tenant", &["customer:42"]), 1);
        drop(cache);

        let reopened = internal_cache(store, 1024);
        assert!(reopened.get("tenant", "tagged").is_none());
    }

    #[test]
    fn internal_l2_enforces_byte_limit_with_eviction() {
        let store = Arc::new(UnifiedStore::new());
        let cache = internal_cache(store, 12);
        cache.put("tenant", "first", l2_only(b"12345678")).unwrap();
        cache.put("tenant", "second", l2_only(b"abcdefgh")).unwrap();

        assert!(cache.get("tenant", "first").is_none());
        assert_eq!(
            cache.get("tenant", "second").unwrap().bytes().as_ref(),
            b"abcdefgh"
        );
        assert!(cache.stats().l2_bytes_in_use() <= 12);
    }

    #[test]
    fn zero_internal_l2_budget_disables_the_backend() {
        let store = Arc::new(UnifiedStore::new());
        let cache = internal_cache(store, 0);
        cache.put("tenant", "key", l2_only(b"not-stored")).unwrap();

        assert!(cache.get("tenant", "key").is_none());
        assert_eq!(cache.stats().l2_bytes_max(), 0);
        assert_eq!(cache.stats().l2_bytes_in_use(), 0);
    }
}
