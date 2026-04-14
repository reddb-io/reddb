//! Join reordering via dynamic programming — Fase 5 P7 building
//! block.
//!
//! Implements the classic Selinger-style DP join ordering
//! algorithm: enumerate every possible left-deep join tree
//! over `n` relations using a `2^n`-sized DP table, score each
//! candidate via the cost model, and return the cheapest plan.
//!
//! Mirrors PG's `joinrels.c::join_search_one_level` modulo:
//!
//! - **No bushy trees**: PG emits both left-deep and bushy join
//!   trees and picks the best. We only emit left-deep for
//!   simplicity — bushy adds another `2^n` to the search space.
//! - **No worst-case fallback**: PG falls back to the GEQO
//!   genetic algorithm when n > 12 (the DP table grows past
//!   ~4 K entries). We hard-cap at `MAX_DP_RELATIONS = 10` and
//!   refuse to plan larger joins via this module — the caller
//!   keeps the legacy heuristic order for those.
//! - **No correlation tracking**: PG threads `RelOptInfo` through
//!   the algorithm with selectivity, distinct values, and
//!   referenced columns. We just track row count + estimated
//!   rows-after-join, computed via the cost model.
//!
//! The module is **not yet wired** into the planner. Wiring
//! plugs into `optimizer.rs::reorder_join_inputs` once that
//! function exists; for now this module is callable from tests
//! and benchmarks only.
//!
//! ## Algorithm
//!
//! Given relations `R0, R1, …, Rn-1` and a join graph that
//! says which pairs share a join predicate:
//!
//! 1. Initialize `dp[singleton(i)] = (R_i.row_count, [i])` for
//!    every relation — base case is each relation by itself.
//! 2. For each subset `S` of size 2..n, ordered by ascending
//!    size:
//!    a. For every way to split `S = L ∪ R` with L and R
//!       non-empty and `dp[L]` and `dp[R]` already computed:
//!       - Verify the split has at least one join predicate
//!         (the join graph). Cartesian products without a
//!         predicate are filtered out unless every other
//!         split is also missing one (handle disconnected
//!         graphs gracefully).
//!       - Compute the cost of joining `L` with `R` using
//!         the cost model.
//!       - If this beats `dp[S]`, replace.
//! 3. The final answer is `dp[full_set]`.
//!
//! Complexity: `O(3^n)` time, `O(2^n)` space. Acceptable for
//! n ≤ 10 (59,049 candidates, ~1024 DP entries).

use std::collections::HashMap;

/// Index into the caller's relation list. The DP works with
/// abstract `RelId`s so the planner can plug arbitrary backing
/// types (tables, subqueries, materialised CTEs).
pub type RelId = u8;

/// Bitmask over `MAX_DP_RELATIONS` relations. Bit `i` set means
/// relation `i` is present in the subset.
type RelMask = u16;

/// Hard cap on the number of relations this DP can handle. Joins
/// over more relations fall back to the legacy heuristic
/// reorderer in `optimizer.rs`.
pub const MAX_DP_RELATIONS: usize = 10;

/// Estimated cardinality (number of rows) after some operation.
pub type Cardinality = f64;

/// Estimated cost (CPU + I/O units) of executing some operation.
pub type Cost = f64;

/// Per-relation statistics fed into the DP. The caller computes
/// these from the existing `CostEstimator` / `StatsProvider`
/// pipeline before invoking `reorder`.
#[derive(Debug, Clone, Copy)]
pub struct RelStats {
    pub id: RelId,
    pub row_count: Cardinality,
}

/// One edge in the join graph — a predicate that constrains two
/// relations to satisfy `left.col = right.col` (or any other
/// equi-join shape). The DP only cares that the edge exists,
/// not the specific columns; selectivity is supplied separately.
#[derive(Debug, Clone, Copy)]
pub struct JoinEdge {
    pub left: RelId,
    pub right: RelId,
    /// Selectivity in `[0, 1]` — fraction of the Cartesian
    /// product surviving the join. 0.01 is a typical equi-join
    /// on a non-unique column; 0.0001 means very selective.
    pub selectivity: f64,
}

