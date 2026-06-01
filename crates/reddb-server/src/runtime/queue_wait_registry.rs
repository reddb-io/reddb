//! Local wait registry for `QUEUE READ … WAIT <duration>` (PRD #718, slice C).
//!
//! Keyed by `(scope, queue)`. Each slot holds a parking_lot::Condvar plus
//! a monotonic generation counter; waiters snapshot the generation, do a
//! second non-blocking probe, then park on the condvar until either a
//! notify bumps the generation, the timeout elapses, or `cancel_all`
//! sets the shutdown flag.
//!
//! Wake-all semantics (first cut): every notify wakes every waiter on
//! the slot. Normal delivery arbitration decides winners — losers
//! re-wait or time out. This is intentionally simple; targeted wake
//! lands in a later slice once arbitration is observable.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use tokio::sync::Notify;

/// What happened to a waiter when it returned from `wait`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// A producer notified this `(scope, queue)` slot — the caller
    /// should re-probe the queue for available work.
    Woken,
    /// The caller's wait budget elapsed without a notify.
    Timeout,
    /// The registry was cancelled (shutdown). Surfaces as an explicit
    /// cancellation error to the caller.
    Cancelled,
}

/// One slot per `(scope, queue)`. The mutex guards the generation
/// counter; the condvar is the parking primitive. Slots are reused
/// across waiters — once registered they live for the runtime's
/// lifetime (the set is bounded by the number of distinct queues).
struct Slot {
    state: Mutex<u64>,
    cond: Condvar,
    /// Async wake head (issue #917). Bumped by the same `notify` /
    /// `cancel_all` that drives the synchronous condvar, so a single
    /// producer wake releases both a parked HTTP condvar waiter and an
    /// awaiting RedWire session for the same `(scope, queue)` key. The
    /// generation counter in `state` makes the async park lost-wake-free
    /// the same way it does for the condvar: a waiter snapshots the
    /// generation before its re-probe, so a notify that lands between
    /// probe and park is observed as a generation move rather than a
    /// missed `notify_waiters`.
    notify: Notify,
}

impl Slot {
    fn new() -> Self {
        Self {
            state: Mutex::new(0),
            cond: Condvar::new(),
            notify: Notify::new(),
        }
    }
}

pub struct QueueWaitRegistry {
    slots: Mutex<HashMap<(String, String), Arc<Slot>>>,
    cancelled: AtomicBool,
    /// Cleared once at construction; bumped on `cancel_all`. Waiters
    /// re-check after every wake to honour cancellation independently
    /// of which slot they were parked on.
    cancel_cond: Condvar,
    cancel_mu: Mutex<()>,
}

