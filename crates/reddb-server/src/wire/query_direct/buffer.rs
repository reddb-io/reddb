//! Admitted direct-scan encoding. No storage sampling occurs in row callbacks.

use super::*;
use crate::runtime::memory_admission::QueryBufferReservation;
use crate::runtime::memory_buffers::{GrowingQueryBuffer, QueryBufferError};

pub(super) struct DirectResultBuffer<'a> {
    buffer: GrowingQueryBuffer<'a>,
    row_count_offset: Option<usize>,
    error: Option<QueryBufferError>,
}

impl<'a> DirectResultBuffer<'a> {
    pub(super) fn new(runtime: &'a RedDBRuntime) -> Self {
        let wire_limit =
            reddb_wire::redwire::MAX_FRAME_SIZE as usize - reddb_wire::redwire::FRAME_HEADER_SIZE;
        let limit = runtime
            .resource_limits()
            .max_result_bytes
            .filter(|limit| *limit > 0)
            .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX))
            .unwrap_or(usize::MAX)
            .min(wire_limit)
            + 5;
        let mut buffer = GrowingQueryBuffer::new(runtime, limit);
        let error = buffer
            .append(5, |out| {
                reddb_wire::legacy::write_frame_header(out, reddb_wire::legacy::MSG_RESULT, 0);
            })
            .err();
        Self {
            buffer,
            row_count_offset: None,
            error,
        }
    }

    pub(super) fn workspace(
        &self,
        bytes: usize,
    ) -> Result<QueryBufferReservation, QueryBufferError> {
        self.buffer.reserve_workspace(bytes)
    }

    pub(super) fn fail(&mut self, error: QueryBufferError) {
        self.error.get_or_insert(error);
    }
    pub(super) fn failed(&self) -> bool {
        self.error.is_some()
    }

    pub(super) fn header<'b>(&mut self, columns: impl ExactSizeIterator<Item = &'b str> + Clone) {
        if self.failed() {
            return;
        }
        if columns.len() > u16::MAX as usize
            || columns.clone().any(|name| name.len() > u16::MAX as usize)
        {
            self.fail(QueryBufferError::ResultLimit);
            return;
        }
        let length = columns
            .clone()
            .try_fold(6usize, |sum, name| sum.checked_add(2 + name.len()));
        let Some(length) = length else {
            self.fail(QueryBufferError::ResultLimit);
            return;
        };
        let mut offset = 0;
        match self.buffer.append(length, |out| {
            offset = encode_result_payload_header(out, columns, 0);
        }) {
            Ok(()) => self.row_count_offset = Some(offset),
            Err(error) => self.fail(error),
        }
    }

    pub(super) fn row(
        &mut self,
        entity: &UnifiedEntity,
        row: &RowData,
        columns: &[WireColumn],
    ) -> bool {
        if self.failed() {
            return false;
        }
        if let Err(error) = append_row(&mut self.buffer, entity, row, columns) {
            self.fail(error);
        }
        !self.failed()
    }

    pub(super) fn encoded_row(
        &self,
        entity: &UnifiedEntity,
        row: &RowData,
        columns: &[WireColumn],
    ) -> Result<GrowingQueryBuffer<'a>, QueryBufferError> {
        let length = row_bytes(entity, row, columns)?;
        let mut buffer = self.buffer.sibling(length);
        buffer.append(length, |out| {
            for column in columns {
                encode_entity_wire_value(out, entity, row, column);
            }
        })?;
        Ok(buffer)
    }

    pub(super) fn append_encoded(&mut self, bytes: &[u8]) -> bool {
        if self.failed() {
            return false;
        }
        if let Err(error) = self
            .buffer
            .append(bytes.len(), |out| out.extend_from_slice(bytes))
        {
            self.fail(error);
        }
        !self.failed()
    }

    pub(super) fn finish(mut self, rows: u32) -> BinaryQueryResponse {
        if let Some(error) = self.error {
            return buffer_error(error);
        }
        let offset = self
            .row_count_offset
            .expect("successful direct response has its column header");
        self.buffer.patch(|out| {
            set_result_payload_row_count(out, offset, rows).expect("reserved row-count field");
            build_legacy_result_frame_in_place(out).expect("bounded complete legacy frame");
        });
        BinaryQueryResponse::Admitted(self.buffer.finish())
    }
}

fn append_row(
    buffer: &mut GrowingQueryBuffer<'_>,
    entity: &UnifiedEntity,
    row: &RowData,
    columns: &[WireColumn],
) -> Result<(), QueryBufferError> {
    let length = row_bytes(entity, row, columns)?;
    buffer.append(length, |out| {
        for column in columns {
            encode_entity_wire_value(out, entity, row, column);
        }
    })
}

