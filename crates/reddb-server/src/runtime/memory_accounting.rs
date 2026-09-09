//! Live usage reporting into the shared accounting pool — ADR 0073 §2.
//!
//! Every governed pool already owns a counter for its own footprint: the page
//! cache knows its resident slot count, the blob cache its `bytes_in_use`, the
//! segments their resident estimates and retained lifetimes, the WAL queue its
//! buffered bytes.
//! This module is the seam that reads those counters and stores them into the
//! one shared pool, so `red.stats` can show per-pool `share_bytes` /
//! `used_bytes` against a single total.
//!
//! **Pull, not push.** The alternative — every subsystem holding an
//! `Arc<MemoryAccounting>` and updating it inline — would thread the pool into
//! the page-cache hit path and the segment insert path, which is exactly the
//! per-operation cost ADR 0073 §3 forbids. Instead the pools keep the atomics
//! they already maintain and a reader samples them. A refresh is a handful of
//! resident-counter reads under the segment publication locks; ordinary read
//! hot paths gain no accounting hook.
//!
//! This sampler reports usage. `memory_admission` applies the growth checks
//! against the shared budget (ADR 0073 §4).

use crate::runtime::RedDBRuntime;
use crate::storage::memory_pools::MemoryPool;

impl RedDBRuntime {
    /// Sample every governed pool and store its footprint into the shared
    /// accounting. Called before the `red.stats` budget section is rendered.
    ///
    /// Samples store/segment inventories without scanning entity payloads.
    /// Maintenance publication can delay a sample; ordinary reads do not call it.
    pub fn refresh_memory_accounting(&self) {
        let mut reservations = self.inner.memory_reservations.lock();
        self.refresh_memory_accounting_with_reservations(&mut reservations);
    }

    /// Caller holds the reservation mutex across sampling and publication. A
    /// completed guard cannot return headroom until this sample sees its writes;
    /// an older sampler cannot overwrite this sample after headroom is returned.
    pub(super) fn refresh_memory_accounting_with_reservations(
        &self,
        reservations: &mut super::memory_admission::MemoryReservations,
    ) {
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
        // Paged stores keep a resident group-commit writer and queue; embedded
        // single-file stores append WAL payloads synchronously to the artifact
        // and retain no live WAL buffer between mutations.
        accounting.report(MemoryPool::WalBuffers, store.wal_buffer_bytes_in_use());
        accounting.observe_total_used(accounting.total_used_bytes());
        assert!(
            reservations.completed_bytes <= reservations.reserved_bytes,
            "invariant: only completed reservations can be reconciled"
        );
        reservations.reserved_bytes -= reservations.completed_bytes;
        reservations.completed_bytes = 0;
    }
}
