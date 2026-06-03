//! Per-principal in-flight-request (connection) caps for the async HTTP
//! edge (issue #934, PRD #930, ADR 0035).
//!
//! The async edge retired the OS-thread cap (#931): admission is now per
//! in-flight request through the global [`HttpConnectionLimiter`], which
//! provides the **async backpressure** half of abuse defense — it bounds
//! total in-flight work without a thread cap, and an idle keep-alive
//! connection holds neither a thread nor a slot. This module adds the
//! **fairness** half: a per-principal ceiling on concurrent in-flight
//! requests so one caller cannot monopolise the global budget. Over-cap
//! requests get a structured refusal (built by the HTTP layer) so clients
//! can back off.
//!
//! Principal identity is the same stable label the QPS quota gate
//! ([`crate::runtime::quota_bucket`]) and the stream-capacity registry
//! ([`super::output_stream::StreamCapacityRegistry`]) use, produced by
//! [`super::routing::principal_for`]: `bearer:<hash>`, `replica:<id>`, or
//! `anon`.
//!
//! The cap is a per-acquire argument read from resolved config, mirroring
//! the stream-capacity registry. A cap of `0` **disables** enforcement
//! (the default): all unauthenticated callers share the single `anon`
//! label, so a finite default would throttle aggregate anonymous traffic
//! — exactly like the QPS quota, which also defaults off. Operators opt in
//! by setting `red.http.max_conns_per_principal`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Process-wide per-principal concurrent-request registry. Holds one
/// in-flight count per principal label; the count is decremented when the
/// [`PrincipalConnPermit`] handed back from a successful `try_acquire` is
/// dropped, so the release path covers every request outcome (success,
/// handler timeout, panic unwind through the frame holding the permit).
#[derive(Debug, Default)]
pub struct PerPrincipalConnLimiter {
    inner: Mutex<HashMap<String, usize>>,
    /// Total refusals issued since process start — surfaced through the
    /// HTTP handler metrics so operators can see a single abuser being
    /// shed without it looking like a global outage.
    rejected: AtomicU64,
}

/// Refusal returned by [`PerPrincipalConnLimiter::try_acquire`] when the
/// principal is already at its cap. Carries the cap that fired and the
/// live count so the structured 429 body lets a client back off precisely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalCapExceeded {
    /// The principal label that hit its cap. Surfaced verbatim; the HTTP
    /// layer escapes it on the wire (it can embed a header-derived
    /// `replica:<id>`).
    pub principal: String,
    pub limit: usize,
    pub current: usize,
}

impl PrincipalCapExceeded {
    /// Stable machine-readable refusal code clients branch on. Distinct
    /// from the stream-capacity `principal_stream_quota_exhausted` so the
    /// two abuse-defense surfaces stay distinguishable in client logs.
    pub const CODE: &'static str = "principal_connection_quota_exhausted";
}

/// RAII slot returned by [`PerPrincipalConnLimiter::try_acquire`].
/// Decrements the principal's count on drop. When the limiter was disabled
/// (cap `0`) the permit is untracked and dropping it is a no-op.
#[must_use = "dropping the permit immediately releases the per-principal slot"]
#[derive(Debug)]
pub struct PrincipalConnPermit {
    /// `None` when the limiter was disabled at acquire time: the permit
    /// tracks nothing and Drop does nothing.
    registry: Option<Arc<PerPrincipalConnLimiter>>,
    principal: String,
}

impl Drop for PrincipalConnPermit {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.take() {
            registry.release(&self.principal);
        }
    }
}

