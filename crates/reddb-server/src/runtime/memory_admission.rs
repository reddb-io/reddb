//! Admission enforcement over the shared memory accounting pool.

use crate::api::{RedDBError, RedDBResult};
use crate::storage::memory_pools::{MemoryPool, MEMORY_POOLS};
use crate::storage::unified::entity::UnifiedEntity;
use reddb_types::Value;

/// Reservations stay charged until a sample taken after their operation finishes.
/// The mutex serializes sampling and admission, never the mutation itself.
#[derive(Debug, Default)]
pub(super) struct MemoryReservations {
    pub(super) reserved_bytes: u64,
    pub(super) completed_bytes: u64,
}

/// Keep this guard through all storage/index writes, including failure cleanup.
/// Dropping it does not sample storage or immediately return headroom: an error
/// or panic can leave resident allocations behind.
#[derive(Debug)]
#[must_use = "hold the reservation until the operation finishes"]
pub(crate) struct MemoryReservation<'runtime> {
    reservations: &'runtime parking_lot::Mutex<MemoryReservations>,
    bytes: u64,
}

impl Drop for MemoryReservation<'_> {
    fn drop(&mut self) {
        let mut reservations = self.reservations.lock();
        reservations.completed_bytes += self.bytes;
        assert!(
            reservations.completed_bytes <= reservations.reserved_bytes,
            "invariant: completed reservations are a subset of reserved bytes"
        );
    }
}

const MAX_PRESSURE_TICKS: usize = 64;
const FIELD_BASE_BYTES: u64 = 64;
const INDEX_ENTRY_BYTES: u64 = 96;

impl crate::RedDBRuntime {
    pub(crate) fn admit_non_evictable_growth(
        &self,
        pool: MemoryPool,
        operation: &str,
        growth_bytes: u64,
    ) -> RedDBResult<MemoryReservation<'_>> {
        if let Some(reservation) = self.try_reserve_memory_growth(growth_bytes) {
            return Ok(reservation);
        }

        let accounting = self.memory_accounting();
        let used = accounting.total_used_bytes();
        if pool == MemoryPool::SegmentArena {
            let store = self.db().store();
            if store.reclaimable_segment_bytes() > 0 {
                let before = used;
                let mut reclaimed = 0;
                for _ in 0..MAX_PRESSURE_TICKS {
                    let pressure_advanced = store.pressure_consolidation_tick();
                    let maintenance_advanced = store.run_maintenance().is_ok();
                    if !pressure_advanced && !maintenance_advanced {
                        break;
                    }
                    let reservation = self.try_reserve_memory_growth(growth_bytes);
                    let after = accounting.total_used_bytes();
                    reclaimed = before.saturating_sub(after);
                    if let Some(reservation) = reservation {
                        accounting.record_pressure_reclamation(reclaimed);
                        return Ok(reservation);
                    }
                }
                accounting.record_pressure_reclamation(reclaimed);
            }
        }