fn row_bytes(
    entity: &UnifiedEntity,
    row: &RowData,
    columns: &[WireColumn],
) -> Result<usize, QueryBufferError> {
    columns
        .iter()
        .try_fold(0usize, |sum, column| {
            sum.checked_add(reddb_wire::legacy::native_value_encoded_len(
                &entity_wire_value(entity, row, column),
            ))
        })
        .ok_or(QueryBufferError::ResultLimit)
}

pub(super) fn buffer_error(error: QueryBufferError) -> BinaryQueryResponse {
    build_legacy_error_frame(match error {
        QueryBufferError::MemoryBudget => b"direct response exceeds shared memory budget",
        QueryBufferError::ResultLimit => b"direct response exceeds result size limit",
    })
    .into()
}

/// WireColumn owns up to two Arc<str> allocations plus its vector slot. Leave
/// room for both Arc headers and alignment without trusting short-name hints.
pub(super) fn column_bytes(name: &str, source: &str) -> usize {
    std::mem::size_of::<WireColumn>()
        .saturating_add(name.len())
        .saturating_add(source.len())
        .saturating_add(64)
}

pub(super) fn named_columns_bytes(names: &[String]) -> usize {
    names
        .iter()
        .map(|name| column_bytes(name, name))
        .fold(0, usize::saturating_add)
}

pub(super) fn query_columns_bytes(
    query: &TableQuery,
    schema: Option<&[String]>,
    row: Option<&RowData>,
) -> usize {
    if query
        .select_items
        .iter()
        .any(|item| matches!(item, SelectItem::Wildcard))
    {
        let base = ["rid", "created_at", "updated_at"]
            .into_iter()
            .map(|name| column_bytes(name, name))
            .sum::<usize>();
        let columns = if let Some(schema) =
            schema.or_else(|| row.and_then(|row| row.schema.as_deref().map(Vec::as_slice)))
        {
            named_columns_bytes(schema)
        } else {
            row.and_then(|row| row.named.as_ref()).map_or(0, |named| {
                named
                    .keys()
                    .map(|name| column_bytes(name, name))
                    .fold(0, usize::saturating_add)
            })
        };
        return base.saturating_add(columns);
    }
    query
        .select_items
        .iter()
        .map(|item| match item {
            SelectItem::Expr {
                expr:
                    Expr::Column {
                        field: FieldRef::TableColumn { column, .. },
                        ..
                    },
                alias,
            } => column_bytes(alias.as_deref().unwrap_or(column), column),
            // The resolver reserves one vector slot per projection item even
            // when a field is not represented by this direct wire projection.
            _ => std::mem::size_of::<WireColumn>(),
        })
        .fold(0, usize::saturating_add)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::memory_pools::MemoryPool;
    use crate::RedDBOptions;

    #[test]
    fn unrepresentable_column_headers_return_errors_without_partial_results() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        let name = "x".repeat(u16::MAX as usize + 1);
        let mut body = DirectResultBuffer::new(&runtime);
        body.header([name.as_str()].into_iter());
        assert_eq!(body.finish(0)[4], reddb_wire::legacy::MSG_ERROR);
        let mut body = DirectResultBuffer::new(&runtime);
        body.header((0..u16::MAX as usize + 1).map(|_| "x"));
        assert_eq!(body.finish(0)[4], reddb_wire::legacy::MSG_ERROR);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }

    #[test]
    fn sparse_wildcard_metadata_counts_names_instead_of_available_values() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        let query = reddb_rql::modes::parse_multi("SELECT * FROM sparse LIMIT 1").unwrap();
        let QueryExpr::Table(query) = query else {
            panic!("table query")
        };
        let row = RowData {
            columns: vec![Value::Integer(7)],
            named: None,
            schema: Some(Arc::new(
                (0..4096).map(|index| format!("field_{index}")).collect(),
            )),
        };
        let mut body = DirectResultBuffer::new(&runtime);
        let charge = body
            .workspace(query_columns_bytes(&query, None, Some(&row)))
            .unwrap();
        let columns = resolve_wire_columns(&query, &row);
        assert_eq!(columns.len(), 4099);
        assert_eq!(columns.capacity(), columns.len());
        assert!(
            query_columns_bytes(&query, None, Some(&row))
                >= columns.capacity() * std::mem::size_of::<WireColumn>()
        );
        body.header(columns.iter().map(|column| column.name.as_ref()));
        drop(columns);
        drop(charge);
        let response = body.finish(0);
        assert_eq!(response[4], reddb_wire::legacy::MSG_RESULT);
        assert_eq!(u16::from_le_bytes(response[5..7].try_into().unwrap()), 4099);
        drop(response);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }
}
