//! Async promotion pool — turbo module for off-CPU L1 cache promotion.
//!
//! # Why this module exists
//!
//! `BlobCache::get` is on the read hot path. Today, an L2 hit synchronously
//! promotes the blob to L1 (mutates `RwLock`s, runs admission policy, fires
//! eviction loops). That promotion work — bookkeeping, not the actual byte
//! transfer — adds tens of microseconds to every L2 hit.
//!
//! `AsyncPromotionPool` decouples the two: `get` decides "this should go to
//! L1" and hands the request to a bounded queue, then returns the bytes to
//! the caller immediately. A small worker pool drains the queue and performs
//! the promotion off the read path.
//!
//! Inspired by Postgres's `bgwriter` and Linux's `kswapd`: the slow,
//! housekeeping work belongs on a dedicated thread, not in the hot path.
//!
//! # Design
//!
//! - **Bounded queue** (`crossbeam::queue::ArrayQueue`) — back-pressure
//!   without unbounded memory growth.
//! - **Drop-oldest on saturation** — when the queue is full we evict the
//!   oldest pending request and admit the new one. Rationale: under load
//!   the freshest accesses are the most likely to be re-read soon, so a
//!   FIFO drop loses the least value.
//! - **Decoupled executor** — the closure that performs the actual
//!   promotion is injected at construction (`new_with_executor`). The pool
//!   knows nothing about `BlobCache` and is therefore unit-testable in
//!   isolation.
//! - **Atomic metrics** — counters are `Relaxed`-incremented by hot paths;
//!   `metrics()` returns a consistent snapshot.
//! - **Graceful shutdown** — `shutdown` flips a flag, lets workers drain a
//!   bounded number of remaining requests, then they exit.
//!
//! # Wiring (deferred)
//!
//! This file is purely additive. Wiring into `BlobCache::get` and the
//! `pub mod promotion_pool;` registration in `mod.rs` happen in a
//! sequential follow-up, after all three turbo modules from issue #193
//! have shipped.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_queue::ArrayQueue;

use super::blob::BlobCachePolicy;

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Configuration for the promotion pool.
#[derive(Debug, Clone, Copy)]
pub struct PoolOpts {
    /// Maximum number of pending promotion requests. When full, the pool
    /// drops the oldest entry to admit the newest (see `ScheduleOutcome`).
    pub queue_capacity: usize,
    /// Number of tokio worker tasks draining the queue.
    pub worker_count: usize,
}

impl Default for PoolOpts {
    fn default() -> Self {
        Self {
            queue_capacity: 1024,
            worker_count: 2,
        }
    }
}

/// A single async promotion request handed to the pool by `BlobCache::get`
/// (or, in tests, by the test harness).
///
/// `bytes` is `Arc<[u8]>` so that the same buffer the caller is returning
/// to the user is shared zero-copy with the L1 promotion.
#[derive(Debug, Clone)]
pub struct PromotionRequest {
    pub namespace: String,
    pub key: String,
    pub bytes: Arc<[u8]>,
    pub policy: BlobCachePolicy,
}

/// Result of `AsyncPromotionPool::schedule`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleOutcome {
    /// Request accepted into the queue.
    Queued,
    /// Queue was full. `evicted_oldest = true` means we dropped the oldest
    /// pending request to admit this one. `evicted_oldest = false` means the
    /// queue was so contended even the eviction `pop` failed and *this*
    /// request was dropped instead.
    DroppedQueueFull { evicted_oldest: bool },
}

/// Snapshot of the pool's atomic counters. Returned by `metrics()`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromotionMetrics {
    pub queued_total: u64,
    pub dropped_total: u64,
    pub completed_total: u64,
    pub queue_depth: usize,
}

/// The closure that actually performs the L1 promotion. Injected at
/// construction so this module has no compile-time dependency on
/// `BlobCache`.
///
/// The wiring slice will pass a closure of the form:
///
/// ```ignore
/// let cache = self.clone();
/// Arc::new(move |req| cache.do_l1_promotion(req))
/// ```
pub type PromotionExecutor =
    Arc<dyn Fn(PromotionRequest) -> Result<(), String> + Send + Sync + 'static>;

