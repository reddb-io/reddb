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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Debug)]
struct Inner {
    cap: usize,
    in_use: AtomicUsize,
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
        assert!(cap > 0, "HttpConnectionLimiter cap must be positive");
        Self {
            inner: Arc::new(Inner {
                cap,
                in_use: AtomicUsize::new(0),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use std::thread;

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
}
