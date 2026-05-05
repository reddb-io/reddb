//! Advisory locks (PG-compatible `pg_advisory_*` family).
//!
//! Connection-scoped, reentrant-safe-only-through-explicit-unlock.
//! Backed by a single global mutex + condition variable — fine for
//! the embedded/single-node workload RedDB targets today. If it ever
//! becomes a hotspot, split into a `DashMap<i64, Mutex<_>>`.
//!
//! Ownership is tracked by `ConnId` (the same id savepoints use).
//! `release_all(conn)` drops every lock a connection holds — call it
//! on connection close so crashed sessions don't wedge other callers
//! forever.

use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::sync::OnceLock;

pub type ConnId = u64;

/// Process-global table. One per runtime is enough; lock ids are
/// a shared namespace across connections by PG semantics.
pub struct AdvisoryLocks {
    state: Mutex<HashMap<i64, ConnId>>,
    cv: Condvar,
}

impl AdvisoryLocks {
    fn new() -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
        }
    }

    /// Try to acquire `key` for `conn`. Returns `true` if the caller
    /// now owns it (either fresh acquire or already holds it —
    /// reentrant within the same connection, matching PG). Never
    /// blocks.
    pub fn try_acquire(&self, key: i64, conn: ConnId) -> bool {
        let mut map = self.state.lock();
        match map.get(&key).copied() {
            Some(owner) if owner == conn => true,
            Some(_) => false,
            None => {
                map.insert(key, conn);
                true
            }
        }
    }

    /// Acquire `key` for `conn`, blocking until the current owner
    /// (if any) releases. Reentrant within the same connection.
    pub fn acquire(&self, key: i64, conn: ConnId) {
        let mut map = self.state.lock();
        loop {
            match map.get(&key).copied() {
                Some(owner) if owner == conn => return,
                Some(_) => self.cv.wait(&mut map),
                None => {
                    map.insert(key, conn);
                    return;
                }
            }
        }
    }

    /// Release `key` for `conn`. Returns `true` if `conn` held the
    /// lock. A mismatch returns `false` and leaves the table
    /// untouched (PG behaviour: `pg_advisory_unlock` returns bool
    /// without panicking on foreign locks).
    pub fn release(&self, key: i64, conn: ConnId) -> bool {
        let mut map = self.state.lock();
        match map.get(&key).copied() {
            Some(owner) if owner == conn => {
                map.remove(&key);
                self.cv.notify_all();
                true
            }
            _ => false,
        }
    }

    /// Release every lock held by `conn`. Returns the number of
    /// locks dropped. Call this on connection close.
    pub fn release_all(&self, conn: ConnId) -> usize {
        let mut map = self.state.lock();
        let before = map.len();
        map.retain(|_, owner| *owner != conn);
        let dropped = before - map.len();
        if dropped > 0 {
            self.cv.notify_all();
        }
        dropped
    }

    /// Test-visible: is `key` currently held (by anyone)?
    #[cfg(test)]
    pub fn is_held(&self, key: i64) -> bool {
        self.state.lock().contains_key(&key)
    }
}

static GLOBAL: OnceLock<AdvisoryLocks> = OnceLock::new();

/// Process-wide singleton accessor. Lazy-init on first call.
pub fn global() -> &'static AdvisoryLocks {
    GLOBAL.get_or_init(AdvisoryLocks::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_acquire_and_release() {
        let locks = AdvisoryLocks::new();
        assert!(locks.try_acquire(1, 100));
        assert!(!locks.try_acquire(1, 200), "other conn cannot steal");
        assert!(locks.try_acquire(1, 100), "same conn is reentrant");
        assert!(locks.release(1, 100));
        assert!(!locks.is_held(1));
    }

    #[test]
    fn release_all_drops_only_owned() {
        let locks = AdvisoryLocks::new();
        assert!(locks.try_acquire(1, 100));
        assert!(locks.try_acquire(2, 100));
        assert!(locks.try_acquire(3, 200));
        assert_eq!(locks.release_all(100), 2);
        assert!(!locks.is_held(1));
        assert!(!locks.is_held(2));
        assert!(locks.is_held(3));
    }

    #[test]
    fn release_mismatch_returns_false() {
        let locks = AdvisoryLocks::new();
        assert!(locks.try_acquire(5, 100));
        assert!(!locks.release(5, 999));
        assert!(locks.is_held(5));
    }
}
