//! Runtime materialized-view refresh and retention sweep.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 6/10, issue #1627).
//! Houses the background-tick surface for scheduled view refresh and retention
//! sweeps:
//!
//! - `materialized_view_metadata` — snapshot of every registered view's
//!   runtime state (feeds `red.materialized_views`).
//! - `retention_sweeper_snapshot` — snapshot of every sweeper's state
//!   (feeds `red.retention`).
//! - `sweep_retention_tick` — one tick of the retention sweeper.
//! - `refresh_due_materialized_views` — claim and refresh views due for
//!   a scheduled refresh.
use super::*;

impl RedDBRuntime {
    /// Snapshot of every registered materialized view's runtime
    /// state — feeds the `red.materialized_views` virtual table.
    /// Issue #583 slice 10.
    pub fn materialized_view_metadata(
        &self,
    ) -> Vec<crate::storage::cache::result::MaterializedViewMetadata> {
        // Issue #595 slice 9c — `current_row_count` is now scraped
        // live from the backing collection rather than read from the
        // cache slot. Mirrors the slice-10 invariant on
        // `queue_pending_gauge` in #527: the live store is the source
        // of truth, the cache slot only carries last-refresh telemetry
        // (timing, error, refresh cadence).
        let store = self.inner.db.store();
        let mut entries = self.inner.materialized_views.read().metadata();
        for entry in &mut entries {
            if let Some(manager) = store.get_collection(&entry.name) {
                entry.current_row_count = manager.count() as u64;
            }
        }
        entries
    }

    /// Drive scheduled refreshes for materialized views with a
    /// `REFRESH EVERY <duration>` clause. Called from the background
    /// scheduler thread (and from unit tests with a fake clock via
    /// `claim_due_at`). Each invocation atomically claims the set of
    /// due views (so two concurrent ticks never double-fire the same
    /// view) and runs each refresh through the standard execution
    /// path — failures are captured in `last_error` and the prior
    /// content stays intact. Issue #583 slice 10.
    /// Snapshot of every tracked retention sweeper state — feeds the
    /// sweeper observability columns on `red.retention`.
    pub(crate) fn retention_sweeper_snapshot(
        &self,
    ) -> Vec<(String, crate::runtime::retention_sweeper::SweeperState)> {
        self.inner.retention_sweeper.read().snapshot()
    }

    /// Drive one tick of the retention sweeper. Mutable collections
    /// physically delete at most `batch_size` expired rows; append-only
    /// collections retire whole expired segments through the operational
    /// manifest. Records the counters that `red.retention` exposes.
    /// Called from the background sweeper thread; safe to invoke directly
    /// from tests with a small batch size to drain rows deterministically.
    /// Issue #584 slice 12.
    ///
    /// Deletes are issued as `DELETE FROM <collection> WHERE
    /// <ts_column> < <cutoff>` through the standard `execute_query`
    /// chokepoint so WAL participation and snapshot guards apply
    /// exactly as for a user-issued DELETE — replicas replay the
    /// sweeper's deletes via the same WAL stream with no special
    /// handling on the replication side.
    ///
    /// Batching is enforced by tightening the cutoff: if more than
    /// `batch_size` rows are expired, the cutoff is dropped to the
    /// `batch_size`-th oldest expired timestamp + 1 so the predicate
    /// matches roughly `batch_size` rows; the remainder is reported
    /// as `current_rows_pending_sweep_estimate` and drained on the
    /// next tick.
    pub fn sweep_retention_tick(&self, batch_size: usize) {
        if batch_size == 0 {
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let store = self.inner.db.store();
        let collections = store.list_collections();
        for name in collections {
            let Some(contract) = self.inner.db.collection_contract(&name) else {
                continue;
            };
            let Some(retention_ms) = contract.retention_duration_ms else {
                continue;
            };
            if contract.append_only {
                let _ = self.retire_expired_append_only_segments(now_ms);
                continue;
            }
            let Some(ts_column) =
                crate::runtime::retention_filter::resolve_timestamp_column(&contract)
            else {
                continue;
            };
            let Some(manager) = store.get_collection(&name) else {
                continue;
            };
            let cutoff = (now_ms as i64).saturating_sub(retention_ms as i64);

            // Single pass: collect expired timestamps. We keep the
            // full Vec rather than a bounded heap because the partial
            // sort below is the simplest correct way to find the
            // batch-th oldest; for the slice's "1000-row default
            // batch" target this is bounded enough for production
            // operation, and the alternative (in-place heap of size
            // batch+1) is a follow-up optimisation.
            let mut expired_ts: Vec<i64> = Vec::new();
            manager.for_each_entity(|entity| {
                let ts = match ts_column.as_str() {
                    "created_at" => Some(entity.created_at as i64),
                    "updated_at" => Some(entity.updated_at as i64),
                    other => entity
                        .data
                        .as_row()
                        .and_then(|row| row.get_field(other))
                        .and_then(|v| match v {
                            crate::storage::schema::Value::TimestampMs(t) => Some(*t),
                            crate::storage::schema::Value::Timestamp(t) => {
                                Some(t.saturating_mul(1_000))
                            }
                            crate::storage::schema::Value::BigInt(t) => Some(*t),
                            crate::storage::schema::Value::UnsignedInteger(t) => {
                                i64::try_from(*t).ok()
                            }
                            crate::storage::schema::Value::Integer(t) => Some(*t),
                            _ => None,
                        }),
                };
                if let Some(t) = ts {
                    if t < cutoff {
                        expired_ts.push(t);
                    }
                }
                true
            });

            let total_expired = expired_ts.len() as u64;
            if total_expired == 0 {
                self.inner
                    .retention_sweeper
                    .write()
                    .record_tick(&name, 0, 0, now_ms);
                continue;
            }

            let (effective_cutoff, pending) = if (total_expired as usize) <= batch_size {
                (cutoff, 0u64)
            } else {
                // Tighten the cutoff to the (batch_size)-th oldest
                // expired timestamp + 1 so DELETE matches roughly
                // `batch_size` rows.
                expired_ts.sort_unstable();
                let nth = expired_ts[batch_size - 1];
                (
                    nth.saturating_add(1),
                    total_expired.saturating_sub(batch_size as u64),
                )
            };

            let stmt = format!(
                "DELETE FROM {} WHERE {} < {}",
                name, ts_column, effective_cutoff
            );
            let deleted = match self.execute_query(&stmt) {
                Ok(r) => r.affected_rows,
                Err(_) => 0,
            };

            self.inner
                .retention_sweeper
                .write()
                .record_tick(&name, deleted, pending, now_ms);
        }
    }

    pub fn refresh_due_materialized_views(&self) {
        let due = {
            let mut cache = self.inner.materialized_views.write();
            cache.claim_due_at(std::time::Instant::now())
        };
        for name in due {
            // Round-trip through `execute_query` (rather than the
            // prepared-statement `execute_query_expr` fast path, which
            // explicitly rejects DDL/maintenance statements). Failures
            // are captured inside the RefreshMaterializedView handler
            // via `record_refresh_failure`; the scheduler ignores the
            // Result so one bad view doesn't halt the loop.
            let stmt = format!("REFRESH MATERIALIZED VIEW {}", name);
            let _ = self.execute_query(&stmt);
        }
    }
}
