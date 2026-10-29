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
    /// Returns whether an eligible collection received a bounded scan attempt.
    pub(super) fn reclaim_history_tick(&self, state: &mut MvccMaintenance) -> bool {
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
