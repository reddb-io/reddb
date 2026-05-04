//! MVCC visibility predicate — the deep module that owns the entire
//! "does snapshot S see entity E?" decision.
//!
//! Pure functions, no I/O, no locks, no allocations. Lifting the rule
//! out of `SnapshotManager`/`Snapshot` lets the correctness story stand
//! on its own and be unit-tested in isolation. Future MVCC tweaks
//! (predicate locks, hot-row pruning, vacuum-aware reads) all funnel
//! through `is_visible` so behaviour change shows up here first.
//!
//! # Visibility rules
//!
//! Given a row stamped `(xmin, xmax)` and a reader snapshot
//! `(snapshot_xid, in_progress, aborted)`:
//!
//! 1. **Pre-MVCC rows** (`xmin == XID_NONE`) are visible to every
//!    snapshot. They predate transaction tracking and stay visible
//!    forever unless explicitly deleted.
//! 2. **Future writers** (`xmin > snapshot_xid`) are invisible — the
//!    row was created after the snapshot opened.
//! 3. **In-progress writers** (`xmin in in_progress`) are invisible —
//!    the writer hadn't committed when the snapshot opened, even if
//!    it commits later.
//! 4. **Aborted writers** (`xmin in aborted`) are invisible forever —
//!    the writer rolled back, so its row never existed for any future
//!    reader.
//! 5. **Past deleters** (`xmax != XID_NONE && xmax <= snapshot_xid &&
//!    xmax not in in_progress && xmax not in aborted`) hide the row —
//!    the deleter's commit happened before the snapshot opened.
//! 6. **In-progress deleters** keep the row visible — the delete
//!    hasn't committed yet from this snapshot's point of view.
//! 7. **Aborted deleters** keep the row visible — the delete rolled
//!    back, so the row is still alive.
//!
//! Rules 6 and 7 collapse into "ignore xmax unless the deleter
//! actually committed before the snapshot opened".

use std::collections::HashSet;

/// Transaction identifier. Re-exported from
/// [`super::snapshot`] so callers don't have to follow the import
/// chain.
pub type Xid = u64;

/// Reserved sentinel meaning "no transaction" — pre-MVCC rows stamp
/// this on `xmin` so they stay visible to every snapshot, and rows
/// without a tombstone stamp this on `xmax`.
pub const XID_NONE: Xid = 0;

