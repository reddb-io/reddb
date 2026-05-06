//! Push-down GROUP BY query planner — issue #161.
//!
//! Deep module: the legacy aggregate path
//! ([`super::aggregate::execute_aggregate_query`]) materialises every
//! row before evaluating accumulators. For the common shape
//!
//! ```text
//! SELECT g, AGG(c1), AGG(c2), … FROM t GROUP BY g
//! ```
//!
//! this is wasteful — the per-row `UnifiedRecord` lives only long
//! enough to be folded into a per-group accumulator and then
//! dropped. On a 10 000-row × 50-group fixture the legacy path
//! materialises 10 000 records; the planner here materialises ~50
//! (one per group, only at the emission boundary).
//!
//! ## Public surface
//!
//! [`AggregateQueryPlanner::plan`] takes a [`AggregateQueryAst`] and a
//! [`ScanIterator`] and returns an [`AggregateRowStream`]. Everything
//! else (accumulator state, key hashing, slot dispatch) is private.
//!
//! ## Scope (first cut)
//!
//! - Single-column `GROUP BY` only.
//! - Aggregates: `COUNT(*)`, `COUNT(col)`, `SUM`, `AVG`, `MIN`, `MAX`.
//! - Hash-based grouping (sorted-input streaming is a follow-up).
//! - Multi-column GROUP BY, `STDDEV`, `PERCENTILE`, etc. fall back
//!   to the legacy path via the dispatch in
//!   [`crate::runtime::query_exec::aggregate`].
//!
//! ## Materialisation invariant
//!
//! In `cfg(test)` builds the module increments
//! [`MATERIALIZED_COUNT`] every time it produces a final
//! per-group row. The push-down path emits *one* row per distinct
//! group, never per scanned row. This is asserted in the test
//! fixture under `tests.rs`.

mod accumulator;
mod ast;
mod planner;
mod scan;

#[cfg(test)]
mod tests;

pub use ast::{AggregateExpr, AggregateOp, AggregateQueryAst, PlanError};
pub use planner::AggregateQueryPlanner;
pub use scan::{AggregateRow, AggregateRowStream, ScanIterator, ScanRow};

#[cfg(test)]
pub(crate) use accumulator::MATERIALIZED_COUNT;
