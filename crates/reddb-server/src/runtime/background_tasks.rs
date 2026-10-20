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
        let stopped = Arc::clone(&self.stopped);
        if let Ok(worker) = builder.spawn(move || work(stopped)) {
            self.workers.lock().push(worker);
        }
    }
}

impl Drop for RuntimeBackgroundTasks {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        let workers = self.workers.get_mut();
        for worker in workers.iter() {
            worker.thread().unpark();
        }
        for worker in workers.drain(..) {
            if worker.join().is_err() {
                tracing::warn!("runtime background worker panicked before shutdown completed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
