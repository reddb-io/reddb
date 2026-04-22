//! Async ML job queue — FIFO queue + worker pool + job-state registry.
//!
//! Callers `submit()` a job and get back an [`MlJobId`] immediately.
//! Worker threads pick jobs off the queue, invoke the caller-supplied
//! [`MlWorkFn`] to perform the actual work, and record progress +
//! terminal status back into the queue's job table.
//!
//! Cancellation is cooperative: setting a job to [`MlJobStatus::Cancelled`]
//! flips a flag that the worker polls between checkpoints. Workers
//! that never check the flag cannot be forcibly killed — this is a
//! deliberate trade-off (no unsafe thread termination, clean up is
//! the algorithm's responsibility).

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

use super::jobs::{now_ms, MlJob, MlJobId, MlJobKind, MlJobStatus};
use super::persist::{key, ns, MlPersistence};

/// Callback invoked on a worker thread to perform the actual work.
///
/// The closure receives a [`JobHandle`] it uses to update progress
/// and to check cancellation. It returns `Ok(metrics_json)` on
/// success (which will be stored on the job record) or `Err(msg)` on
/// failure (surfaced as `error_message`).
pub type MlWorkFn = Arc<dyn Fn(JobHandle) -> Result<String, String> + Send + Sync>;

/// Handle passed into an [`MlWorkFn`]. The worker uses it to report
/// progress and observe cancellation — no other mutations are
/// possible, which keeps contracts small.
#[derive(Clone)]
pub struct JobHandle {
    id: MlJobId,
    shared: Arc<Shared>,
}

impl JobHandle {
    pub fn id(&self) -> MlJobId {
        self.id
    }