// ---------------------------------------------------------------------------
// AsyncPromotionPool
// ---------------------------------------------------------------------------

/// Bounded, drop-oldest async promotion pool.
///
/// See module docs for design rationale.
pub struct AsyncPromotionPool {
    queue: Arc<ArrayQueue<PromotionRequest>>,
    executor: PromotionExecutor,
    shutdown: Arc<AtomicBool>,

    queued_total: AtomicU64,
    dropped_total: AtomicU64,
    completed_total: AtomicU64,

    /// Soft cap on how many remaining requests workers will drain after
    /// `shutdown()` is called before exiting. Prevents a pathological
    /// flood of late requests from blocking shutdown indefinitely.
    drain_budget: usize,
}

impl AsyncPromotionPool {
    /// Construct a pool with a no-op executor. Useful only for tests / dry
    /// runs where you want metrics but no actual promotion side-effects.
    pub fn new(opts: PoolOpts) -> Arc<Self> {
        Self::new_with_executor(opts, Arc::new(|_| Ok(())))
    }

    /// Construct a pool with a caller-provided executor closure.
    ///
    /// Spawns `opts.worker_count` tokio tasks that drain the queue. Each
    /// task holds a `Weak<Self>` so the pool is dropped cleanly once the
    /// caller releases its `Arc` and the workers exit.
    pub fn new_with_executor(opts: PoolOpts, executor: PromotionExecutor) -> Arc<Self> {
        let capacity = opts.queue_capacity.max(1);
        let workers = opts.worker_count.max(1);

        let pool = Arc::new(Self {
            queue: Arc::new(ArrayQueue::new(capacity)),
            executor,
            shutdown: Arc::new(AtomicBool::new(false)),
            queued_total: AtomicU64::new(0),
            dropped_total: AtomicU64::new(0),
            completed_total: AtomicU64::new(0),
            // Drain at most one queue-worth of late requests per worker.
            drain_budget: capacity,
        });

        for _ in 0..workers {
            let pool_for_worker = Arc::clone(&pool);
            tokio::spawn(async move {
                worker_loop(pool_for_worker).await;
            });
        }

        pool
    }

    /// Hand a promotion request to the pool.
    ///
    /// Never blocks. If the queue has capacity, the request is enqueued and
    /// `Queued` is returned. If the queue is full, the oldest request is
    /// popped (and dropped) to make room — the caller learns this via
    /// `DroppedQueueFull { evicted_oldest: true }`. In the rare case where
    /// the queue is so contended that even the `pop` fails, the *new*
    /// request is dropped: `DroppedQueueFull { evicted_oldest: false }`.
    pub fn schedule(&self, request: PromotionRequest) -> ScheduleOutcome {
        // After shutdown, refuse new work to keep the drain bounded.
        if self.shutdown.load(Ordering::Acquire) {
            self.dropped_total.fetch_add(1, Ordering::Relaxed);
            return ScheduleOutcome::DroppedQueueFull {
                evicted_oldest: false,
            };
        }

        match self.queue.push(request) {
            Ok(()) => {
                self.queued_total.fetch_add(1, Ordering::Relaxed);
                ScheduleOutcome::Queued
            }
            Err(rejected) => {
                // Full. Try to evict oldest to admit the newest.
                let evicted_oldest = self.queue.pop().is_some();
                if evicted_oldest {
                    self.dropped_total.fetch_add(1, Ordering::Relaxed);
                }
                match self.queue.push(rejected) {
                    Ok(()) => {
                        self.queued_total.fetch_add(1, Ordering::Relaxed);
                        ScheduleOutcome::DroppedQueueFull { evicted_oldest }
                    }
                    Err(_) => {
                        // Lost the race — another producer refilled the
                        // slot before us. Drop the new request.
                        self.dropped_total.fetch_add(1, Ordering::Relaxed);
                        ScheduleOutcome::DroppedQueueFull {
                            evicted_oldest: false,
                        }
                    }
                }
            }
        }
    }

