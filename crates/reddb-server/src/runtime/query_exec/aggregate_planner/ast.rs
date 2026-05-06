//! Minimal AST consumed by [`super::AggregateQueryPlanner`].
//!
//! Intentionally narrow: the planner only handles the subset of
//! aggregate queries described in the module header. The wider SQL
//! AST lives in [`crate::storage::query::ast`]; the dispatch site
//! lowers the relevant subset into this shape and falls back to the
//! legacy path otherwise.

use crate::storage::schema::Value;

/// Supported aggregate operations.
///
/// `STDDEV`, `PERCENTILE`, `GROUP_CONCAT`, etc. are deliberately
/// absent — see the module header. Adding a variant here also
/// requires a matching `Accumulator` arm in
/// `accumulator.rs::Accumulator::accumulate` and `finalize`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateOp {
    /// `COUNT(*)` — counts every row that reaches the accumulator
    /// (NULL or not).
    CountStar,
    /// `COUNT(col)` — counts rows where the input is non-NULL.
    CountColumn,
    /// `SUM(col)` over numeric input. NULLs ignored.
    Sum,
    /// `AVG(col)` — internally tracks `(sum, count)`; finalised
    /// at emission. NULL when the group has no numeric input.
    Avg,
    /// `MIN(col)` — running extremum, type-preserving.
    Min,
    /// `MAX(col)` — running extremum, type-preserving.
    Max,
}

/// One aggregate column in the SELECT list.
///
/// `output_name` is the column name the planner stamps onto each
/// result row, so the dispatcher above can match positions back to
/// the user-visible projection list without extra plumbing.
#[derive(Debug, Clone)]
pub struct AggregateExpr {
    pub op: AggregateOp,
    /// Index into [`super::ScanRow::agg_inputs`] for this aggregate.
    /// `CountStar` ignores this — by convention the dispatch layer
    /// stores `0` and never reads the slot.
    pub input_index: usize,
    pub output_name: String,
}

/// AST consumed by the planner.
///
/// Single GROUP BY column, single output row per distinct group.
/// `group_by_output_name` is what the planner labels the GROUP BY
/// column on the emitted row.
#[derive(Debug, Clone)]
pub struct AggregateQueryAst {
    pub group_by_output_name: String,
    pub aggregates: Vec<AggregateExpr>,
}

/// Errors the planner surfaces for inputs it cannot handle. The
/// dispatch site treats any of these as "fall back to the legacy
/// path".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// Aggregate list is empty — at least one expression is required.
    NoAggregates,
    /// Output name collision (duplicate column name in the AST).
    DuplicateOutputName(String),
    /// Aggregate references an input slot that the scan iterator
    /// does not expose (defensive — should never fire in production).
    InputIndexOutOfRange { aggregate: String, index: usize },
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::NoAggregates => write!(f, "aggregate plan has no aggregates"),
            PlanError::DuplicateOutputName(n) => write!(f, "duplicate aggregate output name: {n}"),
            PlanError::InputIndexOutOfRange { aggregate, index } => write!(
                f,
                "aggregate `{aggregate}` references input slot {index} which the scan does not expose",
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// Helper for the dispatch site: aggregates always emit a single
/// scalar `Value`, never structured data.
pub type AggregateValue = Value;
