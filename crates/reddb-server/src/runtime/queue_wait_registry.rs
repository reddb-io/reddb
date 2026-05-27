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
}

impl Slot {
    fn new() -> Self {
        Self {
            state: Mutex::new(0),
            cond: Condvar::new(),
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
        slot.cond.notify_all();
    }

    /// Shutdown drain: set the cancellation flag and wake every slot's
    /// condvar so parked waiters return `Cancelled` immediately.
    pub fn cancel_all(&self) {
        self.cancelled.store(true, Ordering::Release);
        let slots = self.slots.lock();
        for slot in slots.values() {
            let _g = slot.state.lock();
            slot.cond.notify_all();
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
