//! Runtime filter evaluation and LIKE/metadata helpers.
use super::*;

pub(in crate::runtime) fn evaluate_runtime_filter(
    record: &UnifiedRecord,
    filter: &Filter,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    evaluate_runtime_filter_with_db(None, record, filter, table_name, table_alias)
}

/// Row adapter for the typed scalar evaluator — bridges `UnifiedRecord`
/// to `crate::storage::query::evaluator::Row` so that `evaluator::evaluate`
/// can look up columns via the same `resolve_runtime_field` path used by
/// the rest of the runtime filter evaluation.
pub(in crate::runtime::join_filter) struct RecordRow<'a> {
    pub(in crate::runtime::join_filter) db: Option<&'a RedDB>,
    pub(in crate::runtime::join_filter) record: &'a UnifiedRecord,
    pub(in crate::runtime::join_filter) table_name: Option<&'a str>,
    pub(in crate::runtime::join_filter) table_alias: Option<&'a str>,
}

impl crate::storage::query::evaluator::Row for RecordRow<'_> {
    fn get(&self, field: &FieldRef) -> Option<Value> {
        reddb_engine::query::runtime::filter::resolve_filter_row_value(
            self.record,
            field,
            self.table_name,
            self.table_alias,
            &crate::runtime::execution_context::secret_query_input,
            |collection, column| {
                self.db
                    .and_then(|db| db.collection_contract_arc(collection))
                    .is_some_and(|contract| {
                        contract
                            .declared_columns
                            .iter()
                            .any(|declared| declared.name == column && !declared.not_null)
                    })
            },
        )
    }
}

pub(in crate::runtime) fn evaluate_runtime_filter_with_db(
    db: Option<&RedDB>,
    record: &UnifiedRecord,
    filter: &Filter,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    evaluate_runtime_filter_result_with_db(db, record, filter, table_name, table_alias)
        .unwrap_or(false)
}

pub(in crate::runtime) fn evaluate_runtime_filter_result_with_db(
    db: Option<&RedDB>,
    record: &UnifiedRecord,
    filter: &Filter,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> crate::RedDBResult<bool> {
    let host = RuntimeFilterHost {
        db,
        record,
        table_name,
        table_alias,
    };
    reddb_engine::query::runtime::filter::evaluate_runtime_filter_result_with_host(
        &host,
        record,
        filter,
        table_name,
        table_alias,
    )
}

struct RuntimeFilterHost<'a> {
    db: Option<&'a RedDB>,
    record: &'a UnifiedRecord,
    table_name: Option<&'a str>,
    table_alias: Option<&'a str>,
}
impl reddb_engine::query::runtime::filter::FilterHost for RuntimeFilterHost<'_> {
    type Error = crate::RedDBError;
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        crate::runtime::execution_context::secret_query_input(value)
    }
    fn evaluate_expression(
        &self,
        expr: &reddb_rql::ast::Expr,
    ) -> crate::RedDBResult<Option<Value>> {
        let row = RecordRow {
            db: self.db,
            record: self.record,
            table_name: self.table_name,
            table_alias: self.table_alias,
        };
        match crate::runtime::expr_eval::evaluate_typed_expr_with_secret_literals(expr, &row) {
            Ok(value) => Ok(Some(value)),
            Err(crate::storage::query::evaluator::EvalError::UnknownFunction { .. }) => {
                Ok(super::expr_eval::evaluate_runtime_expr_with_db(
                    self.db,
                    expr,
                    self.record,
                    self.table_name,
                    self.table_alias,
                ))
            }
            Err(error) => Err(crate::RedDBError::Query(
                if expression_uses_secret(expr, self.record, self.table_name, self.table_alias) {
                    "secret comparison evaluation failed".into()
                } else {
                    error.to_string()
                },
            )),
        }
    }
}

pub(in crate::runtime) use reddb_engine::query::runtime::filter::{
    evaluate_metadata_field_compare, evaluate_metadata_field_in, like_matches, like_matches_bytes,
};
