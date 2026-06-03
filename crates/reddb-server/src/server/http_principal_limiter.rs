//! Per-principal in-flight admission for the async HTTP edge (issue #934,
//! PRD #930).
//!
//! The thread-per-connection cap retired in #931 used to bound *both*
//! total concurrency and any single caller's share of it as a side effect
//! — one greedy client could only ever hold as many slots as it held OS
//! threads, and the global `(2*num_cpus).clamp(8,256)` ceiling capped
//! that. The async edge removed the OS-thread coupling: an idle keep-alive
//! connection is now a parked future, and admission is per in-flight
//! request against a single global [`HttpConnectionLimiter`]. That global
//! cap bounds *total* in-flight work (async backpressure) but no longer
//! bounds any single principal's share — one abusive caller can drain the
//! whole global cap and starve everyone else.
//!
//! This limiter restores the per-caller bound as a first-class control: a
//! small `AtomicUsize`-per-principal in-flight counter consulted at the
//! edge *after* global admission. A principal over its own cap gets a
//! structured 429 refusal (see [`PrincipalCapExceeded`]) carrying the
//! limit, the live count, and the principal label so a well-behaved client
//! can back off without guessing. It is the concurrency sibling of the
//! per-principal QPS quota ([`crate::runtime::quota_bucket::QuotaBucket`],
//! which bounds *rate*) and the per-principal stream cap (ADR 0029 /
//! `StreamCapacityRegistry`, which bounds concurrent *streams*).
//!
//! A `cap` of `0` disables the limiter entirely (every acquire succeeds) so
//! operators can opt out and so small single-tenant deployments pay nothing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Refusal returned by [`PrincipalConnectionLimiter::try_acquire`] when a
/// principal is already at its concurrent in-flight cap. Carries the values
/// a client needs to back off intelligently: its own cap, the live count it
/// hit, and the principal label the server bucketed it under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalCapExceeded {
    pub principal: String,
    pub limit: usize,
    pub current: usize,
}

/// Stable machine-readable refusal code embedded in the structured body so
/// clients branch on it without parsing prose.
pub const PRINCIPAL_INFLIGHT_CODE: &str = "principal_inflight_exhausted";

#[derive(Debug)]
struct Inner {
    /// Per-principal concurrent in-flight ceiling. `0` disables the
    /// limiter (every acquire succeeds, no map mutation).
    cap: usize,
    /// `principal -> live in-flight count`. An entry exists only while a
    /// principal holds at least one permit; it is removed when its count
    /// returns to zero so the map can't grow unbounded under churn.
    in_use: Mutex<HashMap<String, usize>>,
    /// Total refusals since process start, surfaced to `/metrics`.
    rejected: AtomicU64,
}

/// Permit handle — owns one in-flight slot for a single principal. Dropping
/// the permit decrements that principal's count (and evicts the entry at
/// zero). Intentionally `!Clone` so slot accounting can't drift.
#[derive(Debug)]
pub struct PrincipalInflightPermit {
    inner: Arc<Inner>,
    principal: String,
}