impl PerPrincipalConnLimiter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Admit one in-flight request for `principal`, or refuse if the
    /// principal already holds `cap` slots. `cap == 0` disables
    /// enforcement: every call is admitted and returns an untracked
    /// permit (no map mutation, no lock-held accounting).
    pub fn try_acquire(
        self: &Arc<Self>,
        principal: &str,
        cap: usize,
    ) -> Result<PrincipalConnPermit, PrincipalCapExceeded> {
        if cap == 0 {
            return Ok(PrincipalConnPermit {
                registry: None,
                principal: String::new(),
            });
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let current = inner.get(principal).copied().unwrap_or(0);
        if current >= cap {
            // Release the lock before bumping the relaxed counter — the
            // counter is observability-only and need not be under the
            // map lock.
            drop(inner);
            self.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(PrincipalCapExceeded {
                principal: principal.to_string(),
                limit: cap,
                current,
            });
        }
        inner.insert(principal.to_string(), current + 1);
        Ok(PrincipalConnPermit {
            registry: Some(Arc::clone(self)),
            principal: principal.to_string(),
        })
    }

    fn release(&self, principal: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = inner.get_mut(principal) {
            if *count > 0 {
                *count -= 1;
            }
            if *count == 0 {
                inner.remove(principal);
            }
        }
    }

    /// Live in-flight count for `principal` (0 if untracked). Visible for
    /// tests and observability.
    pub fn current(&self, principal: &str) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.get(principal).copied().unwrap_or(0)
    }

    /// Total per-principal refusals since process start.
    pub fn rejected_total(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }

    /// Number of distinct principals currently holding at least one slot.
    /// Visible for tests so they can assert the map drains to empty.
    pub fn tracked_principals(&self) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    #[test]
    fn disabled_cap_admits_everything_without_tracking() {
        let limiter = PerPrincipalConnLimiter::new();
        let mut permits = Vec::new();
        for _ in 0..1_000 {
            permits.push(limiter.try_acquire("alice", 0).expect("disabled admits"));
        }
        // Disabled mode never touches the map.
        assert_eq!(limiter.current("alice"), 0);
        assert_eq!(limiter.tracked_principals(), 0);
        assert_eq!(limiter.rejected_total(), 0);
        drop(permits);
        assert_eq!(limiter.tracked_principals(), 0);
    }

    #[test]
    fn enforces_cap_then_refuses_with_limit_and_current() {
        let limiter = PerPrincipalConnLimiter::new();
        let _p1 = limiter.try_acquire("bob", 2).expect("slot 1");
        let _p2 = limiter.try_acquire("bob", 2).expect("slot 2");
        assert_eq!(limiter.current("bob"), 2);

        let err = limiter.try_acquire("bob", 2).expect_err("over cap");
        assert_eq!(
            err,
            PrincipalCapExceeded {
                principal: "bob".to_string(),
                limit: 2,
                current: 2,
            }
        );
        assert_eq!(limiter.rejected_total(), 1);
    }

    #[test]
    fn permit_drop_restores_capacity_and_drains_map() {
        let limiter = PerPrincipalConnLimiter::new();
        {
            let _p = limiter.try_acquire("carol", 1).expect("slot");
            assert!(limiter.try_acquire("carol", 1).is_err());
            assert_eq!(limiter.current("carol"), 1);
        }
        // Dropping the only permit removes the principal entry entirely.
        assert_eq!(limiter.current("carol"), 0);
        assert_eq!(limiter.tracked_principals(), 0);
        let _reacquired = limiter.try_acquire("carol", 1).expect("reacquire after drop");
        assert_eq!(limiter.current("carol"), 1);
    }

    #[test]
    fn principals_isolate() {
        let limiter = PerPrincipalConnLimiter::new();
        let _a = limiter.try_acquire("alice", 1).expect("alice slot");
        // Alice is full, but bob has his own independent budget.
        assert!(limiter.try_acquire("alice", 1).is_err());
        let _b = limiter.try_acquire("bob", 1).expect("bob unaffected by alice");
        assert_eq!(limiter.current("alice"), 1);
        assert_eq!(limiter.current("bob"), 1);
    }

    #[test]
    fn cap_enforced_under_thread_storm_no_over_issue() {
        // Many threads race try_acquire for the same principal; the count
        // must never exceed the cap and exactly `cap` must succeed while
        // permits are held.
        let cap = 8;
        let limiter = PerPrincipalConnLimiter::new();
        let success = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let permits: Arc<Mutex<Vec<PrincipalConnPermit>>> = Arc::new(Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for _ in 0..64 {
            let l = Arc::clone(&limiter);
            let s = Arc::clone(&success);
            let m = Arc::clone(&max_seen);
            let permits = Arc::clone(&permits);
            handles.push(thread::spawn(move || {
                if let Ok(permit) = l.try_acquire("storm", cap) {
                    s.fetch_add(1, Ordering::Relaxed);
                    m.fetch_max(l.current("storm"), Ordering::Relaxed);
                    permits.lock().unwrap().push(permit);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(success.load(Ordering::Relaxed), cap);
        assert!(max_seen.load(Ordering::Relaxed) <= cap);
        assert_eq!(limiter.current("storm"), cap);
        assert_eq!(limiter.rejected_total(), 64 - cap as u64);

        permits.lock().unwrap().clear();
        assert_eq!(limiter.current("storm"), 0);
        assert_eq!(limiter.tracked_principals(), 0);
    }

    #[test]
    fn refusal_code_is_stable() {
        assert_eq!(
            PrincipalCapExceeded::CODE,
            "principal_connection_quota_exhausted"
        );
    }
}
