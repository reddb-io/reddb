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
pub(in crate::runtime) struct RecordRow<'a> {
    pub(in crate::runtime) db: Option<&'a RedDB>,
    pub(in crate::runtime) record: &'a UnifiedRecord,
    pub(in crate::runtime) table_name: Option<&'a str>,
    pub(in crate::runtime) table_alias: Option<&'a str>,
}

impl crate::storage::query::evaluator::Row for RecordRow<'_> {
    fn get(&self, field: &FieldRef) -> Option<Value> {
        self.resolve(
            field,
            &crate::runtime::execution_context::secret_query_input,
        )
    }

    fn try_get(
        &self,
        field: &FieldRef,
    ) -> Result<Option<Value>, crate::storage::query::evaluator::EvalError> {
        self.resolve(field, &Some)
            .map(crate::runtime::execution_context::try_secret_query_input)
            .transpose()
            .map(|value| value.flatten().or_else(|| self.resolve(field, &|_| None)))
            .map_err(
                |limit| crate::storage::query::evaluator::EvalError::ReadLimitExceeded {
                    required_bytes: limit.required_bytes,
                    limit_bytes: limit.limit_bytes,
                },
            )
    }
}

impl RecordRow<'_> {
    fn resolve(
        &self,
        field: &FieldRef,
        secret_input: &impl Fn(Value) -> Option<Value>,
    ) -> Option<Value> {
        reddb_engine::query::runtime::filter::resolve_filter_row_value(
            self.record,
            field,
            self.table_name,
            self.table_alias,
            secret_input,
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
                super::expr_eval::try_evaluate_runtime_expr_with_db(
                    self.db,
                    expr,
                    self.record,
                    self.table_name,
                    self.table_alias,
                )
            }
            Err(error) => Err(crate::RedDBError::Query(
                if expression_uses_secret(expr, self.record, self.table_name, self.table_alias)
                    && !matches!(
                        &error,
                        crate::storage::query::evaluator::EvalError::ReadLimitExceeded { .. }
                    )
                {
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
