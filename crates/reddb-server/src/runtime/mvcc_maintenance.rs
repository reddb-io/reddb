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
