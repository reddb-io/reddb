//! MVCC Snapshot Manager (Phase 2.3 PG parity)
//!
//! Allocates monotonic transaction IDs ("xids") and tracks the set of
//! currently-active transactions. Powers the visibility rule used by
//! `UnifiedEntity::is_visible`:
//!
//! ```text
//! xmin == 0 || xmin <= snapshot.xid    AND   xmax == 0 || xmax > snapshot.xid
//! ```
//!
//! For Phase 2.3 the manager is in-process only — no WAL logging of xids,
//! no crash recovery of in-flight transactions. Committed rows become
//! permanently visible because their `xmin` is ≤ every future snapshot.
//! Rolled-back rows keep their `xmin` but are flagged via the
//! `aborted_xids` set, which `is_visible` can consult. Phase 2.3.2 adds
//! the WAL integration; Phase 4 adds full ACID recovery.
//!
//! # Isolation levels
//!
//! * `ReadCommitted` — each statement takes a fresh snapshot. Good enough
//!   for most OLTP; supports non-repeatable reads across statements.
//! * `SnapshotIsolation` — one snapshot per transaction. No read skew
//!   within a transaction; writes conflict on first-committer-wins.
//! * `Serializable` — stricter conflict detection (predicate locks). Not
//!   implemented in Phase 2.3; resolver accepts the mode but downgrades
//!   to SnapshotIsolation semantics with a logged warning.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use super::coordinator::IsolationLevel;

/// A transaction identifier. Monotonic across the lifetime of the process.
pub type Xid = u64;

/// Reserved xid meaning "not inside a transaction" — pre-MVCC rows stamp
/// this value so they stay visible to every snapshot.
pub const XID_NONE: Xid = 0;

/// Immutable snapshot taken at transaction start or statement start.
///
/// Callers evaluate `UnifiedEntity::is_visible(snapshot.xid)` on every
/// row returned from storage to filter out rows created by concurrent
/// transactions that hadn't committed when the snapshot was taken.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The snapshot's xid — every row with `xmin <= xid` created before
    /// the snapshot is visible (assuming `xmax` hasn't passed).
    pub xid: Xid,
    /// Transactions that were still active when the snapshot was taken.
    /// Their writes must be *hidden* even when `xmin <= xid`, because
    /// the writer hadn't committed yet from this snapshot's point of view.
    pub in_progress: HashSet<Xid>,
}

impl Snapshot {
    /// Is a row with this xmin/xmax visible under this snapshot?
    ///
    /// Equivalent to `UnifiedEntity::is_visible` but also filters out
    /// rows whose writer is in the in-progress set.
    pub fn sees(&self, xmin: Xid, xmax: Xid) -> bool {
        if xmin != XID_NONE {
            if xmin > self.xid {
                return false;
            }
            if self.in_progress.contains(&xmin) {
                return false;
            }
        }
        if xmax != XID_NONE && xmax <= self.xid && !self.in_progress.contains(&xmax) {
            return false;
        }
        true
    }
}

/// Per-transaction state tracked on the runtime while BEGIN/COMMIT/ROLLBACK
/// is active. Attached to a connection via `RuntimeInner::tx_contexts`.
#[derive(Debug, Clone)]
pub struct TxnContext {
    pub xid: Xid,
    pub isolation: IsolationLevel,
    /// Snapshot captured at BEGIN (SnapshotIsolation / Serializable) or
    /// refreshed per-statement (ReadCommitted).
    pub snapshot: Snapshot,
    /// Ordered list of `(savepoint_name, sub_xid)` entries (Phase
    /// 2.3.2e savepoints). Each SAVEPOINT pushes a freshly-allocated
    /// xid onto this stack; writes stamp xmin/xmax with the top entry
    /// so ROLLBACK TO SAVEPOINT can mark only those writes as aborted.
    /// RELEASE SAVEPOINT pops the named level plus everything above it
    /// without aborting — the sub-xids keep their effects and commit
    /// together with the parent. Empty stack means "writes use `xid`
    /// directly", matching pre-savepoint behaviour.
    pub savepoints: Vec<(String, Xid)>,
    /// Sub-xids popped by `RELEASE SAVEPOINT` that should still commit
    /// alongside the parent. PG semantics: released subtxns keep their
    /// writes — they're promoted to parent-visible at COMMIT. Stored
    /// separately from `savepoints` so their names are gone (cannot be
    /// rolled back or released again) while their xids remain trackable.
    pub released_sub_xids: Vec<Xid>,
}

impl TxnContext {
    /// Xid new writes in this connection should stamp onto tuples — the
    /// innermost open savepoint, or the parent xid when no savepoint is
    /// active.
    pub fn writer_xid(&self) -> Xid {
        self.savepoints.last().map(|(_, x)| *x).unwrap_or(self.xid)
    }
}

/// Central allocator and liveness tracker.
///
/// Uses an atomic counter for xid allocation and a parking_lot-guarded
/// HashSet for in-progress/aborted bookkeeping. The sets stay small —
/// only unfinished transactions plus a finite rollback history — so a
/// plain HashSet outperforms more complex data structures here.
pub struct SnapshotManager {
    next_xid: AtomicU64,
    state: parking_lot::RwLock<ManagerState>,
}

#[derive(Default)]
struct ManagerState {
    /// xids that have started but not yet committed/rolled back.
    active: HashSet<Xid>,
    /// xids that rolled back. `is_visible` MUST treat these as invisible
    /// (the writer never committed). The set is pruned lazily by VACUUM.
    aborted: HashSet<Xid>,
}

