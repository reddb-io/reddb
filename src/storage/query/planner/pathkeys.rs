//! Pathkey tracking — Fase 4 P3 companion module.
//!
//! A `PathKey` describes an output-order guarantee that a plan
//! operator makes to its consumer: "the rows coming out of this
//! operator are sorted by column X ascending". The planner
//! consumes pathkeys to pick optimisations like **incremental
//! sort** (`sort.rs::incremental_sort_top_k`) and **merge
//! join**: if a scan already returns rows in the order a sort
//! needs, the sort collapses to a truncate or a group-by-prefix
//! walk.
//!
//! Mirrors PG's `src/backend/optimizer/path/pathkeys.c` with
//! three simplifications:
//!
//! - **No equivalence classes**: PG tracks equivalent columns
//!   via `EquivalenceClass` so `a.id = b.id` lets a sort on
//!   `a.id` satisfy a requirement for `b.id`. We track pathkeys
//!   per-column with no cross-relation equivalence today.
//! - **No volatility class**: PG splits pathkeys by volatile
//!   function boundaries. reddb scalar functions are Volatile
//!   or Scalar (see function_catalog::FunctionKind) and the
//!   planner walks each operator node individually, so we
//!   don't need a separate class marker.
//! - **No shared costs**: PG dedupes pathkeys across plans so
//!   the DP can compare them by identity. Ours are cheap
//!   Vec<PathKey> copies — optimize when the planner spends
//!   measurable time hashing them.
//!
//! This module is **not yet wired** into the planner. Wiring
//! plugs into `planner/logical.rs` when a plan operator
//! declares the order of its output — scans declare index
//! order, merge-joins combine left+right pathkeys, sorts emit
//! exactly their sort keys, and so on.

use crate::storage::query::ast::FieldRef;

/// Direction component of a pathkey — matches `OrderBy::Direction`
/// but intentionally decoupled so the planner can reason about
/// pathkeys without touching AST types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKeyDirection {
    Ascending,
    Descending,
}

/// Null placement within a pathkey — ascending pathkeys
/// typically place nulls last; descending typically first.
/// Stored explicitly because SQL's `NULLS FIRST` / `NULLS LAST`
/// override the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKeyNulls {
    First,
    Last,
}

/// One column in an ordering guarantee. A `PathKeys` is a
/// Vec<PathKey> representing a lexicographic ordering — the
/// first pathkey dominates, the next breaks ties, and so on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathKey {
    pub field: FieldRef,
    pub direction: PathKeyDirection,
    pub nulls: PathKeyNulls,
}

impl PathKey {
    /// Create an ascending pathkey with NULLS LAST (the PG
    /// default for ASC). Convenience constructor used by
    /// `IndexScan` when the index is ascending.
    pub fn asc(field: FieldRef) -> Self {
        Self {
            field,
            direction: PathKeyDirection::Ascending,
            nulls: PathKeyNulls::Last,
        }
    }

    /// Create a descending pathkey with NULLS FIRST.
    pub fn desc(field: FieldRef) -> Self {
        Self {
            field,
            direction: PathKeyDirection::Descending,
            nulls: PathKeyNulls::First,
        }
    }

    /// Returns true when two pathkeys describe the same column
    /// in the same direction. Null placement is NOT compared —
    /// two pathkeys on the same column are equivalent for
    /// incremental-sort purposes regardless of null placement
    /// because sort stability handles the null edge case.
    pub fn same_column_and_direction(&self, other: &PathKey) -> bool {
        self.field == other.field && self.direction == other.direction
    }
}

/// A list of pathkeys describing a plan operator's output
/// order. The order is lexicographic: `keys[0]` is the primary
/// sort key, `keys[1]` breaks ties, etc.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathKeys {
    pub keys: Vec<PathKey>,
}

impl PathKeys {
    /// Empty pathkeys — the plan operator makes no order
    /// guarantee.
    pub fn unordered() -> Self {
        Self { keys: Vec::new() }
    }

