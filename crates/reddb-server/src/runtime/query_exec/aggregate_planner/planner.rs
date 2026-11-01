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
use super::scan::{AggregateRow, AggregateRowStream, ScanIterator, ScanRow};
use reddb_types::{value_to_canonical_key, CanonicalKey, Value};

/// Push-down GROUP BY accumulator. Retains one state per group, not per input.
pub struct AggregateQueryPlanner<'a> {
    ast: &'a AggregateQueryAst,
    groups: HashMap<GroupKey, (Value, GroupAccumulator)>,
    groups_max: usize,
}

impl<'a> AggregateQueryPlanner<'a> {
    pub fn new(ast: &'a AggregateQueryAst, groups_max: usize) -> Result<Self, PlanError> {
        validate_ast(ast)?;
        Ok(Self {
            ast,
            groups: HashMap::new(),
            groups_max,
        })
    }

    /// Fold an input immediately. Reject a new group before allocating its
    /// accumulator, so a cardinality ceiling stops the source scan itself.
    pub fn push(&mut self, row: ScanRow) -> Result<(), PlanError> {
        let key = canonical_group_key(&row.group_key);
        let count_next = self.groups.len().saturating_add(1);
        let entry = match self.groups.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                if count_next > self.groups_max {
                    return Err(PlanError::GroupLimitExceeded {
                        limit: self.groups_max,
                        current: count_next,
                    });
                }
                entry.insert((row.group_key, GroupAccumulator::new(&self.ast.aggregates)))
            }
        };
        entry.1.accumulate(&self.ast.aggregates, &row.agg_inputs);
        Ok(())
    }

    pub fn finish(self) -> AggregateRowStream {
        let mut emitted = Vec::with_capacity(self.groups.len());
        for (_, (group_value, acc)) in self.groups {
            emitted.push(AggregateRow {
                group_key: group_value,
                aggregate_values: acc.finalize(),
            });
        }
        AggregateRowStream::from_rows(emitted)
    }

    /// Iterator adapter for callers whose source is already pull-based.
    pub fn plan<S: ScanIterator>(
        ast: &'a AggregateQueryAst,
        mut scan: S,
    ) -> Result<AggregateRowStream, PlanError> {
        let mut planner = Self::new(ast, usize::MAX)?;
        while let Some(row) = scan.next_row() {
            planner.push(row)?;
        }
        Ok(planner.finish())
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