impl SnapshotManager {
    pub fn new() -> Self {
        Self {
            // Start at 1 so xid=0 keeps its pre-MVCC "everyone sees it" meaning.
            next_xid: AtomicU64::new(1),
            state: parking_lot::RwLock::new(ManagerState::default()),
        }
    }

    /// Allocate a new xid and mark it active. Returns the xid for
    /// stamping onto `UnifiedEntity::xmin/xmax`.
    pub fn begin(&self) -> Xid {
        let xid = self.next_xid.fetch_add(1, Ordering::Relaxed);
        self.state.write().active.insert(xid);
        xid
    }

    /// Capture a point-in-time snapshot. Must be called after `begin()`
    /// when using SnapshotIsolation/Serializable. ReadCommitted refreshes
    /// this per statement via the same call.
    pub fn snapshot(&self, xid: Xid) -> Snapshot {
        let state = self.state.read();
        // Active xids other than our own appear as "in-progress" to us.
        let in_progress: HashSet<Xid> =
            state.active.iter().copied().filter(|&x| x != xid).collect();
        Snapshot { xid, in_progress }
    }

    /// Mark a transaction as committed. Its writes become visible to
    /// future snapshots; earlier snapshots keep their own view.
    pub fn commit(&self, xid: Xid) {
        let mut state = self.state.write();
        state.active.remove(&xid);
        // Also clear from aborted set in case of prior rollback_to call
        // that touched this xid (defensive; normally a no-op).
        state.aborted.remove(&xid);
    }

    /// Mark a transaction as rolled back. Its writes MUST stay hidden
    /// from every future read — `is_visible` consults the aborted set
    /// before honouring a row's `xmin`.
    pub fn rollback(&self, xid: Xid) {
        let mut state = self.state.write();
        state.active.remove(&xid);
        state.aborted.insert(xid);
    }

    /// Is this xid known to have rolled back? Called by the read path to
    /// skip tuples whose creator never committed.
    pub fn is_aborted(&self, xid: Xid) -> bool {
        self.state.read().aborted.contains(&xid)
    }

    /// Snapshot of every still-active xid (for VACUUM oldest-active-xid
    /// calculation — any row with `xmax < min(active)` is reclaimable).
    pub fn oldest_active_xid(&self) -> Option<Xid> {
        self.state.read().active.iter().copied().min()
    }

    /// Return the next xid that would be allocated. Useful for diagnostics
    /// and for VACUUM to know the upper bound of aborted-xid retention.
    pub fn peek_next_xid(&self) -> Xid {
        self.next_xid.load(Ordering::Relaxed)
    }

    /// Prune the aborted-xid set. Safe to call once every aborted xid is
    /// below `oldest_active`, which guarantees no live snapshot depends
    /// on the distinction between "aborted" and "never existed".
    pub fn prune_aborted(&self, below: Xid) {
        let mut state = self.state.write();
        state.aborted.retain(|&x| x >= below);
    }
}

impl Default for SnapshotManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xids_are_monotonic() {
        let m = SnapshotManager::new();
        let a = m.begin();
        let b = m.begin();
        let c = m.begin();
        assert!(a < b && b < c);
    }

    #[test]
    fn snapshot_excludes_concurrent_writers() {
        let m = SnapshotManager::new();
        let writer = m.begin();
        let reader = m.begin();
        let snap = m.snapshot(reader);
        // Writer is active from reader's perspective → in_progress set.
        assert!(snap.in_progress.contains(&writer));
        // A row written by `writer` with xmin=writer must be invisible.
        assert!(!snap.sees(writer, XID_NONE));
    }

    #[test]
    fn committed_rows_become_visible() {
        let m = SnapshotManager::new();
        let writer = m.begin();
        m.commit(writer);
        let reader = m.begin();
        let snap = m.snapshot(reader);
        // Row stamped with writer's xid is now visible (writer < reader & committed).
        assert!(snap.sees(writer, XID_NONE));
    }

    #[test]
    fn rolled_back_writers_stay_hidden() {
        let m = SnapshotManager::new();
        let writer = m.begin();
        m.rollback(writer);
        assert!(m.is_aborted(writer));
        // Future callers skip tuples with xmin == writer by also consulting is_aborted.
    }

    #[test]
    fn pre_mvcc_rows_always_visible() {
        let m = SnapshotManager::new();
        let reader = m.begin();
        let snap = m.snapshot(reader);
        assert!(snap.sees(XID_NONE, XID_NONE));
    }

    #[test]
    fn deletion_xmax_respected() {
        let m = SnapshotManager::new();
        let creator = m.begin();
        m.commit(creator);
        let deleter = m.begin();
        m.commit(deleter);
        let reader = m.begin();
        let snap = m.snapshot(reader);
        // Reader opens *after* delete → row must be hidden.
        assert!(!snap.sees(creator, deleter));
    }

    #[test]
    fn oldest_active_is_min_live_xid() {
        let m = SnapshotManager::new();
        let a = m.begin();
        let b = m.begin();
        assert_eq!(m.oldest_active_xid(), Some(a));
        m.commit(a);
        assert_eq!(m.oldest_active_xid(), Some(b));
        m.commit(b);
        assert_eq!(m.oldest_active_xid(), None);
    }
}
