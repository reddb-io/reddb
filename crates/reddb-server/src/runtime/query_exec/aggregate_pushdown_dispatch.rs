//! Glue between [`super::aggregate::execute_aggregate_query`] and
//! the new [`super::aggregate_planner::AggregateQueryPlanner`].
//!
//! Lives in its own file so the legacy aggregate executor in
//! `aggregate.rs` stays untouched apart from one early-return —
//! issue #161 explicitly carves out the legacy path as untouchable
//! while Lane A is rewriting `UnifiedRecord` call sites.
//!
//! Eligibility check is intentionally narrow:
//!
//! - Single column GROUP BY by a simple `Expr::Column` (no
//!   functions, no expressions, no `TIME_BUCKET`).
//! - Every aggregate projection is `COUNT(*)`, `COUNT(col)`, `SUM`,
//!   `AVG`, `MIN`, `MAX` over a simple column reference.
//! - No HAVING, no ORDER BY, no aliasing the GROUP BY column
//!   through arbitrary expressions, no WITH EXPAND.
//!
//! Anything outside this envelope returns `Ok(None)` and the
//! caller continues into the legacy path. False negatives are
//! always safe; false positives would be incorrect.

use super::aggregate_planner::{
    AggregateExpr, AggregateOp, AggregateQueryAst, AggregateQueryPlanner, ScanIterator, ScanRow,
};
use super::filter_compiled::{classify_field, resolve_kind, CompiledEntityFilter, EntityFieldKind};
use crate::api::{RedDBError, RedDBResult};
use crate::runtime::table_row_mvcc_resolver::TableRowMvccReadResolver;
use crate::storage::query::ast::{Expr, FieldRef, Projection};
use crate::storage::query::sql_lowering::{
    effective_table_filter, effective_table_group_by_exprs, effective_table_having_filter,
    effective_table_projections,
};
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::storage::schema::Value;
use crate::RedDB;

use super::TableQuery;

/// One aggregate slot we lowered out of the SQL AST plus the
/// resolver that reads its input from a `UnifiedEntity`. The
/// resolver is built once at plan time so the per-row scan does no
/// string compares.
struct LoweredAggregate {
    expr: AggregateExpr,
    /// `None` for `COUNT(*)` — there is no input column.
    input_kind: Option<EntityFieldKind>,
}

