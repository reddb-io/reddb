//! Pluggable wall-clock abstraction for lease, term-fencing, and election timing.
//!
//! Production code always uses `SystemClock`. Tests inject `SimClock`, whose
//! time only moves when the test explicitly calls `advance_ms` or `set_ms`,
//! making timing fully deterministic and seed-reproducible.

use std::sync::atomic::{AtomicU64, Ordering};

/// Abstraction over the wall clock used by lease and fencing logic.
///
/// Implementors must be `Send + Sync` so they can be shared across tasks.
pub trait Clock: Send + Sync {
    /// Returns the current time as milliseconds since the Unix epoch.
    fn now_unix_millis(&self) -> u64;
}

/// Production clock: delegates to `SystemTime::now()`.
pub struct SystemClock;

impl Clock for SystemClock {
    #[inline]
    fn now_unix_millis(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}

/// Deterministic simulation clock for tests.
///
/// Time is frozen at `seed_ms` until the test calls [`SimClock::advance_ms`]
/// or [`SimClock::set_ms`].  All operations on the internal counter use
/// relaxed atomics — `SimClock` is not intended for concurrent mutation, only
/// for sequential test control.
pub struct SimClock {
    now_ms: AtomicU64,
}

impl SimClock {
    /// Create a clock frozen at `seed_ms` milliseconds since the Unix epoch.
    pub fn from_seed(seed_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(seed_ms),
        }
    }

    /// Move the clock forward by `delta_ms` milliseconds.
    pub fn advance_ms(&self, delta_ms: u64) {
        self.now_ms.fetch_add(delta_ms, Ordering::Relaxed);
    }

    /// Jump the clock to an absolute time (milliseconds since Unix epoch).
    pub fn set_ms(&self, ms: u64) {
        self.now_ms.store(ms, Ordering::Relaxed);
    }
}

impl Clock for SimClock {
    #[inline]
    fn now_unix_millis(&self) -> u64 {
        self.now_ms.load(Ordering::Relaxed)
    }
}
