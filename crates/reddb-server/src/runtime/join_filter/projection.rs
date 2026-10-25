//! Native projection adapters retain expression effects and secret output policy.
use super::*;

pub(in crate::runtime) fn project_runtime_record(
    source: &UnifiedRecord,
    projections: &[Projection],
    table_name: Option<&str>,
    table_alias: Option<&str>,
    document_projection: bool,
    entity_projection: bool,
) -> UnifiedRecord {
    project_runtime_record_with_db(
        None,
        source,
        projections,
        table_name,
        table_alias,
        document_projection,
        entity_projection,
    )
    .unwrap_or_else(|_| UnifiedRecord::new())
}

pub(in crate::runtime) fn project_runtime_record_with_db(
    db: Option<&RedDB>,
    source: &UnifiedRecord,
    projections: &[Projection],
    table_name: Option<&str>,
    table_alias: Option<&str>,
    document_projection: bool,
    entity_projection: bool,
) -> crate::RedDBResult<UnifiedRecord> {
    let host = RuntimeProjectionHost {
        db,
        table_name,
        table_alias,
    };
    reddb_engine::query::runtime::projection::project_runtime_record_with_host(
        &host,
        source,
        projections,
        table_name,
        table_alias,
        document_projection,
        entity_projection,
    )
}

pub(in crate::runtime) use reddb_engine::query::labels::{field_ref_name, projection_name};
pub(in crate::runtime) use reddb_engine::query::runtime::projection::{
    collect_visible_columns, expression_uses_secret, filter_uses_secret, projected_columns,
    projection_uses_secret, visible_value_keys,
};

struct RuntimeProjectionHost<'a> {
    db: Option<&'a RedDB>,
    table_name: Option<&'a str>,
    table_alias: Option<&'a str>,
}
impl reddb_engine::query::runtime::projection::ProjectionHost for RuntimeProjectionHost<'_> {
    type Error = crate::RedDBError;
    fn evaluate_expression(
        &self,
        expression: &reddb_rql::ast::Expr,
        source: &UnifiedRecord,
    ) -> Result<Value, crate::storage::query::evaluator::EvalError> {
        let row = RecordRow {
            db: self.db,
            record: source,
            table_name: self.table_name,
            table_alias: self.table_alias,
        };
        crate::runtime::expr_eval::evaluate_typed_expr_with_secret_literals(expression, &row)
    }
    fn evaluate_filter(&self, filter: &Filter, source: &UnifiedRecord) -> bool {
        evaluate_runtime_filter_with_db(self.db, source, filter, self.table_name, self.table_alias)
    }
    fn evaluate_function(
        &self,
        name: &str,
        arguments: &[Projection],
        source: &UnifiedRecord,
    ) -> Option<Value> {
        evaluate_scalar_function_with_db(self.db, name, arguments, source)
    }
    fn evaluate_literal(&self, column: &str, _source: &UnifiedRecord) -> Option<Value> {
        reddb_engine::query::runtime::projection_value::decode_projection_literal(column)
    }
    fn evaluation_error(
        &self,
        error: crate::storage::query::evaluator::EvalError,
        sensitive: bool,
    ) -> Self::Error {
        crate::RedDBError::Query(if sensitive {
            "secret expression evaluation failed".into()
        } else {
            error.to_string()
        })
    }
    fn secret_query_output(&self, value: Value) -> crate::RedDBResult<Value> {
        crate::runtime::execution_context::secret_query_output(value)
    }
}
