//! Experimental storage-free SELECT execution using the native scalar evaluator.
//! Storage-backed statements must go through the full runtime, not this entrypoint.
#![deny(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

use reddb_rql::ast::{FieldRef, QueryExpr, SelectItem};
use reddb_rql::sql_lowering::effective_table_projections;
use reddb_types::Value;

use super::evaluator;
use super::labels::projection_name;

#[derive(Debug)]
pub struct ScalarResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl ScalarResult {
    /// Preserve RedDB's exact integer and decimal tags at the JS/JSON boundary.
    pub fn to_json(&self) -> reddb_types::json::Value {
        reddb_types::json!({
            "columns": self.columns,
            "rows": self.rows.iter().map(|row| {
                row.iter().map(Value::to_json).collect::<Vec<_>>()
            }).collect::<Vec<_>>()
        })
    }
}

#[derive(Debug)]
pub enum ScalarError {
    Parse(reddb_rql::ParseError),
    Unsupported,
    Evaluation(evaluator::EvalError),
}

impl std::fmt::Display for ScalarError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(formatter),
            Self::Unsupported => formatter.write_str(
                "only storage-free scalar SELECT statements are available in this experimental executor",
            ),
            Self::Evaluation(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ScalarError {}

/// Execute a bounded, canonical RQL scalar statement. Unsupported clauses are
/// rejected explicitly so they cannot silently turn into unconditional reads.
pub fn execute(sql: &str) -> Result<ScalarResult, ScalarError> {
    let statement = reddb_rql::parse_frontend(sql).map_err(ScalarError::Parse)?;
    let QueryExpr::Table(query) = statement.into_query_expr() else {
        return Err(ScalarError::Unsupported);
    };
    if query.table != "any"
        || query.source.is_some()
        || query.alias.is_some()
        || query.select_items.is_empty()
        || query.where_expr.is_some()
        || query.filter.is_some()
        || !query.group_by_exprs.is_empty()
        || !query.group_by.is_empty()
        || query.having_expr.is_some()
        || query.having.is_some()
        || !query.order_by.is_empty()
        || query.limit_param.is_some()
        || query.offset_param.is_some()
        || query.expand.is_some()
        || query.as_of.is_some()
        || query.sessionize.is_some()
    {
        return Err(ScalarError::Unsupported);
    }
    let projections = effective_table_projections(&query);
    let empty_row = |_: &FieldRef| None;
    let mut values = Vec::with_capacity(query.select_items.len());
    for item in &query.select_items {
        let SelectItem::Expr { expr, .. } = item else {
            return Err(ScalarError::Unsupported);
        };
        values.push(evaluator::evaluate(expr, &empty_row).map_err(ScalarError::Evaluation)?);
    }
    let rows = if query.limit == Some(0) || query.offset.is_some_and(|offset| offset > 0) {
        Vec::new()
    } else {
        vec![values]
    };
    Ok(ScalarResult {
        columns: projections.iter().map(projection_name).collect(),
        rows,
    })
}