/// Try to execute `query` via the push-down planner. Returns
/// `Ok(None)` when the AST shape is outside the supported envelope
/// — the caller (`execute_aggregate_query`) then runs the legacy
/// materialise-all path.
pub(super) fn try_execute_pushdown_aggregate(
    db: &RedDB,
    query: &TableQuery,
) -> RedDBResult<Option<UnifiedResult>> {
    // ── Eligibility gate ─────────────────────────────────────────
    if query.expand.is_some() {
        return Ok(None);
    }
    if !query.order_by.is_empty() {
        return Ok(None);
    }
    if effective_table_having_filter(query).is_some() {
        return Ok(None);
    }

    let group_by = effective_table_group_by_exprs(query);
    if group_by.len() != 1 {
        return Ok(None);
    }
    let Some((group_col_name, group_label)) = simple_column_group_by(&group_by[0]) else {
        return Ok(None);
    };

    let projections = effective_table_projections(query);
    let Some(plan_pieces) = lower_projections(&projections, &group_label, &group_col_name) else {
        return Ok(None);
    };

    // ── Build resolver kinds ────────────────────────────────────
    let table_name = query.table.as_str();
    let table_alias = query.alias.as_deref().unwrap_or(table_name);

    let group_field = FieldRef::TableColumn {
        table: String::new(),
        column: group_col_name.clone(),
    };
    let group_kind = classify_field(&group_field, table_name, table_alias);
    if !is_fast_kind(&group_kind) {
        return Ok(None);
    }

    let mut lowered: Vec<LoweredAggregate> = Vec::with_capacity(plan_pieces.aggregates.len());
    for piece in plan_pieces.aggregates {
        let input_kind = match piece.input_column.as_deref() {
            None => None,
            Some(col) => {
                let field = FieldRef::TableColumn {
                    table: String::new(),
                    column: col.to_string(),
                };
                let kind = classify_field(&field, table_name, table_alias);
                if !is_fast_kind(&kind) {
                    return Ok(None);
                }
                Some(kind)
            }
        };
        lowered.push(LoweredAggregate {
            expr: piece.expr,
            input_kind,
        });
    }

    // ── Build the AST the planner consumes ──────────────────────
    let ast = AggregateQueryAst {
        group_by_output_name: group_label.clone(),
        aggregates: lowered.iter().map(|l| l.expr.clone()).collect(),
    };

    // ── Pre-collect rows from the entity scan into ScanRow ──────
    //
    // We collect into a Vec<ScanRow> rather than streaming through
    // a callback iterator. The win we care about is "how many full
    // `UnifiedRecord`s are materialised?" — and the answer here is
    // O(group count), not O(row count). A `ScanRow` holds only the
    // GROUP BY value plus the per-aggregate input values; no
    // HashMap, no system fields.
    let manager = db
        .store()
        .get_collection(query.table.as_str())
        .ok_or_else(|| RedDBError::NotFound(query.table.clone()))?;

    let compiled_filter = effective_table_filter(query)
        .as_ref()
        .map(|f| CompiledEntityFilter::compile(f, table_name, table_alias));

    let mut rows: Vec<ScanRow> = Vec::new();
    let table_row_resolver = TableRowMvccReadResolver::current_statement();
    manager.for_each_entity(|entity| {
        if table_row_resolver.resolve_read_candidate(entity).is_none() {
            return true;
        }
        if let Some(f) = compiled_filter.as_ref() {
            if !f.evaluate(entity) {
                return true;
            }
        }
        let group_key = match resolve_kind(&group_kind, entity) {
            Some(v) => v.into_owned(),
            None => return true,
        };
        let mut agg_inputs: Vec<Value> = Vec::with_capacity(lowered.len());
        for slot in &lowered {
            let value = match &slot.input_kind {
                None => Value::Null,
                Some(kind) => match resolve_kind(kind, entity) {
                    Some(v) => v.into_owned(),
                    None => Value::Null,
                },
            };
            agg_inputs.push(value);
        }
        rows.push(ScanRow {
            group_key,
            agg_inputs,
        });
        true
    });

    let stream = AggregateQueryPlanner::plan(&ast, VecScanIter(rows.into_iter()))
        .map_err(|e| RedDBError::Query(format!("aggregate push-down planner: {e}")))?;

    // ── Project planner output back into UnifiedRecord ──────────
    let mut columns: Vec<String> = Vec::with_capacity(1 + ast.aggregates.len());
    columns.push(group_label.clone());
    for agg in &ast.aggregates {
        columns.push(agg.output_name.clone());
    }

    let group_count = stream.len();
    // Issue #769 — enforce the materialization ceiling on the push-down
    // aggregate path too. `group_count` is the planner's distinct-group
    // cardinality (pre OFFSET/LIMIT), the aggregation's materialized row
    // count for this query.
    crate::runtime::materialization_limit::guard(db, "aggregation", group_count)?;
    let mut records: Vec<UnifiedRecord> = Vec::with_capacity(group_count);
    for row in stream {
        let mut record = UnifiedRecord::new();
        record.set(&group_label, row.group_key.clone());
        // Also set under the original column key so downstream
        // ordering / projection paths that still reference the
        // raw column name find the value (matches the legacy
        // path's two-key insertion).
        if group_label != group_col_name {
            record.set(&group_col_name, row.group_key);
        }
        for (agg, value) in ast.aggregates.iter().zip(row.aggregate_values) {
            record.set(&agg.output_name, value);
        }
        records.push(record);
    }

    // OFFSET / LIMIT applied here so the planner can stay narrow.
    if let Some(offset) = query.offset {
        let offset = offset as usize;
        if offset >= records.len() {
            records.clear();
        } else {
            records.drain(..offset);
        }
    }
    if let Some(limit) = query.limit {
        records.truncate(limit as usize);
    }

    Ok(Some(UnifiedResult {
        columns,
        records,
        stats: Default::default(),
        pre_serialized_json: None,
    }))
}

/// Per-projection lowering result. `input_column` is `None` for
/// `COUNT(*)` since there is no source column.
struct LoweredPiece {
    expr: AggregateExpr,
    input_column: Option<String>,
}

struct LoweredPlan {
    aggregates: Vec<LoweredPiece>,
}