/// One DP cell: the cheapest plan for joining a specific subset.
#[derive(Debug, Clone)]
pub struct DpEntry {
    /// Subset of relations covered by this entry, as a bitmask.
    pub mask: RelMask,
    /// Estimated rows produced by this join order.
    pub rows: Cardinality,
    /// Estimated cost to materialise these rows.
    pub cost: Cost,
    /// The join order as a left-deep sequence of RelId values.
    pub order: Vec<RelId>,
}

/// Planner errors raised by the DP.
#[derive(Debug)]
pub enum DpError {
    /// Caller passed more relations than the DP supports.
    TooManyRelations { count: usize, max: usize },
    /// The join graph is disconnected — no edge between any
    /// pair of relations across some split. The caller should
    /// fall back to a Cartesian-product-tolerant ordering.
    Disconnected,
    /// Caller passed an empty relation list.
    Empty,
}

impl std::fmt::Display for DpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyRelations { count, max } => {
                write!(f, "join DP only supports up to {max} relations, got {count}")
            }
            Self::Disconnected => write!(f, "join graph is disconnected"),
            Self::Empty => write!(f, "join DP requires at least one relation"),
        }
    }
}

impl std::error::Error for DpError {}

/// Find the cheapest left-deep join order for the given
/// relations and join graph. Returns the final DP entry whose
/// `order` field contains the relation IDs in execution order
/// (leftmost = build / outer side, rightmost = probe / inner
/// side of the last join).
pub fn reorder(
    rels: &[RelStats],
    edges: &[JoinEdge],
) -> Result<DpEntry, DpError> {
    if rels.is_empty() {
        return Err(DpError::Empty);
    }
    if rels.len() > MAX_DP_RELATIONS {
        return Err(DpError::TooManyRelations {
            count: rels.len(),
            max: MAX_DP_RELATIONS,
        });
    }

    // Index relations by bit position so RelId values map to
    // bitmask offsets. The caller's RelId values may be sparse
    // or out-of-order; we re-key here.
    let mut by_position: Vec<&RelStats> = rels.iter().collect();
    by_position.sort_by_key(|r| r.id);
    let positions: HashMap<RelId, usize> = by_position
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id, i))
        .collect();
    let n = by_position.len();
    let full_mask: RelMask = ((1u32 << n) - 1) as RelMask;

    // Build adjacency lookup: for any pair (l, r) of bit
    // positions, what's the join selectivity? Missing pair
    // means no predicate (Cartesian product).
    let mut adj: HashMap<(usize, usize), f64> = HashMap::new();
    for edge in edges {
        let Some(&l) = positions.get(&edge.left) else {
            continue;
        };
        let Some(&r) = positions.get(&edge.right) else {
            continue;
        };
        adj.insert((l, r), edge.selectivity);
        adj.insert((r, l), edge.selectivity);
    }

    // DP table: maskmap from RelMask → cheapest DpEntry.
    let mut dp: HashMap<RelMask, DpEntry> = HashMap::with_capacity(1 << n);

    // Base case: each relation alone.
    for (i, rel) in by_position.iter().enumerate() {
        let mask: RelMask = 1 << i;
        dp.insert(
            mask,
            DpEntry {
                mask,
                rows: rel.row_count,
                cost: rel.row_count,
                order: vec![rel.id],
            },
        );
    }

    // Fill DP for subsets of increasing size.
    for size in 2..=n {
        for mask in subsets_of_size(full_mask, size) {
            let mut best: Option<DpEntry> = None;
            // Enumerate every non-trivial split of `mask` into
            // (left, right). Iterating the bits of `mask` and
            // walking submasks of `mask` yields every legal pair
            // (left, right) with left ⊆ mask, right = mask ^ left,
            // both non-empty.
            let mut left: RelMask = (mask - 1) & mask;
            while left > 0 {
                let right: RelMask = mask ^ left;
                // Avoid duplicate work: only consider left < right
                // (left-deep DP standard trick).
                if left < right || left.count_ones() < right.count_ones() {
                    if let (Some(l_entry), Some(r_entry)) = (dp.get(&left), dp.get(&right)) {
                        if let Some(candidate) =
                            cost_join(l_entry, r_entry, &adj, &positions, by_position.as_slice())
                        {
                            match &best {
                                None => best = Some(candidate),
                                Some(prev) if candidate.cost < prev.cost => {
                                    best = Some(candidate);
                                }
                                _ => {}
                            }
                        }
                    }
                }
                left = (left - 1) & mask;
            }
            if let Some(entry) = best {
                dp.insert(mask, entry);
            }
        }
    }

    dp.remove(&full_mask).ok_or(DpError::Disconnected)
}

