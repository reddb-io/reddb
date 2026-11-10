//! Foreground handles own periodic workers and drain them before releasing storage.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{Builder, JoinHandle};

#[derive(Default)]
pub(super) struct RuntimeBackgroundTasks {
    stopped: Arc<AtomicBool>,
    workers: parking_lot::Mutex<Vec<JoinHandle<()>>>,
}

impl RuntimeBackgroundTasks {
    pub(super) fn spawn(
        &self,
        builder: Builder,
        work: impl FnOnce(Arc<AtomicBool>) + Send + 'static,
    ) {
        let mut workers = self.workers.lock();
        if self.stopped.load(Ordering::Acquire) {
            return;
        }
        let stopped = Arc::clone(&self.stopped);
        if let Ok(worker) = builder.spawn(move || work(stopped)) {
            workers.push(worker);
        }
    }

    pub(super) fn shutdown(&self) {
        self.stopped.store(true, Ordering::Release);
        let workers = {
            let mut workers = self.workers.lock();
            for worker in workers.iter() {
                worker.thread().unpark();
            }
            workers.drain(..).collect::<Vec<_>>()
        };
        for worker in workers {
            if worker.join().is_err() {
                tracing::warn!("runtime background worker panicked before shutdown completed");
            }
        }
    }
}

impl Drop for RuntimeBackgroundTasks {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::cache::{BlobCachePolicy, BlobCachePut};
    use crate::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn last_foreground_handle_drains_a_worker_that_owns_the_storage_lock() {
        let directory = tempfile::tempdir().expect("temporary database");
        let path = directory.path().join("close.rdb");
        let options = RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
            .expect("paged profile");
        let runtime = RedDBRuntime::with_options(options.clone()).expect("runtime");
        runtime
            .execute_query("CREATE TABLE close_rows (id INT)")
            .expect("table");
        runtime
            .execute_query("INSERT INTO close_rows (id) VALUES (7)")
            .expect("row");
        runtime.checkpoint().expect("checkpoint");
        let weak = Arc::downgrade(&runtime.inner);
        let (started, ready) = mpsc::channel();
        runtime
            .background_tasks
            .as_ref()
            .expect("foreground owner")
            .spawn(
                Builder::new().name("reddb-close-regression".into()),
                move |stopped| {
                    let inner = weak.upgrade().expect("live runtime");
                    started.send(()).expect("worker started");
                    while !stopped.load(Ordering::Acquire) {
                        std::thread::park_timeout(Duration::from_secs(30));
                    }
                    drop(inner);
                },
            );
        ready
            .recv_timeout(Duration::from_secs(5))
            .expect("worker owns inner");

        let clone = runtime.clone();
        let stopped = Arc::clone(&runtime.background_tasks.as_ref().unwrap().stopped);
        drop(runtime);
        assert!(
            !stopped.load(Ordering::Acquire),
            "a live foreground clone must retain workers"
        );
        drop(clone);
        assert!(stopped.load(Ordering::Acquire));

        let reopened =
            RedDBRuntime::with_options(options).expect("lock released before drop returns");
        let result = reopened
            .execute_query("SELECT * FROM close_rows")
            .expect("persisted row");
        assert_eq!(result.result.len(), 1);
    }

    #[test]
    fn graceful_shutdown_drains_workers_before_the_final_checkpoint() {
        let directory = tempfile::tempdir().expect("temporary database");
        let path = directory.path().join("graceful-close.rdb");
        let options = RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
            .expect("paged profile");
        let runtime = RedDBRuntime::with_options(options.clone()).expect("runtime");
        runtime
            .execute_query("CREATE TABLE close_rows (id INT)")
            .expect("table");
        let exited = Arc::new(AtomicBool::new(false));
        let worker_exited = Arc::clone(&exited);
        runtime
            .background_tasks
            .as_ref()
            .expect("foreground owner")
            .spawn(
                Builder::new().name("reddb-graceful-close-regression".into()),
                move |stopped| {
                    while !stopped.load(Ordering::Acquire) {
                        std::thread::park_timeout(Duration::from_secs(30));
                    }
                    worker_exited.store(true, Ordering::Release);
                },
            );

        runtime.graceful_shutdown(false).expect("graceful shutdown");
        assert!(
            runtime
                .background_tasks
                .as_ref()
                .expect("foreground owner")
                .stopped
                .load(Ordering::Acquire),
            "graceful shutdown must stop periodic workers before returning",
        );
        assert!(
            exited.load(Ordering::Acquire),
            "graceful shutdown must join periodic workers before returning",
        );
        drop(runtime);
        RedDBRuntime::with_options(options).expect("reopen after final checkpoint");
    }

    #[test]
    fn blob_cache_worker_reclaims_unread_expired_entries() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        runtime
            .result_blob_cache()
            .put(
                "sessions",
                "expired",
                BlobCachePut::new(b"payload".to_vec())
                    .with_policy(BlobCachePolicy::default().expires_at_unix_ms(1)),
            )
            .expect("cache put");
        assert_eq!(runtime.result_blob_cache().stats().entries(), 1);

        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while runtime.result_blob_cache().stats().entries() != 0
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }

        let stats = runtime.result_blob_cache().stats();
        assert_eq!(
            stats.entries(),
            0,
            "background sweep must physically remove the entry"
        );
        assert_eq!(stats.bytes_in_use(), 0);
        assert_eq!(stats.expirations(), 1);
    }
}
