//! Per-group accumulator state.
//!
//! Each group owns one [`GroupAccumulator`]. `accumulate` folds a
//! single [`super::ScanRow`] into the slot vector; `finalize` emits
//! the per-position [`Value`]s the planner returns.
//!
//! No per-row materialisation lives here — the slot layout is sized
//! once at construction time from the AST and stays constant for
//! the life of the group.

use super::ast::{AggregateExpr, AggregateOp};
use crate::storage::schema::Value;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

/// Test-only counter incremented whenever the planner emits a
/// final per-group row. The legacy path materialises one row per
/// scanned input; this path materialises one per *group*. The
/// gap is the whole point of the optimisation.
#[cfg(test)]
pub(crate) static MATERIALIZED_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn note_materialized() {
    MATERIALIZED_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(test))]
#[inline]
pub(crate) fn note_materialized() {}

/// Per-aggregate accumulator slot.
///
/// One slot per [`AggregateExpr`] in the plan. Variants are picked
/// by [`AggregateOp`] at construction; the hot loop matches on the
/// variant rather than the op so each branch has a stable layout.
enum Slot {
    /// `COUNT(*)` — every accumulated row bumps the counter.
    CountStar { count: u64 },
    /// `COUNT(col)` — non-NULL inputs only.
    CountColumn { count: u64 },
    /// `SUM(col)` — running f64 sum, ignoring NULLs. We track
    /// `seen_any` so we can return NULL for an entirely-NULL group
    /// (matching SQL semantics) instead of `0.0`.
    Sum { sum: f64, seen_any: bool },
    /// `AVG(col)` — `(sum, count)` finalised at emission. Same
    /// "all-NULL → NULL" semantics as SUM.
    Avg { sum: f64, count: u64 },
    /// `MIN(col)` — running extremum, type-preserving. We compare
    /// via the canonical key encoding so types stay consistent
    /// across the scan (no implicit numeric coercion that the
    /// legacy path also avoids).
    Min { current: Option<Value> },
    /// `MAX(col)` — symmetric.
    Max { current: Option<Value> },
}

impl Slot {
    fn for_op(op: AggregateOp) -> Self {
        match op {
            AggregateOp::CountStar => Slot::CountStar { count: 0 },
            AggregateOp::CountColumn => Slot::CountColumn { count: 0 },
            AggregateOp::Sum => Slot::Sum {
                sum: 0.0,
                seen_any: false,
            },
            AggregateOp::Avg => Slot::Avg { sum: 0.0, count: 0 },
            AggregateOp::Min => Slot::Min { current: None },
            AggregateOp::Max => Slot::Max { current: None },
        }
    }
}

/// One per group. Owns one [`Slot`] per aggregate expression, in
/// AST order.
pub(super) struct GroupAccumulator {
    slots: Vec<Slot>,
}

impl GroupAccumulator {
    pub(super) fn new(aggregates: &[AggregateExpr]) -> Self {
        Self {
            slots: aggregates.iter().map(|a| Slot::for_op(a.op)).collect(),
        }
    }

    /// Fold one scan row into every slot.
    ///
    /// Per-aggregate input is read by index; out-of-range indices
    /// are surfaced through the plan-time check in
    /// [`super::AggregateQueryPlanner::plan`] and never reach the
    /// hot loop.
    pub(super) fn accumulate(&mut self, aggregates: &[AggregateExpr], inputs: &[Value]) {
        for (slot, expr) in self.slots.iter_mut().zip(aggregates.iter()) {
            match slot {
                Slot::CountStar { count } => {
                    *count += 1;
                }
                Slot::CountColumn { count } => {
                    if let Some(v) = inputs.get(expr.input_index) {
                        if !matches!(v, Value::Null) {
                            *count += 1;
                        }
                    }
                }
                Slot::Sum { sum, seen_any } => {
                    if let Some(v) = inputs.get(expr.input_index) {
                        if let Some(n) = numeric_value(v) {
                            *sum += n;
                            *seen_any = true;
                        }
                    }
                }
                Slot::Avg { sum, count } => {
                    if let Some(v) = inputs.get(expr.input_index) {
                        if let Some(n) = numeric_value(v) {
                            *sum += n;
                            *count += 1;
                        }
                    }
                }
                Slot::Min { current } => {
                    if let Some(v) = inputs.get(expr.input_index) {
                        update_extreme(current, v, std::cmp::Ordering::Less);
                    }
                }
                Slot::Max { current } => {
                    if let Some(v) = inputs.get(expr.input_index) {
                        update_extreme(current, v, std::cmp::Ordering::Greater);
                    }
                }
            }
        }
    }

    /// Emit the per-aggregate result row. One [`Value`] per slot,
    /// in AST order. Bumps [`MATERIALIZED_COUNT`] in tests so the
    /// "O(group count)" invariant can be asserted.
    pub(super) fn finalize(self) -> Vec<Value> {
        note_materialized();
        self.slots
            .into_iter()
            .map(|slot| match slot {
                Slot::CountStar { count } | Slot::CountColumn { count } => {
                    Value::Integer(count as i64)
                }
                Slot::Sum { sum, seen_any } => {
                    if seen_any {
                        sum_f64_to_value(sum)
                    } else {
                        Value::Null
                    }
                }
                Slot::Avg { sum, count } => {
                    if count == 0 {
                        Value::Null
                    } else {
                        Value::Float(sum / count as f64)
                    }
                }
                Slot::Min { current } | Slot::Max { current } => current.unwrap_or(Value::Null),
            })
            .collect()
    }
}

