//! Stay-readable re-bootstrap with an atomic dataset swap (issue #837,
//! PRD #819).
//!
//! When a replica must re-bootstrap — discard its current dataset and
//! load a fresh snapshot from the primary — it must not go dark. Read
//! capacity is most precious exactly then, because a re-bootstrap is
//! often triggered *because* another node is already down. [`SwapDb`]
//! keeps the old data fully readable for the entire rebuild and swaps
//! to the fresh dataset in one atomic step at the end.
//!
//! ## The two-state guarantee
//!
//! * **Stay-readable.** Non-causal reads ([`SwapDb::read_noncausal`])
//!   are *always* served from the currently-installed dataset — the
//!   old data throughout the rebuild, the new data after the swap.
//!   They never block and never fail.
//! * **Causal correctness.** While a rebuild is in flight the node's
//!   applied frontier describes data it is *about to throw away*, so a
//!   bookmark read served from it could observe a commit that the
//!   post-swap dataset has not yet reached. [`SwapDb::read_causal`]
//!   therefore refuses ([`RebootstrapInProgress`]) for the duration of
//!   the rebuild; the caller routes that read to a caught-up peer. The
//!   same signal is surfaced on the wire via
//!   [`crate::replication::primary::ReplicaState::rebootstrapping`] so
//!   the *client* routing table excludes the node before the read ever
//!   reaches it.
//!
//! ## Atomicity
//!
//! The installed dataset is an `Arc<D>` behind an `RwLock`. A reader
//! clones the `Arc` under a short read lock and then works against its
//! own handle. [`SwapDb::complete_rebootstrap`] takes the write lock
//! just long enough to replace the pointer. A reader that captured the
//! old `Arc` before the swap keeps observing a complete old dataset;
//! a reader that captures after the swap sees a complete new one.
//! There is no window in which a half-built dataset is visible — the
//! swap publishes the fresh `D` only once it is fully constructed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

/// A causal read was requested while the node is re-bootstrapping.
///
/// The node is intentionally refusing to serve a bookmark read from a
/// dataset it is about to discard. The caller is expected to route the
/// read elsewhere (a caught-up peer, or the primary) — never to treat
/// this as a hard error surfaced to the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebootstrapInProgress;

impl std::fmt::Display for RebootstrapInProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "node is re-bootstrapping; causal read must route to a caught-up peer"
        )
    }
}

impl std::error::Error for RebootstrapInProgress {}

/// A dataset that stays readable across an atomic re-bootstrap swap.
///
/// Generic over the installed dataset `D` so the replication engine,
/// the integration tests, and any future caller share one swap
/// discipline rather than re-implementing the lock dance. `D` is held
/// behind an `Arc`, so a "swap" is a single pointer write and old
/// readers keep their snapshot alive.
pub struct SwapDb<D> {
    /// The currently-installed dataset. Readers clone the `Arc`; the
    /// rebuild replaces the pointer under the write lock.
    current: RwLock<Arc<D>>,
    /// `true` from [`Self::begin_rebootstrap`] until the matching
    /// [`Self::complete_rebootstrap`]. Gates causal reads and is the
    /// value mirrored into the topology advertisement.
    rebootstrapping: AtomicBool,
}

impl<D> SwapDb<D> {
    /// Install `data` as the initial dataset. The node starts *not*
    /// re-bootstrapping — it is serving normally.
    pub fn new(data: D) -> Self {
        Self {
            current: RwLock::new(Arc::new(data)),
            rebootstrapping: AtomicBool::new(false),
        }
    }

    /// `true` while a re-bootstrap is in flight. This is exactly the
    /// value the topology advertiser surfaces as
    /// `ReplicaInfo::rebootstrapping`.
    pub fn is_rebootstrapping(&self) -> bool {
        self.rebootstrapping.load(Ordering::Acquire)
    }

