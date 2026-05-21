//! Bounded handler-thread admission for the clear-text HTTP accept loop.
//!
//! Slice 1 of issue #570 / parent #569. The synchronous HTTP transport
//! spawns one OS thread per accepted connection. Without an admission
//! cap the server can degrade into thread-storm and lock starvation
//! under load. `HttpConnectionLimiter` is a single `AtomicUsize`-backed
//! semaphore consulted in the accept loop *before* parsing or handler
//! work. A rejected connection gets a static `503 + Retry-After` written
//! and the socket closed without ever entering the runtime.
//!
//! Hard cap for this slice is `(2 * available_parallelism).clamp(8, 256)`.
//! Config knobs (env / CLI) land in slice 5 per the parent brief.
//!
//! Beyond admission, the limiter keeps a single rejection counter and an
//! injectable monotonic clock (issue #620). Every `try_acquire` that hits
//! the cap bumps the counter; `observe()` snapshots-and-resets it against
//! the elapsed wall to derive a rejection rate. v1 ships a constant
//! `Retry-After`; the rate signal is what a future v2 will use to make
//! `Retry-After` adaptive. The clock is a trait so tests drive the rate
//! deterministically without sleeping.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Monotonic clock abstraction. Production uses [`SystemMonotonicClock`]
/// (a process-start `Instant` baseline); tests inject a fake that can be
/// advanced by hand so the rejection-rate derivation is deterministic.
pub trait MonotonicClock: Send + Sync + std::fmt::Debug {
    /// Nanoseconds elapsed since an arbitrary, fixed epoch. Only
    /// differences are meaningful; the absolute value carries no meaning.
    fn now_nanos(&self) -> u64;
}

/// Real monotonic clock: nanoseconds since the limiter's construction.
#[derive(Debug)]
pub struct SystemMonotonicClock {
    base: Instant,
}

impl SystemMonotonicClock {
    pub fn new() -> Self {
        Self {
            base: Instant::now(),
        }
    }
}

impl Default for SystemMonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl MonotonicClock for SystemMonotonicClock {
    fn now_nanos(&self) -> u64 {
        // u64 nanos overflows ~584 years after construction; safe.
        self.base.elapsed().as_nanos() as u64
    }
}

/// Snapshot returned by [`HttpConnectionLimiter::observe`]: the rejections
/// accumulated since the previous observe, the wall elapsed across that
/// window, and the derived rate. `rejections_per_sec` is `0.0` for a
/// zero-length window (no time has passed) so callers never divide by
/// zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LimiterObservation {
    pub rejected: u64,
    pub elapsed: Duration,
    pub rejections_per_sec: f64,
}

#[derive(Debug)]
struct Inner {
    cap: usize,
    in_use: AtomicUsize,
    /// Rejections accumulated since the last `observe()`. Bumped on every
    /// `try_acquire` that finds the cap full; reset to 0 by `observe()`.
    rejected: AtomicU64,
    /// Clock reading (nanos) captured at the last `observe()`, used as the
    /// lower bound of the next window. Seeded at construction.
    last_observe_nanos: AtomicU64,
    clock: Arc<dyn MonotonicClock>,
}

/// Permit handle — owns one slot of the limiter. Dropping the permit
/// returns the slot. The permit is intentionally `!Clone` so the slot
/// accounting can't drift.
#[derive(Debug)]
pub struct HttpConnectionPermit {
    inner: Arc<Inner>,
}

impl Drop for HttpConnectionPermit {
    fn drop(&mut self) {
        // Release is correct here: we want any writes the handler made
        // to be visible to a thread that subsequently re-acquires this
        // logical slot. Cap is fixed at construction so no need to
        // gate readers behind Acquire — readers of `current()` use
        // Relaxed below for observability only.
        self.inner.in_use.fetch_sub(1, Ordering::Release);
    }
}

#[derive(Debug, Clone)]
pub struct HttpConnectionLimiter {
    inner: Arc<Inner>,
}

impl HttpConnectionLimiter {
    pub fn new(cap: usize) -> Self {
        Self::with_clock(cap, Arc::new(SystemMonotonicClock::new()))
    }

