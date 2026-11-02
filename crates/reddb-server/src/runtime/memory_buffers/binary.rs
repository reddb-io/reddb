//! Materialized binary replies reserve metadata and output before allocation.

use super::QueryBufferError;
use crate::runtime::{RedDBRuntime, RuntimeQueryResult};
use crate::storage::query::unified::UnifiedRecord;
use reddb_types::Value;
use reddb_wire::legacy::{
    encode_native_value, encode_result_payload_header, native_value_encoded_len,
    write_frame_header, MSG_RESULT,
};
use std::sync::Arc;

struct ColumnResolver<'a> {
    name: &'a str,
    index: Option<usize>,
}

impl RedDBRuntime {
    pub(crate) fn serialize_binary_result(
        &self,
        result: &RuntimeQueryResult,
    ) -> Result<bytes::Bytes, QueryBufferError> {
        if let Some(json) = &result.result.pre_serialized_json {
            self.check_binary_payload_size(json.len())?;
            return self.serialize_query_buffer(5 + json.len(), 0, |out| {
                write_frame_header(out, MSG_RESULT, json.len() as u32);
                out.extend_from_slice(json.as_bytes());
            });
        }
        let records = &result.result.records;
        let first = records.first();
        let columns_count = if result.result.columns.is_empty() {
            first.map_or(0, |row| row.field_count())
        } else {
            result.result.columns.len()
        };
        let scratch_bytes = columns_count
            .checked_mul(std::mem::size_of::<ColumnResolver<'_>>())
            .ok_or(QueryBufferError::MemoryBudget)?;
        // Declared before the vector so the allocation drops before its charge,
        // including all early returns and unwinding paths.
        let _scratch = self
            .try_reserve_query_buffer(scratch_bytes)
            .ok_or_else(|| {
                self.memory_accounting().record_admission_denied();
                QueryBufferError::MemoryBudget
            })?;
        let mut columns = Vec::new();
        columns
            .try_reserve_exact(columns_count)
            .map_err(|_| QueryBufferError::MemoryBudget)?;
        let mut add_column = |name| {
            let index =
                first.and_then(|row| row.columns().iter().rposition(|key| key.as_ref() == name));
            columns.push(ColumnResolver { name, index });
        };
        if !result.result.columns.is_empty() {
            for name in &result.result.columns {
                add_column(name.as_str());
            }
        } else if let Some(first) = first {
            for name in first.columns() {
                add_column(name.as_ref());
            }
            if let Some(overflow) = first.overflow() {
                for name in overflow.keys() {
                    add_column(name.as_ref());
                }
            }
        }
        assert_eq!(
            columns.capacity(),
            columns_count,
            "metadata stays within admission"
        );
        let schema = first.and_then(|row| row.columnar_schema());
        // Resolve once, then reuse indexes in both passes. Neither pass clones
        // values; the first only counts text lengths, not text bytes.
        let mut payload_len = Some(6usize);
        for column in &columns {
            payload_len = payload_len.and_then(|len| len.checked_add(2 + column.name.len()));
        }
        visit_binary_values(records, &columns, schema, |value| {
            payload_len =
                payload_len.and_then(|len| len.checked_add(native_value_encoded_len(value)));
        });
        let payload_len = payload_len.ok_or(QueryBufferError::ResultLimit)?;
        self.check_binary_payload_size(payload_len)?;
        self.serialize_query_buffer(5 + payload_len, 0, |out| {
            write_frame_header(out, MSG_RESULT, payload_len as u32);
            encode_result_payload_header(
                out,
                columns.iter().map(|column| column.name),
                records.len() as u32,
            );
            visit_binary_values(records, &columns, schema, |value| {
                encode_native_value(out, value)
            });
        })
    }

    fn check_binary_payload_size(&self, length: usize) -> Result<(), QueryBufferError> {
        let wire_limit =
            reddb_wire::redwire::MAX_FRAME_SIZE as usize - reddb_wire::redwire::FRAME_HEADER_SIZE;
        let result_limit = self
            .resource_limits()
            .max_result_bytes
            .filter(|limit| *limit > 0)
            .unwrap_or(u64::MAX);
        if length > wire_limit || length as u64 > result_limit {
            return Err(QueryBufferError::ResultLimit);
        }
        Ok(())
    }
}

fn visit_binary_values(
    records: &[UnifiedRecord],
    columns: &[ColumnResolver<'_>],
    schema: Option<&Arc<Vec<Arc<str>>>>,
    mut callback: impl FnMut(&Value),
) {
    for record in records {
        let same_schema = schema
            .zip(record.columnar_schema())
            .is_some_and(|(left, right)| Arc::ptr_eq(left, right));
        for column in columns {
            let value = match (same_schema, column.index) {
                (true, Some(index)) => record.schema_values().get(index),
                _ => record.get(column.name),
            };
            callback(value.unwrap_or(&Value::Null));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::memory_pools::MemoryPool;

    fn fixture() -> RuntimeQueryResult {
        RuntimeQueryResult::ok_records("select".into(), vec![], vec![], "select")
    }

    #[test]
    fn binary_encoding_preserves_projected_schema_overflow_and_heterogeneous_rows() {
        let runtime = RedDBRuntime::in_memory().unwrap();
        let schema = Arc::new(vec![Arc::from("a"), Arc::from("b"), Arc::from("a")]);
        let first = UnifiedRecord::from_columnar(
            schema.clone(),
            vec![Value::Integer(1), Value::text("snow雪"), Value::Integer(2)],
        );
        let second = UnifiedRecord::from_columnar(
            schema,
            vec![
                Value::Integer(3),
                Value::Blob(vec![1, 2]),
                Value::Integer(4),
            ],
        );
        let third = UnifiedRecord::from_columnar(
            Arc::new(vec![Arc::from("b"), Arc::from("a")]),
            vec![Value::Boolean(true), Value::Integer(5)],
        );
        let mut fourth = UnifiedRecord::schemaless();
        fourth.set("a", Value::Integer(6));
        fourth.set("extra", Value::Timestamp(-7));
        let mut result = fixture();
        result.result.records = vec![first, second, third, fourth];
        for projected in [
            vec!["a".into(), "extra".into(), "b".into(), "absent".into()],
            vec![],
        ] {
            result.result.columns = projected;
            let columns: Vec<_> = if result.result.columns.is_empty() {
                result.result.records[0]
                    .column_names()
                    .iter()
                    .map(|name| name.to_string())
                    .collect()
            } else {
                result.result.columns.clone()
            };
            let mut body = Vec::new();
            encode_result_payload_header(&mut body, columns.iter().map(String::as_str), 4);
            for row in &result.result.records {
                for column in &columns {
                    reddb_wire::legacy::encode_value(
                        &mut body,
                        &reddb_wire::legacy::WireValue::from(
                            row.get(column).unwrap_or(&Value::Null),
                        ),
                    );
                }
            }
            let buffer = runtime.serialize_binary_result(&result).unwrap();
            assert_eq!(
                buffer.as_ref(),
                reddb_wire::legacy::build_legacy_result_frame(&body)
            );
            assert_eq!(
                runtime
                    .memory_accounting()
                    .used_bytes(MemoryPool::QueryBuffers),
                buffer.len() as u64
            );
            drop(buffer);
            assert_eq!(
                runtime
                    .memory_accounting()
                    .used_bytes(MemoryPool::QueryBuffers),
                0
            );
        }
        // Schema-free first rows must include their overflow column names.
        result.result.columns.clear();
        result.result.records.rotate_right(1);
        let buffer = runtime.serialize_binary_result(&result).unwrap();
        assert_eq!(u16::from_le_bytes([buffer[5], buffer[6]]), 2);
    }

    #[test]
    fn binary_result_budget_is_shared_and_owned_through_slices() {
        let runtime = RedDBRuntime::with_options(
            crate::RedDBOptions::in_memory().with_memory_budget(1024 * 1024),
        )
        .unwrap();
        let result = RuntimeQueryResult::ok_records(
            "select".into(),
            vec!["payload".into()],
            vec![vec![("payload".into(), Value::text("x".repeat(64 * 1024)))]],
            "select",
        );
        let responses = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..32)
                .map(|_| scope.spawn(|| runtime.serialize_binary_result(&result)))
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(responses.iter().any(Result::is_ok));
        assert!(responses
            .iter()
            .any(|response| *response == Err(QueryBufferError::MemoryBudget)));
        let retained = responses
            .iter()
            .find_map(|response| response.as_ref().ok())
            .unwrap()
            .slice(..1);
        let bytes = responses
            .iter()
            .find_map(|response| response.as_ref().ok())
            .unwrap()
            .len();
        drop(responses);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            bytes as u64
        );
        drop(retained);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
        runtime
            .serialize_binary_result(&result)
            .expect("retry after release");
        assert!(
            runtime
                .memory_accounting()
                .enforcement_snapshot()
                .high_water_bytes
                <= runtime.memory_budget().resolved_bytes
        );
    }

    #[test]
    fn binary_json_and_result_limits_use_encoded_size_and_release_metadata() {
        let runtime = RedDBRuntime::with_pool_and_limits(
            crate::RedDBOptions::in_memory().with_memory_budget(1024 * 1024),
            crate::runtime::ConnectionPoolConfig::default(),
            crate::runtime::resource_limits::ResourceLimits {
                max_result_bytes: Some(1024),
                ..Default::default()
            },
        )
        .unwrap();
        let mut result = fixture();
        result.result.pre_serialized_json = Some("x".repeat(1024));
        let buffer = runtime.serialize_binary_result(&result).unwrap();
        assert_eq!(
            buffer.as_ref(),
            reddb_wire::legacy::build_legacy_result_frame(&vec![b'x'; 1024])
        );
        drop(buffer);
        result.result.pre_serialized_json = Some("x".repeat(1025));
        assert_eq!(
            runtime.serialize_binary_result(&result),
            Err(QueryBufferError::ResultLimit)
        );
        result.result.pre_serialized_json = None;
        result.result.columns = vec!["x".repeat(1024)];
        assert_eq!(
            runtime.serialize_binary_result(&result),
            Err(QueryBufferError::ResultLimit)
        );
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }
}
