//! Fallible ledger reads, with scoped filtering and a bounded sort window.
use super::*;
use crate::runtime::memory_admission::QueryBufferReservation;
use crate::storage::unified::memory_size::value_heap_bytes;
use std::mem::size_of;

pub(super) struct Snapshot {
    pub records: Vec<UnifiedRecord>,
    reservation: QueryBufferReservation,
    charged: usize,
    row_bytes: usize,
    sort_columns: usize,
}

fn denied(runtime: &RedDBRuntime) -> RedDBError {
    runtime.memory_accounting().record_admission_denied();
    RedDBError::MaterializationLimitExceeded {
        executor: "control_events",
        limit: usize::try_from(runtime.memory_budget().resolved_bytes).unwrap_or(usize::MAX),
        current: usize::try_from(runtime.memory_accounting().total_used_bytes())
            .unwrap_or(usize::MAX)
            .saturating_add(1),
    }
}

impl Snapshot {
    fn grow(&mut self, runtime: &RedDBRuntime, additional: usize) -> RedDBResult<()> {
        if !self.reservation.try_grow(additional) {
            return Err(denied(runtime));
        }
        self.charged = self.charged.saturating_add(additional);
        Ok(())
    }

    fn bytes(&self, record: &UnifiedRecord) -> usize {
        // Account for retained values and the simultaneously live projection /
        // ordinary ordering keys. Decode scratch has its own reservation.
        CONTROL_EVENT_COLUMNS
            .iter()
            .filter_map(|column| record.get(column))
            .map(value_heap_bytes)
            .fold(0, usize::saturating_add)
            .saturating_add(
                CONTROL_EVENT_COLUMNS
                    .len()
                    .saturating_mul(size_of::<Value>()),
            )
            .saturating_mul(2usize.saturating_add(self.sort_columns))
            .saturating_add(self.sort_columns.saturating_mul(128))
    }

    fn push(&mut self, runtime: &RedDBRuntime, record: UnifiedRecord) -> RedDBResult<()> {
        let bytes = self.bytes(&record);
        self.grow(runtime, bytes)?;
        if self.records.len() == self.records.capacity() {
            let old_capacity = self.records.capacity();
            let capacity = old_capacity.saturating_mul(2).max(8);
            // Admit old and new vectors together before a possible realloc.
            self.grow(runtime, capacity.saturating_mul(size_of::<UnifiedRecord>()))?;
            self.records
                .try_reserve_exact(capacity - self.records.len())
                .map_err(|_| denied(runtime))?;
            self.charged -= old_capacity * size_of::<UnifiedRecord>();
            self.reservation.shrink_to(self.charged);
        }
        self.row_bytes = self.row_bytes.saturating_add(bytes);
        self.records.push(record);
        Ok(())
    }

    fn prune(&mut self, query: &TableQuery, db: &crate::RedDB, keep: usize) -> RedDBResult<()> {
        let old_capacity = self.records.capacity();
        super::super::join_filter::try_sort_records_by_order_by_with_db(
            Some(db),
            &mut self.records,
            &query.order_by,
            Some(&query.table),
            query.alias.as_deref(),
        )?;
        self.records.truncate(keep);
        let retained = self
            .records
            .iter()
            .map(|row| self.bytes(row))
            .fold(0, usize::saturating_add);
        // The ordering helper replaces the vector with one sized to its length.
        // Return the old spare capacity as well as the discarded row payloads.
        self.charged -=
            old_capacity.saturating_sub(self.records.capacity()) * size_of::<UnifiedRecord>();
        self.charged -= self.row_bytes - retained;
        self.row_bytes = retained;
        self.reservation.shrink_to(self.charged);
        Ok(())
    }
}

pub(super) fn read(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    query: &TableQuery,
) -> RedDBResult<Snapshot> {
    runtime.refresh_memory_accounting();
    let reservation = runtime
        .try_reserve_query_buffer_from_sample(0)
        .ok_or_else(|| denied(runtime))?;
    let mut result = Snapshot {
        records: Vec::new(),
        reservation,
        charged: 0,
        row_bytes: 0,
        sort_columns: query.order_by.len(),
    };
    if query.limit == Some(0) {
        return Ok(result);
    }
    let db = runtime.db();
    let Some(manager) = db
        .store()
        .get_collection(super::super::control_events::CONTROL_EVENTS_COLLECTION)
    else {
        return Ok(result);
    };
    // Encoded input plus decoded row share this allowance. A second allowance
    // covers the temporary schema projection while the source row is alive.
    let read_bytes = usize::try_from(runtime.memory_budget().resolved_bytes / 16)
        .unwrap_or(usize::MAX)
        .clamp(1, 16 * 1024 * 1024);
    let _scratch = runtime
        .try_reserve_query_buffer_from_sample(read_bytes * 2)
        .ok_or_else(|| denied(runtime))?;
    let schema = Arc::new(
        CONTROL_EVENT_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let filter = effective_table_filter(query);
    let offset = usize::try_from(query.offset.unwrap_or(0)).unwrap_or(usize::MAX);
    let limit = query
        .limit
        .map(|limit| usize::try_from(limit).unwrap_or(usize::MAX));
    let ordered = !query.order_by.is_empty();
    let keep = limit
        .map(|limit| offset.saturating_add(limit))
        .unwrap_or(usize::MAX);
    let cap = crate::runtime::materialization_limit::max_materialized_rows(db.as_ref())?;
    let mut skipped = 0;
    crate::runtime::function_budget::try_scan_result(
        |visit| manager.try_scan_for_each(None, read_bytes, visit),
        |entity| {
            let Some(row) = entity.data.as_row() else {
                return Ok(true);
            };
            if let Some(tenant) = tenant {
                if !matches!(row.get_field("scope"), Some(Value::Text(scope)) if scope.as_ref() == tenant)
                {
                    return Ok(true);
                }
            }
            let record = UnifiedRecord::with_schema(
                Arc::clone(&schema),
                CONTROL_EVENT_COLUMNS
                    .iter()
                    .map(|column| row.get_field(column).cloned().unwrap_or(Value::Null))
                    .collect(),
            );
            if let Some(filter) = &filter {
                if !super::super::join_filter::evaluate_runtime_filter_result_with_db(
                    Some(db.as_ref()),
                    &record,
                    filter,
                    Some(&query.table),
                    query.alias.as_deref(),
                )? {
                    return Ok(true);
                }
            }
            if !ordered && skipped < offset {
                skipped += 1;
                return Ok(true);
            }
            if result.records.len() >= cap {
                return Err(RedDBError::MaterializationLimitExceeded {
                    executor: "control_events",
                    limit: cap,
                    current: result.records.len().saturating_add(1),
                });
            }
            result.push(runtime, record)?;
            if !ordered {
                return Ok(limit.is_none_or(|limit| result.records.len() < limit));
            }
            // Keep at most two windows (or a small batch), independent of the
            // total ledger length. Stable sorting preserves ties across batches.
            if result.records.len() >= keep.saturating_mul(2).max(64).min(cap) {
                result.prune(query, db.as_ref(), keep)?;
            }
            Ok(true)
        },
    )?;
    if ordered {
        result.prune(query, db.as_ref(), keep)?;
        let skipped = offset.min(result.records.len());
        result.records.drain(..skipped);
        if let Some(limit) = limit {
            result.records.truncate(limit);
        }
    }
    Ok(result)
}
