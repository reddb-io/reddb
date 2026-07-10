//! Admission enforcement over the shared memory accounting pool.

use crate::api::{RedDBError, RedDBResult};
use crate::storage::memory_pools::{MemoryPool, MEMORY_POOLS};
use crate::storage::schema::Value;

const MAX_PRESSURE_TICKS: usize = 64;
const ROW_BASE_BYTES: u64 = 384;
const FIELD_BASE_BYTES: u64 = 64;
const INDEX_ENTRY_BYTES: u64 = 96;

impl crate::RedDBRuntime {
    pub(crate) fn admit_non_evictable_growth(
        &self,
        pool: MemoryPool,
        operation: &str,
        growth_bytes: u64,
    ) -> RedDBResult<()> {
        self.refresh_memory_accounting();
        if self.fits_memory_budget(growth_bytes) {
            return Ok(());
        }

        let accounting = self.memory_accounting();
        let budget = accounting.budget().resolved_bytes;
        let used = accounting.total_used_bytes();
        if pool == MemoryPool::SegmentArena {
            let store = self.db().store();
            if store.reclaimable_segment_bytes() > 0 {
                let before = used;
                let mut reclaimed = 0;
                let mut attempted = false;
                for _ in 0..MAX_PRESSURE_TICKS {
                    let pressure_advanced = store.pressure_consolidation_tick();
                    let maintenance_advanced = store.run_maintenance().is_ok();
                    if !pressure_advanced && !maintenance_advanced {
                        break;
                    }
                    attempted = true;
                    self.refresh_memory_accounting();
                    let after = accounting.total_used_bytes();
                    reclaimed = before.saturating_sub(after);
                    if self.fits_memory_budget(growth_bytes) {
                        accounting.record_pressure_reclamation(reclaimed);
                        return Ok(());
                    }
                }
                if attempted && accounting.total_used_bytes() <= budget {
                    accounting.record_pressure_reclamation(reclaimed);
                    return Ok(());
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

    fn fits_memory_budget(&self, growth_bytes: u64) -> bool {
        let accounting = self.memory_accounting();
        accounting.total_used_bytes().saturating_add(growth_bytes)
            <= accounting.budget().resolved_bytes
    }

    fn didactic_budget_error(&self, _operation: &str, growth_bytes: u64) -> String {
        let accounting = self.memory_accounting();
        let budget = accounting.budget().resolved_bytes;
        let used = accounting.total_used_bytes();
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
             largest consumers: {consumers} - raise the budget or reclaim (see red.stats budget)"
        )
    }
}

pub(crate) fn estimate_row_growth(fields: &[(String, Value)]) -> u64 {
    ROW_BASE_BYTES
        .saturating_add(fields.len() as u64 * FIELD_BASE_BYTES)
        .saturating_add(
            fields
                .iter()
                .map(|(name, _)| name.len() as u64)
                .reduce(u64::saturating_add)
                .unwrap_or(0),
        )
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
