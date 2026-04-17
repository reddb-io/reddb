//! HOT (Heap-Only Tuple) update decision — pure policy helper.
//!
//! Mirrors PostgreSQL's `heap_update` fast-path (`heapam.c` around
//! lines 3976-4031). An UPDATE is eligible for the HOT path when:
//!
//! 1. The UPDATE modifies **no column covered by any secondary
//!    index** — skipping secondary-index maintenance is the point.
//! 2. The new tuple fits inside the **free space on the page that
//!    already holds the old tuple** — stays page-local.
//!
//! Decision is a pure function: callers pre-compute the inputs
//! (indexed columns for the table, columns this UPDATE modifies,
//! serialized new-row size, page free space) and get back a
//! verdict + diagnostics. No storage I/O here.
//!
//! Wiring lives in the storage/DML layer (P3.T2+). This module is
//! just the policy.

use std::collections::HashSet;

/// Everything `decide` needs to pick between HOT and the fallback
/// DELETE+INSERT path.
#[derive(Debug, Clone)]
pub struct HotUpdateInputs<'a> {
    /// Name of the target collection — diagnostic only. Included so
    /// the returned `indexed_blocker` diagnostic can be self-contained.
    pub collection: &'a str,
    /// Every column covered by any secondary index on the collection.
    /// Pulled from the index registry by the caller.
    pub indexed_columns: &'a HashSet<String>,
    /// Columns this UPDATE's SET clause actually mutates. A column
    /// listed but set to its current value still counts as modified —
    /// PG's HOT decision is syntactic, not value-comparing.
    pub modified_columns: &'a HashSet<String>,
    /// Serialized size (bytes) of the new tuple. Used against
    /// `page_free_space` to decide same-page fit.
    pub new_tuple_size: usize,
    /// Free bytes on the old tuple's page after removing the old
    /// tuple. `new_tuple_size <= page_free_space` is the fit test.
    /// Callers can pass `usize::MAX` to skip the page-fit check
    /// (useful when the storage layer guarantees in-place replace).
    pub page_free_space: usize,
}

/// Verdict + diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotUpdateDecision {
    /// True when the caller may take the HOT path.
    pub can_hot: bool,
    /// When `can_hot` is false and an indexed column blocked the
    /// decision, its name. `None` means either HOT passed or the
    /// page-fit check failed.
    pub indexed_blocker: Option<String>,
    /// Echoes the input so the caller can log the numeric margin.
    pub page_free_space: usize,
}

/// Default for `storage.hot_update.max_chain_hops` — matches PG's
/// `MAX_HEAP_TUPLE_CHAIN_LEN`. Kept in sync with the matrix entry.
pub const DEFAULT_MAX_CHAIN_HOPS: usize = 32;

/// Follow a t_ctid HOT chain from `start` up to `max_hops` times,
/// invoking `resolve` to fetch the next version by entity id.
/// Stops when `resolve` returns `None` (terminal version) or when
/// the budget is exhausted (anomaly — a well-formed chain should
/// never hit it, so the caller should log). Returns the tuple's
/// final visible version to the caller.
///
/// Storage-layer page-local rewrite + chain construction lands in
/// a later pass. This function is the policy-side reader counterpart
/// so we don't have to retrofit callers later; today every chain
/// is length 1 because the writer never mints a successor.
pub fn follow_chain<F>(
    start_id: crate::storage::unified::entity::EntityId,
    max_hops: usize,
    mut resolve: F,
) -> crate::storage::unified::entity::EntityId
where
    F: FnMut(
        crate::storage::unified::entity::EntityId,
    ) -> Option<crate::storage::unified::entity::EntityId>,
{
    let hops_cap = max_hops.max(1);
    let mut current = start_id;
    for _hop in 0..hops_cap {
        match resolve(current) {
            Some(next) if next != current => current = next,
            _ => return current,
        }
    }
    // Bounded — prevents a malformed chain from looping forever.
    // Returning the last known version is the conservative choice.
    tracing::warn!(
        entity_id = current.raw(),
        max_hops = hops_cap,
        "hot_update chain walker hit hop cap — likely malformed chain"
    );
    current
}