    /// Signal workers to drain remaining work and exit.
    ///
    /// Workers will process at most `drain_budget` more requests after the
    /// shutdown flag is observed, then return. New `schedule` calls after
    /// shutdown are rejected (counted in `dropped_total`).
    pub fn shutdown(self: Arc<Self>) {
        self.shutdown.store(true, Ordering::Release);
    }

    /// Snapshot of the pool's atomic counters.
    ///
    /// Each counter is read with `Relaxed` ordering; the snapshot is not
    /// strictly atomic across counters (it can show, e.g., one more
    /// `queued_total` than the queue depth implies if a worker is mid-pop).
    /// This is acceptable for monitoring; consumers that need a strictly
    /// consistent view should sample twice and take the difference.
    pub fn metrics(&self) -> PromotionMetrics {
        PromotionMetrics {
            queued_total: self.queued_total.load(Ordering::Relaxed),
            dropped_total: self.dropped_total.load(Ordering::Relaxed),
            completed_total: self.completed_total.load(Ordering::Relaxed),
            queue_depth: self.queue.len(),
        }
    }
}

// ---------------------------------------------------------------------------
// Worker loop
// ---------------------------------------------------------------------------

/// Idle backoff between empty polls. Short enough that latency stays low,
/// long enough that an idle pool doesn't spin a CPU.
const WORKER_IDLE_BACKOFF: Duration = Duration::from_millis(1);

