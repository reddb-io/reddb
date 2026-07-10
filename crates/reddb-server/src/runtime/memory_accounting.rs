//! Live usage reporting into the shared accounting pool — ADR 0073 §2.
//!
//! Every governed pool already owns a counter for its own footprint: the page
//! cache knows its resident slot count, the blob cache its `bytes_in_use`, the
//! segments their `memory_bytes` atomics, the WAL queue its buffered bytes.
//! This module is the seam that reads those counters and stores them into the
//! one shared pool, so `red.stats` can show per-pool `share_bytes` /
//! `used_bytes` against a single total.
//!
//! **Pull, not push.** The alternative — every subsystem holding an
//! `Arc<MemoryAccounting>` and updating it inline — would thread the pool into
//! the page-cache hit path and the segment insert path, which is exactly the
//! per-operation cost ADR 0073 §3 forbids. Instead the pools keep the atomics
//! they already maintain and a reader samples them. A refresh is a handful of
//! relaxed loads plus one read lock per segment; the read hot paths gain
//! nothing at all.
//!
//! Sizing and accounting only: a pool over its share shows up here and is not
//! acted on. Admission enforcement is the next slice (ADR 0073 §4).

use crate::runtime::RedDBRuntime;
use crate::storage::memory_pools::MemoryPool;

impl RedDBRuntime {
    /// Sample every governed pool and store its footprint into the shared
    /// accounting. Called before the `red.stats` budget section is rendered.
    ///
    /// Cheap by construction: no allocation, no new lock on any read hot path,
    /// and each pool contributes counters it was already keeping.
    pub fn refresh_memory_accounting(&self) {
        let accounting = self.memory_accounting();
        let store = self.db().store();

        accounting.report(MemoryPool::PageCache, store.page_cache_bytes_in_use());
        accounting.report(
            MemoryPool::BlobCacheL1,
            self.result_blob_cache().ram_bytes_in_use(),
        );
        accounting.report(MemoryPool::SegmentArena, store.segment_memory_bytes());
        accounting.report(
            MemoryPool::IndexMemory,
            self.index_store_ref().memory_bytes(),
        );
        accounting.report(MemoryPool::WalBuffers, store.wal_buffer_bytes_in_use());
    }
}
