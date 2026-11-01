//! Fallible serialization into buffers admitted by the shared runtime budget.

use super::memory_admission::QueryBufferReservation;
use super::RedDBRuntime;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueryBufferError {
    ResultLimit,
    MemoryBudget,
}

/// Fields drop in declaration order: free the allocation before returning its
/// budget. Bytes owns this object through clones, slices and HTTP transport.
struct OwnedQueryBuffer {
    value: Vec<u8>,
    _reservation: QueryBufferReservation,
}

impl AsRef<[u8]> for OwnedQueryBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.value
    }
}

struct QueryBufferWriter<'runtime> {
    runtime: &'runtime RedDBRuntime,
    value: &'runtime crate::json::Value,
    buffer: Option<OwnedQueryBuffer>,
    limit: usize,
    error: Option<QueryBufferError>,
}

impl RedDBRuntime {
    pub(crate) fn serialize_query_json(
        &self,
        value: &crate::json::Value,
    ) -> Result<bytes::Bytes, QueryBufferError> {
        let mut writer = QueryBufferWriter {
            runtime: self,
            value,
            buffer: None,
            limit: self
                .resource_limits()
                .max_result_bytes
                .filter(|limit| *limit > 0)
                .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
                .unwrap_or(usize::MAX),
            error: None,
        };
        if value.write_compact_to(&mut writer).is_err() {
            return Err(writer.error.expect("writer records its first failure"));
        }
        Ok(match writer.buffer {
            Some(buffer) => bytes::Bytes::from_owner(buffer),
            None => bytes::Bytes::new(),
        })
    }
}

impl QueryBufferWriter<'_> {
    fn grow(&mut self, needed: usize) -> Result<(), QueryBufferError> {
        let capacity = self
            .buffer
            .as_ref()
            .map_or(0, |buffer| buffer.value.capacity());
        if needed <= capacity {
            return Ok(());
        }
        let preferred = capacity
            .saturating_mul(2)
            .max(1024)
            .max(needed)
            .min(self.limit);
        // Reallocation temporarily owns both allocations. Admit the full new
        // capacity while the old buffer is still charged; never credit its
        // bytes until the copy has finished and the old allocation is freed.
        let (capacity, reservation) = match self.runtime.try_reserve_query_buffer(preferred) {
            Some(reservation) => (preferred, reservation),
            None => {
                // Under pressure, exact per-fragment growth can copy the same
                // large prefix thousands of times. Count once without allocating
                // and admit the complete final capacity instead. Ordinary
                // serialization still takes one pass; this fallback takes two.
                let mut count = JsonByteCount {
                    length: 0,
                    limit: self.limit,
                };
                self.value
                    .write_compact_to(&mut count)
                    .map_err(|_| QueryBufferError::ResultLimit)?;
                assert!(
                    count.length >= needed,
                    "full JSON includes the pending fragment"
                );
                (
                    count.length,
                    self.runtime
                        .try_reserve_query_buffer(count.length)
                        .ok_or_else(|| {
                            self.runtime.memory_accounting().record_admission_denied();
                            QueryBufferError::MemoryBudget
                        })?,
                )
            }
        };
        let mut value = Vec::new();
        value
            .try_reserve_exact(capacity)
            .map_err(|_| QueryBufferError::MemoryBudget)?;
        assert_eq!(
            value.capacity(),
            capacity,
            "byte allocation has its admitted capacity"
        );
        if let Some(old) = &self.buffer {
            value.extend_from_slice(&old.value);
        }
        self.buffer = Some(OwnedQueryBuffer {
            value,
            _reservation: reservation,
        });
        Ok(())
    }

    fn append(&mut self, text: &str) -> Result<(), QueryBufferError> {
        let length = self.buffer.as_ref().map_or(0, |buffer| buffer.value.len());
        let needed = length
            .checked_add(text.len())
            .ok_or(QueryBufferError::ResultLimit)?;
        if needed > self.limit {
            return Err(QueryBufferError::ResultLimit);
        }
        if needed > 0 {
            self.grow(needed)?;
            self.buffer
                .as_mut()
                .expect("nonempty append has an allocation")
                .value
                .extend_from_slice(text.as_bytes());
        }
        Ok(())
    }
}

struct JsonByteCount {
    length: usize,
    limit: usize,
}

impl fmt::Write for JsonByteCount {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.length = self.length.checked_add(text.len()).ok_or(fmt::Error)?;
        if self.length > self.limit {
            return Err(fmt::Error);
        }
        Ok(())
    }
}

