//! Final-record expressions, evaluated before uniqueness, indexes and publication.
use super::*;
use reddb_rql::ast::FieldRef;
use reddb_rql::schema_expression::SchemaExpression;
use std::collections::BTreeMap;

use reddb_engine::catalog::schema::generated_order;
pub(crate) use reddb_engine::catalog::schema::has_contract_expressions;

pub(crate) fn validate_contract_expressions(contract: &CollectionContract) -> RedDBResult<()> {
    Ok(reddb_engine::catalog::schema::validate_contract_expressions(contract)?)
}

fn expression_error(column: &str, error: impl std::fmt::Display) -> crate::RedDBError {
    crate::RedDBError::Query(format!("column '{column}' expression: {error}"))
}

pub(super) fn apply_contract_expressions(
    contract: &CollectionContract,
    columns: &[ResolvedColumnRule],
    fields: &mut Vec<(String, Value)>,
) -> RedDBResult<()> {
    if !has_contract_expressions(contract) {
        return Ok(());
    }
    let mut values: BTreeMap<String, Value> = columns
        .iter()
        .map(|column| (column.name.clone(), Value::Null))
        .collect();
    values.extend(fields.iter().cloned());
    let evaluate = |expression: &SchemaExpression, values: &BTreeMap<String, Value>| {
        crate::storage::query::evaluator::evaluate(expression.expression(), &|field: &FieldRef| {
            match field {
                FieldRef::TableColumn { table, column } if table.is_empty() => {
                    values.get(column).cloned()
                }
                _ => None,
            }
        })
    };
    for column in generated_order(contract)? {
        let expression = column
            .generated
            .as_ref()
            .expect("ordered generated field has expression");
        let value =
            evaluate(expression, &values).map_err(|error| expression_error(&column.name, error))?;
        let rule = columns
            .iter()
            .find(|rule| rule.name == column.name)
            .ok_or_else(|| expression_error(&column.name, "missing column type"))?;
        let value = normalize_contract_value(&contract.name, rule, value)?;
        values.insert(column.name.clone(), value.clone());
        fields.push((column.name.clone(), value));
    }
    for column in &contract.declared_columns {
        let Some(expression) = &column.check else {
            continue;
        };
        match evaluate(expression, &values)
            .map_err(|error| expression_error(&column.name, error))?
        {
            Value::Boolean(true) | Value::Null => {}
            Value::Boolean(false) => {
                return Err(expression_error(&column.name, "CHECK constraint violated"))
            }
            _ => return Err(expression_error(&column.name, "CHECK must produce BOOLEAN")),
        }
    }
    Ok(())
}