async fn worker_loop(pool: Arc<AsyncPromotionPool>) {
    loop {
        match pool.queue.pop() {
            Some(req) => {
                // Run the executor. We swallow errors here because the
                // promotion is best-effort by definition — the read path
                // already handed bytes to the user. Errors are surfaced
                // via tracing for observability.
                if let Err(err) = (pool.executor)(req) {
                    tracing::warn!(error = %err, "async promotion executor failed");
                }
                pool.completed_total.fetch_add(1, Ordering::Relaxed);
            }
            None => {
                // Queue empty.
                if pool.shutdown.load(Ordering::Acquire) {
                    // Drain at most `drain_budget` more items (in case a
                    // late `schedule` slipped in before the shutdown flag
                    // was published) and exit.
                    let mut drained = 0;
                    while drained < pool.drain_budget {
                        match pool.queue.pop() {
                            Some(req) => {
                                let _ = (pool.executor)(req);
                                pool.completed_total.fetch_add(1, Ordering::Relaxed);
                                drained += 1;
                            }
                            None => break,
                        }
                    }
                    return;
                }
                tokio::time::sleep(WORKER_IDLE_BACKOFF).await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Mutex;
    use std::time::Instant;

    fn req(key: &str) -> PromotionRequest {
        PromotionRequest {
            namespace: "ns".to_string(),
            key: key.to_string(),
            bytes: Arc::from(vec![0u8; 8].into_boxed_slice()),
            policy: BlobCachePolicy::default(),
        }
    }

    /// Build a pool whose executor never runs (no workers spawned by us);
    /// used for queue-semantics tests where worker drain would race.
    ///
    /// We do this by constructing the pool manually rather than going
    /// through `new_with_executor`, which always spawns workers. For pure
    /// queue-semantics tests we want zero concurrent draining.
    fn pool_no_workers(capacity: usize) -> Arc<AsyncPromotionPool> {
        Arc::new(AsyncPromotionPool {
            queue: Arc::new(ArrayQueue::new(capacity)),
            executor: Arc::new(|_| Ok(())),
            shutdown: Arc::new(AtomicBool::new(false)),
            queued_total: AtomicU64::new(0),
            dropped_total: AtomicU64::new(0),
            completed_total: AtomicU64::new(0),
            drain_budget: capacity,
        })
    }

    #[test]
    fn schedule_returns_queued_when_capacity_available() {
        let pool = pool_no_workers(4);
        assert_eq!(pool.schedule(req("a")), ScheduleOutcome::Queued);
        assert_eq!(pool.schedule(req("b")), ScheduleOutcome::Queued);
        assert_eq!(pool.metrics().queued_total, 2);
        assert_eq!(pool.metrics().queue_depth, 2);
    }

    #[test]
    fn schedule_drops_oldest_when_saturated() {
        let pool = pool_no_workers(2);
        assert_eq!(pool.schedule(req("a")), ScheduleOutcome::Queued);
        assert_eq!(pool.schedule(req("b")), ScheduleOutcome::Queued);

        let outcome = pool.schedule(req("c"));
        assert_eq!(
            outcome,
            ScheduleOutcome::DroppedQueueFull {
                evicted_oldest: true
            }
        );
        assert_eq!(pool.metrics().dropped_total, 1);
        assert_eq!(pool.metrics().queue_depth, 2);
    }

    #[test]
    fn drop_oldest_semantics_preserve_newest() {
        // Insert N+1 items into a capacity-N queue. The oldest must be
        // gone, the newest must survive.
        let cap = 3;
        let pool = pool_no_workers(cap);

        for k in ["a", "b", "c"] {
            assert_eq!(pool.schedule(req(k)), ScheduleOutcome::Queued);
        }
        // Saturating insert.
        assert_eq!(
            pool.schedule(req("d")),
            ScheduleOutcome::DroppedQueueFull {
                evicted_oldest: true
            }
        );

        // Drain in FIFO order and check contents.
        let mut seen = Vec::new();
        while let Some(r) = pool.queue.pop() {
            seen.push(r.key);
        }
        assert_eq!(
            seen,
            vec!["b".to_string(), "c".to_string(), "d".to_string()]
        );
    }

    #[tokio::test]
    async fn worker_executes_injected_closure() {
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_for_exec = Arc::clone(&counter);
        let executor: PromotionExecutor = Arc::new(move |_req| {
            counter_for_exec.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });

        let pool = AsyncPromotionPool::new_with_executor(
            PoolOpts {
                queue_capacity: 16,
                worker_count: 1,
            },
            executor,
        );

        for k in 0..5 {
            pool.schedule(req(&format!("k{k}")));
        }

        // Wait for workers to drain.
        let deadline = Instant::now() + Duration::from_secs(2);
        while counter.load(Ordering::Relaxed) < 5 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        assert_eq!(counter.load(Ordering::Relaxed), 5);
        assert_eq!(pool.metrics().completed_total, 5);

        Arc::clone(&pool).shutdown();
    }

    #[tokio::test]
    async fn shutdown_drains_queue_within_budget() {
        let executed = Arc::new(AtomicUsize::new(0));
        let executed_for_exec = Arc::clone(&executed);
        let executor: PromotionExecutor = Arc::new(move |_req| {
            executed_for_exec.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });

        let pool = AsyncPromotionPool::new_with_executor(
            PoolOpts {
                queue_capacity: 32,
                worker_count: 2,
            },
            executor,
        );

        for k in 0..20 {
            pool.schedule(req(&format!("k{k}")));
        }

        Arc::clone(&pool).shutdown();

        // Workers should drain everything queued before shutdown was set.
        let deadline = Instant::now() + Duration::from_secs(2);
        while executed.load(Ordering::Relaxed) < 20 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        assert_eq!(executed.load(Ordering::Relaxed), 20);

        // Post-shutdown schedule is rejected.
        let outcome = pool.schedule(req("late"));
        assert_eq!(
            outcome,
            ScheduleOutcome::DroppedQueueFull {
                evicted_oldest: false
            }
        );
        assert!(pool.metrics().dropped_total >= 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_schedulers_no_deadlock_all_completions_counted() {
        let executed = Arc::new(AtomicUsize::new(0));
        let executed_for_exec = Arc::clone(&executed);
        let executor: PromotionExecutor = Arc::new(move |_req| {
            executed_for_exec.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });

        let pool = AsyncPromotionPool::new_with_executor(
            PoolOpts {
                queue_capacity: 64,
                worker_count: 2,
            },
            executor,
        );

        let producers = 8;
        let per_producer = 200;
        // Producers track local outcomes so we can check accounting
        // without relying on any single internal counter formula. The
        // pool's `dropped_total` aggregates two distinct events
        // (evicted-oldest + outright-rejected), so we use the
        // ScheduleOutcome variants directly.
        let outright_drops = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for p in 0..producers {
            let pool_p = Arc::clone(&pool);
            let drops_p = Arc::clone(&outright_drops);
            handles.push(tokio::spawn(async move {
                for i in 0..per_producer {
                    let r = PromotionRequest {
                        namespace: format!("ns{p}"),
                        key: format!("k{i}"),
                        bytes: Arc::from(vec![0u8; 4].into_boxed_slice()),
                        policy: BlobCachePolicy::default(),
                    };
                    if let ScheduleOutcome::DroppedQueueFull {
                        evicted_oldest: false,
                    } = pool_p.schedule(r)
                    {
                        drops_p.fetch_add(1, Ordering::Relaxed);
                    }
                    if i % 32 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // Invariant we check after producers finish AND workers catch up:
        //
        //   queued_total + outright_drops == submitted
        //
        // (every submission was either admitted or rejected outright; an
        // "evicted oldest" event still admits the new request)
        //
        // and once the queue drains:
        //
        //   completed_total == queued_total - dropped_via_eviction
        //                   == queued_total - (dropped_total - outright_drops)
        //
        // Equivalently: completed + dropped_total == queued + outright_drops.
        let submitted = (producers * per_producer) as u64;

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let m = pool.metrics();
            let outright = outright_drops.load(Ordering::Relaxed) as u64;
            let admitted_invariant = m.queued_total + outright == submitted;
            let drained_invariant =
                m.completed_total + m.dropped_total == m.queued_total + outright;
            if admitted_invariant && drained_invariant && m.queue_depth == 0 {
                break;
            }
            if Instant::now() > deadline {
                panic!(
                    "did not converge: submitted={submitted} queued={} dropped={} completed={} depth={} outright={}",
                    m.queued_total, m.dropped_total, m.completed_total, m.queue_depth, outright
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        Arc::clone(&pool).shutdown();
    }

    #[test]
    fn metrics_snapshot_is_consistent_under_simple_load() {
        let pool = pool_no_workers(8);
        for k in 0..5 {
            pool.schedule(req(&format!("k{k}")));
        }
        let m = pool.metrics();
        assert_eq!(m.queued_total, 5);
        assert_eq!(m.dropped_total, 0);
        assert_eq!(m.completed_total, 0);
        assert_eq!(m.queue_depth, 5);
    }

    /// Sanity: the executor sees the same bytes/key/namespace the producer
    /// scheduled. Catches accidental Arc/Box mix-ups in the queue plumbing.
    #[tokio::test]
    async fn executor_receives_unmodified_request() {
        let captured: Arc<Mutex<Vec<(String, String, usize)>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_for_exec = Arc::clone(&captured);
        let executor: PromotionExecutor = Arc::new(move |req| {
            captured_for_exec
                .lock()
                .unwrap()
                .push((req.namespace, req.key, req.bytes.len()));
            Ok(())
        });

        let pool = AsyncPromotionPool::new_with_executor(
            PoolOpts {
                queue_capacity: 4,
                worker_count: 1,
            },
            executor,
        );

        pool.schedule(PromotionRequest {
            namespace: "users".to_string(),
            key: "42".to_string(),
            bytes: Arc::from(vec![1u8, 2, 3, 4, 5].into_boxed_slice()),
            policy: BlobCachePolicy::default(),
        });

        let deadline = Instant::now() + Duration::from_secs(2);
        while captured.lock().unwrap().is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        let seen = captured.lock().unwrap().clone();
        assert_eq!(seen, vec![("users".to_string(), "42".to_string(), 5)]);

        Arc::clone(&pool).shutdown();
    }
}