    /// Update the `progress` field (0..=100). Values > 100 are
    /// clamped. Non-monotonic updates are allowed — workers that
    /// retry a checkpoint can move progress backwards.
    pub fn set_progress(&self, progress: u8) {
        let snapshot = {
            let mut guard = match self.shared.state.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if let Some(job) = find_job_mut(&mut guard.jobs, self.id) {
                if !job.is_terminal() {
                    job.progress = progress.min(100);
                    Some(job.clone())
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(job) = snapshot {
            persist_job(&self.shared, &job);
        }
    }

    /// True when the operator has requested cancellation. Workers
    /// should poll this at safe boundaries (per batch / per
    /// generation) and return promptly on a positive.
    pub fn is_cancelled(&self) -> bool {
        let guard = match self.shared.state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard
            .jobs
            .iter()
            .find(|j| j.id == self.id)
            .map(|j| j.status == MlJobStatus::Cancelled)
            .unwrap_or(false)
    }
}

struct Shared {
    state: Mutex<QueueState>,
    signal: Condvar,
    backend: Option<Arc<dyn MlPersistence>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared")
            .field("has_backend", &self.backend.is_some())
            .finish()
    }
}

fn persist_job(shared: &Arc<Shared>, job: &MlJob) {
    let Some(backend) = shared.backend.as_ref() else {
        return;
    };
    let raw = job.to_json();
    let _ = backend.put(ns::JOBS, &key::job(job.id), &raw);
}

#[derive(Debug)]
struct QueueState {
    /// Pending job ids ordered FIFO.
    pending: VecDeque<MlJobId>,
    /// All jobs known to the queue, terminal or not. Callers list /
    /// inspect through this vec.
    jobs: Vec<MlJob>,
    /// True once `shutdown()` has been called.
    shutting_down: bool,
    /// Monotonic id counter. u128 so replicas can eventually mint
    /// ids without coordination.
    next_id: u128,
}

/// Queue + worker pool pair. Safe to clone — every clone shares the
/// underlying queue via `Arc`.
#[derive(Clone)]
pub struct MlJobQueue {
    shared: Arc<Shared>,
    worker_fn: MlWorkFn,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl std::fmt::Debug for MlJobQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MlJobQueue")
            .field(
                "worker_count",
                &self.workers.lock().map(|w| w.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl MlJobQueue {
    /// Spin up a queue with `worker_count` threads. `worker_fn` is
    /// invoked once per job. No durable backend — see
    /// [`Self::start_with_backend`] for the persisted variant.
    pub fn start(worker_count: usize, worker_fn: MlWorkFn) -> Self {
        Self::start_with(worker_count, worker_fn, None)
    }

    /// Spin up a queue that persists every state transition to
    /// `backend`. On startup the queue rehydrates any non-terminal
    /// records and re-enqueues them (they were Running when the
    /// previous process died — treat as Queued on resume).
    pub fn start_with_backend(
        worker_count: usize,
        worker_fn: MlWorkFn,
        backend: Arc<dyn MlPersistence>,
    ) -> Self {
        Self::start_with(worker_count, worker_fn, Some(backend))
    }

    fn start_with(
        worker_count: usize,
        worker_fn: MlWorkFn,
        backend: Option<Arc<dyn MlPersistence>>,
    ) -> Self {
        // Rehydrate first so `next_id` is strictly greater than every
        // previously-issued id — collision-free across restarts.
        let mut initial_jobs: Vec<MlJob> = Vec::new();
        let mut initial_pending: VecDeque<MlJobId> = VecDeque::new();
        let mut resume_next_id: u128 = 1;
        if let Some(be) = backend.as_ref() {
            if let Ok(rows) = be.list(ns::JOBS) {
                for (_, raw) in rows {
                    let Some(mut job) = MlJob::from_json(&raw) else {
                        continue;
                    };
                    // A Running job from a prior process is now stuck —
                    // requeue it so a worker can pick it up. Progress
                    // resets to zero so the operator can tell it's a
                    // resumed job from `SELECT * FROM ML_JOBS`.
                    if job.status == MlJobStatus::Running {
                        job.status = MlJobStatus::Queued;
                        job.progress = 0;
                        job.started_at_ms = 0;
                    }
                    if job.status == MlJobStatus::Queued {
                        initial_pending.push_back(job.id);
                    }
                    resume_next_id = resume_next_id.max(job.id.saturating_add(1));
                    initial_jobs.push(job);
                }
            }
        }

        let shared = Arc::new(Shared {
            state: Mutex::new(QueueState {
                pending: initial_pending,
                jobs: initial_jobs.clone(),
                shutting_down: false,
                next_id: resume_next_id,
            }),
            signal: Condvar::new(),
            backend,
        });

        // Flush rehydrated pending-state back to the backend so the
        // status change (Running → Queued) is durable.
        for job in &initial_jobs {
            if job.status == MlJobStatus::Queued {
                persist_job(&shared, job);
            }
        }

        let workers = Arc::new(Mutex::new(Vec::with_capacity(worker_count.max(1))));
        let queue = MlJobQueue {
            shared: Arc::clone(&shared),
            worker_fn: Arc::clone(&worker_fn),
            workers: Arc::clone(&workers),
        };
        for _ in 0..worker_count.max(1) {
            let shared_w = Arc::clone(&shared);
            let worker_fn_w = Arc::clone(&worker_fn);
            let handle = thread::spawn(move || worker_loop(shared_w, worker_fn_w));
            if let Ok(mut guard) = workers.lock() {
                guard.push(handle);
            }
        }
        queue
    }

    /// Enqueue a new job. Returns the assigned id so the caller can
    /// poll status later.
    pub fn submit(
        &self,
        kind: MlJobKind,
        target_name: impl Into<String>,
        spec_json: impl Into<String>,
    ) -> MlJobId {
        let snapshot = {
            let mut guard = match self.shared.state.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            let id = guard.next_id;
            guard.next_id = guard.next_id.saturating_add(1);
            let job = MlJob::new(id, kind, target_name.into(), spec_json.into());
            let snapshot = job.clone();
            guard.jobs.push(job);
            guard.pending.push_back(id);
            snapshot
        };
        persist_job(&self.shared, &snapshot);
        self.shared.signal.notify_one();
        snapshot.id
    }

    /// Fetch a job by id.
    pub fn get(&self, id: MlJobId) -> Option<MlJob> {
        let guard = match self.shared.state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.jobs.iter().find(|j| j.id == id).cloned()
    }

    /// Snapshot every job (terminal + live). Callers use this to
    /// back `SELECT * FROM ML_JOBS`.
    pub fn list(&self) -> Vec<MlJob> {
        let guard = match self.shared.state.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.jobs.clone()
    }

    /// Request cooperative cancellation. Returns `true` if the job
    /// was still cancellable, `false` if it had already reached a
    /// terminal state or does not exist.
    pub fn cancel(&self, id: MlJobId) -> bool {
        let snapshot = {
            let mut guard = match self.shared.state.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            let Some(job) = find_job_mut(&mut guard.jobs, id) else {
                return false;
            };
            if job.is_terminal() {
                return false;
            }
            let was_queued = job.status == MlJobStatus::Queued;
            job.status = MlJobStatus::Cancelled;
            job.finished_at_ms = now_ms();
            let snapshot = job.clone();
            if was_queued {
                // Drop from pending so no worker picks it up; workers
                // already running observe `is_cancelled()` themselves.
                guard.pending.retain(|pid| *pid != id);
            }
            snapshot
        };
        persist_job(&self.shared, &snapshot);
        true
    }

    /// Stop every worker thread after they finish their current job.
    /// Pending jobs are left in the queue — a future process start
    /// would pick them up once persistence is wired.
    pub fn shutdown(&self) {
        {
            let mut guard = match self.shared.state.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            guard.shutting_down = true;
        }
        self.shared.signal.notify_all();
        let Ok(mut workers) = self.workers.lock() else {
            return;
        };
        for handle in workers.drain(..) {
            let _ = handle.join();
        }
    }
}

fn find_job_mut(jobs: &mut [MlJob], id: MlJobId) -> Option<&mut MlJob> {
    jobs.iter_mut().find(|j| j.id == id)
}

fn worker_loop(shared: Arc<Shared>, worker_fn: MlWorkFn) {
    loop {
        // Claim the next queued job, marking it running in the same
        // critical section so two workers can't pick the same one.
        let (next_id, running_snapshot) = {
            let guard = match shared.state.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            let mut guard = match shared
                .signal
                .wait_while(guard, |s| s.pending.is_empty() && !s.shutting_down)
            {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if guard.shutting_down && guard.pending.is_empty() {
                return;
            }
            let id = match guard.pending.pop_front() {
                Some(id) => id,
                None => continue,
            };
            let mut snapshot = None;
            if let Some(job) = find_job_mut(&mut guard.jobs, id) {
                // A cancel-while-queued slipped through between the
                // wait and the pop; skip the work.
                if job.status == MlJobStatus::Cancelled {
                    continue;
                }
                job.status = MlJobStatus::Running;
                job.started_at_ms = now_ms();
                snapshot = Some(job.clone());
            }
            (id, snapshot)
        };
        if let Some(job) = running_snapshot {
            persist_job(&shared, &job);
        }

        let handle = JobHandle {
            id: next_id,
            shared: Arc::clone(&shared),
        };
        let outcome = (worker_fn)(handle);

        let post_snapshot = {
            let mut guard = match shared.state.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            if let Some(job) = find_job_mut(&mut guard.jobs, next_id) {
                // The operator may have cancelled mid-flight — respect
                // that state rather than overwriting it.
                if job.status == MlJobStatus::Cancelled {
                    if job.finished_at_ms == 0 {
                        job.finished_at_ms = now_ms();
                    }
                    Some(job.clone())
                } else {
                    match outcome {
                        Ok(metrics) => {
                            job.status = MlJobStatus::Completed;
                            job.progress = 100;
                            job.metrics_json = Some(metrics);
                        }
                        Err(err) => {
                            job.status = MlJobStatus::Failed;
                            job.error_message = Some(err);
                        }
                    }
                    job.finished_at_ms = now_ms();
                    Some(job.clone())
                }
            } else {
                None
            }
        };
        if let Some(job) = post_snapshot {
            persist_job(&shared, &job);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    fn wait_until<F: Fn() -> bool>(predicate: F, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        predicate()
    }

    #[test]
    fn submit_and_run_to_completion() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_w = Arc::clone(&counter);
        let q = MlJobQueue::start(
            1,
            Arc::new(move |handle| {
                counter_w.fetch_add(1, Ordering::SeqCst);
                handle.set_progress(50);
                handle.set_progress(100);
                Ok("{\"ok\":true}".to_string())
            }),
        );
        let id = q.submit(MlJobKind::Train, "spam", "{}");
        assert!(wait_until(
            || q.get(id).map(|j| j.is_terminal()).unwrap_or(false),
            Duration::from_secs(2),
        ));
        let job = q.get(id).unwrap();
        assert_eq!(job.status, MlJobStatus::Completed);
        assert_eq!(job.progress, 100);
        assert_eq!(job.metrics_json.as_deref(), Some("{\"ok\":true}"));
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        q.shutdown();
    }

    #[test]
    fn failed_work_records_error() {
        let q = MlJobQueue::start(1, Arc::new(|_| Err("bad hyperparameters".to_string())));
        let id = q.submit(MlJobKind::Train, "spam", "{}");
        assert!(wait_until(
            || q.get(id).map(|j| j.is_terminal()).unwrap_or(false),
            Duration::from_secs(2),
        ));
        let job = q.get(id).unwrap();
        assert_eq!(job.status, MlJobStatus::Failed);
        assert_eq!(job.error_message.as_deref(), Some("bad hyperparameters"));
        q.shutdown();
    }

    #[test]
    fn cancel_while_queued_prevents_execution() {
        let ran = Arc::new(AtomicUsize::new(0));
        let ran_w = Arc::clone(&ran);
        // One worker, occupied by a long job to force queueing.
        let q = MlJobQueue::start(
            1,
            Arc::new(move |handle| {
                if handle.id() == 1 {
                    // Hold the first job long enough for #2 to sit queued.
                    thread::sleep(Duration::from_millis(100));
                } else {
                    ran_w.fetch_add(1, Ordering::SeqCst);
                }
                Ok("{}".to_string())
            }),
        );
        let _first = q.submit(MlJobKind::Train, "a", "{}");
        let second = q.submit(MlJobKind::Train, "b", "{}");
        assert!(q.cancel(second));
        thread::sleep(Duration::from_millis(250));
        let job = q.get(second).unwrap();
        assert_eq!(job.status, MlJobStatus::Cancelled);
        assert_eq!(ran.load(Ordering::SeqCst), 0, "cancelled job must not run");
        q.shutdown();
    }

    #[test]
    fn cancel_after_terminal_is_noop() {
        let q = MlJobQueue::start(1, Arc::new(|_| Ok("{}".to_string())));
        let id = q.submit(MlJobKind::Train, "x", "{}");
        assert!(wait_until(
            || q.get(id).map(|j| j.is_terminal()).unwrap_or(false),
            Duration::from_secs(2),
        ));
        assert!(!q.cancel(id));
        q.shutdown();
    }

    #[test]
    fn cooperative_cancellation_observed_by_worker() {
        // Uses a barrier-like counter so the main thread can see the
        // worker actually ran, then cancels and waits for the worker
        // to observe the flag. `cancel()` flips terminal status
        // immediately, so we cannot poll `is_terminal()` to prove the
        // worker co-operated — we poll the observation counter.
        let observed = Arc::new(AtomicUsize::new(0));
        let iters = Arc::new(AtomicUsize::new(0));
        let observed_w = Arc::clone(&observed);
        let iters_w = Arc::clone(&iters);
        let q = MlJobQueue::start(
            1,
            Arc::new(move |handle| {
                for _ in 0..200 {
                    iters_w.fetch_add(1, Ordering::SeqCst);
                    if handle.is_cancelled() {
                        observed_w.fetch_add(1, Ordering::SeqCst);
                        return Err("cancelled".to_string());
                    }
                    handle.set_progress(10);
                    thread::sleep(Duration::from_millis(5));
                }
                Ok("{}".to_string())
            }),
        );
        let id = q.submit(MlJobKind::Train, "slow", "{}");
        assert!(wait_until(
            || iters.load(Ordering::SeqCst) > 0,
            Duration::from_secs(2),
        ));
        assert!(q.cancel(id));
        assert!(wait_until(
            || observed.load(Ordering::SeqCst) >= 1,
            Duration::from_secs(2),
        ));
        let job = q.get(id).unwrap();
        assert_eq!(job.status, MlJobStatus::Cancelled);
        q.shutdown();
    }

    #[test]
    fn backend_persists_submit_and_completion() {
        use super::super::persist::InMemoryMlPersistence;
        let backend = Arc::new(InMemoryMlPersistence::new());
        let q = MlJobQueue::start_with_backend(
            1,
            Arc::new(|_| Ok("{\"f1\":0.9}".to_string())),
            backend.clone(),
        );
        let id = q.submit(MlJobKind::Train, "spam", "{}");
        assert!(wait_until(
            || q.get(id).map(|j| j.is_terminal()).unwrap_or(false),
            Duration::from_secs(2),
        ));
        // Raw record must exist and must reflect the completed status.
        let raw = backend
            .get(super::ns::JOBS, &super::key::job(id))
            .unwrap()
            .unwrap();
        let decoded = MlJob::from_json(&raw).unwrap();
        assert_eq!(decoded.status, MlJobStatus::Completed);
        assert_eq!(decoded.metrics_json.as_deref(), Some("{\"f1\":0.9}"));
        q.shutdown();
    }

    #[test]
    fn resume_from_backend_requeues_running_jobs() {
        use super::super::persist::InMemoryMlPersistence;
        let backend: Arc<dyn super::MlPersistence> = Arc::new(InMemoryMlPersistence::new());

        // Simulate a prior process: one queued + one running + one
        // completed job in the store.
        let pending = MlJob {
            id: 5,
            kind: MlJobKind::Train,
            target_name: "a".into(),
            status: MlJobStatus::Queued,
            progress: 0,
            created_at_ms: 1,
            started_at_ms: 0,
            finished_at_ms: 0,
            error_message: None,
            spec_json: "{}".into(),
            metrics_json: None,
        };
        let stuck = MlJob {
            id: 6,
            kind: MlJobKind::Train,
            target_name: "b".into(),
            status: MlJobStatus::Running,
            progress: 40,
            created_at_ms: 2,
            started_at_ms: 3,
            finished_at_ms: 0,
            error_message: None,
            spec_json: "{}".into(),
            metrics_json: None,
        };
        let done = MlJob {
            id: 7,
            kind: MlJobKind::Train,
            target_name: "c".into(),
            status: MlJobStatus::Completed,
            progress: 100,
            created_at_ms: 3,
            started_at_ms: 4,
            finished_at_ms: 5,
            error_message: None,
            spec_json: "{}".into(),
            metrics_json: Some("{}".into()),
        };
        for j in [&pending, &stuck, &done] {
            backend
                .put(super::ns::JOBS, &super::key::job(j.id), &j.to_json())
                .unwrap();
        }

        let ran = Arc::new(AtomicUsize::new(0));
        let ran_w = Arc::clone(&ran);
        let q = MlJobQueue::start_with_backend(
            2,
            Arc::new(move |_| {
                ran_w.fetch_add(1, Ordering::SeqCst);
                Ok("{}".to_string())
            }),
            backend.clone(),
        );

        assert!(wait_until(
            || ran.load(Ordering::SeqCst) >= 2,
            Duration::from_secs(3),
        ));
        // Both previously non-terminal jobs were re-run; the completed
        // one stayed as-is.
        assert_eq!(q.get(5).unwrap().status, MlJobStatus::Completed);
        assert_eq!(q.get(6).unwrap().status, MlJobStatus::Completed);
        assert_eq!(q.get(7).unwrap().status, MlJobStatus::Completed);

        // next_id must have advanced past the largest resumed id.
        let fresh_id = q.submit(MlJobKind::Train, "d", "{}");
        assert!(fresh_id > 7);

        q.shutdown();
    }

    #[test]
    fn multiple_workers_drain_backlog() {
        let q = MlJobQueue::start(
            3,
            Arc::new(|handle| {
                handle.set_progress(50);
                thread::sleep(Duration::from_millis(20));
                Ok("{}".to_string())
            }),
        );
        let ids: Vec<_> = (0..20)
            .map(|i| q.submit(MlJobKind::Train, format!("m{i}"), "{}"))
            .collect();
        assert!(wait_until(
            || ids
                .iter()
                .all(|id| q.get(*id).map(|j| j.is_terminal()).unwrap_or(false)),
            Duration::from_secs(5),
        ));
        for id in ids {
            assert_eq!(q.get(id).unwrap().status, MlJobStatus::Completed);
        }
        q.shutdown();
    }
}
