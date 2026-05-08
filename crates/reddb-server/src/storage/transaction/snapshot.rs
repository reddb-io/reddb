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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use super::coordinator::IsolationLevel;

/// Default autocommit-xid pool batch size. Each refill reserves this
/// many xids in a single `fetch_add` so back-to-back autocommit inserts
/// share one atomic op instead of paying it per row. Sized small to
/// keep a pristine `peek_next_xid()` close to the truth — VACUUM and
/// diagnostics treat reserved-but-unused xids as already-committed.
pub(crate) const AUTOCOMMIT_POOL_BATCH: u64 = 16;

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
    /// Delegates to [`super::visibility::is_visible`] — the deep
    /// module that owns the full MVCC visibility predicate. The
    /// `aborted` argument is empty here because `Snapshot` does not
    /// carry the manager-level aborted set; callers that need the
    /// rolled-back-writer rule should consult [`SnapshotManager`]
    /// directly, or evolve `Snapshot` to embed an aborted view.
    pub fn sees(&self, xmin: Xid, xmax: Xid) -> bool {
        super::visibility::is_visible(xmin, xmax, self.xid, &self.in_progress, &HashSet::new())
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
///
/// # Autocommit xid pool
///
/// Single-row autocommit writes (`MutationEngine::append_one`) need an
/// xid that's "born committed" — they call `begin()` then `commit()`
/// back-to-back before the row is even durable. The pre-commit pool
/// (`autocommit_pool_*`) batches the reservation: one
/// `next_xid.fetch_add(BATCH)` reserves a contiguous range of xids,
/// each handed out via a single atomic without touching the
/// `RwLock<ManagerState>`. Pool xids are never inserted into `active`
/// or `aborted` so they look like already-committed transactions to
/// every snapshot — identical visibility semantics to the legacy
/// `begin()/commit()` pair (which also leaves the xid in neither set).
pub struct SnapshotManager {
    next_xid: AtomicU64,
    state: parking_lot::RwLock<ManagerState>,
    /// Reservation window for the autocommit pool. A single
    /// `parking_lot::Mutex` protects two `u64`s — `next` (next xid to
    /// hand out) and `end` (exclusive upper bound). When `next == end`
    /// the next caller refills by reserving `AUTOCOMMIT_POOL_BATCH`
    /// xids in a single `next_xid.fetch_add`, dropping the lock cost
    /// from one acquire-per-xid (the legacy `begin()`+`commit()` pair)
    /// to one acquire-per-`AUTOCOMMIT_POOL_BATCH` xids. A plain Mutex
    /// is enough here — the critical section is two stores and an
    /// atomic add, and contention is bounded by the writer count.
    autocommit_pool: parking_lot::Mutex<AutocommitPool>,
}

#[derive(Default)]
struct AutocommitPool {
    next: Xid,
    end: Xid,
}

#[derive(Default)]
struct ManagerState {
    /// xids that have started but not yet committed/rolled back.
    active: HashSet<Xid>,
    /// xids that rolled back. `is_visible` MUST treat these as invisible
    /// (the writer never committed). The set is pruned lazily by VACUUM.
    aborted: HashSet<Xid>,
    /// xids that must NOT be reclaimed by VACUUM because some higher-level
    /// object (a VCS commit, a long-lived replica snapshot) still points
    /// at them. Reference-counted so multiple pins coexist; decrementing
    /// to zero removes the entry. `prune_aborted` skips any xid present
    /// here so its row versions stay readable.
    pinned: HashMap<Xid, u32>,
}

impl SnapshotManager {
    pub fn new() -> Self {
        Self {
            // Start at 1 so xid=0 keeps its pre-MVCC "everyone sees it" meaning.
            next_xid: AtomicU64::new(1),
            state: parking_lot::RwLock::new(ManagerState::default()),
            // Pool starts empty — first caller triggers a refill.
            autocommit_pool: parking_lot::Mutex::new(AutocommitPool::default()),
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

    /// Allocate an xid that is *born committed* — for autocommit
    /// callers (`MutationEngine::append_one`) that previously paid two
    /// `state.write()` lock acquisitions per row to insert-then-remove
    /// from the active set.
    ///
    /// The returned xid is never inserted into `active` and never into
    /// `aborted`, which matches the steady state of the legacy
    /// `begin()/commit()` pair when called back-to-back: the xid leaves
    /// the manager's tracking sets unobservably. Concurrent readers
    /// therefore see it as an already-committed transaction once
    /// `xmin <= snapshot.xid`, which is exactly the semantics the
    /// autocommit path needs.
    ///
    /// Implementation: a small reservation pool (`AUTOCOMMIT_POOL_BATCH`
    /// xids) is reserved with one `fetch_add`. Each caller hands itself
    /// the next xid via a single atomic. When the pool drains, the
    /// next caller serialises briefly through `autocommit_pool_refill`
    /// to bump the window, then falls back into the lock-free hot path.
    ///
    /// Durability note: this method does NOT make the row durable —
    /// it only allocates the identifier. The caller must complete the
    /// usual WAL-append + fsync cycle before acknowledging the write.
    /// Pre-allocating the xid is safe because the xid carries no
    /// promise that any row exists; it's just a number for `xmin`.
    pub fn allocate_committed_xid(&self) -> Xid {
        let mut pool = self.autocommit_pool.lock();
        if pool.next >= pool.end {
            // Reserve the next contiguous range. A single
            // `fetch_add(BATCH)` on the global counter — equivalent to
            // BATCH back-to-back `begin()` calls in terms of xid
            // numbering, but with zero `state.write()` traffic.
            let start = self
                .next_xid
                .fetch_add(AUTOCOMMIT_POOL_BATCH, Ordering::Relaxed);
            pool.next = start;
            pool.end = start + AUTOCOMMIT_POOL_BATCH;
        }
        let xid = pool.next;
        pool.next += 1;
        xid
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
    /// on the distinction between "aborted" and "never existed". Pinned
    /// xids are always retained so higher-level references (VCS commits,
    /// replica snapshots) stay readable.
    pub fn prune_aborted(&self, below: Xid) {
        let mut state = self.state.write();
        let ManagerState {
            aborted, pinned, ..
        } = &mut *state;
        aborted.retain(|&x| x >= below || pinned.contains_key(&x));
    }

    /// Pin an xid so its row versions stay reclaim-safe across VACUUM.
    /// Reference-counted — call `unpin` once per `pin` to release.
    pub fn pin(&self, xid: Xid) {
        if xid == XID_NONE {
            return;
        }
        let mut state = self.state.write();
        *state.pinned.entry(xid).or_insert(0) += 1;
    }

    /// Decrement an xid's pin count. At zero it is removed and becomes
    /// VACUUM-eligible again. No-op if the xid was never pinned.
    pub fn unpin(&self, xid: Xid) {
        if xid == XID_NONE {
            return;
        }
        let mut state = self.state.write();
        if let Some(count) = state.pinned.get_mut(&xid) {
            if *count <= 1 {
                state.pinned.remove(&xid);
            } else {
                *count -= 1;
            }
        }
    }

    /// Is this xid currently pinned?
    pub fn is_pinned(&self, xid: Xid) -> bool {
        self.state.read().pinned.contains_key(&xid)
    }

    /// Current pin count for an xid (0 if not pinned). Diagnostic only.
    pub fn pin_count(&self, xid: Xid) -> u32 {
        self.state.read().pinned.get(&xid).copied().unwrap_or(0)
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
    fn pin_blocks_prune_of_aborted_xid() {
        let m = SnapshotManager::new();
        let writer = m.begin();
        m.rollback(writer);
        assert!(m.is_aborted(writer));
        m.pin(writer);
        // Even with a high watermark, pinned xid survives prune.
        m.prune_aborted(writer + 1);
        assert!(m.is_aborted(writer));
        m.unpin(writer);
        m.prune_aborted(writer + 1);
        assert!(!m.is_aborted(writer));
    }

    #[test]
    fn pin_is_reference_counted() {
        let m = SnapshotManager::new();
        let x = m.begin();
        m.pin(x);
        m.pin(x);
        assert_eq!(m.pin_count(x), 2);
        m.unpin(x);
        assert_eq!(m.pin_count(x), 1);
        assert!(m.is_pinned(x));
        m.unpin(x);
        assert_eq!(m.pin_count(x), 0);
        assert!(!m.is_pinned(x));
        // Extra unpin is a no-op.
        m.unpin(x);
        assert_eq!(m.pin_count(x), 0);
    }

    #[test]
    fn pin_xid_none_is_noop() {
        let m = SnapshotManager::new();
        m.pin(XID_NONE);
        assert!(!m.is_pinned(XID_NONE));
        assert_eq!(m.pin_count(XID_NONE), 0);
    }

    #[test]
    fn allocate_committed_xid_is_monotonic_and_unique() {
        let m = SnapshotManager::new();
        let mut seen = HashSet::new();
        let mut last = 0u64;
        // Drive at least three pool refills (BATCH=16 → 50 covers it).
        for _ in 0..50 {
            let x = m.allocate_committed_xid();
            assert!(x > last, "xids must be strictly increasing: {x} > {last}");
            assert!(seen.insert(x), "duplicate xid handed out: {x}");
            last = x;
        }
    }

    #[test]
    fn allocate_committed_xid_skips_active_set() {
        let m = SnapshotManager::new();
        let _x = m.allocate_committed_xid();
        // Pool xids must never appear in the active set — they are
        // born committed. `oldest_active_xid` reflects only `begin()`
        // callers (real BEGIN-wrapped transactions).
        assert_eq!(m.oldest_active_xid(), None);
    }

    #[test]
    fn allocate_committed_xid_visible_to_subsequent_snapshots() {
        let m = SnapshotManager::new();
        let writer = m.allocate_committed_xid();
        let reader = m.begin();
        let snap = m.snapshot(reader);
        // Pool xid must be invisible to in_progress/aborted (it's in
        // neither) and visible because writer < reader. This matches
        // the legacy begin()+commit() pair's visibility exactly.
        assert!(!snap.in_progress.contains(&writer));
        assert!(!m.is_aborted(writer));
        assert!(snap.sees(writer, XID_NONE));
    }

    #[test]
    fn allocate_committed_xid_does_not_block_concurrent_begin() {
        // Smoke test: an open BEGIN-wrapped tx coexists with pool
        // allocation; pool xids end up between the begin and commit
        // without being added to `active`.
        let m = SnapshotManager::new();
        let tx = m.begin();
        let auto1 = m.allocate_committed_xid();
        let auto2 = m.allocate_committed_xid();
        m.commit(tx);
        assert!(tx < auto1 && auto1 < auto2);
        // `active` should be empty after commit.
        assert_eq!(m.oldest_active_xid(), None);
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
