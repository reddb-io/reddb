//! Hypertable chunk pruning for the SELECT planner.
//!
//! Phase 0 of PRD #850 — *activate* the dormant partition-pruning
//! primitive for hypertables. The machinery already exists
//! ([`super::partition_pruning`] + the per-chunk time bounds tracked by
//! [`HypertableRegistry`](crate::storage::timeseries::HypertableRegistry))
//! but nothing in the SELECT path consulted it, so every query against a
//! hypertable scanned every chunk.
//!
//! This module is the bridge: given a hypertable's spec, its chunk set,
//! and the SELECT `WHERE` clause, it returns only the chunks whose
//! declared `[start_ns, end_ns)` interval can contain a row the temporal
//! predicate admits. Chunks proven disjoint from the predicate window are
//! dropped; everything else is kept.
//!
//! **Soundness contract** — the pruner never drops a chunk that could
//! hold a matching row. A row in chunk `C` carries a timestamp inside
//! `[C.start_ns, C.end_ns_exclusive)`; if any timestamp in that interval
//! satisfies the predicate, `C` is kept. When the `WHERE` clause does not
//! constrain the time column (or uses a shape the lowering can't reason
//! about), the pruner is conservative and keeps every chunk — exactly the
//! Timescale / Postgres contract.

use crate::storage::query::ast::{CompareOp, FieldRef, Filter};
use crate::storage::schema::Value;
use crate::storage::timeseries::{ChunkMeta, HypertableSpec};

use super::partition_pruning::{
    prune_range, PruneKind, PruneOp, PrunePartitioning, PrunePredicate, PruneValue, RangeChild,
};

/// Stable per-chunk name used to thread results through the generic
/// range pruner. `start_ns` is unique within a hypertable, so this is a
/// 1:1 key back to the originating [`ChunkMeta`].
fn chunk_name(chunk: &ChunkMeta) -> String {
    format!("{}:{}", chunk.id.hypertable, chunk.id.start_ns)
}

/// Column a `FieldRef` targets, when it is a plain column / property
/// reference. A hypertable time predicate lowers to a bare `TableColumn`.
fn field_column(field: &FieldRef) -> Option<&str> {
    match field {
        FieldRef::TableColumn { column, .. } => Some(column.as_str()),
        FieldRef::NodeProperty { property, .. } | FieldRef::EdgeProperty { property, .. } => {
            Some(property.as_str())
        }
        FieldRef::NodeId { .. } => None,
    }
}

/// Lower an integer-shaped time value. The time axis is unix-ns
/// `BIGINT`, so only integer variants are actionable; anything else
/// yields `None`, which makes the enclosing predicate `Opaque` (keep
/// every chunk) — conservative and correct.
fn int_value(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(n) | Value::Timestamp(n) | Value::Duration(n) => Some(*n),
        Value::UnsignedInteger(n) => i64::try_from(*n).ok(),
        _ => None,
    }
}

fn map_op(op: CompareOp) -> PruneOp {
    match op {
        CompareOp::Eq => PruneOp::Eq,
        CompareOp::Ne => PruneOp::NotEq,
        CompareOp::Lt => PruneOp::Lt,
        CompareOp::Le => PruneOp::LtEq,
        CompareOp::Gt => PruneOp::Gt,
        CompareOp::Ge => PruneOp::GtEq,
    }
}

