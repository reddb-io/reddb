//! Partition pruning — given a table's partition spec, a list of
//! child partitions with their bounds, and a simplified view of the
//! WHERE clause, return the subset of partitions that can possibly
//! contain rows matching the filter.
//!
//! This module is deliberately decoupled from the full planner AST:
//! callers distill the relevant WHERE fragment into a [`PrunePredicate`]
//! tree before invoking the pruner. That keeps this unit-testable
//! end-to-end without dragging half the query engine into the test
//! binary, and lets three different entry points (hypertable router,
//! classic `PARTITION BY`, projection dispatcher) share the same
//! pruning rules.
//!
//! **Pruning rules**
//!
//! * RANGE: keep a child when its `[low, high)` interval overlaps the
//!   predicate's derived interval on the partition key column.
//! * LIST: keep a child when its literal values intersect the
//!   predicate's allowed set.
//! * HASH: only equality predicates on the key are actionable —
//!   otherwise every child is kept (nothing to prune safely).
//!
//! When the predicate references columns other than the partition
//! key, the pruner is **conservative**: it keeps every child. That
//! preserves correctness at the cost of missing some opportunities —
//! exactly the Timescale / Postgres contract.

use std::ops::Bound;

/// Simplified predicate tree the planner hands to the pruner. Only
/// the shapes we can actually act on are named; anything else the
/// planner tags `Opaque`, which keeps every partition.
#[derive(Debug, Clone)]
pub enum PrunePredicate {
    And(Vec<PrunePredicate>),
    Or(Vec<PrunePredicate>),
    /// `column op value`.
    Compare {
        column: String,
        op: PruneOp,
        value: PruneValue,
    },
    /// `column IN (v1, v2, …)`.
    In {
        column: String,
        values: Vec<PruneValue>,
    },
    /// Something we can't interpret. Acts as "every row possibly
    /// matches" for pruning purposes.
    Opaque,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneOp {
    Eq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    NotEq,
}

/// Minimal typed value compared with bounds. Keeping it tiny and
/// comparable in-crate avoids wiring against the massive `Value`
/// enum from schema layer — callers lower their storage value to
/// one of these variants before calling the pruner.
#[derive(Debug, Clone, PartialEq)]
pub enum PruneValue {
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
}

impl PartialOrd for PruneValue {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        use PruneValue::*;
        match (self, other) {
            (Int(a), Int(b)) => a.partial_cmp(b),
            (Float(a), Float(b)) => a.partial_cmp(b),
            (Int(a), Float(b)) => (*a as f64).partial_cmp(b),
            (Float(a), Int(b)) => a.partial_cmp(&(*b as f64)),
            (Text(a), Text(b)) => a.partial_cmp(b),
            (Bool(a), Bool(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

/// Partitioning strategy (mirrors `storage::query::PartitionKind` but
/// kept local so this module has no upstream AST dependency).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneKind {
    Range,
    List,
    Hash,
}

/// Spec + key column name. Hash prunes need a modulus count that
/// matches the catalog — the pruner stores it as metadata only.
#[derive(Debug, Clone)]
pub struct PrunePartitioning {
    pub kind: PruneKind,
    pub column: String,
}

/// Bounds of a single RANGE child.
#[derive(Debug, Clone)]
pub struct RangeChild {
    pub name: String,
    /// `None` on either side means "unbounded".
    pub low: Option<PruneValue>,
    pub high_exclusive: Option<PruneValue>,
}

/// A single LIST child that holds one or more literal values.
#[derive(Debug, Clone)]
pub struct ListChild {
    pub name: String,
    pub values: Vec<PruneValue>,
}

/// A single HASH child — `remainder` is the residue this partition
/// owns, `modulus` is the global modulus.
#[derive(Debug, Clone)]
pub struct HashChild {
    pub name: String,
    pub modulus: u32,
    pub remainder: u32,
}

/// The pruner's output: the subset of child names that cannot be
/// proven to miss every matching row. Ordering mirrors the input.
pub fn prune_range(
    partitioning: &PrunePartitioning,
    children: &[RangeChild],
    predicate: &PrunePredicate,
) -> Vec<String> {
    debug_assert!(matches!(partitioning.kind, PruneKind::Range));
    let interval = derive_interval(predicate, &partitioning.column);
    children
        .iter()
        .filter(|child| child_range_overlaps(child, &interval))
        .map(|c| c.name.clone())
        .collect()
}

pub fn prune_list(
    partitioning: &PrunePartitioning,
    children: &[ListChild],
    predicate: &PrunePredicate,
) -> Vec<String> {
    debug_assert!(matches!(partitioning.kind, PruneKind::List));
    let allowed = derive_allowed_set(predicate, &partitioning.column);
    children
        .iter()
        .filter(|child| match &allowed {
            AllowedSet::Any => true,
            AllowedSet::Only(set) => child.values.iter().any(|v| set.iter().any(|a| a == v)),
        })
        .map(|c| c.name.clone())
        .collect()
}

pub fn prune_hash(
    partitioning: &PrunePartitioning,
    children: &[HashChild],
    predicate: &PrunePredicate,
) -> Vec<String> {
    debug_assert!(matches!(partitioning.kind, PruneKind::Hash));
    let Some(value) = derive_single_eq(predicate, &partitioning.column) else {
        return children.iter().map(|c| c.name.clone()).collect();
    };
    let hashed = hash_prune_value(&value);
    children
        .iter()
        .filter(|child| {
            child.modulus == 0 || (hashed % child.modulus as u64) == child.remainder as u64
        })
        .map(|c| c.name.clone())
        .collect()
}

// -------------------------------------------------------------------------
// Interval derivation (RANGE pruning)
// -------------------------------------------------------------------------

/// Closed/open interval on the partition key derived from the
/// predicate. `low` uses `Bound::Included/Excluded` semantics.
#[derive(Debug, Clone)]
struct Interval {
    low: Bound<PruneValue>,
    high: Bound<PruneValue>,
    /// A `NotEq(val)` hole — pruner ignores for RANGE (holes rarely
    /// eliminate chunks) but LIST pruning uses the allowed set.
    _holes: Vec<PruneValue>,
}

impl Interval {
    fn unbounded() -> Self {
        Self {
            low: Bound::Unbounded,
            high: Bound::Unbounded,
            _holes: Vec::new(),
        }
    }
}

fn derive_interval(predicate: &PrunePredicate, column: &str) -> Interval {
    match predicate {
        PrunePredicate::Compare {
            column: c,
            op,
            value,
        } if c == column => match op {
            PruneOp::Eq => Interval {
                low: Bound::Included(value.clone()),
                high: Bound::Included(value.clone()),
                _holes: Vec::new(),
            },
            PruneOp::Lt => Interval {
                low: Bound::Unbounded,
                high: Bound::Excluded(value.clone()),
                _holes: Vec::new(),
            },
            PruneOp::LtEq => Interval {
                low: Bound::Unbounded,
                high: Bound::Included(value.clone()),
                _holes: Vec::new(),
            },
            PruneOp::Gt => Interval {
                low: Bound::Excluded(value.clone()),
                high: Bound::Unbounded,
                _holes: Vec::new(),
            },
            PruneOp::GtEq => Interval {
                low: Bound::Included(value.clone()),
                high: Bound::Unbounded,
                _holes: Vec::new(),
            },
            PruneOp::NotEq => {
                let mut i = Interval::unbounded();
                i._holes.push(value.clone());
                i
            }
        },
        PrunePredicate::In { column: c, values } if c == column && !values.is_empty() => {
            let mut min = None;
            let mut max = None;
            for v in values {
                if min.as_ref().map(|m: &PruneValue| v < m).unwrap_or(true) {
                    min = Some(v.clone());
                }
                if max.as_ref().map(|m: &PruneValue| v > m).unwrap_or(true) {
                    max = Some(v.clone());
                }
            }
            Interval {
                low: min.map(Bound::Included).unwrap_or(Bound::Unbounded),
                high: max.map(Bound::Included).unwrap_or(Bound::Unbounded),
                _holes: Vec::new(),
            }
        }
        PrunePredicate::And(children) => {
            let mut acc = Interval::unbounded();
            for child in children {
                let i = derive_interval(child, column);
                acc = intersect(&acc, &i);
            }
            acc
        }
        PrunePredicate::Or(children) => {
            // Conservative: union of child intervals. If any child is
            // unbounded or opaque, the union is unbounded.
            let mut min: Option<Bound<PruneValue>> = None;
            let mut max: Option<Bound<PruneValue>> = None;
            for child in children {
                let i = derive_interval(child, column);
                if matches!(i.low, Bound::Unbounded) {
                    min = Some(Bound::Unbounded);
                } else if min != Some(Bound::Unbounded) {
                    min = Some(widest_low(min.clone(), i.low.clone()));
                }
                if matches!(i.high, Bound::Unbounded) {
                    max = Some(Bound::Unbounded);
                } else if max != Some(Bound::Unbounded) {
                    max = Some(widest_high(max.clone(), i.high.clone()));
                }
            }
            Interval {
                low: min.unwrap_or(Bound::Unbounded),
                high: max.unwrap_or(Bound::Unbounded),
                _holes: Vec::new(),
            }
        }
        _ => Interval::unbounded(),
    }
}

fn widest_low(current: Option<Bound<PruneValue>>, next: Bound<PruneValue>) -> Bound<PruneValue> {
    match (current, next) {
        (None, b) => b,
        (Some(Bound::Unbounded), _) | (_, Bound::Unbounded) => Bound::Unbounded,
        (Some(a), b) => {
            if bound_low_less_or_equal(&a, &b) {
                a
            } else {
                b
            }
        }
    }
}

fn widest_high(current: Option<Bound<PruneValue>>, next: Bound<PruneValue>) -> Bound<PruneValue> {
    match (current, next) {
        (None, b) => b,
        (Some(Bound::Unbounded), _) | (_, Bound::Unbounded) => Bound::Unbounded,
        (Some(a), b) => {
            if bound_high_greater_or_equal(&a, &b) {
                a
            } else {
                b
            }
        }
    }
}

fn bound_low_less_or_equal(a: &Bound<PruneValue>, b: &Bound<PruneValue>) -> bool {
    match (a, b) {
        (Bound::Unbounded, _) => true,
        (_, Bound::Unbounded) => false,
        (Bound::Included(x), Bound::Included(y)) | (Bound::Excluded(x), Bound::Excluded(y)) => {
            x.partial_cmp(y).map(|o| o.is_le()).unwrap_or(true)
        }
        (Bound::Included(x), Bound::Excluded(y)) => {
            x.partial_cmp(y).map(|o| o.is_le()).unwrap_or(true)
        }
        (Bound::Excluded(x), Bound::Included(y)) => {
            x.partial_cmp(y).map(|o| o.is_lt()).unwrap_or(true)
        }
    }
}

fn bound_high_greater_or_equal(a: &Bound<PruneValue>, b: &Bound<PruneValue>) -> bool {
    match (a, b) {
        (Bound::Unbounded, _) => true,
        (_, Bound::Unbounded) => false,
        (Bound::Included(x), Bound::Included(y)) | (Bound::Excluded(x), Bound::Excluded(y)) => {
            x.partial_cmp(y).map(|o| o.is_ge()).unwrap_or(true)
        }
        (Bound::Included(x), Bound::Excluded(y)) => {
            x.partial_cmp(y).map(|o| o.is_ge()).unwrap_or(true)
        }
        (Bound::Excluded(x), Bound::Included(y)) => {
            x.partial_cmp(y).map(|o| o.is_gt()).unwrap_or(true)
        }
    }
}

fn intersect(a: &Interval, b: &Interval) -> Interval {
    let low = tighter_low(&a.low, &b.low);
    let high = tighter_high(&a.high, &b.high);
    Interval {
        low,
        high,
        _holes: Vec::new(),
    }
}

fn tighter_low(a: &Bound<PruneValue>, b: &Bound<PruneValue>) -> Bound<PruneValue> {
    match (a, b) {
        (Bound::Unbounded, other) | (other, Bound::Unbounded) => other.clone(),
        (x, y) => {
            if bound_low_less_or_equal(x, y) {
                y.clone()
            } else {
                x.clone()
            }
        }
    }
}

fn tighter_high(a: &Bound<PruneValue>, b: &Bound<PruneValue>) -> Bound<PruneValue> {
    match (a, b) {
        (Bound::Unbounded, other) | (other, Bound::Unbounded) => other.clone(),
        (x, y) => {
            if bound_high_greater_or_equal(x, y) {
                y.clone()
            } else {
                x.clone()
            }
        }
    }
}

fn child_range_overlaps(child: &RangeChild, interval: &Interval) -> bool {
    // Child `[low, high_exclusive)` vs interval `[i.low, i.high]`.
    // Conservatively report overlap whenever either side is unknown.
    let child_low_ok = match &interval.high {
        Bound::Unbounded => true,
        Bound::Included(upper) => match &child.low {
            None => true,
            Some(cl) => cl.partial_cmp(upper).map(|o| o.is_le()).unwrap_or(true),
        },
        Bound::Excluded(upper) => match &child.low {
            None => true,
            Some(cl) => cl.partial_cmp(upper).map(|o| o.is_lt()).unwrap_or(true),
        },
    };
    let child_high_ok = match &interval.low {
        Bound::Unbounded => true,
        Bound::Included(lower) => match &child.high_exclusive {
            None => true,
            Some(ch) => ch.partial_cmp(lower).map(|o| o.is_gt()).unwrap_or(true),
        },
        Bound::Excluded(lower) => match &child.high_exclusive {
            None => true,
            Some(ch) => ch.partial_cmp(lower).map(|o| o.is_gt()).unwrap_or(true),
        },
    };
    child_low_ok && child_high_ok
}

// -------------------------------------------------------------------------
// LIST pruning
// -------------------------------------------------------------------------

enum AllowedSet {
    Any,
    Only(Vec<PruneValue>),
}

fn derive_allowed_set(predicate: &PrunePredicate, column: &str) -> AllowedSet {
    match predicate {
        PrunePredicate::Compare {
            column: c,
            op: PruneOp::Eq,
            value,
        } if c == column => AllowedSet::Only(vec![value.clone()]),
        PrunePredicate::In { column: c, values } if c == column && !values.is_empty() => {
            AllowedSet::Only(values.clone())
        }
        PrunePredicate::And(children) => {
            // AND tightens: start with Any, intersect as we go.
            let mut acc: Option<Vec<PruneValue>> = None;
            for child in children {
                match derive_allowed_set(child, column) {
                    AllowedSet::Any => {}
                    AllowedSet::Only(set) => {
                        acc = Some(match acc {
                            None => set,
                            Some(existing) => existing
                                .into_iter()
                                .filter(|v| set.iter().any(|w| w == v))
                                .collect(),
                        });
                    }
                }
            }
            acc.map(AllowedSet::Only).unwrap_or(AllowedSet::Any)
        }
        PrunePredicate::Or(children) => {
            // OR widens: union. Any opaque child makes the set Any.
            let mut merged: Vec<PruneValue> = Vec::new();
            for child in children {
                match derive_allowed_set(child, column) {
                    AllowedSet::Any => return AllowedSet::Any,
                    AllowedSet::Only(set) => {
                        for v in set {
                            if !merged.iter().any(|m| m == &v) {
                                merged.push(v);
                            }
                        }
                    }
                }
            }
            AllowedSet::Only(merged)
        }
        _ => AllowedSet::Any,
    }
}

// -------------------------------------------------------------------------
// HASH pruning
// -------------------------------------------------------------------------

fn derive_single_eq(predicate: &PrunePredicate, column: &str) -> Option<PruneValue> {
    match predicate {
        PrunePredicate::Compare {
            column: c,
            op: PruneOp::Eq,
            value,
        } if c == column => Some(value.clone()),
        PrunePredicate::And(children) => {
            for child in children {
                if let Some(v) = derive_single_eq(child, column) {
                    return Some(v);
                }
            }
            None
        }
        _ => None,
    }
}

fn hash_prune_value(v: &PruneValue) -> u64 {
    // FNV-1a 64-bit. Good-enough, zero-dep.
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut h = OFFSET;
    let bytes: Vec<u8> = match v {
        PruneValue::Int(i) => i.to_le_bytes().to_vec(),
        PruneValue::Float(f) => f.to_le_bytes().to_vec(),
        PruneValue::Text(s) => s.as_bytes().to_vec(),
        PruneValue::Bool(b) => vec![*b as u8],
    };
    for b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(PRIME);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pint(v: i64) -> PruneValue {
        PruneValue::Int(v)
    }

    fn range_child(name: &str, low: Option<i64>, high_exclusive: Option<i64>) -> RangeChild {
        RangeChild {
            name: name.to_string(),
            low: low.map(pint),
            high_exclusive: high_exclusive.map(pint),
        }
    }

    fn spec_range() -> PrunePartitioning {
        PrunePartitioning {
            kind: PruneKind::Range,
            column: "ts".to_string(),
        }
    }

    fn spec_list() -> PrunePartitioning {
        PrunePartitioning {
            kind: PruneKind::List,
            column: "region".to_string(),
        }
    }

    fn spec_hash() -> PrunePartitioning {
        PrunePartitioning {
            kind: PruneKind::Hash,
            column: "user_id".to_string(),
        }
    }

    #[test]
    fn range_keeps_only_chunks_overlapping_equality() {
        let children = vec![
            range_child("c0", Some(0), Some(100)),
            range_child("c1", Some(100), Some(200)),
            range_child("c2", Some(200), Some(300)),
        ];
        let pred = PrunePredicate::Compare {
            column: "ts".into(),
            op: PruneOp::Eq,
            value: pint(150),
        };
        assert_eq!(prune_range(&spec_range(), &children, &pred), vec!["c1"]);
    }

    #[test]
    fn range_handles_greater_than_predicate() {
        let children = vec![
            range_child("c0", Some(0), Some(100)),
            range_child("c1", Some(100), Some(200)),
            range_child("c2", Some(200), Some(300)),
        ];
        let pred = PrunePredicate::Compare {
            column: "ts".into(),
            op: PruneOp::GtEq,
            value: pint(150),
        };
        let kept = prune_range(&spec_range(), &children, &pred);
        assert_eq!(kept, vec!["c1", "c2"]);
    }

    #[test]
    fn range_handles_between_via_and() {
        let children = vec![
            range_child("c0", Some(0), Some(100)),
            range_child("c1", Some(100), Some(200)),
            range_child("c2", Some(200), Some(300)),
            range_child("c3", Some(300), Some(400)),
        ];
        let pred = PrunePredicate::And(vec![
            PrunePredicate::Compare {
                column: "ts".into(),
                op: PruneOp::GtEq,
                value: pint(150),
            },
            PrunePredicate::Compare {
                column: "ts".into(),
                op: PruneOp::Lt,
                value: pint(250),
            },
        ]);
        let kept = prune_range(&spec_range(), &children, &pred);
        assert_eq!(kept, vec!["c1", "c2"]);
    }

    #[test]
    fn range_conservative_when_predicate_on_different_column() {
        let children = vec![range_child("c0", Some(0), Some(100))];
        let pred = PrunePredicate::Compare {
            column: "other".into(),
            op: PruneOp::Eq,
            value: pint(42),
        };
        assert_eq!(prune_range(&spec_range(), &children, &pred), vec!["c0"]);
    }

    #[test]
    fn range_handles_unbounded_children() {
        let children = vec![
            range_child("past", None, Some(0)),
            range_child("mid", Some(0), Some(100)),
            range_child("future", Some(100), None),
        ];
        let pred = PrunePredicate::Compare {
            column: "ts".into(),
            op: PruneOp::GtEq,
            value: pint(50),
        };
        let kept = prune_range(&spec_range(), &children, &pred);
        assert_eq!(kept, vec!["mid", "future"]);
    }

    #[test]
    fn range_in_clause_bounds() {
        let children = vec![
            range_child("c0", Some(0), Some(100)),
            range_child("c1", Some(100), Some(200)),
            range_child("c2", Some(200), Some(300)),
        ];
        let pred = PrunePredicate::In {
            column: "ts".into(),
            values: vec![pint(50), pint(250)],
        };
        let kept = prune_range(&spec_range(), &children, &pred);
        // Conservative: covers [50, 250] so touches c0, c1, c2.
        assert_eq!(kept, vec!["c0", "c1", "c2"]);
    }

    #[test]
    fn list_prunes_non_matching_value_sets() {
        let children = vec![
            ListChild {
                name: "us".to_string(),
                values: vec![PruneValue::Text("us-east".into())],
            },
            ListChild {
                name: "eu".to_string(),
                values: vec![PruneValue::Text("eu-west".into())],
            },
            ListChild {
                name: "apac".to_string(),
                values: vec![PruneValue::Text("ap-south".into())],
            },
        ];
        let pred = PrunePredicate::Compare {
            column: "region".into(),
            op: PruneOp::Eq,
            value: PruneValue::Text("eu-west".into()),
        };
        assert_eq!(prune_list(&spec_list(), &children, &pred), vec!["eu"]);
    }

    #[test]
    fn list_handles_in_clause() {
        let children = vec![
            ListChild {
                name: "a".to_string(),
                values: vec![PruneValue::Text("a".into())],
            },
            ListChild {
                name: "b".to_string(),
                values: vec![PruneValue::Text("b".into())],
            },
            ListChild {
                name: "c".to_string(),
                values: vec![PruneValue::Text("c".into())],
            },
        ];
        let pred = PrunePredicate::In {
            column: "region".into(),
            values: vec![PruneValue::Text("a".into()), PruneValue::Text("c".into())],
        };
        let kept = prune_list(&spec_list(), &children, &pred);
        assert_eq!(kept, vec!["a", "c"]);
    }

    #[test]
    fn list_keeps_all_when_predicate_is_opaque() {
        let children = vec![ListChild {
            name: "x".to_string(),
            values: vec![PruneValue::Text("x".into())],
        }];
        let pred = PrunePredicate::Opaque;
        assert_eq!(prune_list(&spec_list(), &children, &pred), vec!["x"]);
    }

    #[test]
    fn hash_routes_to_single_partition_on_equality() {
        let children = vec![
            HashChild {
                name: "h0".to_string(),
                modulus: 4,
                remainder: 0,
            },
            HashChild {
                name: "h1".to_string(),
                modulus: 4,
                remainder: 1,
            },
            HashChild {
                name: "h2".to_string(),
                modulus: 4,
                remainder: 2,
            },
            HashChild {
                name: "h3".to_string(),
                modulus: 4,
                remainder: 3,
            },
        ];
        let pred = PrunePredicate::Compare {
            column: "user_id".into(),
            op: PruneOp::Eq,
            value: pint(42),
        };
        let kept = prune_hash(&spec_hash(), &children, &pred);
        assert_eq!(
            kept.len(),
            1,
            "hash eq must land on one partition: {kept:?}"
        );
    }

    #[test]
    fn hash_keeps_all_without_equality() {
        let children = vec![
            HashChild {
                name: "h0".to_string(),
                modulus: 2,
                remainder: 0,
            },
            HashChild {
                name: "h1".to_string(),
                modulus: 2,
                remainder: 1,
            },
        ];
        let pred = PrunePredicate::Compare {
            column: "user_id".into(),
            op: PruneOp::Gt,
            value: pint(0),
        };
        let kept = prune_hash(&spec_hash(), &children, &pred);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn and_tightens_across_child_predicates() {
        let children = vec![
            range_child("c0", Some(0), Some(50)),
            range_child("c1", Some(50), Some(100)),
            range_child("c2", Some(100), Some(150)),
        ];
        let pred = PrunePredicate::And(vec![
            PrunePredicate::Compare {
                column: "ts".into(),
                op: PruneOp::GtEq,
                value: pint(60),
            },
            PrunePredicate::Compare {
                column: "ts".into(),
                op: PruneOp::Lt,
                value: pint(90),
            },
        ]);
        assert_eq!(prune_range(&spec_range(), &children, &pred), vec!["c1"]);
    }

    #[test]
    fn or_widens_across_child_predicates() {
        let children = vec![
            range_child("c0", Some(0), Some(100)),
            range_child("c1", Some(100), Some(200)),
            range_child("c2", Some(200), Some(300)),
        ];
        let pred = PrunePredicate::Or(vec![
            PrunePredicate::Compare {
                column: "ts".into(),
                op: PruneOp::Eq,
                value: pint(50),
            },
            PrunePredicate::Compare {
                column: "ts".into(),
                op: PruneOp::Eq,
                value: pint(250),
            },
        ]);
        let kept = prune_range(&spec_range(), &children, &pred);
        // Conservative OR widens to [50, 250] → touches all 3 chunks.
        assert_eq!(kept, vec!["c0", "c1", "c2"]);
    }
}