impl fmt::Write for QueryBufferWriter<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.error.is_some() {
            return Err(fmt::Error);
        }
        self.append(text).map_err(|error| {
            self.error = Some(error);
            fmt::Error
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::memory_pools::MemoryPool;
    use crate::RedDBOptions;
    use std::fmt::Write;

    #[test]
    fn buffer_growth_admits_both_allocations_and_keeps_the_original_on_refusal() {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(1024 * 1024))
                .expect("runtime");
        runtime.refresh_memory_accounting();
        let headroom =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let value = crate::json::Value::String("x".repeat(2047));
        let mut writer = QueryBufferWriter {
            runtime: &runtime,
            value: &value,
            buffer: None,
            limit: usize::MAX,
            error: None,
        };
        writer
            .write_str(&"x".repeat(1024))
            .expect("first allocation");
        let other = runtime
            .try_reserve_query_buffer(
                usize::try_from(headroom).expect("test budget fits usize") - 1024 - 2048,
            )
            .expect("leave room for 2048 more bytes");
        assert!(writer.write_str(&"y".repeat(1025)).is_err());
        assert_eq!(writer.error, Some(QueryBufferError::MemoryBudget));
        let original = &writer
            .buffer
            .as_ref()
            .expect("original survives refusal")
            .value;
        assert_eq!(original, &vec![b'x'; 1024]);
        assert_eq!(original.capacity(), 1024);
        assert!(writer.write_str("ignored after failure").is_err());
        assert!(
            runtime
                .memory_accounting()
                .enforcement_snapshot()
                .high_water_bytes
                <= runtime.memory_budget().resolved_bytes
        );
        drop(other);
        drop(writer);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
        runtime
            .serialize_query_json(&crate::json::Value::String("z".repeat(4096)))
            .expect("released budget is reusable");
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }

    #[test]
    fn owned_json_preserves_the_canonical_encoding_and_outlives_the_runtime() {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(1024 * 1024))
                .expect("runtime");
        let value = crate::json::Value::from(
            crate::json::parse_json(
                r#"{"nested":[null,true,false,-4,1.25,"quotes\"slash\\newline\n雪"],"empty":{}}"#,
            )
            .expect("JSON fixture"),
        );
        let accounting = std::sync::Arc::clone(&runtime.inner.memory_accounting);
        let buffer = runtime.serialize_query_json(&value).expect("admitted JSON");
        assert_eq!(buffer.as_ref(), value.to_string_compact().as_bytes());
        let charged = accounting.used_bytes(MemoryPool::QueryBuffers);
        assert!(charged >= buffer.len() as u64);
        let slice = buffer.slice(0..1);
        drop(runtime);
        drop(buffer);
        assert_eq!(slice.as_ref(), b"{");
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), charged);
        drop(slice);
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), 0);
    }

    #[test]
    fn tight_headroom_admits_the_final_size_without_repeated_tiny_reallocations() {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(1024 * 1024))
                .expect("runtime");
        runtime.refresh_memory_accounting();
        let headroom =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        // The last geometric growth would temporarily own 32KiB + 64KiB.
        // The complete escaped JSON needs only 60,000 bytes, which fits
        // alongside the existing 32KiB prefix within this shared headroom.
        let other = runtime
            .try_reserve_query_buffer(
                usize::try_from(headroom).expect("test budget fits usize") - (3 * 32768 - 1000),
            )
            .expect("hold most of the shared budget");
        let value = crate::json::Value::String("\n".repeat(29999));
        let buffer = runtime
            .serialize_query_json(&value)
            .expect("exact final capacity fits");
        assert_eq!(buffer.len(), 60000);
        assert_eq!(buffer.as_ref(), value.to_string_compact().as_bytes());
        assert!(
            runtime
                .memory_accounting()
                .enforcement_snapshot()
                .high_water_bytes
                <= runtime.memory_budget().resolved_bytes
        );
        drop(other);
        drop(buffer);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }

    #[test]
    fn result_limit_includes_json_escaping_and_releases_partial_buffers() {
        let runtime = RedDBRuntime::with_pool_and_limits(
            RedDBOptions::in_memory().with_memory_budget(1024 * 1024),
            super::super::ConnectionPoolConfig::default(),
            super::super::resource_limits::ResourceLimits {
                max_result_bytes: Some(1024),
                ..Default::default()
            },
        )
        .expect("runtime");
        let fitting = crate::json::Value::String("\n".repeat(511));
        let buffer = runtime
            .serialize_query_json(&fitting)
            .expect("exactly 1024 encoded bytes");
        assert_eq!(buffer.len(), 1024);
        drop(buffer);
        let oversized = crate::json::Value::String("\n".repeat(512));
        assert_eq!(
            runtime.serialize_query_json(&oversized),
            Err(QueryBufferError::ResultLimit)
        );
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
        runtime
            .serialize_query_json(&fitting)
            .expect("retry after partial serialization");
    }

    #[test]
    fn concurrent_serializers_share_admission_and_return_every_reservation() {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(1024 * 1024))
                .expect("runtime");
        let value = crate::json::Value::String("x".repeat(64 * 1024));
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..32)
                .map(|_| scope.spawn(|| runtime.serialize_query_json(&value)))
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("serializer thread"))
                .collect::<Vec<_>>()
        });
        assert!(results.iter().any(Result::is_ok), "fitting buffers succeed");
        assert!(
            results
                .iter()
                .any(|result| *result == Err(QueryBufferError::MemoryBudget)),
            "concurrent held buffers exceed one shared budget"
        );
        runtime.refresh_memory_accounting();
        let accounting = runtime.memory_accounting();
        assert!(accounting.total_used_bytes() <= runtime.memory_budget().resolved_bytes);
        assert!(
            accounting.enforcement_snapshot().high_water_bytes
                <= runtime.memory_budget().resolved_bytes
        );
        drop(results);
        assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), 0);
        assert!(
            runtime.serialize_query_json(&value).is_ok(),
            "headroom returns after all joins drop"
        );
    }
}
