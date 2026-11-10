use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Mutex, RwLock};
use reddb_file::{
    decode_l2_v2_frame, encode_l2_key, encode_l2_v2_frame, L2BlobFrame, L2Record,
    L2_FORMAT_V2_FRAMED,
};
use reddb_types::Value;

use super::cache::{BlobCacheKey, BlobCacheNamespaceSweep, BlobCacheSweepOutcome, CacheError};
use super::config::DEFAULT_BLOB_SYNOPSIS_CAPACITY;
use super::entry::Entry;
use super::l2_backend::BlobCacheL2Backend;
use crate::storage::cache::compressor::{Compressed, L2BlobCompressor};
use crate::storage::cache::extended_ttl::ExtendedTtlPolicy;
use crate::storage::primitives::split_block_bloom::SplitBlockBloom;
use crate::storage::unified::store::{MvccReclaimCursor, RowPublication};
use crate::storage::{
    EntityData, EntityId, EntityKind, Metadata, MetadataValue, RowData, UnifiedEntity, UnifiedStore,
};

pub(crate) const COLLECTION: &str = "red.cache.blob_l2";
const CONTROL_KEY: &str = "__control";
const RECORD_MAGIC: &[u8; 4] = b"RLC1";
const RECORD_HEADER_BYTES: usize = 8;
const DELETE_BATCH: usize = 256;
const EXPIRY_BATCH: usize = 64;

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
    expiry_rebuild_cursor: Mutex<Option<MvccReclaimCursor>>,
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
            expiry_rebuild_cursor: Mutex::new(None),
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
        expires_at_unix_ms: Option<u64>,
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
                        expiry_metadata(expires_at_unix_ms),
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
        expires_at_unix_ms: Option<u64>,
    ) -> Result<(), CacheError> {
        self.store
            .publish_rows(RowPublication {
                inserts: vec![(
                    COLLECTION.to_string(),
                    cache_row(&storage_key(key), Value::Blob(encoded)),
                    expiry_metadata(expires_at_unix_ms),
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

    fn rebuild_expiry_projection(
        &self,
        manager: &crate::storage::SegmentManager,
        budget: usize,
    ) -> Result<(usize, bool), CacheError> {
        if manager.metadata_expiry_projection_ready() {
            return Ok((0, true));
        }
        let mut cursor = self.expiry_rebuild_cursor.lock();
        if cursor.is_none() {
            *cursor = self.store.mvcc_reclaim_cursor(COLLECTION.to_string());
        }
        let Some(active) = cursor.as_mut() else {
            manager.mark_metadata_expiry_projection_ready();
            return Ok((0, true));
        };
        let (ids, scanned) = self
            .store
            .retention_scan_tick(active, budget)
            .map_err(store_error)?;
        for id in ids {
            manager.rebuild_metadata_expiry(id).map_err(|error| {
                CacheError::L2Io(format!("rebuild L2 expiry projection failed: {error}"))
            })?;
        }
        let complete = active.finished();
        if complete {
            manager.mark_metadata_expiry_projection_ready();
            *cursor = None;
        }
        Ok((scanned, complete))
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
    fn namespace_generation(&self, namespace: &str) -> Option<u64> {
        let _operation = self.operation.lock();
        match find_value(&self.store, &generation_key(namespace)) {
            Some((_, Value::UnsignedInteger(generation))) => Some(generation),
            _ => None,
        }
    }

    fn set_namespace_generation(&self, namespace: &str, generation: u64) -> Result<(), CacheError> {
        let _operation = self.operation.lock();
        let key = generation_key(namespace);
        let previous = find_value(&self.store, &key).map(|(id, _)| (COLLECTION.to_string(), id));
        self.store
            .publish_rows(RowPublication {
                inserts: vec![(
                    COLLECTION.to_string(),
                    cache_row(&key, Value::UnsignedInteger(generation)),
                    None,
                )],
                deletes: previous.into_iter().collect(),
            })
            .map_err(store_error)
    }

    fn invalidate_namespace(&self, namespace: &str, generation: u64) -> Result<(), CacheError> {
        let operation = self.operation.lock();
        let mut ids = Vec::new();
        let mut reclaimed = 0u64;
        self.visit_records(|id, record| {
            if record.namespace == namespace {
                ids.push(id);
                reclaimed = reclaimed.saturating_add(record.byte_len);
            }
            ids.len() < DELETE_BATCH
        });
        let more = ids.len() == DELETE_BATCH;
        let generation_key = generation_key(namespace);
        let previous_generation = find_value(&self.store, &generation_key).map(|(id, _)| id);
        let new_bytes = self
            .bytes_in_use
            .load(Ordering::Relaxed)
            .saturating_sub(reclaimed);
        let mut deletes: Vec<_> = ids
            .into_iter()
            .map(|id| (COLLECTION.to_string(), id))
            .collect();
        deletes.extend(previous_generation.map(|id| (COLLECTION.to_string(), id)));
        let mut inserts = vec![(
            COLLECTION.to_string(),
            cache_row(&generation_key, Value::UnsignedInteger(generation)),
            None,
        )];
        if reclaimed > 0 {
            deletes.extend(
                find_value(&self.store, CONTROL_KEY).map(|(id, _)| (COLLECTION.to_string(), id)),
            );
            inserts.push((
                COLLECTION.to_string(),
                cache_row(CONTROL_KEY, Value::UnsignedInteger(new_bytes)),
                None,
            ));
        }
        self.store
            .publish_rows(RowPublication { inserts, deletes })
            .map_err(store_error)?;
        if reclaimed > 0 {
            self.bytes_in_use.store(new_bytes, Ordering::Relaxed);
        }
        drop(operation);
        // The durable generation fences all remaining records. Reclaim any
        // additional bounded batches using the ordinary deletion path.
        if more {
            self.delete_namespace(namespace);
        }
        Ok(())
    }

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
            record.expires_at_unix_ms,
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
            stored.record.expires_at_unix_ms,
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

    fn sweep_expired(
        &self,
        now_ms: u64,
        entries_limit: usize,
        millis_limit: Option<u32>,
    ) -> BlobCacheSweepOutcome {
        let started = Instant::now();
        let time_limit = millis_limit.map(|millis| Duration::from_millis(u64::from(millis)));
        let hard_limit = entries_limit.min(EXPIRY_BATCH);
        if hard_limit == 0 || millis_limit == Some(0) {
            return BlobCacheSweepOutcome::default();
        }
        let _operation = self.operation.lock();
        let Some(manager) = self.store.get_collection(COLLECTION) else {
            return BlobCacheSweepOutcome {
                completed: true,
                ..BlobCacheSweepOutcome::default()
            };
        };

        let (rebuild_scanned, projection_ready) =
            match self.rebuild_expiry_projection(&manager, hard_limit) {
                Ok(result) => result,
                Err(_) => return BlobCacheSweepOutcome::default(),
            };
        let rebuild_used_budget = rebuild_scanned >= hard_limit
            || time_limit.is_some_and(|limit| started.elapsed() >= limit);
        if !projection_ready || rebuild_used_budget {
            return BlobCacheSweepOutcome {
                entries_scanned: rebuild_scanned,
                elapsed_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
                completed: false,
                ..BlobCacheSweepOutcome::default()
            };
        }
        let expiry_limit = hard_limit.saturating_sub(rebuild_scanned);

        // Once ready, the projection lookup is ordered by deadline and never
        // scans cache rows. Older or bounded-residency checkpoints reach this
        // point after the paced reconstruction above completes.
        let mut candidates = manager.expired_metadata_ids(now_ms, expiry_limit.saturating_add(1));
        let mut truncated = candidates.len() > expiry_limit;
        candidates.truncate(expiry_limit);
        let candidate_count = candidates.len();
        let mut scanned = 0usize;
        let mut expired = Vec::new();
        let mut reclaimed = 0u64;
        let mut by_namespace = BTreeMap::<String, BlobCacheNamespaceSweep>::new();

        for id in candidates {
            if time_limit.is_some_and(|limit| started.elapsed() >= limit) {
                truncated = true;
                break;
            }
            scanned = scanned.saturating_add(1);
            let Some(entity) = self.store.get(COLLECTION, id) else {
                manager.remove_metadata_expiry(id);
                continue;
            };
            let Some((record, _)) = decode_entity_record(&entity) else {
                // A corrupt cache row is derived state. Remove it instead of
                // leaving a permanently-due projection entry. Its unknown
                // byte charge remains conservatively counted.
                expired.push((id, None));
                continue;
            };
            by_namespace
                .entry(record.namespace.clone())
                .or_default()
                .entries_scanned += 1;
            if record.is_expired_at(now_ms) {
                reclaimed = reclaimed.saturating_add(record.byte_len);
                expired.push((id, Some(record)));
            } else {
                // Defensive repair for a stale volatile projection. Normal
                // atomic replacement removes the old id and installs the new
                // deadline in the same publication.
                let metadata = expiry_metadata(record.expires_at_unix_ms);
                manager.restore_metadata_expiry(&entity, metadata.as_ref());
            }
        }

        let mut outcome = BlobCacheSweepOutcome {
            entries_scanned: rebuild_scanned.saturating_add(scanned),
            completed: !truncated && scanned == candidate_count,
            elapsed_ms: u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX),
            by_namespace,
            ..BlobCacheSweepOutcome::default()
        };
        if expired.is_empty() {
            return outcome;
        }

        let ids = expired.iter().map(|(id, _)| *id).collect();
        let current = self.bytes_in_use.load(Ordering::Relaxed);
        let new_bytes = current.saturating_sub(reclaimed);
        if self.publish_deletions(ids, new_bytes).is_err() {
            outcome.completed = false;
            return outcome;
        }
        self.bytes_in_use.store(new_bytes, Ordering::Relaxed);
        outcome.entries_evicted = expired.len();
        outcome.expirations = expired.len();
        outcome.bytes_reclaimed = reclaimed;
        for record in expired.into_iter().filter_map(|(_, record)| record) {
            let namespace = outcome.by_namespace.entry(record.namespace).or_default();
            namespace.entries_evicted += 1;
            namespace.bytes_reclaimed = namespace.bytes_reclaimed.saturating_add(record.byte_len);
        }
        outcome.elapsed_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        outcome
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

fn expiry_metadata(expires_at_unix_ms: Option<u64>) -> Option<Metadata> {
    expires_at_unix_ms.map(|deadline| {
        let mut metadata = Metadata::new();
        metadata.set("_expires_at", MetadataValue::Timestamp(deadline));
        metadata
    })
}

fn find_value(store: &UnifiedStore, key: &str) -> Option<(EntityId, Value)> {
    store
        .get_collection(COLLECTION)?
        .try_project_key_entity(key, None, true, usize::MAX, |entity| {
            let value = entity
                .data
                .as_row()
                .and_then(|row| row.get_field("value"))
                .cloned()
                .unwrap_or(Value::Null);
            (entity.id, value)
        })
        .ok()
        .flatten()
}

fn storage_key(key: &BlobCacheKey) -> String {
    hex::encode(encode_l2_key(&key.namespace, &key.key))
}

fn generation_key(namespace: &str) -> String {
    format!("__generation:{}", hex::encode(namespace.as_bytes()))
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
        BlobCache, BlobCacheConfig, BlobCachePolicy, BlobCachePut, BlobCacheSweeper, L1Admission,
        L2Compression, SweepLimit,
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
    fn namespace_invalidation_publishes_generation_and_reclamation_atomically() {
        let store = Arc::new(UnifiedStore::new());
        let cache = internal_cache(Arc::clone(&store), 4096);
        cache.put("tenant", "first", l2_only(b"first")).unwrap();
        cache.put("other", "keep", l2_only(b"other")).unwrap();
        let before = cache.stats().l2_bytes_in_use();
        store
            .test_fail_deferred_wal_append
            .store(true, Ordering::Release);
        assert!(cache.try_invalidate_namespace("tenant").is_err());
        assert_eq!(cache.stats().l2_bytes_in_use(), before);
        drop(cache);

        let cache = internal_cache(Arc::clone(&store), 4096);
        assert_eq!(
            cache.get("tenant", "first").unwrap().bytes().as_ref(),
            b"first"
        );
        assert_eq!(
            cache.get("other", "keep").unwrap().bytes().as_ref(),
            b"other"
        );
        assert!(cache.try_invalidate_namespace("tenant").unwrap());
        assert_eq!(
            cache.stats().l2_bytes_in_use(),
            u64::try_from(b"other".len()).expect("payload length")
        );
        drop(cache);

        let reopened = internal_cache(store, 4096);
        assert!(reopened.get("tenant", "first").is_none());
        assert_eq!(
            reopened.get("other", "keep").unwrap().bytes().as_ref(),
            b"other"
        );
    }

    #[test]
    fn namespace_invalidation_reclaims_more_than_one_batch() {
        let store = Arc::new(UnifiedStore::new());
        let cache = internal_cache(Arc::clone(&store), 4096);
        for i in 0..(DELETE_BATCH + 2) {
            cache.put("tenant", i.to_string(), l2_only(b"x")).unwrap();
        }
        cache.put("other", "keep", l2_only(b"y")).unwrap();
        assert!(cache.try_invalidate_namespace("tenant").unwrap());
        assert_eq!(cache.stats().l2_bytes_in_use(), 1);
        drop(cache);
        let reopened = internal_cache(store, 4096);
        assert!(reopened.get("tenant", "0").is_none());
        assert!(reopened
            .get("tenant", &(DELETE_BATCH + 1).to_string())
            .is_none());
        assert_eq!(
            reopened.get("other", "keep").unwrap().bytes().as_ref(),
            b"y"
        );
    }

    #[test]
    fn internal_l2_sweeper_uses_deadlines_without_reading_values() {
        let store = Arc::new(UnifiedStore::new());
        let cache = internal_cache(Arc::clone(&store), 1024);
        cache
            .put(
                "tenant",
                "expired",
                BlobCachePut::new(b"expired".to_vec()).with_policy(
                    BlobCachePolicy::default()
                        .expires_at_unix_ms(1)
                        .l1_admission(L1Admission::Never),
                ),
            )
            .unwrap();
        cache
            .put(
                "tenant",
                "future",
                BlobCachePut::new(b"future".to_vec()).with_policy(
                    BlobCachePolicy::default()
                        .expires_at_unix_ms(u64::MAX)
                        .l1_admission(L1Admission::Never),
                ),
            )
            .unwrap();
        let manager = store.get_collection(COLLECTION).expect("L2 collection");
        assert_eq!(manager.scheduled_metadata_expiry_count(), 2);

        let report = BlobCacheSweeper::sweep_expired(&cache, SweepLimit::Entries(1));

        assert_eq!(report.entries_scanned, 1);
        assert_eq!(report.entries_evicted, 1);
        assert_eq!(manager.scheduled_metadata_expiry_count(), 1);
        assert_eq!(
            cache.stats().l2_bytes_in_use(),
            u64::try_from(b"future".len()).expect("payload length")
        );
        assert_eq!(
            cache.get("tenant", "future").unwrap().bytes().as_ref(),
            b"future"
        );
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
