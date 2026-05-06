//! Scan iterator contract for [`super::AggregateQueryPlanner`].
//!
//! The dispatcher wraps the production entity scan in a
//! [`ScanIterator`] that yields per-row values *without* building a
//! full `UnifiedRecord` — that's the entire point of the push-down.
//!
//! Tests live in `tests.rs` and use a trivial `Vec<ScanRow>` adapter
//! to exercise the planner deterministically.

use crate::storage::schema::Value;

/// One row delivered by [`ScanIterator::next_row`].
///
/// `group_key` is the value of the single GROUP BY column for this
/// row (typed — `Value::Null` is a legal grouping key, matching SQL
/// "everything-NULL" group semantics).
///
/// `agg_inputs` is positional: aggregate at AST index `i` reads
/// `agg_inputs[i]`. The `CountStar` aggregate ignores its slot.
#[derive(Debug, Clone)]
pub struct ScanRow {
    pub group_key: Value,
    pub agg_inputs: Vec<Value>,
}

/// One emitted row from the planner: GROUP BY value plus the
/// finalised aggregate per AST position.
///
/// The dispatcher converts this into the surrounding `UnifiedRecord`
/// shape — keeping the planner's output type narrow means the unit
/// tests don't have to deal with the wider record machinery.
#[derive(Debug, Clone)]
pub struct AggregateRow {
    pub group_key: Value,
    /// Parallel to [`super::AggregateQueryAst::aggregates`].
    pub aggregate_values: Vec<Value>,
}

/// Producer side of the push-down.
///
/// A single `next_row()` call returns the next row or `None` at
/// end-of-stream. Errors propagate through the planner's `Result`
/// return — the trait itself is fail-fast (returning `None`).
///
/// Implementors are free to stream, prefetch, or block — the
/// planner only consumes one row at a time and never re-reads.
pub trait ScanIterator {
    fn next_row(&mut self) -> Option<ScanRow>;
}

/// Result type emitted by [`super::AggregateQueryPlanner::plan`].
///
/// Materialised eagerly today — the per-group state is finalised
/// after the scan completes, so streaming would require either a
/// sorted scan (follow-up) or splitting accumulation from emission
/// (also follow-up). Both are explicit non-goals in this slice.
#[derive(Debug, Clone)]
pub struct AggregateRowStream {
    rows: Vec<AggregateRow>,
}

impl AggregateRowStream {
    pub(super) fn from_rows(rows: Vec<AggregateRow>) -> Self {
        Self { rows }
    }

    /// Number of distinct groups emitted.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Consume the stream into the underlying `Vec`. Order is
    /// HashMap iteration order today — the dispatcher applies any
    /// `ORDER BY` afterwards.
    pub fn into_vec(self) -> Vec<AggregateRow> {
        self.rows
    }

    pub fn iter(&self) -> std::slice::Iter<'_, AggregateRow> {
        self.rows.iter()
    }
}

impl IntoIterator for AggregateRowStream {
    type Item = AggregateRow;
    type IntoIter = std::vec::IntoIter<AggregateRow>;

    fn into_iter(self) -> Self::IntoIter {
        self.rows.into_iter()
    }
}