impl Default for QueueWaitRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl QueueWaitRegistry {
    pub fn new() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            cancelled: AtomicBool::new(false),
            cancel_cond: Condvar::new(),
            cancel_mu: Mutex::new(()),
        }
    }

    /// Returns the current cancellation flag without resetting it.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Drop the shutdown flag back to false. Used by tests that share
    /// a process-wide registry instance across cases.
    pub fn reset_cancelled(&self) {
        self.cancelled.store(false, Ordering::Release);
    }

    /// Number of in-flight waiter references parked on `(scope, queue)`
    /// beyond the registry's own retained slot reference. Each live
    /// [`Snapshot`] (sync condvar path) and [`AsyncWaiter`] (RedWire
    /// async edge) holds one `Arc<Slot>` clone, so this is the count of
    /// waiters currently registered on the key. Returns 0 when no slot
    /// has been created yet.
    ///
    /// The observable the queue-wait cancellation surface relies on
    /// (issue #920 AC #3): when an in-flight wait is cancelled — by a
    /// connection close aborting the wait task, or by `cancel_all` at
    /// shutdown — its waiter is dropped and this count falls back to 0,
    /// proving the slot reference (and the tokio worker holding it) was
    /// released promptly rather than lingering to the wait deadline.
    pub fn live_waiters(&self, scope: &str, queue: &str) -> usize {
        let slots = self.slots.lock();
        slots
            .get(&(scope.to_string(), queue.to_string()))
            .map(|slot| Arc::strong_count(slot).saturating_sub(1))
            .unwrap_or(0)
    }

    fn slot(&self, scope: &str, queue: &str) -> Arc<Slot> {
        let mut slots = self.slots.lock();
        if let Some(existing) = slots.get(&(scope.to_string(), queue.to_string())) {
            return existing.clone();
        }
        let fresh = Arc::new(Slot::new());
        slots.insert((scope.to_string(), queue.to_string()), fresh.clone());
        fresh
    }

    /// Snapshot the current generation for `(scope, queue)`. Callers
    /// take this BEFORE their second non-blocking probe so a notify
    /// that fires between the probe and `wait_until` bumps the
    /// generation and the condvar wait returns immediately
    /// (lost-wake-free).
    pub fn snapshot(&self, scope: &str, queue: &str) -> Snapshot {
        let slot = self.slot(scope, queue);
        let gen = *slot.state.lock();
        Snapshot { slot, gen }
    }

    /// Park on the snapshot's slot until the generation moves past
    /// `snapshot.gen`, the deadline elapses, or `cancel_all` fires.
    pub fn wait_until(&self, snapshot: &Snapshot, deadline: Instant) -> WaitOutcome {
        if self.is_cancelled() {
            return WaitOutcome::Cancelled;
        }
        let mut guard = snapshot.slot.state.lock();
        loop {
            if self.is_cancelled() {
                return WaitOutcome::Cancelled;
            }
            if *guard != snapshot.gen {
                return WaitOutcome::Woken;
            }
            let now = Instant::now();
            if now >= deadline {
                return WaitOutcome::Timeout;
            }
            let remaining = deadline - now;
            let result = snapshot.slot.cond.wait_for(&mut guard, remaining);
            if self.is_cancelled() {
                return WaitOutcome::Cancelled;
            }
            if *guard != snapshot.gen {
                return WaitOutcome::Woken;
            }
            if result.timed_out() && Instant::now() >= deadline {
                return WaitOutcome::Timeout;
            }
        }
    }

    /// Bump the generation on `(scope, queue)` and wake every parked
    /// waiter. Idempotent — a slot with no waiters just bumps the
    /// generation, which is correct (next waiter that arrives before
    /// snapshot still sees a fresh starting point).
    pub fn notify(&self, scope: &str, queue: &str) {
        let slot = self.slot(scope, queue);
        let mut guard = slot.state.lock();
        *guard = guard.wrapping_add(1);
        drop(guard);
        // Bump both wake heads off the same generation move: the
        // synchronous condvar (HTTP path) and the async Notify (the
        // RedWire session edge, issue #917).
        slot.cond.notify_all();
        slot.notify.notify_waiters();
    }

    /// Shutdown drain: set the cancellation flag and wake every slot's
    /// condvar so parked waiters return `Cancelled` immediately.
    pub fn cancel_all(&self) {
        self.cancelled.store(true, Ordering::Release);
        let slots = self.slots.lock();
        for slot in slots.values() {
            let _g = slot.state.lock();
            slot.cond.notify_all();
            // Wake async waiters too — they re-check `is_cancelled`
            // after the park returns and surface `Cancelled`.
            slot.notify.notify_waiters();
        }
        drop(slots);
        let _g = self.cancel_mu.lock();
        self.cancel_cond.notify_all();
    }
}

/// Opaque token captured before the second non-blocking probe. Holding
/// onto the slot keeps it alive even if the registry is dropped
/// between operations (which doesn't happen in production but keeps
/// tests safe).
pub struct Snapshot {
    slot: Arc<Slot>,
    gen: u64,
}

/// Async analogue of [`Snapshot`] for the RedWire session edge (issue
/// #917). Captures the slot and its generation before the caller's
/// re-probe; [`QueueWaitRegistry::wait_until_async`] then parks on the
/// slot's async wake head without holding a blocking OS thread.
pub struct AsyncWaiter {
    slot: Arc<Slot>,
    gen: u64,
}

impl QueueWaitRegistry {
    /// Register an async waiter on `(scope, queue)`. Mirrors
    /// [`snapshot`](Self::snapshot): take this BEFORE the second
    /// non-blocking probe so a notify firing between the probe and
    /// [`wait_until_async`](Self::wait_until_async) is seen as a
    /// generation move (lost-wake-free).
    pub fn async_waiter(&self, scope: &str, queue: &str) -> AsyncWaiter {
        let slot = self.slot(scope, queue);
        let gen = *slot.state.lock();
        AsyncWaiter { slot, gen }
    }

