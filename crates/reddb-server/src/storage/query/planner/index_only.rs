//! Index-only scan decision helper — Fase 5 P2.
//!
//! Given a query's projection list and an available index with
//! a covering set of columns, plus the table's visibility map,
//! decide when it's safe to return rows directly from the
//! index without fetching the heap.
//!
//! Index-only scan is safe when ALL of:
//!
//! 1. Every projected column is present in the index (the
//!    index "covers" the projection).
//! 2. Every filter column is present in the index (so the
//!    scan can evaluate the WHERE clause without a heap
//!    fetch for filtering).
//! 3. The target page is marked all-visible in the
//!    visibility map — otherwise the row may have been
//!    updated / deleted since the last vacuum and the
//!    index entry could point at a dead tuple.
//!
//! When (1) and (2) hold but (3) doesn't for some pages,
//! the scan can still use the index-only path for the
//! all-visible pages and fall back to heap fetch for the
//! remainder. PG calls this "partial index-only". We ship
//! the decision primitive today; the execution-layer
//! fallback logic lands in the wiring commit.
//!
//! This module is **not yet wired** into the planner. Wiring
//! plugs into `planner/logical.rs::choose_scan_strategy` when
//! that helper grows an "index-only candidate" check.

use std::collections::HashSet;

/// Decision outcome for a potential index-only scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexOnlyDecision {
    /// Index covers everything and every target page is marked
    /// all-visible — the scan can return rows directly from
    /// the index.
    FullCover,
    /// Index covers the columns but at least one target page
    /// isn't marked all-visible. Scan can mix index-only reads
    /// with heap fallback per-page; the executor must consult
    /// the visibility map at runtime for each candidate.
    PartialCover,
    /// Index doesn't cover the required columns. Must fall
    /// back to a regular index scan with heap fetches.
    NotCovering,
}

/// Information about the index that may be used for an
/// index-only scan. Simpler than the full `IndexStats` /
/// `IndexKind` types so this helper doesn't pull the whole
/// planner dep graph into its scope.
#[derive(Debug, Clone)]
pub struct CoveringIndex {
    pub name: String,
    /// Columns the index physically stores in its leaf entries.
    /// An index that stores only `(user_id, email)` covers the
    /// projection `SELECT email FROM users WHERE user_id = ?`
    /// but not `SELECT email, created_at`.
    pub covered_columns: Vec<String>,
}

impl CoveringIndex {
    /// Returns true when every column in `required` is present
    /// in `covered_columns`. Case-sensitive; the planner should
    /// normalize column names before calling.
    pub fn covers(&self, required: &[String]) -> bool {
        let set: HashSet<&str> = self.covered_columns.iter().map(String::as_str).collect();
        required.iter().all(|c| set.contains(c.as_str()))
    }
}

/// Decide whether an index-only scan is viable for the given
/// query shape. Inputs are intentionally minimal — the
/// planner supplies only what this helper needs so it can be
/// exercised from unit tests without the full optimizer
/// context.
///
/// - `projected` — column names in the SELECT list.
/// - `filter_cols` — column names referenced by the WHERE clause.
/// - `index` — the candidate index.
/// - `all_visible_pages` — fraction in [0.0, 1.0] of target
///   pages that the visibility map reports as all-visible.
///   The planner computes this via `VisibilityMap::all_visible_count
///   / total_pages`.
pub fn decide(
    projected: &[String],
    filter_cols: &[String],
    index: &CoveringIndex,
    all_visible_fraction: f64,
) -> IndexOnlyDecision {
    // Merge projected + filter columns into a single required set.
    let mut required: Vec<String> = Vec::with_capacity(projected.len() + filter_cols.len());
    required.extend(projected.iter().cloned());
    required.extend(filter_cols.iter().cloned());

    if !index.covers(&required) {
        return IndexOnlyDecision::NotCovering;
    }
    // Coverage passes; decide on visibility.
    if all_visible_fraction >= 0.999 {
        IndexOnlyDecision::FullCover
    } else {
        IndexOnlyDecision::PartialCover
    }
}

/// Estimate the speedup factor an index-only scan would give
/// over a regular index scan with heap fetches. Used by the
/// cost model to pick between the two. The formula is simple:
/// 1x when `FullCover`, scaling down with visibility coverage
/// for `PartialCover`, and 1.0 (no speedup) for `NotCovering`.
///
/// PG's actual formula weights the page-hit cost against the
/// buffer-pool hit ratio; we approximate with the visibility
/// fraction since `all_visible ≈ cache_friendly` in practice.
pub fn speedup_factor(decision: IndexOnlyDecision, all_visible_fraction: f64) -> f64 {
    match decision {
        IndexOnlyDecision::NotCovering => 1.0,
        IndexOnlyDecision::FullCover => {
            // 5× is a conservative estimate — PG typically sees
            // 3-10× for well-covered indexes. The planner cost
            // model compares with the raw index-scan cost so
            // the ratio is what matters, not the absolute.
            5.0
        }
        IndexOnlyDecision::PartialCover => {
            // Linear interpolation between the two: 1.0 at
            // 0% all-visible, 5.0 at 100%.
            1.0 + 4.0 * all_visible_fraction.clamp(0.0, 1.0)
        }
    }
}
