//! Incremental output buffers that admit growth without sampling inside scans.

use super::{OwnedQueryBuffer, QueryBufferError};
use crate::runtime::memory_admission::QueryBufferReservation;
use crate::runtime::RedDBRuntime;

pub(crate) struct GrowingQueryBuffer<'a> {
    runtime: &'a RedDBRuntime,
    buffer: Option<OwnedQueryBuffer>,
    limit: usize,
}

impl<'a> GrowingQueryBuffer<'a> {
    /// Call outside storage locks. Later growth uses only the admission ledger.
    pub(crate) fn new(runtime: &'a RedDBRuntime, limit: usize) -> Self {
        runtime.refresh_memory_accounting();
        Self {
            runtime,
            buffer: None,
            limit,
        }
    }

    /// An empty sibling shares the already sampled governor. Safe in a scan
    /// callback; all siblings compete for the same budget as mutations.
    pub(crate) fn sibling(&self, limit: usize) -> Self {
        Self {
            runtime: self.runtime,
            buffer: None,
            limit: limit.min(self.limit),
        }
    }

    pub(crate) fn reserve_workspace(
        &self,
        bytes: usize,
    ) -> Result<QueryBufferReservation, QueryBufferError> {
        self.runtime
            .try_reserve_query_buffer_from_sample(bytes)
            .ok_or_else(|| {
                self.runtime.memory_accounting().record_admission_denied();
                QueryBufferError::MemoryBudget
            })
    }

    pub(crate) fn len(&self) -> usize {
        self.buffer.as_ref().map_or(0, |buffer| buffer.value.len())
    }

    pub(crate) fn append(
        &mut self,
        bytes: usize,
        encode: impl FnOnce(&mut Vec<u8>),
    ) -> Result<(), QueryBufferError> {
        let needed = self
            .len()
            .checked_add(bytes)
            .ok_or(QueryBufferError::ResultLimit)?;
        if needed > self.limit {
            return Err(QueryBufferError::ResultLimit);
        }
        let capacity = self
            .buffer
            .as_ref()
            .map_or(0, |buffer| buffer.value.capacity());
        if needed > capacity {
            // Never grow by repeated exact per-row increments under pressure.
            // Geometric growth bounds total copy work; both old and new owners
            // must fit until the copy and old allocation release finish.
            let capacity = needed
                .max(capacity.saturating_mul(2))
                .max(1024)
                .min(self.limit);
            let reservation = self.reserve_workspace(capacity)?;
            let mut value = Vec::new();
            value
                .try_reserve_exact(capacity)
                .map_err(|_| QueryBufferError::MemoryBudget)?;
            assert_eq!(
                value.capacity(),
                capacity,
                "output allocation matches admission"
            );
            if let Some(old) = &self.buffer {
                value.extend_from_slice(&old.value);
            }
            self.buffer = Some(OwnedQueryBuffer {
                value,
                _reservation: reservation,
            });
        }
        if needed == 0 {
            return Ok(());
        }
        let buffer = self
            .buffer
            .as_mut()
            .expect("nonempty output has an admitted allocation");
        let capacity = buffer.value.capacity();
        encode(&mut buffer.value);
        assert_eq!(
            buffer.value.len(),
            needed,
            "encoder appends its declared byte length"
        );
        assert_eq!(
            buffer.value.capacity(),
            capacity,
            "encoder stays within admission"
        );
        Ok(())
    }

    pub(crate) fn patch(&mut self, patch: impl FnOnce(&mut [u8])) {
        if let Some(buffer) = &mut self.buffer {
            patch(&mut buffer.value);
        }
    }

    pub(crate) fn finish(self) -> bytes::Bytes {
        self.buffer
            .map_or_else(bytes::Bytes::new, bytes::Bytes::from_owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::memory_pools::MemoryPool;
    use crate::RedDBOptions;

    fn runtime() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(1024 * 1024))
            .unwrap()
    }

