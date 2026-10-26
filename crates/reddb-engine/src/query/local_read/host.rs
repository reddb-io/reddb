use super::{EvalError, LocalReadError};
use crate::{
    catalog::schema::ResolvedColumnRule,
    query::{
        evaluator,
        logical_plan::{AccessPathDecision, CanonicalLogicalNode, CardinalityEstimate},
        planning::TablePlanningHost,
        runtime::{
            filter::resolve_filter_row_value,
            ordering::{sort_records_by_order_by_with_host, OrderingHost},
            projection::{project_runtime_record_with_host, ProjectionHost},
            table_plan::TablePlanHost,
        },
        unified::UnifiedRecord,
    },
};
use reddb_rql::ast::{Expr, FieldRef, Filter, Projection, TableQuery};
use reddb_types::Value;
use std::cell::RefCell;

pub(crate) struct ReadHost<'a> {
    query: &'a TableQuery,
    columns: &'a [ResolvedColumnRule],
    projections: &'a [Projection],
    records: RefCell<Option<Vec<UnifiedRecord>>>,
    record_count: usize,
    error: RefCell<Option<EvalError>>,
}
impl<'a> ReadHost<'a> {
    pub(crate) fn new(
        query: &'a TableQuery,
        columns: &'a [ResolvedColumnRule],
        projections: &'a [Projection],
        records: Vec<UnifiedRecord>,
    ) -> Self {
        let record_count = records.len();
        Self {
            query,
            columns,
            projections,
            records: RefCell::new(Some(records)),
            record_count,
            error: RefCell::new(None),
        }
    }
    pub(crate) fn evaluate(
        &self,
        expression: &Expr,
        record: &UnifiedRecord,
    ) -> Result<Value, EvalError> {
        evaluator::evaluate(expression, &|field: &FieldRef| {
            resolve_filter_row_value(
                record,
                field,
                Some(&self.query.table),
                self.query.alias.as_deref().or(Some(&self.query.table)),
                &plaintext,
                |collection, name| {
                    collection == self.query.table
                        && self
                            .columns
                            .iter()
                            .any(|column| column.name == name && !column.not_null)
                },
            )
        })
    }
    fn take_error(&self) -> Result<(), LocalReadError> {
        match self.error.borrow_mut().take() {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }
}
fn plaintext(value: Value) -> Option<Value> {
    if matches!(value, Value::Secret(_) | Value::Password(_)) {
        None
    } else {
        Some(value)
    }
}

impl TablePlanningHost for ReadHost<'_> {
    fn access_path(&self, _: &TableQuery) -> AccessPathDecision {
        AccessPathDecision {
            path: "table_scan",
            index_hint: None,
            reason: "local committed resident scan".into(),
            warning: None,
        }
    }
    fn scan_cardinality(&self, _: &TableQuery, _: &AccessPathDecision) -> CardinalityEstimate {
        CardinalityEstimate::full_scan(self.record_count as f64)
    }
    fn uses_document_path_filter(&self, _: &TableQuery) -> bool {
        false
    }
    fn filter_cardinality(&self, _: &TableQuery, _: bool) -> CardinalityEstimate {
        CardinalityEstimate::full_scan(self.record_count as f64)
    }
    fn is_document_projection(&self, _: &TableQuery) -> bool {
        false
    }
}

impl TablePlanHost for ReadHost<'_> {
    type Error = LocalReadError;
    fn scan(&self, _: &CanonicalLogicalNode) -> Result<Vec<UnifiedRecord>, Self::Error> {
        Ok(self
            .records
            .borrow_mut()
            .take()
            .expect("admitted unary table plan scans once"))
    }
    fn try_filter_lookup(&self) -> Result<Option<Vec<UnifiedRecord>>, Self::Error> {
        Ok(None)
    }
    fn filter_records(
        &self,
        records: Vec<UnifiedRecord>,
        _: bool,
    ) -> Result<Vec<UnifiedRecord>, Self::Error> {
        let Some(predicate) = &self.query.where_expr else {
            return Ok(records);
        };
        let mut output = Vec::with_capacity(records.len());
        for record in records {
            match self.evaluate(predicate, &record)? {
                Value::Boolean(true) => output.push(record),
                Value::Boolean(false) | Value::Null => {}
                _ => {
                    return Err(LocalReadError::Query(
                        "WHERE must evaluate to boolean or NULL".into(),
                    ))
                }
            }
        }
        Ok(output)
    }
    fn sort_records(
        &self,
        _: &CanonicalLogicalNode,
        records: &mut Vec<UnifiedRecord>,
    ) -> Result<(), Self::Error> {
        sort_records_by_order_by_with_host(
            self,
            records,
            &self.query.order_by,
            Some(&self.query.table),
            self.query.alias.as_deref(),
        );
        self.take_error()
    }
    fn rank_records(&self, _: &mut Vec<UnifiedRecord>) {
        unreachable!("universal/ranked queries are rejected at admission")
    }
    fn project_records(
        &self,
        _: &CanonicalLogicalNode,
        records: Vec<UnifiedRecord>,
    ) -> Result<Vec<UnifiedRecord>, Self::Error> {
        let output = records
            .iter()
            .map(|record| {
                project_runtime_record_with_host(
                    self,
                    record,
                    self.projections,
                    Some(&self.query.table),
                    self.query.alias.as_deref(),
                    false,
                    false,
                )
            })
            .collect::<Result<_, _>>()?;
        self.take_error()?;
        Ok(output)
    }
    fn query_error(&self, message: String) -> Self::Error {
        LocalReadError::Query(message)
    }
}

impl ProjectionHost for ReadHost<'_> {
    type Error = LocalReadError;
    fn canonical_expression(&self, projection_index: usize) -> Option<&Expr> {
        match self.query.select_items.get(projection_index)? {
            reddb_rql::ast::SelectItem::Expr { expr, .. } => Some(expr),
            reddb_rql::ast::SelectItem::Wildcard => None,
        }
    }
    fn evaluate_expression(
        &self,
        expression: &Expr,
        record: &UnifiedRecord,
    ) -> Result<Value, EvalError> {
        self.evaluate(expression, record)
    }
    fn evaluate_filter(&self, _: &Filter, _: &UnifiedRecord) -> bool {
        false
    }
    fn evaluate_function(&self, _: &str, _: &[Projection], _: &UnifiedRecord) -> Option<Value> {
        None
    }
    fn evaluate_literal(&self, column: &str, _: &UnifiedRecord) -> Option<Value> {
        crate::query::runtime::projection_value::decode_projection_literal(column)
    }
    fn evaluation_error(&self, error: EvalError, _: bool) -> Self::Error {
        error.into()
    }
    fn secret_query_output(&self, _: Value) -> Result<Value, Self::Error> {
        Err(LocalReadError::Unsupported(
            "secret expressions are not admitted by local SELECT",
        ))
    }
}
impl OrderingHost for ReadHost<'_> {
    fn evaluate_expression(&self, expression: &Expr, record: &UnifiedRecord) -> Option<Value> {
        match self.evaluate(expression, record) {
            Ok(value) => Some(value),
            Err(error) => {
                self.error.borrow_mut().get_or_insert(error);
                None
            }
        }
    }
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        plaintext(value)
    }
    fn record_identity_key(&self, record: &UnifiedRecord) -> String {
        record
            .get("rid")
            .map(Value::display_string)
            .unwrap_or_default()
    }
    fn topk_disabled(&self) -> bool {
        true
    }
}
