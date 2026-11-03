//! Paced table-history maintenance for standalone embedded runtimes.
use std::time::{Duration, Instant};

use super::RedDBRuntime;
use crate::storage::profile::DeployProfile;
use crate::storage::unified::store::MvccReclaimCursor;

#[derive(Default)]
pub(super) struct MvccMaintenance {
    current: Option<MvccReclaimCursor>,
    after_collection: Option<String>,
    retry_after: Option<Instant>,
    last_warning: Option<Instant>,
}

impl RedDBRuntime {
    /// Foreground pressure and the periodic worker share one paced cursor.
    /// Never wait behind another collector while a mutation holds row locks.
    pub(super) fn try_reclaim_history_tick(&self) -> bool {
        let Some(mut state) = self.inner.mvcc_maintenance.try_lock() else {
            return false;
        };
        self.reclaim_history_tick(&mut state)
    }

    fn reclaim_history_tick(&self, state: &mut MvccMaintenance) -> bool {
        let options = self.inner.db.options();
        // Replicated profiles need a distributed reader/replica low-water
        // mark before local physical history can be reclaimed automatically.
        if options.read_only
            || options.storage_profile.deploy_profile != DeployProfile::Embedded
            || options.replication.role != crate::replication::ReplicationRole::Standalone
            || self.inner.lifecycle.phase() != super::lifecycle::Phase::Ready
            || !self.config_bool("runtime.mvcc.autovacuum.enabled", true)
        {
            state.current = None;
            return false;
        }
        // A collector must not append physical GC to an unrelated, possibly
        // aborted DML event capture. The periodic worker can resume this cursor.
        if crate::storage::UnifiedStore::deferred_store_wal_capture_active() {
            return false;
        }
        if state.retry_after.is_some_and(|at| Instant::now() < at) {
            return false;
        }
        let store = self.inner.db.store();
        if state
            .current
            .as_ref()
            .is_some_and(|cursor| store.get_collection(&cursor.collection).is_none())
        {
            state.current = None;
        }
        if state.current.is_none() {
            let mut collections = store.list_collections();
            collections.sort_unstable();
            if collections.is_empty() {
                return false;
            }
            let index = state.after_collection.as_ref().map_or(0, |after| {
                collections.partition_point(|name| name <= after) % collections.len()
            });
            state.current = store.mvcc_reclaim_cursor(collections[index].clone());
        }
        let Some(cursor) = state.current.as_mut() else {
            return false;
        };
        // Native GC retains one leaf and decodes one current row at a time.
        // Charge encoded/decoded workspace before taking the publication fence.
        let _workspace = if store.config().resident_memory_bytes_max.is_some() {
            let Some(reservation) =
                self.try_reserve_query_buffer(store.mvcc_reclaim_read_limit().saturating_mul(2))
            else {
                return false;
            };
            Some(reservation)
        } else {
            None
        };
        let scan_budget = self
            .config_u64("runtime.mvcc.autovacuum.scan_rows_per_tick", 4096)
            .clamp(1, 4096) as usize;
        let reclaim_budget = self
            .config_u64("runtime.mvcc.autovacuum.reclaim_rows_per_tick", 256)
            .clamp(1, 256) as usize;
        let result = store.reclaim_mvcc_tick(
            cursor,
            &self.snapshot_manager(),
            self.mvcc_vacuum_cutoff_xid(),
            scan_budget,
            reclaim_budget,
        );
        match result {
            Ok(reclaimed) => {
                state.retry_after = None;
                if reclaimed > 0 {
                    tracing::debug!(collection = %cursor.collection, reclaimed_versions = reclaimed,
                        "paced MVCC history reclamation");
                }
                if cursor.finished() {
                    state.after_collection = Some(cursor.collection.clone());
                    state.current = None;
                }
                true
            }
            Err(error) => {
                state.retry_after = Some(Instant::now() + Duration::from_secs(5));
                if state
                    .last_warning
                    .is_none_or(|at| at.elapsed() >= Duration::from_secs(60))
                {
                    tracing::warn!(%error, "MVCC history reclamation deferred");
                    state.last_warning = Some(Instant::now());
                }
                // An unsuccessful WAL-backed batch is not recorded as progress.
                state.current = None;
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{RedDBOptions, RedDBRuntime};

    #[cfg(unix)]
    #[test]
    fn constant_live_data_churn_reuses_pages_after_cold_gc() {
        use reddb_types::Value;
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("churn.rdb");
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path)
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        // Scheduling is part of this fixture: no sleeps or races against the
        // periodic worker. Every cycle drives the same bounded production GC.
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE churn_rows (id INTEGER PRIMARY KEY, payload TEXT)")
            .unwrap();
        let payload = |generation: usize, id: usize| {
            let mut state = (generation * 32 + id) as u64 + 1;
            (0..64 * 1024)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    (b'a' + (state % 26) as u8) as char
                })
                .collect::<String>()
        };
        let allocated = || {
            [&path, &path.with_extension("rdb-uwal")]
                .iter()
                .map(|p| std::fs::metadata(p).map_or(0, |m| m.blocks() * 512))
                .sum::<u64>()
        };
        let mut warmup_allocated = 0;
        let mut peak_after_warmup = 0;
        for generation in 0..120 {
            for start in (0..32).step_by(8) {
                runtime
                    .execute_query(&format!(
                        "DELETE FROM churn_rows WHERE id >= {start} AND id < {}",
                        start + 8,
                    ))
                    .unwrap();
            }
            for start in (0..32).step_by(8) {
                let values = (start..start + 8)
                    .map(|id| format!("({id}, '{}')", payload(generation, id)))
                    .collect::<Vec<_>>()
                    .join(",");
                runtime
                    .execute_query(&format!(
                        "INSERT INTO churn_rows (id, payload) VALUES {values}"
                    ))
                    .unwrap();
            }
            // Force the regression's hard case: obsolete versions are cold,
            // and the cleanup must find them without a resident-only scan.
            runtime.checkpoint().unwrap();
            let store = runtime.db().store();
            let mut cursor = store.mvcc_reclaim_cursor("churn_rows".into()).unwrap();
            let mut reclaimed = 0;
            for _ in 0..128 {
                let count = store
                    .reclaim_mvcc_tick(
                        &mut cursor,
                        &runtime.snapshot_manager(),
                        runtime.mvcc_vacuum_cutoff_xid(),
                        64,
                        8,
                    )
                    .unwrap();
                assert!(count <= 8, "bounded deletion work");
                reclaimed += count;
                if cursor.finished() {
                    break;
                }
            }
            assert!(
                cursor.finished(),
                "bounded sweep must converge at cycle {generation}"
            );
            assert_eq!(
                reclaimed,
                if generation == 0 { 0 } else { 32 },
                "cycle {generation}"
            );
            drop(cursor);
            runtime.checkpoint().unwrap();
            let result = runtime
                .execute_query("SELECT COUNT(*) AS n FROM churn_rows")
                .unwrap();
            assert_eq!(
                result.result.records[0].get("n").unwrap().display_string(),
                "32"
            );
            if generation == 19 {
                warmup_allocated = allocated();
            }
            if generation >= 20 {
                peak_after_warmup = peak_after_warmup.max(allocated());
            }
        }
        let growth = peak_after_warmup.saturating_sub(warmup_allocated);
        eprintln!("churn_cycles=120 warmup_allocated={warmup_allocated} peak_after_warmup={peak_after_warmup} growth_bytes={growth}");
        assert!(
            growth <= 64 * 1024 * 1024,
            "storage grew {growth} bytes with constant live data"
        );
        drop(runtime);
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path)
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        for id in 0..32 {
            let result = runtime
                .execute_query(&format!("SELECT payload FROM churn_rows WHERE id = {id}"))
                .unwrap();
            assert_eq!(result.result.records.len(), 1);
            assert_eq!(
                result.result.records[0].get("payload"),
                Some(&Value::text(payload(119, id).as_str()))
            );
        }
    }

    #[test]
    fn cold_history_reclamation_waits_for_a_retained_reader() {
        let directory = tempfile::tempdir().unwrap();
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(directory.path().join("reader.rdb"))
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .unwrap();
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE cold_history (id INTEGER)")
            .unwrap();
        runtime
            .execute_query("INSERT INTO cold_history (id) VALUES (1)")
            .unwrap();
        let (_, retention) = runtime.snapshot_manager().fresh_retained_snapshot();
        runtime
            .execute_query("DELETE FROM cold_history WHERE id = 1")
            .unwrap();
        runtime.checkpoint().unwrap();
        let store = runtime.db().store();
        let manager = store.get_collection("cold_history").unwrap();
        assert!(manager.has_backing());
        let sweep = || {
            let mut cursor = store.mvcc_reclaim_cursor("cold_history".into()).unwrap();
            let mut reclaimed = 0;
            for _ in 0..8 {
                reclaimed += store
                    .reclaim_mvcc_tick(
                        &mut cursor,
                        &runtime.snapshot_manager(),
                        runtime.mvcc_vacuum_cutoff_xid(),
                        1,
                        1,
                    )
                    .unwrap();
                if cursor.finished() {
                    break;
                }
            }
            assert!(cursor.finished());
            reclaimed
        };
        assert_eq!(sweep(), 0);
        assert_eq!(
            manager.count(),
            1,
            "old reader still needs the cold preimage"
        );
        drop(retention);
        assert_eq!(sweep(), 1);
        assert_eq!(manager.count(), 0);
    }

    fn fixture() -> RedDBRuntime {
        let mut runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_memory_budget(2 * 1024 * 1024),
        )
        .expect("bounded runtime");
        drop(runtime.background_tasks.take());
        runtime
            .execute_query("CREATE TABLE pressure_history (id INTEGER)")
            .expect("table");
        runtime
            .vcs_set_versioned("pressure_history", false)
            .expect("no VCS history");
        runtime
            .execute_query("INSERT INTO pressure_history (id) VALUES (1)")
            .expect("seed row");
        runtime
    }

    fn physical_count(runtime: &RedDBRuntime) -> usize {
        runtime
            .db()
            .store()
            .get_collection("pressure_history")
            .expect("manager")
            .count()
    }

    fn collect(runtime: &RedDBRuntime) {
        for _ in 0..64 {
            runtime.try_reclaim_history_tick();
        }
    }

    #[test]
    fn foreground_history_reclamation_waits_for_a_retained_reader() {
        let runtime = fixture();
        let (_snapshot, retention) = runtime.snapshot_manager().fresh_retained_snapshot();
        runtime
            .execute_query("DELETE FROM pressure_history WHERE id = 1")
            .expect("delete");
        collect(&runtime);
        assert_eq!(
            physical_count(&runtime),
            1,
            "old reader still needs the preimage"
        );
        drop(retention);
        collect(&runtime);
        assert_eq!(
            physical_count(&runtime),
            0,
            "released reader permits reclamation"
        );
    }

    #[test]
    fn foreground_history_reclamation_does_not_join_a_deferred_wal_capture() {
        use crate::storage::UnifiedStore;
        struct CaptureGuard;
        impl Drop for CaptureGuard {
            fn drop(&mut self) {
                if UnifiedStore::deferred_store_wal_capture_active() {
                    drop(UnifiedStore::take_deferred_store_wal_capture());
                }
            }
        }
        let runtime = fixture();
        runtime
            .execute_query("DELETE FROM pressure_history WHERE id = 1")
            .expect("delete");
        UnifiedStore::begin_deferred_store_wal_capture();
        let guard = CaptureGuard;
        for _ in 0..64 {
            assert!(!runtime.try_reclaim_history_tick());
        }
        assert_eq!(physical_count(&runtime), 1);
        let actions = UnifiedStore::take_deferred_store_wal_capture();
        assert!(
            actions.is_empty(),
            "physical GC cannot join a caller's rollback journal"
        );
        drop(guard);
        collect(&runtime);
        assert_eq!(physical_count(&runtime), 0);
    }

    #[test]
    fn foreground_history_reclamation_respects_autovacuum_disable() {
        let runtime = fixture();
        runtime
            .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
            .expect("disable automatic GC");
        runtime
            .execute_query("DELETE FROM pressure_history WHERE id = 1")
            .expect("delete");
        for _ in 0..64 {
            assert!(!runtime.try_reclaim_history_tick());
        }
        assert_eq!(physical_count(&runtime), 1);
    }
}