    #[test]
    fn concurrent_callback_buffers_cannot_oversubscribe_shared_headroom() {
        let runtime = runtime();
        let template = GrowingQueryBuffer::new(&runtime, 128 * 1024);
        let available =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        std::thread::scope(|scope| {
            let mut workers = Vec::new();
            for _ in 0..16 {
                let mut buffer = template.sibling(128 * 1024);
                workers.push(scope.spawn(move || {
                    buffer
                        .append(128 * 1024, |out| out.resize(128 * 1024, 0))
                        .ok()
                        .map(|()| buffer.finish())
                }));
            }
            let owners: Vec<_> = workers
                .into_iter()
                .filter_map(|worker| worker.join().unwrap())
                .collect();
            assert_eq!(owners.len() as u64, available / (128 * 1024));
            runtime.refresh_memory_accounting();
            assert_eq!(
                runtime
                    .memory_accounting()
                    .used_bytes(MemoryPool::QueryBuffers),
                owners.len() as u64 * 128 * 1024
            );
        });
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }

    #[test]
    fn growth_admits_overlap_before_encoding_and_releases_last_clone() {
        let runtime = runtime();
        let mut buffer = GrowingQueryBuffer::new(&runtime, 8192);
        buffer.append(1024, |out| out.resize(1024, 7)).unwrap();
        let available =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let held = runtime
            .try_reserve_query_buffer(available as usize - 1536)
            .unwrap();
        assert_eq!(
            buffer.append(1, |_| panic!("encoder must not run on refusal")),
            Err(QueryBufferError::MemoryBudget)
        );
        assert_eq!(buffer.len(), 1024);
        drop(held);
        buffer
            .append(1, |out| {
                // The old 1 KiB allocation is freed before the callback; the new
                // 2 KiB allocation was admitted while both were still alive.
                assert_eq!(
                    runtime
                        .memory_accounting()
                        .used_bytes(MemoryPool::QueryBuffers),
                    2048
                );
                out.push(8);
            })
            .unwrap();
        let bytes = buffer.finish();
        let clone = bytes.clone();
        drop(bytes);
        assert_eq!(clone[1023], 7);
        assert_eq!(clone[1024], 8);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            2048
        );
        drop(clone);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }

    #[test]
    fn callback_growth_does_not_wait_for_storage_sampler_mutex() {
        let runtime = runtime();
        let mut buffer = GrowingQueryBuffer::new(&runtime, 8192);
        let reservations = runtime.inner.memory_reservations.lock();
        let samples = reservations.samples;
        std::thread::scope(|scope| {
            let (send, receive) = std::sync::mpsc::channel();
            let worker = scope.spawn(move || {
                buffer.append(2048, |out| out.resize(2048, 1)).unwrap();
                send.send(buffer.finish()).unwrap();
            });
            let result = receive.recv_timeout(std::time::Duration::from_secs(2));
            // Release before joining even if the regression has deadlocked.
            drop(reservations);
            worker.join().unwrap();
            assert_eq!(
                result.expect("growth must not acquire sampler mutex").len(),
                2048
            );
        });
        assert_eq!(runtime.inner.memory_reservations.lock().samples, samples);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }

    #[test]
    fn mutation_and_query_owners_compete_across_resampling() {
        let runtime = runtime();
        let mut buffer = GrowingQueryBuffer::new(&runtime, 512 * 1024);
        let available =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let mutation = runtime.try_reserve_memory_growth(available - 4096).unwrap();
        runtime.refresh_memory_accounting();
        assert_eq!(
            buffer.append(8192, |_| panic!("mutation owns the headroom")),
            Err(QueryBufferError::MemoryBudget)
        );
        drop(mutation);
        // Completed mutation remains charged until publication is sampled.
        assert_eq!(
            buffer.append(8192, |_| panic!("not reconciled yet")),
            Err(QueryBufferError::MemoryBudget)
        );
        runtime.refresh_memory_accounting();
        buffer.append(8192, |out| out.resize(8192, 0)).unwrap();
        runtime.refresh_memory_accounting();
        let available =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        assert!(runtime.try_reserve_memory_growth(available + 1).is_none());
        let mutation = runtime.try_reserve_memory_growth(available).unwrap();
        drop(mutation);
        drop(buffer);
        runtime.refresh_memory_accounting();
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }

    #[test]
    fn size_limit_is_checked_before_allocation_or_encoder() {
        let runtime = runtime();
        let mut buffer = GrowingQueryBuffer::new(&runtime, 16);
        assert_eq!(
            buffer.append(17, |_| panic!("over limit")),
            Err(QueryBufferError::ResultLimit)
        );
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
        buffer.append(16, |out| out.resize(16, 1)).unwrap();
        assert_eq!(
            buffer.append(usize::MAX, |_| panic!("overflow")),
            Err(QueryBufferError::ResultLimit)
        );
        assert_eq!(buffer.finish().len(), 16);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }
}
