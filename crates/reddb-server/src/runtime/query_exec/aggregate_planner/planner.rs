//! [`AggregateQueryPlanner::plan`] entry point.
//!
//! Single-column hash GROUP BY. Per-row work is:
//!
//! 1. Fetch next [`super::ScanRow`] from the iterator.
//! 2. Compute the canonical key from `row.group_key`.
//! 3. Find-or-insert the [`super::GroupAccumulator`].
//! 4. Fold the row into the accumulator.
//!
//! The full per-row record is never materialised — only the
//! aggregate inputs the AST referenced. After the scan ends every
//! group is finalised (one [`Value::*`] per aggregate slot) and
//! returned as an [`super::AggregateRowStream`].

use std::collections::HashMap;
use std::collections::HashSet;

use super::accumulator::GroupAccumulator;
use super::ast::{AggregateOp, AggregateQueryAst, PlanError};
use super::scan::{AggregateRow, AggregateRowStream, ScanIterator};
use crate::storage::schema::{value_to_canonical_key, CanonicalKey, Value};

/// Push-down GROUP BY planner — see module header.
pub struct AggregateQueryPlanner;

impl AggregateQueryPlanner {
    /// Plan a GROUP BY query against the supplied scan iterator.
    /// Returns a stream of one row per distinct group with the
    /// configured aggregates evaluated.
    pub fn plan<S: ScanIterator>(
        ast: &AggregateQueryAst,
        mut scan: S,
    ) -> Result<AggregateRowStream, PlanError> {
        validate_ast(ast)?;

        // Per-group state. Keyed on canonical encoding of the
        // GROUP BY value — `Value` itself is not `Eq` for floats
        // and generally messy to use as a key.
        let mut groups: HashMap<GroupKey, (Value, GroupAccumulator)> = HashMap::new();

        while let Some(row) = scan.next_row() {
            let key = canonical_group_key(&row.group_key);
            let entry = groups.entry(key).or_insert_with(|| {
                (
                    row.group_key.clone(),
                    GroupAccumulator::new(&ast.aggregates),
                )
            });
            entry.1.accumulate(&ast.aggregates, &row.agg_inputs);
        }

        let mut emitted = Vec::with_capacity(groups.len());
        for (_, (group_value, acc)) in groups {
            let aggregate_values = acc.finalize();
            emitted.push(AggregateRow {
                group_key: group_value,
                aggregate_values,
            });
        }

        Ok(AggregateRowStream::from_rows(emitted))
    }
}

/// Plan-time validation. Cheap, runs once before the scan.
fn validate_ast(ast: &AggregateQueryAst) -> Result<(), PlanError> {
    if ast.aggregates.is_empty() {
        return Err(PlanError::NoAggregates);
    }
    let mut seen = HashSet::with_capacity(ast.aggregates.len());
    for agg in &ast.aggregates {
        if !seen.insert(agg.output_name.as_str()) {
            return Err(PlanError::DuplicateOutputName(agg.output_name.clone()));
        }
    }
    Ok(())
}

/// Hashable group key. `CanonicalKey` covers most types; the
/// fallback for non-canonicalisable values (non-finite floats,
/// vectors) is a sentinel — they can still group, they just can't
/// be range-indexed. The fallback is mostly defensive: production
/// queries rarely group by floats.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum GroupKey {
    Canonical(CanonicalKey),
    /// Best-effort key for `Value`s that don't canonicalise. The
    /// `String` is the value's display rendering — collisions are
    /// possible but vanishingly unlikely for real data; this only
    /// fires for vector / non-finite-float groupings, which the
    /// scope notes flag as out-of-scope anyway.
    Fallback(String),
}

fn canonical_group_key(value: &Value) -> GroupKey {
    match value_to_canonical_key(value) {
        Some(k) => GroupKey::Canonical(k),
        None => GroupKey::Fallback(format!("{:?}", value)),
    }
}

/// Reachable from sibling files — the dispatcher needs to know
/// whether an [`AggregateOp`] is supported before lowering.
pub(crate) fn op_is_supported(op: AggregateOp) -> bool {
    matches!(
        op,
        AggregateOp::CountStar
            | AggregateOp::CountColumn
            | AggregateOp::Sum
            | AggregateOp::Avg
            | AggregateOp::Min
            | AggregateOp::Max
    )
}