/// The full MVCC visibility predicate.
///
/// Returns `true` iff a row stamped `(xmin, xmax)` should be returned
/// by a reader holding a snapshot with the given `snapshot_xid`,
/// `in_progress` writers, and `aborted` writers. See the module
/// header for the rule-by-rule breakdown.
///
/// This function is the single source of truth for visibility. Read
/// paths, vacuum, and replication apply paths all delegate here.
#[inline]
pub fn is_visible(
    xmin: Xid,
    xmax: Xid,
    snapshot_xid: Xid,
    in_progress: &HashSet<Xid>,
    aborted: &HashSet<Xid>,
) -> bool {
    // Rule 1: pre-MVCC rows are always visible (subject to xmax check
    // below). Skip the writer-side checks.
    if xmin != XID_NONE {
        // Rule 2: writer is in the future relative to this snapshot.
        if xmin > snapshot_xid {
            return false;
        }
        // Rule 3: writer was active when the snapshot opened.
        if in_progress.contains(&xmin) {
            return false;
        }
        // Rule 4: writer rolled back — its row never existed.
        if aborted.contains(&xmin) {
            return false;
        }
    }

    // Rules 5/6/7: deletion only counts when the deleter actually
    // committed before the snapshot opened. An in-progress or aborted
    // deleter leaves the row alive.
    if xmax != XID_NONE
        && xmax <= snapshot_xid
        && !in_progress.contains(&xmax)
        && !aborted.contains(&xmax)
    {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty() -> HashSet<Xid> {
        HashSet::new()
    }

    fn set(xs: &[Xid]) -> HashSet<Xid> {
        xs.iter().copied().collect()
    }

    // Rule 1 — pre-MVCC rows.

    #[test]
    fn pre_mvcc_row_visible_to_every_snapshot() {
        for snapshot_xid in [1, 100, 1_000_000] {
            assert!(is_visible(XID_NONE, XID_NONE, snapshot_xid, &empty(), &empty()));
        }
    }

    #[test]
    fn pre_mvcc_row_with_committed_deleter_is_hidden() {
        // xmin = 0 (pre-MVCC), xmax = 5 (deleter committed before snapshot 10).
        assert!(!is_visible(XID_NONE, 5, 10, &empty(), &empty()));
    }

    #[test]
    fn pre_mvcc_row_with_in_progress_deleter_stays_visible() {
        assert!(is_visible(XID_NONE, 5, 10, &set(&[5]), &empty()));
    }

    // Rule 2 — future writers.

    #[test]
    fn future_writer_invisible() {
        // Writer xid 11 > snapshot xid 10.
        assert!(!is_visible(11, XID_NONE, 10, &empty(), &empty()));
    }

    #[test]
    fn writer_at_snapshot_xid_is_visible() {
        // The reader's own writes must be visible (read-your-own-writes).
        assert!(is_visible(10, XID_NONE, 10, &empty(), &empty()));
    }

    // Rule 3 — in-progress writers.

    #[test]
    fn in_progress_writer_invisible() {
        // Writer xid is below the snapshot but the snapshot caught it
        // mid-flight via the in_progress set.
        assert!(!is_visible(5, XID_NONE, 10, &set(&[5]), &empty()));
    }

    #[test]
    fn in_progress_writer_unrelated_to_aborted_check() {
        // A writer can be both `in_progress` (this snapshot's view) and
        // already-committed (later view) — for *this* snapshot, the
        // in_progress flag wins.
        assert!(!is_visible(
            5,
            XID_NONE,
            10,
            &set(&[5]),
            &empty()
        ));
    }

    // Rule 4 — aborted writers.

    #[test]
    fn aborted_writer_never_visible() {
        // Writer rolled back: invisible to every snapshot, regardless of
        // ordering.
        for snapshot_xid in [5, 10, 100] {
            assert!(!is_visible(
                5,
                XID_NONE,
                snapshot_xid,
                &empty(),
                &set(&[5])
            ));
        }
    }

    #[test]
    fn aborted_writer_takes_precedence_over_pre_mvcc_check() {
        // xmin is non-zero AND in the aborted set → invisible even
        // though xmin <= snapshot_xid would otherwise match.
        assert!(!is_visible(3, XID_NONE, 10, &empty(), &set(&[3])));
    }

    // Rule 5 — past committed deleters.

    #[test]
    fn committed_past_deleter_hides_row() {
        // creator 3 (committed), deleter 5 (committed), reader at 10.
        assert!(!is_visible(3, 5, 10, &empty(), &empty()));
    }

    #[test]
    fn future_deleter_keeps_row_visible() {
        // Deleter is in the future relative to snapshot; row must still
        // be visible.
        assert!(is_visible(3, 11, 10, &empty(), &empty()));
    }

    #[test]
    fn deleter_at_snapshot_xid_hides_row() {
        // The reader's own delete is visible to itself (read-your-own-
        // writes consistency on deletes).
        assert!(!is_visible(3, 10, 10, &empty(), &empty()));
    }

    // Rule 6 — in-progress deleter.

    #[test]
    fn in_progress_deleter_keeps_row_visible() {
        // Deleter started but hasn't committed yet → row still alive
        // from this snapshot's POV.
        assert!(is_visible(3, 5, 10, &set(&[5]), &empty()));
    }

    // Rule 7 — aborted deleter.

    #[test]
    fn aborted_deleter_keeps_row_visible() {
        // Delete rolled back → row still alive.
        assert!(is_visible(3, 5, 10, &empty(), &set(&[5])));
    }

    // Composite scenarios.

    #[test]
    fn aborted_writer_with_aborted_deleter_still_invisible() {
        // The writer never existed in this snapshot's view — deleter
        // status doesn't change that.
        assert!(!is_visible(3, 5, 10, &empty(), &set(&[3, 5])));
    }

    #[test]
    fn in_progress_writer_with_in_progress_deleter_still_invisible() {
        // Writer-side rule fires first: row's creator is mid-flight.
        assert!(!is_visible(3, 5, 10, &set(&[3, 5]), &empty()));
    }

    #[test]
    fn writer_committed_deleter_aborted_visible() {
        // Common after a rolled-back DELETE: row's creator is fine,
        // delete never happened.
        assert!(is_visible(3, 5, 10, &empty(), &set(&[5])));
    }

    #[test]
    fn writer_committed_deleter_in_future_visible() {
        // The delete hasn't been issued yet from snapshot's POV.
        assert!(is_visible(3, 50, 10, &empty(), &empty()));
    }

    // Edge cases.

    #[test]
    fn snapshot_xid_zero_pre_mvcc_only() {
        // A snapshot with xid 0 has not allocated yet — should still
        // see pre-MVCC rows but no MVCC-stamped rows.
        assert!(is_visible(XID_NONE, XID_NONE, 0, &empty(), &empty()));
        assert!(!is_visible(1, XID_NONE, 0, &empty(), &empty()));
    }

    #[test]
    fn very_large_xids_still_compare_correctly() {
        // Sanity check that `>`/`<=` on u64 do the right thing near the
        // top of the range.
        let big = u64::MAX - 10;
        assert!(is_visible(big, XID_NONE, big, &empty(), &empty()));
        assert!(!is_visible(big + 1, XID_NONE, big, &empty(), &empty()));
    }

    #[test]
    fn empty_in_progress_and_aborted_sets_are_safe() {
        // The hot path is small set lookups; make sure empty sets do
        // not panic and do not falsely match anything.
        assert!(is_visible(5, XID_NONE, 10, &empty(), &empty()));
    }
}
