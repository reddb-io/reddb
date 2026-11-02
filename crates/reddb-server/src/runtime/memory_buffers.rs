//! Fallible serialization into buffers admitted by the shared runtime budget.

use super::memory_admission::QueryBufferReservation;
use super::RedDBRuntime;
use std::fmt;

mod binary;

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
    /// Admit an exact output allocation and its existing encoding scratch.
    /// The callback must free its scratch before returning; the output's charge
    /// travels with Bytes through queues, clones and the final socket write.
    pub(crate) fn serialize_query_buffer(
        &self,
        capacity: usize,
        scratch_bytes: usize,
        encode: impl FnOnce(&mut Vec<u8>),
    ) -> Result<bytes::Bytes, QueryBufferError> {
        let total = capacity
            .checked_add(scratch_bytes)
            .ok_or(QueryBufferError::MemoryBudget)?;
        let mut reservation = self.try_reserve_query_buffer(total).ok_or_else(|| {
            self.memory_accounting().record_admission_denied();
            QueryBufferError::MemoryBudget
        })?;
        let mut value = Vec::new();
        value
            .try_reserve_exact(capacity)
            .map_err(|_| QueryBufferError::MemoryBudget)?;
        encode(&mut value);
        assert_eq!(
            value.capacity(),
            capacity,
            "encoder must stay within admitted capacity"
        );
        reservation.shrink_to(value.capacity());
        Ok(bytes::Bytes::from_owner(OwnedQueryBuffer {
            value,
            _reservation: reservation,
        }))
    }

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
    fn binary_response_refuses_when_rows_fit_but_serialization_does_not() {
        use crate::runtime::query_request::{PreparedRegistry, QueryRequest, QueryRequestExecutor};
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(1024 * 1024))
                .expect("runtime");
        runtime
            .execute_query("CREATE TABLE binary_response_budget (payload TEXT)")
            .unwrap();
        runtime
            .execute_query_with_params(
                "INSERT INTO binary_response_budget (payload) VALUES ($1)",
                &[reddb_types::Value::text("x".repeat(64 * 1024))],
            )
            .unwrap();
        // OFFSET excludes the separate direct-scan implementation so this
        // exercises the real materialized query/serialization path.
        let sql = "SELECT payload FROM binary_response_budget OFFSET 0";
        assert!(crate::wire::query_direct::try_handle_query_binary_direct(&runtime, sql).is_none());
        let mut statements = crate::wire::listener::PreparedStatements::default();
        // The binary PREPARE contract auto-parameterizes literals; it does
        // not bind SQL $N placeholders. OFFSET remains a shape property.
        let prepare = reddb_wire::redwire::encode_prepare_payload(7, sql).unwrap();
        assert_eq!(
            crate::wire::listener::handle_prepare(&runtime, &prepare, &mut statements)[4],
            reddb_wire::legacy::MSG_PREPARED_OK
        );
        let execute = reddb_wire::redwire::encode_execute_prepared_payload(7, &[]).unwrap();
        let baseline =
            crate::wire::listener::handle_execute_prepared(&runtime, &execute, &statements);
        assert_eq!(
            baseline[4],
            reddb_wire::legacy::MSG_RESULT,
            "prepared baseline: {}",
            String::from_utf8_lossy(&baseline[5..])
        );
        drop(baseline);
        let prepared = PreparedRegistry::new();
        let result = QueryRequestExecutor::new(&runtime, &prepared)
            .execute(QueryRequest::sql(sql, Vec::new()))
            .unwrap();
        assert_eq!(result.result.records.len(), 1);
        assert!(result.result.pre_serialized_json.is_none());
        let result_charge = runtime
            .memory_accounting()
            .used_bytes(MemoryPool::QueryBuffers);
        assert!(result_charge >= 64 * 1024);
        drop(result);
        runtime.refresh_memory_accounting();
        let available =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let held = runtime
            .try_reserve_query_buffer((available - result_charge - 16 * 1024) as usize)
            .expect("leave room for result but not its serialized copy");
        let response = crate::wire::listener::handle_query_binary(&runtime, sql.as_bytes());
        assert_eq!(
            response[4],
            reddb_wire::legacy::MSG_ERROR,
            "a successful 64 KiB binary reply must not bypass its remaining 16 KiB encoding budget"
        );
        assert!(String::from_utf8_lossy(&response[5..])
            .to_lowercase()
            .contains("memory"));
        drop(response);
        let prepared_response =
            crate::wire::listener::handle_execute_prepared(&runtime, &execute, &statements);
        assert_eq!(prepared_response[4], reddb_wire::legacy::MSG_ERROR);
        assert!(
            String::from_utf8_lossy(&prepared_response[5..])
                .to_lowercase()
                .contains("memory"),
            "prepared refusal: {}",
            String::from_utf8_lossy(&prepared_response[5..])
        );
        drop(prepared_response);
        drop(held);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
        let response = crate::wire::listener::handle_query_binary(&runtime, sql.as_bytes());
        assert_eq!(
            response[4],
            reddb_wire::legacy::MSG_RESULT,
            "retry after budget release"
        );
        drop(response);
        let prepared_response =
            crate::wire::listener::handle_execute_prepared(&runtime, &execute, &statements);
        assert_eq!(
            prepared_response[4],
            reddb_wire::legacy::MSG_RESULT,
            "prepared retry after budget release"
        );
    }

    #[test]
    fn exact_serialization_admits_copy_and_scratch_before_running_encoder() {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(1024 * 1024))
                .expect("runtime");
        let scratch = vec![b'x'; 4096];
        let bytes = runtime
            .serialize_query_buffer(8192, scratch.capacity(), |buffer| {
                assert_eq!(
                    runtime
                        .memory_accounting()
                        .used_bytes(MemoryPool::QueryBuffers),
                    12288
                );
                buffer.extend_from_slice(&scratch);
                drop(scratch);
            })
            .expect("admitted copy");
        assert_eq!(bytes.len(), 4096);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            8192
        );
        drop(bytes);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
        assert_eq!(
            runtime.serialize_query_buffer(1024 * 1024, 1, |_| panic!("denied before encoding")),
            Err(QueryBufferError::MemoryBudget)
        );
        assert_eq!(
            runtime.serialize_query_buffer(usize::MAX, 1, |_| panic!("overflow before encoding")),
            Err(QueryBufferError::MemoryBudget)
        );
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }

    #[test]
    fn exact_serialization_unwind_releases_the_admitted_buffer() {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(1024 * 1024))
                .expect("runtime");
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = runtime.serialize_query_buffer(4096, 4096, |buffer| {
                buffer.extend_from_slice(b"partial");
                panic!("encoder failed");
            });
        }));
        assert!(panic.is_err());
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
        runtime
            .serialize_query_buffer(4096, 0, |buffer| buffer.extend_from_slice(b"retry"))
            .expect("reservation reusable");
    }

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
