//! Local paged CLI servers must drain their recovery WAL without remote backup.
//! Checkpoint publication is still atomic: see the explicit ADR 0038 exception.
use std::time::{Duration, Instant};

use super::RedDBRuntime;
use crate::replication::ReplicationRole;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const CHECKPOINT_INTERVAL_MIN: Duration = Duration::from_secs(30);
const DIRTY_AGE_MAX: Duration = Duration::from_secs(60);
const WAL_PRESSURE_BYTES: u64 = 16 * 1024 * 1024;

struct LocalCheckpointMaintenance {
    dirty_since: Option<Instant>,
    next_attempt: Instant,
}

impl LocalCheckpointMaintenance {
    fn new(now: Instant) -> Self {
        Self {
            dirty_since: None,
            next_attempt: now + CHECKPOINT_INTERVAL_MIN,
        }
    }

    fn due(&mut self, now: Instant, bytes: u64) -> bool {
        if bytes == 0 {
            self.dirty_since = None;
            return false;
        }
        let dirty_since = *self.dirty_since.get_or_insert(now);
        now >= self.next_attempt
            && (bytes >= WAL_PRESSURE_BYTES || now.duration_since(dirty_since) >= DIRTY_AGE_MAX)
    }

    fn tick(&mut self, runtime: &RedDBRuntime, now: Instant) {
        let store = runtime.inner.db.store();
        let result = store.wal_disk_bytes();
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(error) => {
                if now >= self.next_attempt {
                    tracing::warn!(%error, "cannot observe local checkpoint WAL pressure");
                    self.next_attempt = now + CHECKPOINT_INTERVAL_MIN;
                }
                return;
            }
        };
        if !self.due(now, bytes) {
            return;
        }
        let started = Instant::now();
        tracing::debug!(
            target: "reddb::checkpoint",
            wal_bytes_before = bytes,
            "local checkpoint attempt started"
        );
        match runtime.checkpoint() {
            Ok(()) => {
                self.dirty_since = None;
                tracing::info!(
                    wal_bytes_before = bytes,
                    duration_ms = started.elapsed().as_millis(),
                    "local recovery WAL checkpoint completed"
                );
            }
            Err(error) => {
                // Only the existing durable publication path may truncate WAL.
                // Retry through that path without deleting recovery files here.
                tracing::warn!(%error, wal_bytes_before = bytes, "local WAL checkpoint deferred");
            }
        }
        // Cooldown begins after completion, including slow/failed checkpoints.
        self.next_attempt = now + started.elapsed() + CHECKPOINT_INTERVAL_MIN;
    }
}

impl RedDBRuntime {
    pub(crate) fn start_local_checkpoint_maintenance(&self) {
        let options = self.inner.db.options();
        if options.read_only
            || options.remote_backend.is_some()
            || options.replication.role != ReplicationRole::Standalone
            || !self.inner.db.store().is_paged()
        {
            return;
        }
        // Actual storage, rather than the selected profile, is authoritative:
        // default embedded options can reopen an existing paged database.
        let weak = std::sync::Arc::downgrade(&self.inner);
        self.background_tasks
            .as_ref()
            .expect("foreground runtime owns checkpoint worker")
            .spawn(
                std::thread::Builder::new()
                    .name("reddb-local-checkpoint".into())
                    .stack_size(8 * 1024 * 1024),
                move |stopped| {
                    let mut state = LocalCheckpointMaintenance::new(Instant::now());
                    loop {
                        std::thread::park_timeout(POLL_INTERVAL);
                        if stopped.load(std::sync::atomic::Ordering::Acquire) {
                            break;
                        }
                        let Some(inner) = weak.upgrade() else {
                            break;
                        };
                        let runtime = RedDBRuntime {
                            background_tasks: None,
                            inner,
                        };
                        state.tick(&runtime, Instant::now());
                    }
                },
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedDBOptions, StorageDeployPreset};

    #[test]
    fn size_pressure_and_dirty_age_are_paced_and_clean_wal_is_skipped() {
        let now = Instant::now();
        let mut state = LocalCheckpointMaintenance::new(now);
        assert!(!state.due(now, WAL_PRESSURE_BYTES));
        assert!(state.due(now + CHECKPOINT_INTERVAL_MIN, WAL_PRESSURE_BYTES));

        let mut state = LocalCheckpointMaintenance::new(now);
        assert!(!state.due(now, 1));
        assert!(!state.due(now + DIRTY_AGE_MAX - POLL_INTERVAL, 1));
        assert!(state.due(now + DIRTY_AGE_MAX, 1));
        assert!(!state.due(now + DIRTY_AGE_MAX, 0));
        assert!(!state.due(now + DIRTY_AGE_MAX + POLL_INTERVAL, 1));
    }

    #[test]
    fn dirty_tick_checkpoints_real_wal_and_close_releases_worker_and_file_lock() {
        let directory = tempfile::tempdir().expect("temporary database");
        let path = directory.path().join("checkpoint.rdb");
        let options = RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
            .expect("paged profile");
        let runtime = RedDBRuntime::with_options(options.clone()).expect("runtime");
        runtime
            .execute_query("CREATE TABLE checkpoint_rows (id INT, value TEXT)")
            .expect("table");
        runtime.checkpoint().expect("initial checkpoint");
        runtime
            .execute_query("INSERT INTO checkpoint_rows (id, value) VALUES (7, 'acknowledged')")
            .expect("row");
        let store = runtime.inner.db.store();
        assert!(store.wal_disk_bytes().expect("WAL size") > 0);
        let now = Instant::now();
        let mut state = LocalCheckpointMaintenance::new(now);
        state.tick(&runtime, now);
        state.tick(&runtime, now + DIRTY_AGE_MAX);
        assert_eq!(store.wal_disk_bytes().expect("checkpoint drained WAL"), 0);
        let completed = runtime
            .inner
            .checkpoint_projection_stats
            .snapshot(0)
            .checkpoints_completed;
        state.tick(&runtime, now + DIRTY_AGE_MAX * 2);
        assert_eq!(
            runtime
                .inner
                .checkpoint_projection_stats
                .snapshot(0)
                .checkpoints_completed,
            completed
        );

        runtime.start_local_checkpoint_maintenance();
        drop(store);
        drop(runtime);
        let reopened = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("worker drained before lock release; default options reopen paged file");
        assert!(reopened.inner.db.store().is_paged());
        let records = reopened
            .execute_query("SELECT * FROM checkpoint_rows")
            .expect("durable row");
        assert_eq!(records.result.len(), 1);
        reopened.start_local_checkpoint_maintenance();
    }
}