/// Lower the runtime `Filter` AST to the pruner's [`PrunePredicate`],
/// keeping only the fragments that reference `time_column`. Any shape we
/// can't act on (a predicate on another column, a `LIKE`, a `NOT`, a
/// non-integer literal) collapses to `Opaque`, which the pruner reads as
/// "every chunk possibly matches".
fn lower_filter(filter: &Filter, time_column: &str) -> PrunePredicate {
    match filter {
        Filter::Compare { field, op, value } => match (field_column(field), int_value(value)) {
            (Some(col), Some(v)) if col == time_column => PrunePredicate::Compare {
                column: time_column.to_string(),
                op: map_op(*op),
                value: PruneValue::Int(v),
            },
            _ => PrunePredicate::Opaque,
        },
        Filter::Between { field, low, high } => {
            match (field_column(field), int_value(low), int_value(high)) {
                (Some(col), Some(lo), Some(hi)) if col == time_column => PrunePredicate::And(vec![
                    PrunePredicate::Compare {
                        column: time_column.to_string(),
                        op: PruneOp::GtEq,
                        value: PruneValue::Int(lo),
                    },
                    PrunePredicate::Compare {
                        column: time_column.to_string(),
                        op: PruneOp::LtEq,
                        value: PruneValue::Int(hi),
                    },
                ]),
                _ => PrunePredicate::Opaque,
            }
        }
        Filter::In { field, values } => match field_column(field) {
            Some(col) if col == time_column => {
                let lowered: Option<Vec<PruneValue>> = values
                    .iter()
                    .map(|v| int_value(v).map(PruneValue::Int))
                    .collect();
                match lowered {
                    Some(vs) if !vs.is_empty() => PrunePredicate::In {
                        column: time_column.to_string(),
                        values: vs,
                    },
                    // A non-integer member taints the set — keep all.
                    _ => PrunePredicate::Opaque,
                }
            }
            _ => PrunePredicate::Opaque,
        },
        Filter::And(a, b) => PrunePredicate::And(vec![
            lower_filter(a, time_column),
            lower_filter(b, time_column),
        ]),
        Filter::Or(a, b) => PrunePredicate::Or(vec![
            lower_filter(a, time_column),
            lower_filter(b, time_column),
        ]),
        // NOT / LIKE / IS NULL / field-to-field / opaque expressions all
        // stay conservative.
        _ => PrunePredicate::Opaque,
    }
}

/// Return the subset of `chunks` that may contain a row matching
/// `filter`'s temporal predicate.
///
/// * `filter == None` (no `WHERE`) → every chunk is kept.
/// * a `WHERE` that doesn't constrain the time column → every chunk is
///   kept (conservative).
/// * a temporal predicate → only chunks whose `[start_ns, end_ns)`
///   interval overlaps the predicate window survive.
///
/// Ordering mirrors the input `chunks` slice.
pub fn prune_hypertable_chunks(
    spec: &HypertableSpec,
    chunks: &[ChunkMeta],
    filter: Option<&Filter>,
) -> Vec<ChunkMeta> {
    let Some(filter) = filter else {
        return chunks.to_vec();
    };
    let predicate = lower_filter(filter, &spec.time_column);
    // No actionable temporal constraint → nothing to prune.
    if matches!(predicate, PrunePredicate::Opaque) {
        return chunks.to_vec();
    }

    let partitioning = PrunePartitioning {
        kind: PruneKind::Range,
        column: spec.time_column.clone(),
    };
    let children: Vec<RangeChild> = chunks
        .iter()
        .map(|c| RangeChild {
            name: chunk_name(c),
            low: Some(PruneValue::Int(c.id.start_ns as i64)),
            high_exclusive: Some(PruneValue::Int(c.end_ns_exclusive as i64)),
        })
        .collect();

    let kept: std::collections::HashSet<String> = prune_range(&partitioning, &children, &predicate)
        .into_iter()
        .collect();

    chunks
        .iter()
        .filter(|c| kept.contains(&chunk_name(c)))
        .cloned()
        .collect()
}

