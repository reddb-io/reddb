//! Bounded admission for synchronous engine work reached from async transports.
//!
//! Connections remain cheap Tokio tasks. Requests that need the synchronous
//! engine acquire one execution permit, while excess admitted requests wait in
//! a bounded FIFO queue. The embedded API does not use this component: it is a
//! server-transport boundary, not an engine primitive.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionAdmissionError {
    QueueFull,
    Closed,
}

#[derive(Debug)]
struct Inner {
    active_max: usize,
    queued_max: usize,
    queued: AtomicUsize,
    semaphore: Arc<Semaphore>,
    bulk_semaphore: Option<Arc<Semaphore>>,
}

/// One active synchronous-engine slot.
///
/// The Tokio semaphore permit is owned so it can move into `spawn_blocking`
/// and remain held until the synchronous call really returns.
#[derive(Debug)]
pub(crate) struct ExecutionPermit {
    _permit: OwnedSemaphorePermit,
    _bulk_permit: Option<OwnedSemaphorePermit>,
    wait: Duration,
}

impl ExecutionPermit {
    pub(crate) fn wait(&self) -> Duration {
        self.wait
    }
}

/// Shared execution slots plus a bounded FIFO wait queue.
#[derive(Debug, Clone)]
pub(crate) struct ExecutionAdmission {
    inner: Arc<Inner>,
}

impl ExecutionAdmission {
    /// Host-sized default shared by every server transport attached to one
    /// runtime. Embedded calls do not acquire it.
    pub(crate) fn for_host() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|parallelism| parallelism.get())
            .unwrap_or(1);
        let total = (2 * cores).clamp(8, 256);
        Self::new(cores.min(total), total.saturating_sub(cores))
    }

    pub(crate) fn new(active_max: usize, queued_max: usize) -> Self {
        assert!(
            active_max > 0,
            "execution admission active_max must be positive"
        );
        Self {
            inner: Arc::new(Inner {
                active_max,
                queued_max,
                queued: AtomicUsize::new(0),
                semaphore: Arc::new(Semaphore::new(active_max)),
                bulk_semaphore: (active_max > 1).then(|| Arc::new(Semaphore::new(active_max - 1))),
            }),
        }
    }

    pub(crate) fn active_max(&self) -> usize {
        self.inner.active_max
    }

    pub(crate) fn active(&self) -> usize {
        self.inner
            .active_max
            .saturating_sub(self.inner.semaphore.available_permits())
    }

    pub(crate) fn queued_max(&self) -> usize {
        self.inner.queued_max
    }

    pub(crate) fn queued(&self) -> usize {
        self.inner.queued.load(Ordering::Acquire)
    }

    /// Acquire an execution slot. Tokio's semaphore preserves FIFO order among
    /// registered waiters. Cancellation is safe: dropping this future releases
    /// its queue accounting through [`QueueGuard`].
    pub(crate) async fn acquire(&self) -> Result<ExecutionPermit, ExecutionAdmissionError> {
        self.acquire_inner(false).await
    }

    /// Acquire the worker reserved for small latency-sensitive probes. Bulk
    /// work cannot occupy the final host slot when at least two slots exist.
    pub(crate) async fn acquire_latency(&self) -> Result<ExecutionPermit, ExecutionAdmissionError> {
        self.acquire_inner(true).await
    }

    pub(crate) async fn acquire_classified(
        &self,
        latency_sensitive: bool,
    ) -> Result<ExecutionPermit, ExecutionAdmissionError> {
        self.acquire_inner(latency_sensitive).await
    }

    async fn acquire_inner(
        &self,
        latency_sensitive: bool,
    ) -> Result<ExecutionPermit, ExecutionAdmissionError> {
        let started = Instant::now();

        let bulk_permit = if latency_sensitive {
            None
        } else if let Some(semaphore) = &self.inner.bulk_semaphore {
            match Arc::clone(semaphore).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    let queue_guard = QueueGuard::enter(Arc::clone(&self.inner))?;
                    let permit = Arc::clone(semaphore)
                        .acquire_owned()
                        .await
                        .map_err(|_| ExecutionAdmissionError::Closed)?;
                    drop(queue_guard);
                    Some(permit)
                }
            }
        } else {
            None
        };

        if let Ok(permit) = Arc::clone(&self.inner.semaphore).try_acquire_owned() {
            return Ok(ExecutionPermit {
                _permit: permit,
                _bulk_permit: bulk_permit,
                wait: started.elapsed(),
            });
        }

        let queue_guard = QueueGuard::enter(Arc::clone(&self.inner))?;
        let permit = Arc::clone(&self.inner.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| ExecutionAdmissionError::Closed)?;
        drop(queue_guard);

        Ok(ExecutionPermit {
            _permit: permit,
            _bulk_permit: bulk_permit,
            wait: started.elapsed(),
        })
    }
}

