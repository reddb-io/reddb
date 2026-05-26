//! Named crash-injection points for `vector.turbo` (issue #694).
//!
//! Defines the contract that the crash-safety slice (#673) drives its
//! kill-point tests against. Production builds compile [`fire`] down
//! to a no-op; test builds and the `turbo-crash-inject` feature build
//! pull in the installable [`TurboCrashInjector`] trait so a test can
//! observe — or panic at — any of the four named boundaries in the
//! durable-write sequence.
//!
//! Boundary semantics, in the INSERT write order:
//!
//! 1. [`InjectionPoint::BeforeWalFsync`] — fired right before the WAL
//!    durability handshake on the per-vector `WalRecord::VectorInsert`.
//!    A crash here loses the insert entirely (WAL replay is empty).
//! 2. [`InjectionPoint::BeforeIndexCommit`] — fired after the WAL
//!    insert is durable but before the in-memory `TurboQuantIndex`
//!    observes the new vector. A crash here is recovered on boot by
//!    WAL replay.
//! 3. [`InjectionPoint::BeforeExtentFsync`] — fired after the
//!    in-memory index has been mutated but before the encoded codes
//!    are appended to the persistent `TurboExtent`. A crash here is
//!    recovered the same way (WAL replay re-encodes, the
//!    deterministic codec seed reproduces the identical extent
//!    bytes).
//! 4. [`InjectionPoint::MidCheckpoint`] — fired from inside the
//!    checkpoint accounting loop when it encounters a turbo
//!    `VectorInsert` record. A crash here leaves the WAL intact and
//!    recovery resumes from the prior checkpoint LSN.
//!
//! This slice (#694) wires the production call sites for points 1–3
//! in the INSERT path and point 4 in the checkpoint accounting loop.
//! #673 attaches the actual kill-point tests against these names.

/// Named boundary in the durable-write / checkpoint pipeline. Stable
/// public contract: variant names appear in #673's test assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InjectionPoint {
    BeforeWalFsync,
    BeforeIndexCommit,
    BeforeExtentFsync,
    MidCheckpoint,
}

/// Test-only contract for observing the named boundaries.
/// Implementations may panic or longjmp to simulate a process crash.
#[cfg(any(test, feature = "turbo-crash-inject"))]
pub trait TurboCrashInjector: Send + Sync {
    fn before(&self, point: InjectionPoint);
}

#[cfg(any(test, feature = "turbo-crash-inject"))]
mod test_only {
    use super::*;
    use std::sync::Arc;
    use std::sync::RwLock;

    static INJECTOR: RwLock<Option<Arc<dyn TurboCrashInjector>>> = RwLock::new(None);

    /// Install a process-global injector. Test-only / feature-gated.
    /// Returns the previously installed injector, if any.
    pub fn install(
        injector: Option<Arc<dyn TurboCrashInjector>>,
    ) -> Option<Arc<dyn TurboCrashInjector>> {
        let mut guard = INJECTOR.write().expect("turbo injector lock");
        std::mem::replace(&mut *guard, injector)
    }

    pub(crate) fn current() -> Option<Arc<dyn TurboCrashInjector>> {
        INJECTOR.read().ok().and_then(|g| g.clone())
    }
}

#[cfg(any(test, feature = "turbo-crash-inject"))]
pub use test_only::install;

/// Fire a named injection point. No-op in production builds.
#[inline]
pub fn fire(point: InjectionPoint) {
    #[cfg(any(test, feature = "turbo-crash-inject"))]
    {
        if let Some(injector) = test_only::current() {
            injector.before(point);
        }
    }
    #[cfg(not(any(test, feature = "turbo-crash-inject")))]
    {
        let _ = point;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct Counter {
        hits: AtomicUsize,
        target: InjectionPoint,
    }

    impl TurboCrashInjector for Counter {
        fn before(&self, point: InjectionPoint) {
            if point == self.target {
                self.hits.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    // The injector slot is process-global; tests that touch it must
    // serialize against each other to avoid stomping on each other's
    // installed state.
    fn injector_test_lock() -> &'static Mutex<()> {
        static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn fire_invokes_installed_injector_at_named_point() {
        let _guard = injector_test_lock().lock().unwrap();
        let counter = Arc::new(Counter {
            hits: AtomicUsize::new(0),
            target: InjectionPoint::BeforeIndexCommit,
        });
        let prev = install(Some(counter.clone()));
        fire(InjectionPoint::BeforeWalFsync);
        fire(InjectionPoint::BeforeIndexCommit);
        fire(InjectionPoint::BeforeIndexCommit);
        fire(InjectionPoint::BeforeExtentFsync);
        fire(InjectionPoint::MidCheckpoint);
        assert_eq!(counter.hits.load(Ordering::Relaxed), 2);
        let _ = install(prev);
    }

    #[test]
    fn fire_without_installed_injector_is_a_no_op() {
        let _guard = injector_test_lock().lock().unwrap();
        let prev = install(None);
        fire(InjectionPoint::BeforeWalFsync);
        fire(InjectionPoint::MidCheckpoint);
        let _ = install(prev);
    }
}