    /// Await the waiter's slot until a notify bumps the generation
    /// past `waiter.gen`, the deadline elapses, or `cancel_all` fires.
    /// Holds no OS thread for the wait duration — the tokio worker is
    /// released back to the runtime while parked (the property the
    /// RedWire async transport edge relies on).
    pub async fn wait_until_async(&self, waiter: &AsyncWaiter, deadline: Instant) -> WaitOutcome {
        if self.is_cancelled() {
            return WaitOutcome::Cancelled;
        }
        loop {
            // Arm the notification future BEFORE the generation check
            // so a `notify_waiters` racing with this check cannot be
            // lost: `enable()` registers interest, then the generation
            // re-read catches any bump that already happened.
            let notified = waiter.slot.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            if self.is_cancelled() {
                return WaitOutcome::Cancelled;
            }
            if *waiter.slot.state.lock() != waiter.gen {
                return WaitOutcome::Woken;
            }
            let now = Instant::now();
            if now >= deadline {
                return WaitOutcome::Timeout;
            }
            let sleep = tokio::time::sleep(deadline - now);
            tokio::select! {
                _ = notified => {
                    if self.is_cancelled() {
                        return WaitOutcome::Cancelled;
                    }
                    if *waiter.slot.state.lock() != waiter.gen {
                        return WaitOutcome::Woken;
                    }
                    // Spurious (or a notify that did not move our
                    // generation) — re-arm and re-check.
                }
                _ = sleep => {
                    if self.is_cancelled() {
                        return WaitOutcome::Cancelled;
                    }
                    if *waiter.slot.state.lock() != waiter.gen {
                        return WaitOutcome::Woken;
                    }
                    return WaitOutcome::Timeout;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn notify_wakes_parked_waiter() {
        let reg = Arc::new(QueueWaitRegistry::new());
        let snap = reg.snapshot("default", "q");
        let reg_clone = reg.clone();
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            reg_clone.notify("default", "q");
        });
        let outcome = reg.wait_until(&snap, Instant::now() + Duration::from_secs(2));
        t.join().unwrap();
        assert_eq!(outcome, WaitOutcome::Woken);
    }

    #[test]
    fn timeout_returns_when_no_notify() {
        let reg = QueueWaitRegistry::new();
        let snap = reg.snapshot("default", "q");
        let start = Instant::now();
        let outcome = reg.wait_until(&snap, start + Duration::from_millis(120));
        assert_eq!(outcome, WaitOutcome::Timeout);
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(100), "elapsed={elapsed:?}");
    }

    #[test]
    fn cancel_returns_cancelled_to_parked_waiters() {
        let reg = Arc::new(QueueWaitRegistry::new());
        let snap = reg.snapshot("default", "q");
        let reg_clone = reg.clone();
        let t = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            reg_clone.cancel_all();
        });
        let outcome = reg.wait_until(&snap, Instant::now() + Duration::from_secs(5));
        t.join().unwrap();
        assert_eq!(outcome, WaitOutcome::Cancelled);
    }

    #[test]
    fn notify_before_wait_is_observed_through_generation() {
        let reg = QueueWaitRegistry::new();
        let snap = reg.snapshot("default", "q");
        // notify fires BEFORE wait_until — the generation bump must
        // still make wait_until return Woken without parking.
        reg.notify("default", "q");
        let outcome = reg.wait_until(&snap, Instant::now() + Duration::from_secs(5));
        assert_eq!(outcome, WaitOutcome::Woken);
    }

    #[test]
    fn notify_on_unrelated_queue_does_not_wake() {
        let reg = QueueWaitRegistry::new();
        let snap = reg.snapshot("default", "q1");
        reg.notify("default", "q2");
        let outcome = reg.wait_until(&snap, Instant::now() + Duration::from_millis(60));
        assert_eq!(outcome, WaitOutcome::Timeout);
    }

    #[tokio::test]
    async fn notify_wakes_both_sync_and_async_waiter_for_same_key() {
        // Issue #917 AC: a single `notify(scope, queue)` releases both
        // a synchronous condvar waiter and an async waiter parked on
        // the same key. The sync park runs on a blocking thread; the
        // async park awaits the wake head on this task.
        let reg = Arc::new(QueueWaitRegistry::new());

        let sync_reg = reg.clone();
        let sync_park = tokio::task::spawn_blocking(move || {
            let snap = sync_reg.snapshot("t", "q");
            sync_reg.wait_until(&snap, Instant::now() + Duration::from_secs(5))
        });

        let async_waiter = reg.async_waiter("t", "q");
        let async_reg = reg.clone();
        let async_park = tokio::spawn(async move {
            async_reg
                .wait_until_async(&async_waiter, Instant::now() + Duration::from_secs(5))
                .await
        });

        // Give both waiters time to park before the single notify.
        tokio::time::sleep(Duration::from_millis(50)).await;
        reg.notify("t", "q");

        assert_eq!(async_park.await.unwrap(), WaitOutcome::Woken);
        assert_eq!(sync_park.await.unwrap(), WaitOutcome::Woken);
    }

    #[tokio::test]
    async fn async_waiter_times_out_without_notify() {
        let reg = QueueWaitRegistry::new();
        let waiter = reg.async_waiter("t", "q");
        let start = Instant::now();
        let outcome = reg
            .wait_until_async(&waiter, start + Duration::from_millis(120))
            .await;
        assert_eq!(outcome, WaitOutcome::Timeout);
        assert!(start.elapsed() >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn async_notify_before_wait_is_observed_through_generation() {
        // A notify that fires AFTER the waiter snapshots the generation
        // but BEFORE the await must still return Woken (no lost wake).
        let reg = QueueWaitRegistry::new();
        let waiter = reg.async_waiter("t", "q");
        reg.notify("t", "q");
        let outcome = reg
            .wait_until_async(&waiter, Instant::now() + Duration::from_secs(5))
            .await;
        assert_eq!(outcome, WaitOutcome::Woken);
    }

    #[tokio::test]
    async fn cancel_all_wakes_async_waiter_with_cancelled() {
        // Issue #920 AC #4: the same `cancel_all` that wakes the
        // synchronous condvar waiters wakes an async waiter parked on
        // the registry's async wake head, surfacing `Cancelled` (not a
        // timeout, not a spurious `Woken`).
        let reg = Arc::new(QueueWaitRegistry::new());
        let waiter = reg.async_waiter("t", "q");
        let reg_clone = reg.clone();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            reg_clone.cancel_all();
        });
        // A generous deadline: the wait must end on cancellation, well
        // before this elapses, or the assertion below is meaningless.
        let outcome = reg
            .wait_until_async(&waiter, Instant::now() + Duration::from_secs(5))
            .await;
        canceller.await.unwrap();
        assert_eq!(outcome, WaitOutcome::Cancelled);
    }

    #[tokio::test]
    async fn cancelled_async_waiter_releases_its_slot_reference() {
        // Issue #920 AC #3: a cancelled async wait drops its waiter and
        // hence its `Arc<Slot>` clone promptly, so `live_waiters` falls
        // back to 0 — the slot reference (and the worker holding it) is
        // not stranded until the original wait deadline.
        let reg = Arc::new(QueueWaitRegistry::new());
        let reg_task = reg.clone();
        let park = tokio::spawn(async move {
            let waiter = reg_task.async_waiter("t", "q");
            reg_task
                .wait_until_async(&waiter, Instant::now() + Duration::from_secs(30))
                .await
        });
        // Let the task register its waiter and park.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(reg.live_waiters("t", "q"), 1, "waiter should be parked");

        // Abort mid-wait (the connection-close analogue) and confirm the
        // slot reference is released well before the 30s deadline.
        park.abort();
        let mut released = false;
        for _ in 0..200 {
            if reg.live_waiters("t", "q") == 0 {
                released = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(released, "aborted waiter must drop its slot reference");
    }

    #[test]
    fn wake_all_releases_every_parked_waiter() {
        let reg = Arc::new(QueueWaitRegistry::new());
        let mut handles = Vec::new();
        for _ in 0..5 {
            let reg = reg.clone();
            handles.push(thread::spawn(move || {
                let snap = reg.snapshot("default", "q");
                reg.wait_until(&snap, Instant::now() + Duration::from_secs(2))
            }));
        }
        thread::sleep(Duration::from_millis(80));
        reg.notify("default", "q");
        for h in handles {
            assert_eq!(h.join().unwrap(), WaitOutcome::Woken);
        }
    }
}