    /// Single-key pathkeys ordered by `field` ascending.
    pub fn asc(field: FieldRef) -> Self {
        Self {
            keys: vec![PathKey::asc(field)],
        }
    }

    /// Single-key pathkeys ordered by `field` descending.
    pub fn desc(field: FieldRef) -> Self {
        Self {
            keys: vec![PathKey::desc(field)],
        }
    }

    /// True when the operator emits rows with no guaranteed
    /// order. Consumers that need a specific order must add a
    /// Sort node above.
    pub fn is_unordered(&self) -> bool {
        self.keys.is_empty()
    }

    /// Returns the number of leading pathkeys that match
    /// `required`. Used by the planner to decide whether to
    /// emit an incremental sort: if `prefix_match` returns
    /// ≥ 1, the input already satisfies a prefix of the
    /// required order and an incremental sort is cheaper
    /// than a full sort.
    ///
    /// Example:
    /// - self:     `[a ASC, b ASC]`
    /// - required: `[a ASC, b ASC, c DESC]`
    /// - returns:  `2`
    ///
    /// - self:     `[a ASC]`
    /// - required: `[b ASC, a ASC]`
    /// - returns:  `0` (prefix doesn't match)
    pub fn prefix_match(&self, required: &PathKeys) -> usize {
        let mut matched = 0;
        for (mine, req) in self.keys.iter().zip(required.keys.iter()) {
            if mine.same_column_and_direction(req) {
                matched += 1;
            } else {
                break;
            }
        }
        matched
    }

    /// True when `self` fully satisfies `required` — i.e.
    /// `self.prefix_match(required) == required.keys.len()`
    /// AND `self` has at least as many keys as `required`.
    /// Callers use this to decide whether a Sort node can be
    /// elided entirely.
    pub fn satisfies(&self, required: &PathKeys) -> bool {
        if required.keys.len() > self.keys.len() {
            return false;
        }
        self.prefix_match(required) == required.keys.len()
    }

    /// Append a pathkey, returning a new `PathKeys` with one
    /// more column at the end. Used by the planner when
    /// composing pathkeys from an index scan + a tiebreaker.
    pub fn appended(&self, key: PathKey) -> Self {
        let mut keys = self.keys.clone();
        keys.push(key);
        Self { keys }
    }

    /// Truncate to the first `n` keys. Used when a plan
    /// operator only preserves a prefix of its input's order
    /// (e.g. hash aggregation destroys ordering beyond the
    /// grouping columns).
    pub fn truncated(&self, n: usize) -> Self {
        Self {
            keys: self.keys.iter().take(n).cloned().collect(),
        }
    }

    /// Number of pathkeys in the list.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Is the pathkey list empty?
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Strategy hint returned by `plan_sort` — tells the runtime
/// whether to use a full sort, an incremental sort, or skip
/// the sort operator entirely because the input is already
/// sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortStrategy {
    /// Input is already in the required order — no sort
    /// needed. The planner can elide the Sort node.
    None,
    /// Input matches a prefix of the required order. Use
    /// `incremental_sort_top_k` with the matching prefix
    /// length.
    Incremental {
        prefix_len: usize,
    },
    /// Input has no relevant order. Full sort required.
    Full,
}

/// Decide how to sort an input with given `input_order` to
/// satisfy `required_order`. Returns a strategy hint the
/// planner can convert into a plan node.
pub fn plan_sort(input_order: &PathKeys, required_order: &PathKeys) -> SortStrategy {
    if required_order.is_unordered() {
        return SortStrategy::None;
    }
    if input_order.satisfies(required_order) {
        return SortStrategy::None;
    }
    let prefix = input_order.prefix_match(required_order);
    if prefix == 0 {
        return SortStrategy::Full;
    }
    SortStrategy::Incremental { prefix_len: prefix }
}
