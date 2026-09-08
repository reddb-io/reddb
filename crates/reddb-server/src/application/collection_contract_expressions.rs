//! Final-record expressions, evaluated before uniqueness, indexes and publication.
use super::*;
use reddb_rql::ast::FieldRef;
use reddb_rql::expr_typing::type_expr;
use reddb_rql::schema_expression::SchemaExpression;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn validate_contract_expressions(contract: &CollectionContract) -> RedDBResult<()> {
    let columns = resolved_contract_columns(contract)?;
    let scope = |table: &str, name: &str| {
        if !table.is_empty() {
            return None;
        }
        columns
            .iter()
            .find(|column| column.name == name)
            .map(|column| column.data_type)
    };
    for column in &contract.declared_columns {
        if column.generated.is_some() && column.default.is_some() {
            return Err(expression_error(
                &column.name,
                "GENERATED cannot have DEFAULT",
            ));
        }
        for (expression, check) in [(&column.generated, false), (&column.check, true)] {
            let Some(expression) = expression else {
                continue;
            };
            let typed = type_expr(expression.expression(), &scope)
                .map_err(|error| expression_error(&column.name, error))?;
            if check && !matches!(typed.ty, DataType::Boolean | DataType::Nullable) {
                return Err(expression_error(&column.name, "CHECK must produce BOOLEAN"));
            }
        }
    }
    generated_order(contract).map(|_| ())
}

fn expression_error(column: &str, error: impl std::fmt::Display) -> crate::RedDBError {
    crate::RedDBError::Query(format!("column '{column}' expression: {error}"))
}

fn generated_order(
    contract: &CollectionContract,
) -> RedDBResult<Vec<&crate::physical::DeclaredColumnContract>> {
    let mut pending: Vec<_> = contract
        .declared_columns
        .iter()
        .filter(|column| column.generated.is_some())
        .collect();
    let mut ready: BTreeSet<&str> = contract
        .declared_columns
        .iter()
        .filter(|column| column.generated.is_none())
        .map(|column| column.name.as_str())
        .collect();
    let mut ordered = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let index = pending.iter().position(|column| {
            column.generated.as_ref().is_some_and(|expression| {
                expression
                    .dependencies()
                    .iter()
                    .all(|name| ready.contains(name.as_str()))
            })
        });
        let Some(index) = index else {
            return Err(crate::RedDBError::Query(
                "generated field dependency cycle or unknown column".to_string(),
            ));
        };
        let column = pending.remove(index);
        ready.insert(&column.name);
        ordered.push(column);
    }
    Ok(ordered)
}

pub(crate) fn has_contract_expressions(contract: &CollectionContract) -> bool {
    contract
        .declared_columns
        .iter()
        .any(|column| column.generated.is_some() || column.check.is_some())
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