        self.refresh_memory_accounting();
        accounting.record_admission_denied();
        Err(RedDBError::InvalidOperation(
            self.didactic_budget_error(operation, growth_bytes),
        ))
    }

    fn try_reserve_memory_growth(&self, growth_bytes: u64) -> Option<MemoryReservation<'_>> {
        let mut reservations = self.inner.memory_reservations.lock();
        self.refresh_memory_accounting_with_reservations(&mut reservations);
        let accounting = self.memory_accounting();
        let projected = accounting
            .total_used_bytes()
            .checked_add(reservations.reserved_bytes)?
            .checked_add(growth_bytes)?;
        if projected > accounting.budget().resolved_bytes {
            return None;
        }
        reservations.reserved_bytes += growth_bytes;
        Some(MemoryReservation {
            reservations: &self.inner.memory_reservations,
            bytes: growth_bytes,
        })
    }

    fn didactic_budget_error(&self, _operation: &str, growth_bytes: u64) -> String {
        let accounting = self.memory_accounting();
        let budget = accounting.budget().resolved_bytes;
        let reserved = self.inner.memory_reservations.lock().reserved_bytes;
        let used = accounting.total_used_bytes().saturating_add(reserved);
        let shortfall = used.saturating_add(growth_bytes).saturating_sub(budget);
        let percent_used = used.saturating_mul(100).checked_div(budget).unwrap_or(100);
        let mut pools = accounting.snapshot();
        pools.sort_by_key(|usage| std::cmp::Reverse(usage.used_bytes));
        let consumers = pools
            .iter()
            .take(3)
            .map(|usage| {
                format!(
                    "{} {}",
                    didactic_pool_name(usage.pool),
                    format_mib(usage.used_bytes)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            "operation needs ~{shortfall} bytes over budget {budget} ({percent_used}% used); \
             largest consumers: {consumers}; {reserved} bytes reserved - raise the budget or reclaim (see red.stats budget)"
        )
    }
}

pub(crate) fn estimate_row_growth(fields: &[(String, Value)]) -> u64 {
    entity_base_bytes().saturating_add(estimate_value_fields_bytes(fields))
}

pub(crate) fn estimate_timeseries_point_growth(
    metric: &str,
    tags: &std::collections::HashMap<String, String>,
    fields: &[(String, Value)],
) -> u64 {
    entity_base_bytes()
        .saturating_add(64)
        .saturating_add(metric.len() as u64)
        .saturating_add(
            tags.iter()
                .map(|(name, value)| {
                    FIELD_BASE_BYTES
                        .saturating_add(name.len() as u64)
                        .saturating_add(value.len() as u64)
                })
                .fold(0, u64::saturating_add),
        )
        .saturating_add(estimate_value_fields_bytes(fields))
}

pub(crate) fn estimate_index_growth(rows: &[Vec<(String, Value)>], columns: &[String]) -> u64 {
    rows.iter()
        .map(|fields| {
            INDEX_ENTRY_BYTES.saturating_add(
                columns
                    .iter()
                    .filter_map(|column| {
                        fields
                            .iter()
                            .find(|(name, _)| name == column)
                            .map(|(_, value)| estimate_value_bytes(value))
                    })
                    .fold(0, u64::saturating_add),
            )
        })
        .fold(0, u64::saturating_add)
}

fn estimate_value_bytes(value: &Value) -> u64 {
    match value {
        Value::Text(text) => text.len() as u64,
        Value::Blob(bytes) | Value::Json(bytes) => bytes.len() as u64,
        Value::Vector(values) => values.len() as u64 * 4,
        Value::NodeRef(value)
        | Value::EdgeRef(value)
        | Value::Email(value)
        | Value::Url(value)
        | Value::RowRef(value, _)
        | Value::VectorRef(value, _) => value.len() as u64,
        _ => 16,
    }
}

fn entity_base_bytes() -> u64 {
    std::mem::size_of::<UnifiedEntity>() as u64
}

fn estimate_value_fields_bytes(fields: &[(String, Value)]) -> u64 {
    fields
        .iter()
        .map(|(name, value)| {
            FIELD_BASE_BYTES
                .saturating_add(name.len() as u64)
                .saturating_add(estimate_value_bytes(value))
        })
        .fold(0, u64::saturating_add)
}

fn didactic_pool_name(pool: MemoryPool) -> &'static str {
    match pool {
        MemoryPool::SegmentArena => "segments",
        MemoryPool::PageCache => "page-cache",
        MemoryPool::IndexMemory => "indexes",
        MemoryPool::BlobCacheL1 => "blob-l1",
        MemoryPool::WalBuffers => "wal-buffers",
    }
}

fn format_mib(bytes: u64) -> String {
    let mib_tenths = bytes.saturating_mul(10) / (1 << 20);
    format!("{}.{} MiB", mib_tenths / 10, mib_tenths % 10)
}

