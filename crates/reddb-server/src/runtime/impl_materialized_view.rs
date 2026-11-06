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
    /// collections retire at most `batch_size` whole expired segments across
    /// collections through the operational manifest. Records the counters that
    /// `red.retention` exposes.
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
    /// Mutable scans inspect at most `batch_size` physical positions across
    /// collections and resume from their previous position on the next tick.
    pub fn sweep_retention_tick(&self, batch_size: usize) {
        if batch_size == 0 {
            return;
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let store = self.inner.db.store();
        let mut collections = store.list_collections();
        collections.sort();
        if let Some(next) = self.inner.retention_sweeper.read().next_collection() {
            if let Some(start) = collections.iter().position(|name| name == next) {
                collections.rotate_left(start);
            }
        }
        let mut remaining = batch_size;
        let mut append_only_checked = false;
        for (index, name) in collections.iter().enumerate() {
            let Some(contract) = self.inner.db.collection_contract(&name) else {
                continue;
            };
            let Some(retention_ms) = contract.retention_duration_ms else {
                continue;
            };
            if contract.append_only {
                if !append_only_checked {
                    append_only_checked = true;
                    if let Err(err) = self.retire_expired_append_only_segments(now_ms, batch_size) {
                        tracing::warn!(error = %err, "append-only retention sweep failed");
                    }
                }
                continue;
            }
            let Some(ts_column) =
                crate::runtime::retention_filter::resolve_timestamp_column(&contract)
            else {
                continue;
            };
            if store.get_collection(&name).is_none() {
                continue;
            }
            if remaining == 0 {
                continue;
            }
            let cutoff = super::retention_sweeper::cutoff_ms(now_ms, retention_ms);

            let candidates = self
                .inner
                .retention_sweeper
                .write()
                .scan_tick(&store, &name, remaining);
            let (candidates, scanned) = match candidates {
                Ok(result) => result,
                Err(_) => {
                    self.inner
                        .retention_sweeper
                        .write()
                        .record_tick(&name, 0, 1, now_ms);
                    // The failed scan may already have consumed its budget.
                    // Retry other collections on a later tick.
                    remaining = 0;
                    self.inner
                        .retention_sweeper
                        .write()
                        .resume_at(collections[(index + 1) % collections.len()].clone());
                    continue;
                }
            };
            remaining -= scanned;
            if remaining == 0 && !collections.is_empty() {
                self.inner
                    .retention_sweeper
                    .write()
                    .resume_at(collections[(index + 1) % collections.len()].clone());
            }
            if candidates.is_empty() {
                self.inner
                    .retention_sweeper
                    .write()
                    .record_tick(&name, 0, 1, now_ms);
                continue;
            }

            let stmt = format!("DELETE FROM {} WHERE {} < {}", name, ts_column, cutoff);
            let result = super::retention_sweeper::with_delete_candidates(
                candidates,
                &name,
                &ts_column,
                || self.execute_query(&stmt),
            );
            let (deleted, pending) = match result {
                Ok(r) => (r.affected_rows, u64::from(scanned > 0)),
                Err(_) => (0, 1),
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
            // expression-only `execute_query_expr` entry, which
            // explicitly rejects DDL/maintenance statements). Failures
            // are captured inside the RefreshMaterializedView handler
            // via `record_refresh_failure`; the scheduler ignores the
            // Result so one bad view doesn't halt the loop.
            let stmt = format!("REFRESH MATERIALIZED VIEW {}", name);
            let _ = self.execute_query(&stmt);
        }
    }
}