    /// Construct with an explicit clock. Production uses [`new`], which
    /// wires the real monotonic clock; tests inject a fake to drive the
    /// rejection-rate derivation deterministically.
    pub fn with_clock(cap: usize, clock: Arc<dyn MonotonicClock>) -> Self {
        assert!(cap > 0, "HttpConnectionLimiter cap must be positive");
        let base = clock.now_nanos();
        Self {
            inner: Arc::new(Inner {
                cap,
                in_use: AtomicUsize::new(0),
                rejected: AtomicU64::new(0),
                last_observe_nanos: AtomicU64::new(base),
                clock,
            }),
        }
    }

    /// Default cap: `(2 * available_parallelism).clamp(8, 256)`.
    pub fn with_default_cap() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let cap = (2 * cores).clamp(8, 256);
        Self::new(cap)
    }

    pub fn cap(&self) -> usize {
        self.inner.cap
    }

    pub fn current(&self) -> usize {
        self.inner.in_use.load(Ordering::Relaxed)
    }

    /// Returns `Some(permit)` on success, `None` if the cap is full.
    /// No blocking, no allocation on the hot path.
    pub fn try_acquire(&self) -> Option<HttpConnectionPermit> {
        let mut observed = self.inner.in_use.load(Ordering::Relaxed);
        loop {
            if observed >= self.inner.cap {
                // Cap full: count the rejection for the rate signal. This
                // is the only mutation on the reject path — no alloc, no
                // lock, no parsing, as the accept loop requires.
                self.inner.rejected.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            match self.inner.in_use.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::Acquire,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    return Some(HttpConnectionPermit {
                        inner: Arc::clone(&self.inner),
                    });
                }
                Err(actual) => observed = actual,
            }
        }
    }

    /// Rejections accumulated since the last [`observe`](Self::observe).
    /// Read-only: it accumulates monotonically within a window and is
    /// reset only by `observe`.
    pub fn rejected_since_last_observe(&self) -> u64 {
        self.inner.rejected.load(Ordering::Relaxed)
    }

    /// Snapshot-and-reset the rejection window: returns the rejections
    /// since the previous `observe` together with the elapsed wall and the
    /// derived per-second rate, then resets the counter and arms the next
    /// window at the current clock reading. `rejections_per_sec` is `0.0`
    /// when no time has elapsed (avoids a divide-by-zero on back-to-back
    /// observes).
    pub fn observe(&self) -> LimiterObservation {
        let now = self.inner.clock.now_nanos();
        let last = self.inner.last_observe_nanos.swap(now, Ordering::Relaxed);
        let rejected = self.inner.rejected.swap(0, Ordering::Relaxed);
        let elapsed_nanos = now.saturating_sub(last);
        let rejections_per_sec = if elapsed_nanos == 0 {
            0.0
        } else {
            rejected as f64 * 1_000_000_000.0 / elapsed_nanos as f64
        };
        LimiterObservation {
            rejected,
            elapsed: Duration::from_nanos(elapsed_nanos),
            rejections_per_sec,
        }
    }
}

/// Per-handler total wall-clock deadline (issue #621), armed against the
/// same [`MonotonicClock`] abstraction the limiter uses. The clear-text
/// (and TLS) HTTP handler arms one of these at spawn and polls
/// [`expired`](Self::expired) at coarse boundaries (between parse, route
/// dispatch, and write). Production wires [`SystemMonotonicClock`], so the
/// deadline tracks real wall time; tests inject a fake clock to drive
/// expiry deterministically without `sleep()`.
///
/// This bounds — but does not pre-empt — handler lifetime: a thread blocked
/// inside a true syscall is still released only by the per-socket
/// read/write timeouts. The deadline reclaims a limiter slot for the
/// internal-lock-contention case the PRD (#569) targets.
#[derive(Debug, Clone)]
pub struct HandlerDeadline {
    clock: Arc<dyn MonotonicClock>,
    /// Absolute clock reading (nanos) at or after which the handler is
    /// over budget. Saturating-added at arm time so a near-`u64::MAX`
    /// base can never wrap into the past.
    deadline_nanos: u64,
}