#[allow(dead_code)]
fn _assert_all_pools_named() {
    for pool in MEMORY_POOLS {
        let _ = didactic_pool_name(pool);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedDBOptions, RedDBRuntime};

    #[test]
    fn concurrent_growth_cannot_spend_the_same_headroom() {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(128 * 1024))
                .expect("runtime");
        runtime.refresh_memory_accounting();
        let headroom = runtime.memory_accounting().budget().resolved_bytes
            - runtime.memory_accounting().total_used_bytes();
        let barrier = std::sync::Barrier::new(2);
        let admitted = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    barrier.wait();
                    let reservation = runtime.admit_non_evictable_growth(
                        MemoryPool::SegmentArena,
                        "concurrent growth",
                        headroom,
                    );
                    if reservation.is_ok() {
                        admitted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                    barrier.wait();
                    drop(reservation);
                });
            }
        });
        assert_eq!(
            admitted.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "only one writer may reserve the available headroom"
        );
        let _reservation = runtime
            .admit_non_evictable_growth(MemoryPool::SegmentArena, "released growth", headroom)
            .expect("unused reservations are reusable");
    }

    fn runtime_with_headroom() -> (RedDBRuntime, u64) {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(128 * 1024))
                .expect("runtime");
        runtime.refresh_memory_accounting();
        let headroom = runtime.memory_accounting().budget().resolved_bytes
            - runtime.memory_accounting().total_used_bytes();
        (runtime, headroom)
    }

    #[test]
    fn sampling_preserves_active_reservations_across_pools() {
        let (runtime, headroom) = runtime_with_headroom();
        let segment_bytes = headroom / 2;
        let segment = runtime
            .admit_non_evictable_growth(MemoryPool::SegmentArena, "segment", segment_bytes)
            .expect("segment reservation");
        let index = runtime
            .admit_non_evictable_growth(MemoryPool::IndexMemory, "index", headroom - segment_bytes)
            .expect("index uses only remaining headroom");
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..16 {
                    runtime.refresh_memory_accounting();
                }
            });
            scope.spawn(|| {
                for _ in 0..16 {
                    assert!(runtime
                        .admit_non_evictable_growth(
                            MemoryPool::IndexMemory,
                            "no remaining headroom",
                            1,
                        )
                        .is_err());
                }
            });
        });
        drop(segment);
        let replacement = runtime
            .admit_non_evictable_growth(
                MemoryPool::SegmentArena,
                "reuse only completed segment",
                segment_bytes,
            )
            .expect("completed reservation can be reconciled while index remains active");
        assert!(runtime
            .admit_non_evictable_growth(MemoryPool::IndexMemory, "index is still reserved", 1,)
            .is_err());
        drop((index, replacement));
        let _reservation = runtime
            .admit_non_evictable_growth(MemoryPool::SegmentArena, "all released", headroom)
            .expect("all unused headroom returns");
    }

    #[test]
    fn failed_growth_is_resampled_before_its_reservation_returns() {
        let (runtime, _) = runtime_with_headroom();
        let store = runtime.db().store();
        store.create_collection("partial").expect("collection");
        runtime.refresh_memory_accounting();
        let budget = runtime.memory_accounting().budget().resolved_bytes;
        let before = runtime.memory_accounting().total_used_bytes();
        let result: RedDBResult<()> = (|| {
            let _reservation = runtime.admit_non_evictable_growth(
                MemoryPool::SegmentArena,
                "partial operation",
                budget - before,
            )?;
            store
                .insert_auto(
                    "partial",
                    UnifiedEntity::vector(
                        crate::storage::EntityId::new(0),
                        "partial",
                        vec![1.0; 1024],
                    ),
                )
                .expect("resident write before error");
            Err(RedDBError::InvalidOperation(
                "failure after write".to_string(),
            ))
        })();
        assert!(result.is_err());
        assert!(
            runtime
                .admit_non_evictable_growth(
                    MemoryPool::SegmentArena,
                    "old headroom",
                    budget - before,
                )
                .is_err(),
            "a failed operation can still leave resident data"
        );
        let after = runtime.memory_accounting().total_used_bytes();
        assert!(after > before);
        let _reservation = runtime
            .admit_non_evictable_growth(
                MemoryPool::IndexMemory,
                "actual remaining headroom",
                budget - after,
            )
            .expect("completed reservation is replaced by resident usage, not leaked");
    }

    #[test]
    fn unwinding_releases_unused_reservations() {
        let (runtime, headroom) = runtime_with_headroom();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _reservation = runtime
                .admit_non_evictable_growth(MemoryPool::SegmentArena, "unwinding", headroom)
                .expect("reservation");
            panic!("operation failed");
        }));
        assert!(result.is_err());
        let _reservation = runtime
            .admit_non_evictable_growth(MemoryPool::SegmentArena, "after unwind", headroom)
            .expect("unwinding cannot leak an unused reservation");
    }

    #[test]
    fn timeseries_index_admission_keeps_the_segment_reservation() {
        let (runtime, _) = runtime_with_headroom();
        runtime
            .execute_query("CREATE TIMESERIES samples")
            .expect("timeseries");
        runtime
            .execute_query("CREATE INDEX body_idx ON samples (body) USING HASH")
            .expect("index");
        runtime.refresh_memory_accounting();
        let headroom = runtime.memory_accounting().budget().resolved_bytes
            - runtime.memory_accounting().total_used_bytes();
        let fields = vec![("body".to_string(), Value::text("pending"))];
        let segment_bytes =
            estimate_timeseries_point_growth("cpu", &std::collections::HashMap::new(), &fields);
        let index_bytes =
            estimate_index_growth(std::slice::from_ref(&fields), &["body".to_string()]);
        let held = runtime
            .admit_non_evictable_growth(
                MemoryPool::IndexMemory,
                "competing writer",
                headroom - segment_bytes - index_bytes / 2,
            )
            .expect("leave enough for the segment alone");
        let sql = "INSERT INTO samples (metric, value, body) VALUES ('cpu', 1.0, 'pending')";
        let error = runtime
            .execute_query(sql)
            .expect_err("combined segment and index must fit");
        assert!(error.to_string().contains("bytes reserved"), "{error}");
        assert!(runtime
            .execute_query("SELECT * FROM samples")
            .expect("read after denial")
            .result
            .records
            .is_empty());
        // The failed nested admission releases its segment reservation while
        // the other writer remains active.
        let _remaining = runtime
            .admit_non_evictable_growth(
                MemoryPool::SegmentArena,
                "returned segment",
                segment_bytes + index_bytes / 2,
            )
            .expect("failed insert releases its unused reservation");
        drop((_remaining, held));
        runtime
            .execute_query(sql)
            .expect("same insert fits after competing writer finishes");
        assert_eq!(
            runtime
                .execute_query("SELECT * FROM samples")
                .expect("read after insert")
                .result
                .records
                .len(),
            1
        );
    }

    #[test]
    fn row_indexes_must_fit_before_single_or_batch_insert() {
        for batch_size in [1, 3] {
            for explicit_indexes in [false, true] {
                let (runtime, _) = runtime_with_headroom();
                runtime
                    .execute_query("CREATE TABLE indexed_rows (id INT, body TEXT)")
                    .expect("table");
                if explicit_indexes {
                    runtime
                        .execute_query("CREATE INDEX id_hash ON indexed_rows (id) USING HASH")
                        .expect("id index");
                    runtime
                        .execute_query("CREATE INDEX body_hash ON indexed_rows (body) USING HASH")
                        .expect("body index");
                }
                runtime.refresh_memory_accounting();
                let headroom = runtime.memory_budget().resolved_bytes
                    - runtime.memory_accounting().total_used_bytes();
                let fields = vec![
                    ("id".to_string(), Value::Integer(1)),
                    ("body".to_string(), Value::text("payload")),
                ];
                let row_bytes = estimate_row_growth(&fields) * batch_size;
                let held = runtime
                    .admit_non_evictable_growth(
                        MemoryPool::SegmentArena,
                        "competing operation",
                        headroom - row_bytes,
                    )
                    .expect("leave room for rows but no indexes");
                let rows = (1..=batch_size)
                    .map(|id| format!("({id}, 'payload')"))
                    .collect::<Vec<_>>()
                    .join(", ");
                let sql = format!("INSERT INTO indexed_rows (id, body) VALUES {rows}");
                assert!(runtime.execute_query(&sql).is_err(),
                    "index growth must be reserved: batch={batch_size}, explicit={explicit_indexes}");
                assert!(runtime
                    .execute_query("SELECT * FROM indexed_rows")
                    .expect("read after rejection")
                    .result
                    .records
                    .is_empty());
                if !explicit_indexes {
                    assert!(
                        runtime
                            .index_store_ref()
                            .list_indices("indexed_rows")
                            .is_empty(),
                        "rejection must precede implicit index creation"
                    );
                }
                drop(held);
                runtime
                    .execute_query(&sql)
                    .expect("retry after competing reservation finishes");
                assert_eq!(
                    runtime
                        .execute_query("SELECT * FROM indexed_rows")
                        .expect("read after retry")
                        .result
                        .records
                        .len(),
                    batch_size as usize
                );
            }
        }
    }

    #[test]
    fn auto_index_disabled_does_not_reserve_a_phantom_index() {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory()
                .with_memory_budget(128 * 1024)
                .with_auto_index_id(false),
        )
        .expect("runtime");
        runtime
            .execute_query("CREATE TABLE unindexed_rows (id INT)")
            .expect("table");
        runtime.refresh_memory_accounting();
        let headroom =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let row_bytes = estimate_row_growth(&[("id".to_string(), Value::Integer(1))]);
        let _held = runtime
            .admit_non_evictable_growth(
                MemoryPool::SegmentArena,
                "competing operation",
                headroom - row_bytes,
            )
            .expect("leave exactly the row estimate");
        runtime
            .execute_query("INSERT INTO unindexed_rows (id) VALUES (1)")
            .expect("no index growth when auto-indexing is disabled");
        assert!(runtime
            .index_store_ref()
            .list_indices("unindexed_rows")
            .is_empty());
    }

    #[test]
    fn retained_memory_pressure_cannot_admit_growth_larger_than_the_budget() {
        let budget = 128 * 1024;
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(budget))
                .expect("runtime");
        let store = runtime.db().store();
        store.create_collection("pressure").expect("collection");
        let manager = store.get_collection("pressure").expect("manager");
        let ids = manager
            .bulk_insert(
                (1..=10)
                    .map(|id| {
                        UnifiedEntity::vector(
                            crate::storage::EntityId::new(id),
                            "pressure",
                            vec![1.0; 256],
                        )
                    })
                    .collect(),
            )
            .expect("bulk");
        manager.force_seal().expect("seal");
        for id in ids.iter().take(8) {
            manager.delete(*id).expect("delete");
        }
        assert!(manager.reclaimable_bytes() > 0);
        let result =
            runtime.admit_non_evictable_growth(MemoryPool::SegmentArena, "test growth", budget);
        assert!(
            result.is_err(),
            "reclamation cannot make budget plus live data fit"
        );
        assert!(
            manager.stats().consolidation.runs_completed > 0,
            "pressure ran consolidation"
        );
        let _reservation = runtime
            .admit_non_evictable_growth(MemoryPool::SegmentArena, "small growth", 1)
            .expect("small growth still fits after reclamation");
    }
}