    /// The currently-installed dataset, cloned as an `Arc`. Always
    /// available — this is the stay-readable path. During a rebuild it
    /// returns the *old* data; after [`Self::complete_rebootstrap`] it
    /// returns the new data.
    pub fn snapshot(&self) -> Arc<D> {
        Arc::clone(&self.current.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Serve a non-causal read: always the currently-installed
    /// dataset, rebuild in flight or not. Never blocks on the rebuild,
    /// never fails. Identical to [`Self::snapshot`]; named for intent
    /// at the call site.
    pub fn read_noncausal(&self) -> Arc<D> {
        self.snapshot()
    }

    /// Serve a causal (bookmark) read.
    ///
    /// Returns the installed dataset only when the node is *not*
    /// re-bootstrapping. While a rebuild is in flight it returns
    /// [`RebootstrapInProgress`] so the caller bounces the read to a
    /// caught-up peer — never serving a bookmark from data the node is
    /// about to discard.
    pub fn read_causal(&self) -> Result<Arc<D>, RebootstrapInProgress> {
        if self.is_rebootstrapping() {
            return Err(RebootstrapInProgress);
        }
        Ok(self.snapshot())
    }

    /// Enter the re-bootstrap state. Idempotent: calling it while
    /// already rebuilding is a no-op. The installed dataset is left
    /// untouched, so non-causal reads keep flowing from the old data
    /// while the fresh snapshot loads in the background.
    pub fn begin_rebootstrap(&self) {
        self.rebootstrapping.store(true, Ordering::Release);
    }

    /// Atomically install `fresh` as the new dataset and leave the
    /// re-bootstrap state.
    ///
    /// The pointer swap happens under the write lock; the
    /// `rebootstrapping` flag is cleared only *after* the new dataset
    /// is published, so there is no instant at which the node both
    /// claims to be caught up and still serves the old data to a
    /// causal reader. Returns the previously-installed dataset (the
    /// old `Arc`) so the caller can keep or drop it; outstanding
    /// readers that already cloned it stay valid regardless.
    pub fn complete_rebootstrap(&self, fresh: D) -> Arc<D> {
        let new = Arc::new(fresh);
        let old = {
            let mut guard = self.current.write().unwrap_or_else(|e| e.into_inner());
            std::mem::replace(&mut *guard, new)
        };
        // Publish the new dataset before re-enabling causal reads.
        self.rebootstrapping.store(false, Ordering::Release);
        old
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_noncausal_reads_from_old_data_during_rebuild() {
        let db = SwapDb::new(vec![1, 2, 3]);
        db.begin_rebootstrap();
        assert!(db.is_rebootstrapping());
        // Old data stays readable for non-causal reads.
        assert_eq!(*db.read_noncausal(), vec![1, 2, 3]);
    }

    #[test]
    fn refuses_causal_reads_during_rebuild() {
        let db = SwapDb::new(vec![1, 2, 3]);
        assert!(db.read_causal().is_ok());
        db.begin_rebootstrap();
        assert_eq!(db.read_causal(), Err(RebootstrapInProgress));
    }

    #[test]
    fn swap_replaces_data_and_resumes_causal_reads() {
        let db = SwapDb::new(vec![1, 2, 3]);
        db.begin_rebootstrap();
        let old = db.complete_rebootstrap(vec![9, 9, 9, 9]);
        assert_eq!(*old, vec![1, 2, 3]);
        assert!(!db.is_rebootstrapping());
        // New data is now served on both paths.
        assert_eq!(*db.read_noncausal(), vec![9, 9, 9, 9]);
        assert_eq!(*db.read_causal().expect("causal ok"), vec![9, 9, 9, 9]);
    }

    #[test]
    fn swap_is_atomic_old_reader_keeps_complete_old_dataset() {
        let db = SwapDb::new(vec![1, 2, 3]);
        // Capture the dataset before the swap.
        let pre = db.read_noncausal();
        db.begin_rebootstrap();
        db.complete_rebootstrap(vec![7, 8]);
        // The pre-swap handle still observes the *whole* old dataset —
        // never a torn/half-built view.
        assert_eq!(*pre, vec![1, 2, 3]);
        // A fresh read sees the new dataset.
        assert_eq!(*db.read_noncausal(), vec![7, 8]);
    }

    #[test]
    fn begin_rebootstrap_is_idempotent() {
        let db = SwapDb::new(0u64);
        db.begin_rebootstrap();
        db.begin_rebootstrap();
        assert!(db.is_rebootstrapping());
        db.complete_rebootstrap(42);
        assert!(!db.is_rebootstrapping());
        assert_eq!(*db.snapshot(), 42);
    }

    #[test]
    fn rebuild_then_swap_cycle_can_repeat() {
        let db = SwapDb::new(1u32);
        for n in 2..=5 {
            db.begin_rebootstrap();
            assert!(db.read_causal().is_err());
            db.complete_rebootstrap(n);
            assert_eq!(*db.read_causal().expect("ok"), n);
        }
    }
}