impl Drop for PrincipalInflightPermit {
    fn drop(&mut self) {
        let mut map = self.inner.in_use.lock().expect("principal limiter mutex");
        if let Some(count) = map.get_mut(&self.principal) {
            *count -= 1;
            if *count == 0 {
                map.remove(&self.principal);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct PrincipalConnectionLimiter {
    inner: Arc<Inner>,
}

impl PrincipalConnectionLimiter {
    /// Build a limiter with the given per-principal concurrent in-flight
    /// cap. `0` disables enforcement.
    pub fn new(cap: usize) -> Self {
        Self {
            inner: Arc::new(Inner {
                cap,
                in_use: Mutex::new(HashMap::new()),
                rejected: AtomicU64::new(0),
            }),
        }
    }

    /// The configured per-principal cap. `0` means disabled.
    pub fn cap(&self) -> usize {
        self.inner.cap
    }

    /// `true` when enforcement is active (cap > 0).
    pub fn is_enforced(&self) -> bool {
        self.inner.cap > 0
    }

    /// Live in-flight count for a single principal (`0` if it holds none).
    /// Observability only.
    pub fn current_for(&self, principal: &str) -> usize {
        let map = self.inner.in_use.lock().expect("principal limiter mutex");
        map.get(principal).copied().unwrap_or(0)
    }

    /// Number of principals currently holding at least one permit.
    pub fn tracked_principals(&self) -> usize {
        let map = self.inner.in_use.lock().expect("principal limiter mutex");
        map.len()
    }

    /// Total refusals issued since construction. Surfaced via
    /// `reddb_http_principal_inflight_rejected_total`.
    pub fn rejected_total(&self) -> u64 {
        self.inner.rejected.load(Ordering::Relaxed)
    }

    /// Admit one in-flight request for `principal`. Returns `Ok(permit)`
    /// when the principal is under its cap (slot reserved, released on
    /// permit drop) or `Err(PrincipalCapExceeded)` when it is already at
    /// the cap. A disabled limiter (cap `0`) always admits and never
    /// touches the map.
    pub fn try_acquire(
        &self,
        principal: &str,
    ) -> Result<PrincipalInflightPermit, PrincipalCapExceeded> {
        if self.inner.cap == 0 {
            return Ok(PrincipalInflightPermit {
                inner: Arc::clone(&self.inner),
                principal: principal.to_string(),
            });
        }
        let mut map = self.inner.in_use.lock().expect("principal limiter mutex");
        let count = map.entry(principal.to_string()).or_insert(0);
        if *count >= self.inner.cap {
            let current = *count;
            // Drop the lock before bumping the (Relaxed) counter — keep the
            // critical section to the map mutation only.
            drop(map);
            self.inner.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(PrincipalCapExceeded {
                principal: principal.to_string(),
                limit: self.inner.cap,
                current,
            });
        }
        *count += 1;
        Ok(PrincipalInflightPermit {
            inner: Arc::clone(&self.inner),
            principal: principal.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    #[test]
    fn disabled_cap_admits_everything_without_tracking() {
        let limiter = PrincipalConnectionLimiter::new(0);
        assert!(!limiter.is_enforced());
        let mut permits = Vec::new();
        for _ in 0..1_000 {
            permits.push(limiter.try_acquire("alice").expect("disabled admits"));
        }
        // cap==0 short-circuits before the map, so nothing is tracked.
        assert_eq!(limiter.tracked_principals(), 0);
        assert_eq!(limiter.rejected_total(), 0);
    }

    #[test]
    fn admits_up_to_cap_then_refuses_with_structured_detail() {
        let limiter = PrincipalConnectionLimiter::new(3);
        let p1 = limiter.try_acquire("alice").expect("slot 1");
        let p2 = limiter.try_acquire("alice").expect("slot 2");
        let p3 = limiter.try_acquire("alice").expect("slot 3");
        assert_eq!(limiter.current_for("alice"), 3);

        let err = limiter.try_acquire("alice").expect_err("over cap");
        assert_eq!(
            err,
            PrincipalCapExceeded {
                principal: "alice".to_string(),
                limit: 3,
                current: 3,
            }
        );
        assert_eq!(limiter.rejected_total(), 1);
        drop((p1, p2, p3));
    }

    #[test]
    fn dropping_a_permit_frees_a_slot() {
        let limiter = PrincipalConnectionLimiter::new(1);
        let p = limiter.try_acquire("bob").expect("first slot");
        assert!(limiter.try_acquire("bob").is_err());
        drop(p);
        // Entry evicted at zero, then re-acquirable.
        assert_eq!(limiter.current_for("bob"), 0);
        assert_eq!(limiter.tracked_principals(), 0);
        let _p = limiter.try_acquire("bob").expect("reacquire after drop");
        assert_eq!(limiter.current_for("bob"), 1);
    }

    #[test]
    fn principals_are_isolated() {
        let limiter = PrincipalConnectionLimiter::new(1);
        let _alice = limiter.try_acquire("alice").expect("alice slot");
        // alice is saturated, but bob has his own independent budget.
        assert!(limiter.try_acquire("alice").is_err());
        let _bob = limiter.try_acquire("bob").expect("bob unaffected");
        assert_eq!(limiter.tracked_principals(), 2);
    }

    #[test]
    fn entry_evicted_when_last_permit_drops() {
        let limiter = PrincipalConnectionLimiter::new(4);
        let a = limiter.try_acquire("carol").expect("1");
        let b = limiter.try_acquire("carol").expect("2");
        assert_eq!(limiter.tracked_principals(), 1);
        drop(a);
        assert_eq!(limiter.current_for("carol"), 1);
        assert_eq!(limiter.tracked_principals(), 1);
        drop(b);
        assert_eq!(limiter.tracked_principals(), 0);
    }

    #[test]
    fn concurrent_acquire_never_over_issues_per_principal() {
        // Many threads race the same principal; the high-water count
        // must never exceed the cap and successes must equal the cap.
        let cap = 8;
        let limiter = PrincipalConnectionLimiter::new(cap);
        let success = Arc::new(AtomicUsize::new(0));
        let denied = Arc::new(AtomicUsize::new(0));
        let held: Arc<Mutex<Vec<PrincipalInflightPermit>>> = Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for _ in 0..64 {
            let l = limiter.clone();
            let s = Arc::clone(&success);
            let d = Arc::clone(&denied);
            let h = Arc::clone(&held);
            handles.push(thread::spawn(move || match l.try_acquire("storm") {
                Ok(p) => {
                    s.fetch_add(1, Ordering::Relaxed);
                    h.lock().unwrap().push(p);
                }
                Err(_) => {
                    d.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(success.load(Ordering::Relaxed), cap);
        assert_eq!(denied.load(Ordering::Relaxed), 64 - cap);
        assert_eq!(limiter.current_for("storm"), cap);
        assert_eq!(limiter.rejected_total() as usize, 64 - cap);

        held.lock().unwrap().clear();
        assert_eq!(limiter.current_for("storm"), 0);
    }

    #[test]
    fn clone_shares_state() {
        let a = PrincipalConnectionLimiter::new(2);
        let b = a.clone();
        let _p = a.try_acquire("dave").unwrap();
        assert_eq!(b.current_for("dave"), 1);
        assert_eq!(b.cap(), 2);
    }
}