/// Compute the cost of joining two DP entries. Returns `None`
/// when the join would be a Cartesian product (no predicate
/// connects any pair across the split) AND the caller's join
/// graph isn't fully disconnected — we prefer ordered joins
/// over Cartesian products and only emit the latter when
/// nothing else is possible.
fn cost_join(
    left: &DpEntry,
    right: &DpEntry,
    adj: &HashMap<(usize, usize), f64>,
    positions: &HashMap<RelId, usize>,
    rels: &[&RelStats],
) -> Option<DpEntry> {
    // Find the strongest predicate connecting any pair across
    // the split. We use min-selectivity (most selective edge)
    // as the join's effective filter.
    let left_positions: Vec<usize> = mask_to_positions(left.mask);
    let right_positions: Vec<usize> = mask_to_positions(right.mask);
    let mut min_selectivity: Option<f64> = None;
    for l in &left_positions {
        for r in &right_positions {
            if let Some(&sel) = adj.get(&(*l, *r)) {
                min_selectivity = Some(min_selectivity.map_or(sel, |m| m.min(sel)));
            }
        }
    }

    // Estimate output rows: Cartesian product * predicate
    // selectivity. Without a predicate, fall back to 1.0 (full
    // Cartesian) — the planner accepts this only when no
    // predicate-bearing alternative exists.
    let selectivity = min_selectivity.unwrap_or(1.0);
    let out_rows = left.rows * right.rows * selectivity;

    // Cost model: hash-join cost ≈ build_side + probe_side +
    // output. Pick the smaller side for build (standard hash
    // join optimisation).
    let build = left.rows.min(right.rows);
    let probe = left.rows.max(right.rows);
    let join_cost = build * 1.5 + probe + out_rows;
    let total_cost = left.cost + right.cost + join_cost;

    // Build the merged order. Left-deep convention: if `left`
    // already represents a join chain, append `right`'s
    // relations to the end.
    let mut order = left.order.clone();
    order.extend(&right.order);

    // Skip plain Cartesian products when an alternative exists.
    // The DP framework calls this for every split; the caller
    // discards the entry by passing None when min_selectivity is
    // missing AND the split contains more than one relation on
    // each side. Single-relation legs are always allowed.
    if min_selectivity.is_none() && left_positions.len() > 0 && right_positions.len() > 0 {
        // If both sides are singletons, allow the Cartesian
        // (the user's join graph might genuinely be empty).
        // Otherwise reject — the DP will pick a different split.
        if left_positions.len() > 1 || right_positions.len() > 1 {
            return None;
        }
    }

    let _ = (positions, rels); // currently unused — reserved for selectivity refinement

    Some(DpEntry {
        mask: left.mask | right.mask,
        rows: out_rows,
        cost: total_cost,
        order,
    })
}

/// Yield every subset of `universe` with exactly `k` bits set.
/// Used by the DP main loop to walk subsets in size order.
/// Iterative — generates ~`C(n, k)` masks per call.
fn subsets_of_size(universe: RelMask, k: usize) -> Vec<RelMask> {
    let n = (universe.count_ones() as usize).max(k);
    let mut out = Vec::new();
    for mask in 1..=universe {
        if (mask as RelMask) & universe == mask as RelMask
            && (mask as RelMask).count_ones() as usize == k
        {
            out.push(mask as RelMask);
        }
        let _ = n;
    }
    out
}

/// Convert a bitmask back into a list of bit positions. Used by
/// `cost_join` to walk pairs across a split.
fn mask_to_positions(mask: RelMask) -> Vec<usize> {
    let mut out = Vec::with_capacity(mask.count_ones() as usize);
    let mut m = mask;
    let mut pos = 0;
    while m > 0 {
        if m & 1 == 1 {
            out.push(pos);
        }
        m >>= 1;
        pos += 1;
    }
    out
}