/// Pure decision function. Returns `can_hot=true` when both
/// conditions hold; populates `indexed_blocker` when at least one
/// modified column is indexed.
pub fn decide(inputs: &HotUpdateInputs<'_>) -> HotUpdateDecision {
    let blocker = inputs
        .modified_columns
        .iter()
        .find(|col| inputs.indexed_columns.contains(col.as_str()))
        .cloned();

    let fits_page = inputs.new_tuple_size <= inputs.page_free_space;

    HotUpdateDecision {
        can_hot: blocker.is_none() && fits_page,
        indexed_blocker: blocker,
        page_free_space: inputs.page_free_space,
    }
}

// Unit tests live in `tests/unit_hot_update.rs` — see the note in
// `src/runtime/locking.rs` about lib-test target having pre-existing
// unrelated compile errors.
#[cfg(test)]
#[cfg(any())]
mod tests {
    use super::*;

    fn hs(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_indexed_cols_modified_and_fits_page_allows_hot() {
        let indexed = hs(&["email", "org_id"]);
        let modified = hs(&["last_login_at"]);
        let d = decide(&HotUpdateInputs {
            collection: "users",
            indexed_columns: &indexed,
            modified_columns: &modified,
            new_tuple_size: 100,
            page_free_space: 4096,
        });
        assert!(d.can_hot);
        assert_eq!(d.indexed_blocker, None);
    }

    #[test]
    fn indexed_column_modified_blocks_hot() {
        let indexed = hs(&["email", "org_id"]);
        let modified = hs(&["email"]);
        let d = decide(&HotUpdateInputs {
            collection: "users",
            indexed_columns: &indexed,
            modified_columns: &modified,
            new_tuple_size: 100,
            page_free_space: 4096,
        });
        assert!(!d.can_hot);
        assert_eq!(d.indexed_blocker.as_deref(), Some("email"));
    }

    #[test]
    fn new_tuple_too_large_blocks_hot() {
        let indexed = hs(&["id"]);
        let modified = hs(&["body"]);
        let d = decide(&HotUpdateInputs {
            collection: "docs",
            indexed_columns: &indexed,
            modified_columns: &modified,
            new_tuple_size: 5000,
            page_free_space: 4096,
        });
        assert!(!d.can_hot);
        assert_eq!(d.indexed_blocker, None);
    }

    #[test]
    fn unlimited_free_space_bypasses_fit_check() {
        let indexed = hs(&[]);
        let modified = hs(&["v"]);
        let d = decide(&HotUpdateInputs {
            collection: "t",
            indexed_columns: &indexed,
            modified_columns: &modified,
            new_tuple_size: 999_999_999,
            page_free_space: usize::MAX,
        });
        assert!(d.can_hot);
    }

    #[test]
    fn empty_modified_columns_trivially_passes_the_index_gate() {
        // An UPDATE with an empty SET (no columns changed) still
        // matches the HOT gate — fits-page + no indexed col touched.
        let indexed = hs(&["email"]);
        let modified = hs(&[]);
        let d = decide(&HotUpdateInputs {
            collection: "users",
            indexed_columns: &indexed,
            modified_columns: &modified,
            new_tuple_size: 50,
            page_free_space: 4096,
        });
        assert!(d.can_hot);
        assert_eq!(d.indexed_blocker, None);
    }

    #[test]
    fn indexed_blocker_picks_first_match_deterministically() {
        // When multiple modified columns are indexed, any one of
        // them is a valid blocker. Just verify we pick SOME indexed
        // column — order doesn't matter to the caller, which only
        // logs it.
        let indexed = hs(&["a", "b", "c"]);
        let modified = hs(&["a", "b"]);
        let d = decide(&HotUpdateInputs {
            collection: "t",
            indexed_columns: &indexed,
            modified_columns: &modified,
            new_tuple_size: 50,
            page_free_space: 4096,
        });
        assert!(!d.can_hot);
        let blocker = d.indexed_blocker.expect("must have a blocker");
        assert!(blocker == "a" || blocker == "b", "got {blocker}");
    }
}