impl HandlerDeadline {
    /// Arm a deadline `timeout` from now, read off `clock`. The clock is
    /// shared (`Arc`) so the same instance can be reused across handlers.
    pub fn arm(clock: Arc<dyn MonotonicClock>, timeout: Duration) -> Self {
        let now = clock.now_nanos();
        let deadline_nanos = now.saturating_add(timeout.as_nanos() as u64);
        Self {
            clock,
            deadline_nanos,
        }
    }

    /// `true` once the clock has reached the armed deadline. Checked at
    /// coarse boundaries — never inside a blocking call.
    pub fn expired(&self) -> bool {
        self.clock.now_nanos() >= self.deadline_nanos
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::thread;

    /// Hand-advanced clock for deterministic rejection-rate tests.
    #[derive(Debug, Default)]
    struct FakeClock {
        nanos: AtomicU64,
    }

    impl FakeClock {
        fn advance(&self, d: Duration) {
            self.nanos
                .fetch_add(d.as_nanos() as u64, Ordering::Relaxed);
        }
    }

    impl MonotonicClock for FakeClock {
        fn now_nanos(&self) -> u64 {
            self.nanos.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn cap_and_current_track_observed_state() {
        let limiter = HttpConnectionLimiter::new(3);
        assert_eq!(limiter.cap(), 3);
        assert_eq!(limiter.current(), 0);

        let p1 = limiter.try_acquire().expect("slot 1");
        assert_eq!(limiter.current(), 1);
        let p2 = limiter.try_acquire().expect("slot 2");
        assert_eq!(limiter.current(), 2);
        let p3 = limiter.try_acquire().expect("slot 3");
        assert_eq!(limiter.current(), 3);

        assert!(limiter.try_acquire().is_none());
        assert_eq!(limiter.current(), 3);

        drop(p2);
        assert_eq!(limiter.current(), 2);
        let p4 = limiter.try_acquire().expect("slot reused");
        assert_eq!(limiter.current(), 3);
        drop((p1, p3, p4));
        assert_eq!(limiter.current(), 0);
    }

    #[test]
    fn permit_drop_restores_capacity() {
        let limiter = HttpConnectionLimiter::new(1);
        {
            let _p = limiter.try_acquire().expect("acquired");
            assert!(limiter.try_acquire().is_none());
        }
        assert_eq!(limiter.current(), 0);
        let _p = limiter.try_acquire().expect("reacquired after drop");
        assert_eq!(limiter.current(), 1);
    }

    #[test]
    fn cap_enforced_under_thread_storm_no_over_issue() {
        // Many threads race try_acquire; verify the high-water-mark
        // never exceeds the cap, and the total successful acquires
        // matches the cap when permits are held.
        let cap = 8;
        let limiter = HttpConnectionLimiter::new(cap);
        let success = Arc::new(AtomicUsize::new(0));
        let denied = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let permits: Arc<std::sync::Mutex<Vec<HttpConnectionPermit>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for _ in 0..64 {
            let l = limiter.clone();
            let s = Arc::clone(&success);
            let d = Arc::clone(&denied);
            let m = Arc::clone(&max_seen);
            let permits = Arc::clone(&permits);
            handles.push(thread::spawn(move || match l.try_acquire() {
                Some(p) => {
                    s.fetch_add(1, Ordering::Relaxed);
                    let now = l.current();
                    m.fetch_max(now, Ordering::Relaxed);
                    permits.lock().unwrap().push(p);
                }
                None => {
                    d.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(success.load(Ordering::Relaxed), cap);
        assert_eq!(denied.load(Ordering::Relaxed), 64 - cap);
        assert!(max_seen.load(Ordering::Relaxed) <= cap);
        assert_eq!(limiter.current(), cap);

        permits.lock().unwrap().clear();
        assert_eq!(limiter.current(), 0);
    }

    #[test]
    fn clone_shares_state() {
        let a = HttpConnectionLimiter::new(2);
        let b = a.clone();
        let _p = a.try_acquire().unwrap();
        assert_eq!(b.current(), 1);
        assert_eq!(b.cap(), 2);
    }

    #[test]
    fn default_cap_in_bounds() {
        let limiter = HttpConnectionLimiter::with_default_cap();
        assert!(limiter.cap() >= 8);
        assert!(limiter.cap() <= 256);
    }

    #[test]
    fn rejected_accumulates_within_window_and_resets_on_observe() {
        let limiter = HttpConnectionLimiter::new(1);
        let _held = limiter.try_acquire().expect("first slot");

        assert_eq!(limiter.rejected_since_last_observe(), 0);
        // Each over-cap acquire bumps the counter monotonically.
        for expected in 1..=4 {
            assert!(limiter.try_acquire().is_none());
            assert_eq!(limiter.rejected_since_last_observe(), expected);
        }

        // observe() drains the window; the counter resets.
        let obs = limiter.observe();
        assert_eq!(obs.rejected, 4);
        assert_eq!(limiter.rejected_since_last_observe(), 0);

        // A subsequent observe with no rejections reports zero.
        assert!(limiter.try_acquire().is_none());
        assert_eq!(limiter.observe().rejected, 1);
        assert_eq!(limiter.observe().rejected, 0);
    }

    #[test]
    fn fake_clock_rejection_rate_derivation() {
        let clock = Arc::new(FakeClock::default());
        let limiter = HttpConnectionLimiter::with_clock(1, clock.clone());
        let _held = limiter.try_acquire().expect("first slot");

        // 10 rejections across a 2s window -> 5 rejections/sec.
        for _ in 0..10 {
            assert!(limiter.try_acquire().is_none());
        }
        clock.advance(Duration::from_secs(2));
        let obs = limiter.observe();
        assert_eq!(obs.rejected, 10);
        assert_eq!(obs.elapsed, Duration::from_secs(2));
        assert!((obs.rejections_per_sec - 5.0).abs() < 1e-9);

        // Next window: 3 rejections across 500ms -> 6 rejections/sec.
        for _ in 0..3 {
            assert!(limiter.try_acquire().is_none());
        }
        clock.advance(Duration::from_millis(500));
        let obs = limiter.observe();
        assert_eq!(obs.rejected, 3);
        assert!((obs.rejections_per_sec - 6.0).abs() < 1e-9);
    }

    #[test]
    fn observe_with_zero_elapsed_reports_zero_rate_not_nan() {
        let clock = Arc::new(FakeClock::default());
        let limiter = HttpConnectionLimiter::with_clock(1, clock.clone());
        let _held = limiter.try_acquire().expect("first slot");
        assert!(limiter.try_acquire().is_none());
        // No clock advance: back-to-back observe must not divide by zero.
        let obs = limiter.observe();
        assert_eq!(obs.elapsed, Duration::ZERO);
        assert_eq!(obs.rejected, 1);
        assert_eq!(obs.rejections_per_sec, 0.0);
    }

    #[test]
    fn handler_deadline_not_expired_before_timeout() {
        let clock = Arc::new(FakeClock::default());
        let deadline = HandlerDeadline::arm(clock.clone(), Duration::from_millis(200));
        // Right after arming: not expired.
        assert!(!deadline.expired());
        // Advance to just under the budget: still not expired.
        clock.advance(Duration::from_millis(199));
        assert!(!deadline.expired());
    }

    #[test]
    fn handler_deadline_expires_at_and_after_timeout() {
        let clock = Arc::new(FakeClock::default());
        let deadline = HandlerDeadline::arm(clock.clone(), Duration::from_millis(200));
        // Exactly at the deadline: expired (`>=`).
        clock.advance(Duration::from_millis(200));
        assert!(deadline.expired());
        // And it stays expired as time marches on — no real sleeps used.
        clock.advance(Duration::from_secs(5));
        assert!(deadline.expired());
    }

    #[test]
    fn handler_deadline_arm_saturates_without_wrapping() {
        // A near-u64::MAX base must not wrap the deadline into the past.
        #[derive(Debug)]
        struct MaxClock;
        impl MonotonicClock for MaxClock {
            fn now_nanos(&self) -> u64 {
                u64::MAX - 10
            }
        }
        let deadline = HandlerDeadline::arm(Arc::new(MaxClock), Duration::from_secs(30));
        // now (u64::MAX - 10) < saturated deadline (u64::MAX) -> not expired.
        assert!(!deadline.expired());
    }
}