/// Walk the SELECT list. Each item must be either:
///
/// - the GROUP BY column itself (passes through unchanged), or
/// - a supported aggregate function over either `*` or a simple
///   column reference.
///
/// Anything else returns `None` to signal "not eligible".
fn lower_projections(
    projections: &[Projection],
    group_label: &str,
    group_col_name: &str,
) -> Option<LoweredPlan> {
    let mut aggregates: Vec<LoweredPiece> = Vec::new();
    let mut saw_group_column = false;

    for proj in projections {
        match proj {
            // GROUP BY pass-through — labelled either by the
            // canonical column name or by an explicit alias.
            Projection::Column(name) if name == group_col_name || name == group_label => {
                saw_group_column = true;
            }
            Projection::Alias(name, alias)
                if (name == group_col_name || name == group_label) && alias == group_label =>
            {
                saw_group_column = true;
            }
            Projection::Field(field, alias) => {
                match field {
                    FieldRef::TableColumn { column, .. }
                        if column == group_col_name
                            && alias.as_deref().is_none_or(|a| a == group_label) =>
                    {
                        saw_group_column = true;
                        continue;
                    }
                    _ => {}
                }
                return None;
            }
            Projection::Function(name, args) => {
                let lowered = lower_aggregate_function(name, args)?;
                aggregates.push(lowered);
            }
            _ => return None,
        }
    }

    if !saw_group_column {
        // SELECT must include the GROUP BY column for this slice —
        // staying conservative keeps the projection conversion
        // boring.
        return None;
    }
    if aggregates.is_empty() {
        return None;
    }

    // Re-index the aggregate input slots so they match the
    // ScanRow.agg_inputs order (one slot per aggregate, in AST
    // order). The legacy path used a separate slot indirection;
    // for our narrow scope a positional layout is fine.
    for (idx, piece) in aggregates.iter_mut().enumerate() {
        piece.expr.input_index = idx;
    }

    Some(LoweredPlan { aggregates })
}

/// Lower one `Projection::Function(name, args)` into an aggregate
/// piece, or return `None` if the shape is unsupported.
fn lower_aggregate_function(name: &str, args: &[Projection]) -> Option<LoweredPiece> {
    let base = name.split(':').next().unwrap_or(name);
    let upper = base.to_ascii_uppercase();
    let op = match upper.as_str() {
        "COUNT" => match args.first() {
            None => AggregateOp::CountStar,
            Some(Projection::All) => AggregateOp::CountStar,
            Some(Projection::Column(c)) if c == "*" => AggregateOp::CountStar,
            Some(_) => AggregateOp::CountColumn,
        },
        "SUM" => AggregateOp::Sum,
        "AVG" => AggregateOp::Avg,
        "MIN" => AggregateOp::Min,
        "MAX" => AggregateOp::Max,
        _ => return None,
    };

    let input_column = match op {
        AggregateOp::CountStar => None,
        _ => simple_column_arg(args.first()?)?,
    };
    if matches!(op, AggregateOp::CountColumn) {
        // CountColumn also needs a simple column.
        input_column.as_ref()?;
    }

    let output_name = render_aggregate_label(&upper, args);
    Some(LoweredPiece {
        expr: AggregateExpr {
            op,
            // Re-indexed by the caller after collection.
            input_index: 0,
            output_name,
        },
        input_column,
    })
}

/// Returns `Some(column)` if `arg` is a simple column reference.
/// `Some(None)` is reserved for `*` — but only `COUNT(*)` uses
/// that and we route it before reaching here.
fn simple_column_arg(arg: &Projection) -> Option<Option<String>> {
    match arg {
        Projection::Column(c) if c != "*" && !c.starts_with("LIT:") => Some(Some(c.clone())),
        Projection::Field(FieldRef::TableColumn { column, .. }, _) => Some(Some(column.clone())),
        _ => None,
    }
}

/// Build the user-facing aggregate column name. `COUNT(*)` →
/// `"COUNT(*)"`, `SUM(amount)` → `"SUM(amount)"`. Mirrors the
/// labels the legacy `aggregate.rs` path produces, so functional
/// parity tests see the same column names.
fn render_aggregate_label(name: &str, args: &[Projection]) -> String {
    let arg_str = match args.first() {
        None => "*".to_string(),
        Some(Projection::All) => "*".to_string(),
        Some(Projection::Column(c)) => c.clone(),
        Some(Projection::Field(FieldRef::TableColumn { column, .. }, _)) => column.clone(),
        Some(_) => "?".to_string(),
    };
    format!("{name}({arg_str})")
}

/// Recognise GROUP BY shapes the planner can lower. Returns
/// `(canonical_column_name, output_label)` — for now both are the
/// column name; aliased GROUP BYs fall through to the legacy path.
fn simple_column_group_by(expr: &Expr) -> Option<(String, String)> {
    match expr {
        Expr::Column {
            field: FieldRef::TableColumn { column, .. },
            ..
        } => Some((column.clone(), column.clone())),
        _ => None,
    }
}

fn is_fast_kind(kind: &EntityFieldKind) -> bool {
    !matches!(
        kind,
        EntityFieldKind::DocumentPath(_) | EntityFieldKind::Unknown
    )
}

/// Tiny iterator adapter so we can hand a `Vec<ScanRow>` to the
/// planner without exposing a public iterator type from this file.
struct VecScanIter(std::vec::IntoIter<ScanRow>);

impl ScanIterator for VecScanIter {
    fn next_row(&mut self) -> Option<ScanRow> {
        self.0.next()
    }
}