/// Smallest `[lo, hi)` nanosecond window that contains every kept
/// chunk's declared interval, or `None` when `kept` is empty.
///
/// A `None` return is the planner's signal that *no* chunk overlaps the
/// predicate, so the scan can be skipped entirely — there is provably no
/// matching row. A `Some((lo, hi))` window is a superset of every kept
/// chunk and therefore of every row the predicate admits, so a caller
/// may use it to bound the physical scan without dropping a match.
pub fn kept_scan_bounds(kept: &[ChunkMeta]) -> Option<(u64, u64)> {
    if kept.is_empty() {
        return None;
    }
    let mut lo = u64::MAX;
    let mut hi = 0u64;
    for c in kept {
        lo = lo.min(c.id.start_ns);
        hi = hi.max(c.end_ns_exclusive);
    }
    Some((lo, hi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::timeseries::ChunkId;
    use proptest::prelude::*;

    /// Build a `ChunkMeta` covering `[start, end)`. Row stats are set to
    /// the interval bounds so the fixture is internally consistent.
    fn chunk(hypertable: &str, start: u64, end: u64) -> ChunkMeta {
        let mut meta = ChunkMeta::new(
            ChunkId {
                hypertable: hypertable.to_string(),
                start_ns: start,
            },
            end,
        );
        // Pretend a row lands at each boundary so min/max are sane.
        meta.observe(start);
        if end > start {
            meta.observe(end - 1);
        }
        meta
    }

    fn spec() -> HypertableSpec {
        HypertableSpec::new("metrics", "ts", 100)
    }

    fn ts_compare(op: CompareOp, v: i64) -> Filter {
        Filter::Compare {
            field: FieldRef::column("metrics", "ts"),
            op,
            value: Value::Integer(v),
        }
    }

    fn kept_starts(kept: &[ChunkMeta]) -> Vec<u64> {
        kept.iter().map(|c| c.id.start_ns).collect()
    }

    #[test]
    fn no_filter_keeps_every_chunk() {
        let chunks = vec![chunk("metrics", 0, 100), chunk("metrics", 100, 200)];
        let kept = prune_hypertable_chunks(&spec(), &chunks, None);
        assert_eq!(kept_starts(&kept), vec![0, 100]);
    }

    #[test]
    fn predicate_on_other_column_keeps_every_chunk() {
        let chunks = vec![chunk("metrics", 0, 100), chunk("metrics", 100, 200)];
        let filter = Filter::Compare {
            field: FieldRef::column("metrics", "host"),
            op: CompareOp::Eq,
            value: Value::Text("a".into()),
        };
        let kept = prune_hypertable_chunks(&spec(), &chunks, Some(&filter));
        assert_eq!(kept_starts(&kept), vec![0, 100]);
    }

    #[test]
    fn equality_keeps_only_the_owning_chunk() {
        let chunks = vec![
            chunk("metrics", 0, 100),
            chunk("metrics", 100, 200),
            chunk("metrics", 200, 300),
        ];
        let filter = ts_compare(CompareOp::Eq, 150);
        let kept = prune_hypertable_chunks(&spec(), &chunks, Some(&filter));
        assert_eq!(kept_starts(&kept), vec![100]);
    }

    #[test]
    fn between_keeps_overlapping_chunks_only() {
        let chunks = vec![
            chunk("metrics", 0, 100),
            chunk("metrics", 100, 200),
            chunk("metrics", 200, 300),
            chunk("metrics", 300, 400),
        ];
        let filter = Filter::Between {
            field: FieldRef::column("metrics", "ts"),
            low: Value::Integer(150),
            high: Value::Integer(250),
        };
        let kept = prune_hypertable_chunks(&spec(), &chunks, Some(&filter));
        assert_eq!(kept_starts(&kept), vec![100, 200]);
    }

    #[test]
    fn and_of_bounds_tightens_window() {
        let chunks = vec![
            chunk("metrics", 0, 100),
            chunk("metrics", 100, 200),
            chunk("metrics", 200, 300),
        ];
        let filter = Filter::And(
            Box::new(ts_compare(CompareOp::Ge, 120)),
            Box::new(ts_compare(CompareOp::Lt, 190)),
        );
        let kept = prune_hypertable_chunks(&spec(), &chunks, Some(&filter));
        assert_eq!(kept_starts(&kept), vec![100]);
    }

    #[test]
    fn disjoint_window_prunes_everything() {
        let chunks = vec![chunk("metrics", 0, 100), chunk("metrics", 100, 200)];
        let filter = ts_compare(CompareOp::Ge, 1_000);
        let kept = prune_hypertable_chunks(&spec(), &chunks, Some(&filter));
        assert!(kept.is_empty());
        assert_eq!(kept_scan_bounds(&kept), None);
    }

    #[test]
    fn scan_bounds_span_kept_chunks() {
        let kept = vec![chunk("metrics", 100, 200), chunk("metrics", 200, 300)];
        assert_eq!(kept_scan_bounds(&kept), Some((100, 300)));
    }

    // ---------------------------------------------------------------
    // Property: pruning is sound — it never drops a chunk that contains
    // a timestamp satisfying the predicate. Regardless of chunk layout
    // or predicate shape, every chunk holding a matching point survives.
    // ---------------------------------------------------------------

    /// Predicate shapes the property test exercises, with both an
    /// executable `Filter` and a reference SQL evaluator.
    #[derive(Debug, Clone)]
    enum Pred {
        Cmp(CompareOp, i64),
        Between(i64, i64),
        In(Vec<i64>),
        And(Box<Pred>, Box<Pred>),
        Or(Box<Pred>, Box<Pred>),
    }

    fn pred_to_filter(p: &Pred) -> Filter {
        match p {
            Pred::Cmp(op, v) => ts_compare(*op, *v),
            Pred::Between(lo, hi) => Filter::Between {
                field: FieldRef::column("metrics", "ts"),
                low: Value::Integer(*lo),
                high: Value::Integer(*hi),
            },
            Pred::In(vs) => Filter::In {
                field: FieldRef::column("metrics", "ts"),
                values: vs.iter().map(|v| Value::Integer(*v)).collect(),
            },
            Pred::And(a, b) => {
                Filter::And(Box::new(pred_to_filter(a)), Box::new(pred_to_filter(b)))
            }
            Pred::Or(a, b) => Filter::Or(Box::new(pred_to_filter(a)), Box::new(pred_to_filter(b))),
        }
    }

    /// Ground-truth SQL semantics for a single timestamp.
    fn eval(p: &Pred, ts: i64) -> bool {
        match p {
            Pred::Cmp(op, v) => match op {
                CompareOp::Eq => ts == *v,
                CompareOp::Ne => ts != *v,
                CompareOp::Lt => ts < *v,
                CompareOp::Le => ts <= *v,
                CompareOp::Gt => ts > *v,
                CompareOp::Ge => ts >= *v,
            },
            Pred::Between(lo, hi) => ts >= *lo && ts <= *hi,
            Pred::In(vs) => vs.contains(&ts),
            Pred::And(a, b) => eval(a, ts) && eval(b, ts),
            Pred::Or(a, b) => eval(a, ts) || eval(b, ts),
        }
    }

    fn leaf_pred() -> impl Strategy<Value = Pred> {
        prop_oneof![
            (
                prop_oneof![
                    Just(CompareOp::Eq),
                    Just(CompareOp::Ne),
                    Just(CompareOp::Lt),
                    Just(CompareOp::Le),
                    Just(CompareOp::Gt),
                    Just(CompareOp::Ge),
                ],
                0i64..60,
            )
                .prop_map(|(op, v)| Pred::Cmp(op, v)),
            (0i64..60, 0i64..60).prop_map(|(a, b)| Pred::Between(a.min(b), a.max(b))),
            prop::collection::vec(0i64..60, 1..4).prop_map(Pred::In),
        ]
    }

    fn pred_strategy() -> impl Strategy<Value = Pred> {
        leaf_pred().prop_recursive(3, 12, 2, |inner| {
            prop_oneof![
                (inner.clone(), inner.clone())
                    .prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
                (inner.clone(), inner).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            ]
        })
    }

    /// A layout of small, contiguous chunks so every contained timestamp
    /// can be enumerated exhaustively in the soundness check.
    fn chunks_strategy() -> impl Strategy<Value = Vec<ChunkMeta>> {
        prop::collection::vec(1u64..8, 1..8).prop_map(|widths| {
            let mut out = Vec::with_capacity(widths.len());
            let mut start = 0u64;
            for w in widths {
                out.push(chunk("metrics", start, start + w));
                start += w;
            }
            out
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn pruning_never_drops_a_chunk_with_a_matching_point(
            chunks in chunks_strategy(),
            pred in pred_strategy(),
        ) {
            let filter = pred_to_filter(&pred);
            let kept = prune_hypertable_chunks(&spec(), &chunks, Some(&filter));
            let kept_keys: std::collections::HashSet<u64> =
                kept.iter().map(|c| c.id.start_ns).collect();

            for c in &chunks {
                // Enumerate every timestamp the chunk could hold.
                let contains_match = (c.id.start_ns..c.end_ns_exclusive)
                    .any(|ts| eval(&pred, ts as i64));
                if contains_match {
                    prop_assert!(
                        kept_keys.contains(&c.id.start_ns),
                        "dropped chunk [{}, {}) that contains a matching row for {:?}",
                        c.id.start_ns,
                        c.end_ns_exclusive,
                        pred,
                    );
                }
            }
        }
    }
}