struct QueueGuard {
    inner: Arc<Inner>,
}

impl QueueGuard {
    fn enter(inner: Arc<Inner>) -> Result<Self, ExecutionAdmissionError> {
        inner
            .queued
            .try_update(Ordering::AcqRel, Ordering::Relaxed, |queued| {
                (queued < inner.queued_max).then_some(queued + 1)
            })
            .map_err(|_| ExecutionAdmissionError::QueueFull)?;
        Ok(Self { inner })
    }
}

impl Drop for QueueGuard {
    fn drop(&mut self) {
        self.inner.queued.fetch_sub(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Barrier;

    #[tokio::test]
    async fn waiter_acquires_after_active_permit_releases() {
        let admission = ExecutionAdmission::new(1, 2);
        let first = admission.acquire().await.expect("first execution slot");
        assert_eq!(admission.active(), 1);

        let waiter_admission = admission.clone();
        let waiter = tokio::spawn(async move { waiter_admission.acquire().await });
        tokio::task::yield_now().await;
        assert_eq!(admission.queued(), 1);

        drop(first);
        let second = waiter
            .await
            .expect("waiter task")
            .expect("queued execution slot");
        assert_eq!(admission.queued(), 0);
        assert_eq!(admission.active(), 1);
        drop(second);
        assert_eq!(admission.active(), 0);
    }

    #[tokio::test]
    async fn queue_capacity_is_hard_bounded() {
        let admission = ExecutionAdmission::new(1, 1);
        let active = admission.acquire().await.expect("active slot");

        let waiter_admission = admission.clone();
        let waiter = tokio::spawn(async move { waiter_admission.acquire().await });
        tokio::task::yield_now().await;
        assert_eq!(admission.queued(), 1);
        assert!(matches!(
            admission.acquire().await,
            Err(ExecutionAdmissionError::QueueFull)
        ));

        waiter.abort();
        let _ = waiter.await;
        assert_eq!(admission.queued(), 0);
        drop(active);
    }

    #[tokio::test]
    async fn cancellation_releases_queue_accounting() {
        let admission = ExecutionAdmission::new(1, 1);
        let active = admission.acquire().await.expect("active slot");
        let waiter_admission = admission.clone();
        let waiter = tokio::spawn(async move { waiter_admission.acquire().await });
        tokio::task::yield_now().await;
        assert_eq!(admission.queued(), 1);

        waiter.abort();
        let _ = waiter.await;
        assert_eq!(admission.queued(), 0);
        drop(active);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn active_work_never_exceeds_configured_workers() {
        let admission = ExecutionAdmission::new(2, 6);
        let barrier = Arc::new(Barrier::new(9));
        let active = Arc::new(AtomicUsize::new(0));
        let active_high_water = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();

        for _ in 0..8 {
            let admission = admission.clone();
            let barrier = Arc::clone(&barrier);
            let active = Arc::clone(&active);
            let active_high_water = Arc::clone(&active_high_water);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                let _permit = admission.acquire().await.expect("execution slot");
                let now = active.fetch_add(1, Ordering::AcqRel) + 1;
                active_high_water.fetch_max(now, Ordering::Relaxed);
                tokio::task::yield_now().await;
                active.fetch_sub(1, Ordering::Release);
            }));
        }

        barrier.wait().await;
        for task in tasks {
            task.await.expect("worker task");
        }
        assert_eq!(active_high_water.load(Ordering::Relaxed), 2);
        assert_eq!(admission.active(), 0);
        assert_eq!(admission.queued(), 0);
    }

    #[tokio::test]
    async fn bulk_work_cannot_occupy_latency_reserve() {
        let admission = ExecutionAdmission::new(3, 3);
        let first = admission.acquire().await.expect("first bulk slot");
        let second = admission.acquire().await.expect("second bulk slot");
        assert_eq!(admission.active(), 2);

        let waiting_admission = admission.clone();
        let waiting = tokio::spawn(async move { waiting_admission.acquire().await });
        tokio::task::yield_now().await;
        assert_eq!(admission.queued(), 1);

        let latency = admission
            .acquire_latency()
            .await
            .expect("reserved latency slot");
        assert_eq!(admission.active(), 3);
        drop(latency);
        assert_eq!(admission.active(), 2);

        drop(first);
        let third = waiting
            .await
            .expect("bulk waiter")
            .expect("third bulk slot");
        drop(second);
        drop(third);
        assert_eq!(admission.active(), 0);
    }
}