fn sum_f64_to_value(f: f64) -> Value {
    if f.fract() == 0.0 && f >= i64::MIN as f64 && f <= i64::MAX as f64 {
        Value::Integer(f as i64)
    } else {
        Value::Float(f)
    }
}

/// Cast a `Value` into `f64` for SUM/AVG. NULL and non-numeric
/// values yield `None`; the caller decides how to react (skip, or
/// flip the all-NULL flag). Mirrors the casts the legacy path
/// performs in `aggregate.rs::value_to_f64`, but kept private to
/// this module so the planner has no dependency on the legacy
/// internals.
///
/// That claim used to be false for `Decimal`: this cast was raw while the
/// legacy path divided by the fixed scale, so `SUM`/`AVG` over a `DECIMAL`
/// column returned results differing by 10^4 depending on which route the
/// planner picked (#2058). Both now go through `schema::decimal_to_f64`, and
/// `decimal_sum_agrees_with_the_legacy_aggregate_path` pins them together —
/// duplicating the match arms is what let them drift silently.
fn numeric_value(v: &Value) -> Option<f64> {
    match v {
        Value::Integer(i) => Some(*i as f64),
        Value::UnsignedInteger(u) => Some(*u as f64),
        Value::Float(f) if f.is_finite() => Some(*f),
        Value::Decimal(d) => Some(crate::storage::schema::decimal_to_f64(*d)),
        Value::Boolean(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Update `current` if `candidate` extends `target_ordering`.
///
/// `Ordering::Less` → MIN behaviour (replace when candidate < current).
/// `Ordering::Greater` → MAX behaviour.
///
/// NULL inputs are skipped. Non-comparable pairs (different kinds,
/// non-finite floats) leave `current` untouched — same conservative
/// rule the legacy path uses, since SQL doesn't define an order
/// across families.
fn update_extreme(current: &mut Option<Value>, candidate: &Value, target: std::cmp::Ordering) {
    if matches!(candidate, Value::Null) {
        return;
    }
    let Some(cand_key) = crate::storage::schema::value_to_canonical_key(candidate) else {
        return;
    };
    match current {
        None => {
            *current = Some(candidate.clone());
        }
        Some(cur) => {
            let Some(cur_key) = crate::storage::schema::value_to_canonical_key(cur) else {
                *current = Some(candidate.clone());
                return;
            };
            // Only compare within the same canonical family — cross-family
            // ordering would silently coerce, masking shape bugs.
            if cur_key.family() != cand_key.family() {
                return;
            }
            if cand_key.cmp(&cur_key) == target {
                *current = Some(candidate.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::numeric_value;
    use crate::runtime::query_exec::aggregate::value_to_f64;
    use crate::storage::schema::Value;

    /// The planner's `SUM`/`AVG` cast and the legacy path's must agree on
    /// every value they both accept.
    ///
    /// They did not: the planner cast `Decimal` raw while the legacy path
    /// divided by the fixed scale, so the same `SUM` over a `DECIMAL` column
    /// returned values 10^4 apart depending on the route chosen (#2058). Both
    /// now delegate to `schema::decimal_to_f64`; this pins them so a future
    /// edit to one match arm cannot silently reintroduce the split.
    #[test]
    fn decimal_sum_agrees_with_the_legacy_aggregate_path() {
        let cases = [
            Value::Decimal(387_600),   // 38.76
            Value::Decimal(-771_500),  // -77.15
            Value::Decimal(1_234_567), // 123.4567 — the rendering test's value
            Value::Decimal(0),
            Value::Decimal(i64::MAX),
            Value::Decimal(i64::MIN),
            Value::Integer(42),
            Value::UnsignedInteger(7),
            Value::Float(1.5),
            Value::Null,
        ];
        for value in cases {
            assert_eq!(
                numeric_value(&value),
                value_to_f64(&value),
                "planner and legacy aggregate casts disagree on {value:?}"
            );
        }
    }

    /// Types the two routes still disagree on, tracked in #2060.
    ///
    /// This documents current behaviour; it does not endorse it. `SUM` over a
    /// `BIGINT` column returning a number on one route and `NULL` on the other
    /// is a defect, but closing it means deciding whether `SUM(boolean)` is
    /// supported at all and how non-finite floats aggregate — semantics calls
    /// rather than mechanical edits, so #2058 fixed only the `Decimal` arm and
    /// left these visible instead of silently narrowing the pin above.
    ///
    /// When #2060 is fixed this test will fail. That is the point: update it
    /// then, and fold the surviving cases into the agreement test.
    #[test]
    fn planner_and_legacy_diverge_on_bigint_boolean_and_nonfinite_float() {
        // The planner has no BigInt arm; the legacy path does.
        assert_eq!(numeric_value(&Value::BigInt(9)), None);
        assert_eq!(value_to_f64(&Value::BigInt(9)), Some(9.0));

        // The planner counts booleans; the legacy path rejects them.
        assert_eq!(numeric_value(&Value::Boolean(true)), Some(1.0));
        assert_eq!(value_to_f64(&Value::Boolean(true)), None);

        // The planner guards on `is_finite`; the legacy path does not.
        assert_eq!(numeric_value(&Value::Float(f64::NAN)), None);
        assert!(value_to_f64(&Value::Float(f64::NAN)).is_some_and(f64::is_nan));
    }

    /// A `DECIMAL` read for aggregation must equal what the engine renders and
    /// compares it as — scale 4, the value the write side parses at.
    #[test]
    fn decimal_aggregation_matches_the_rendered_value() {
        // `Decimal(1_234_567)` renders as "123.4567" (pinned in entity_json).
        assert_eq!(numeric_value(&Value::Decimal(1_234_567)), Some(123.4567));
    }
}
