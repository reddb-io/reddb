use super::cache::{BlobCacheKey, BlobCacheSweepOutcome, CacheError};
use super::entry::Entry;
use super::l2::BlobCacheL2;
use crate::storage::cache::compressor::Compressed;

pub(super) trait BlobCacheL2Backend: Send + Sync {
    fn namespace_generation(&self, namespace: &str) -> Option<u64>;
    fn set_namespace_generation(&self, namespace: &str, generation: u64) -> Result<(), CacheError>;
    fn invalidate_namespace(&self, namespace: &str, generation: u64) -> Result<(), CacheError> {
        self.set_namespace_generation(namespace, generation)?;
        self.delete_namespace(namespace);
        Ok(())
    }
    fn get(&self, key: &BlobCacheKey, now_ms: u64, generation: u64) -> Option<Entry>;

    fn put(
        &self,
        key: &BlobCacheKey,
        entry: &Entry,
        old_entry_size: u64,
        compressed: Compressed,
    ) -> Result<(), CacheError>;

    fn record_size(&self, key: &BlobCacheKey) -> u64;

    fn update_expiry(
        &self,
        key: &BlobCacheKey,
        namespace_generation: u64,
        now_ms: u64,
        expires_at_unix_ms: Option<u64>,
    ) -> Result<bool, CacheError>;

    fn delete_key(&self, key: &BlobCacheKey) -> Option<u64>;
    fn delete_namespace(&self, namespace: &str) -> usize;
    fn has_namespace(&self, namespace: &str) -> bool;
    fn delete_prefix(&self, namespace: &str, prefix: &str) -> usize;
    fn sweep_expired(
        &self,
        now_ms: u64,
        entries_limit: usize,
        millis_limit: Option<u32>,
    ) -> BlobCacheSweepOutcome;
    fn stats_bytes_in_use(&self) -> u64;
    fn stats_metadata_reads(&self) -> u64;
    fn stats_negative_skips(&self) -> u64;
    fn stats_synopsis_metadata_reads(&self) -> u64;
    fn stats_synopsis_bytes(&self) -> u64;
    fn resident_metadata_bytes(&self) -> u64;
    fn stats_compression_original_bytes(&self) -> u64;
    fn stats_compression_stored_bytes(&self) -> u64;
    fn stats_compression_skipped_total(&self) -> u64;
    fn stats_bytes_saved_total(&self) -> u64;
    fn synopsis_may_contain(&self, namespace: &str, key: &str) -> bool;

    #[cfg(test)]
    fn inject_synopsis_maybe_present(&self, namespace: &str, key: &str);

    #[cfg(test)]
    fn inject_fault_after_blob_write_once(&self);

    #[cfg(test)]
    fn inject_v1_entry(&self, key: &BlobCacheKey, payload: &[u8]) -> Result<(), CacheError>;
}

impl BlobCacheL2Backend for BlobCacheL2 {
    fn namespace_generation(&self, namespace: &str) -> Option<u64> {
        self.namespace_generation(namespace)
    }

    fn set_namespace_generation(&self, namespace: &str, generation: u64) -> Result<(), CacheError> {
        self.set_namespace_generation(namespace, generation)
    }

    fn get(&self, key: &BlobCacheKey, now_ms: u64, generation: u64) -> Option<Entry> {
        self.get(key, now_ms, generation)
    }

    fn put(
        &self,
        key: &BlobCacheKey,
        entry: &Entry,
        old_entry_size: u64,
        compressed: Compressed,
    ) -> Result<(), CacheError> {
        self.put(key, entry, old_entry_size, compressed)
    }

    fn record_size(&self, key: &BlobCacheKey) -> u64 {
        self.record_size(key)
    }

    fn update_expiry(
        &self,
        key: &BlobCacheKey,
        namespace_generation: u64,
        now_ms: u64,
        expires_at_unix_ms: Option<u64>,
    ) -> Result<bool, CacheError> {
        self.update_expiry(key, namespace_generation, now_ms, expires_at_unix_ms)
    }

    fn delete_key(&self, key: &BlobCacheKey) -> Option<u64> {
        self.delete_key(key)
    }

    fn delete_namespace(&self, namespace: &str) -> usize {
        self.delete_namespace(namespace)
    }

    fn has_namespace(&self, namespace: &str) -> bool {
        self.has_namespace(namespace)
    }

    fn delete_prefix(&self, namespace: &str, prefix: &str) -> usize {
        self.delete_prefix(namespace, prefix)
    }

    fn sweep_expired(
        &self,
        _now_ms: u64,
        _entries_limit: usize,
        _millis_limit: Option<u32>,
    ) -> BlobCacheSweepOutcome {
        // The standalone pager is a compatibility backend. The server uses
        // StoreBlobCacheL2, whose persisted deadline projection provides the
        // automatic no-scan sweep.
        BlobCacheSweepOutcome {
            completed: true,
            ..BlobCacheSweepOutcome::default()
        }
    }

    fn stats_bytes_in_use(&self) -> u64 {
        self.stats_bytes_in_use()
    }

    fn stats_metadata_reads(&self) -> u64 {
        self.stats_metadata_reads()
    }

    fn stats_negative_skips(&self) -> u64 {
        self.stats_negative_skips()
    }

    fn stats_synopsis_metadata_reads(&self) -> u64 {
        self.stats_synopsis_metadata_reads()
    }

    fn stats_synopsis_bytes(&self) -> u64 {
        self.stats_synopsis_bytes()
    }

    fn resident_metadata_bytes(&self) -> u64 {
        self.resident_metadata_bytes()
    }

    fn stats_compression_original_bytes(&self) -> u64 {
        self.stats_compression_original_bytes()
    }

    fn stats_compression_stored_bytes(&self) -> u64 {
        self.stats_compression_stored_bytes()
    }

    fn stats_compression_skipped_total(&self) -> u64 {
        self.stats_compression_skipped_total()
    }

    fn stats_bytes_saved_total(&self) -> u64 {
        self.stats_bytes_saved_total()
    }

    fn synopsis_may_contain(&self, namespace: &str, key: &str) -> bool {
        self.synopsis_may_contain(namespace, key)
    }

    #[cfg(test)]
    fn inject_synopsis_maybe_present(&self, namespace: &str, key: &str) {
        self.inject_synopsis_maybe_present(namespace, key);
    }

    #[cfg(test)]
    fn inject_fault_after_blob_write_once(&self) {
        self.inject_fault_after_blob_write_once();
    }

    #[cfg(test)]
    fn inject_v1_entry(&self, key: &BlobCacheKey, payload: &[u8]) -> Result<(), CacheError> {
        self.inject_v1_entry(key, payload)
    }
}
