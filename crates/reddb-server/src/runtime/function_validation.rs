//! Closed effect boundary for stored RQL bodies.
use crate::{RedDBError, RedDBResult};
use reddb_rql::ast::*;
use reddb_rql::stored_function::{FunctionDefinition, FunctionEffect, FunctionReturn};
use reddb_types::{DataType, Value};

pub(super) fn validate_definition(
    definition: &FunctionDefinition,
) -> RedDBResult<Vec<(String, QueryExpr)>> {
    if definition.name.is_empty()
        || definition.body.len() > reddb_rql::stored_function::FUNCTION_SOURCE_BYTES_MAX
    {
        return Err(error("invalid function name or body size"));
    }
    validate_parameters(&definition.parameters)?;
    match &definition.returns {
        FunctionReturn::Scalar(sql_type) => validate_type(sql_type)?,
        FunctionReturn::Table(columns) => {
            if columns.is_empty() {
                return Err(error("function return table must declare columns"));
            }
            validate_parameters(columns)?;
        }
    }
    let statements = reddb_rql::stored_function::parse_function_body(&definition.body)
        .map_err(|cause| error(cause.to_string()))?;
    for (_, statement) in &statements {
        validate_statement(statement, definition.effect)?;
        if crate::storage::query::user_params::collect_indices(statement)
            .iter()
            .any(|index| *index >= definition.parameters.len())
        {
            return Err(error("function body references an undeclared parameter"));
        }
    }
    Ok(statements)
}

fn validate_parameters(
    parameters: &[reddb_rql::stored_function::FunctionParameter],
) -> RedDBResult<()> {
    if parameters.len() > reddb_rql::stored_function::FUNCTION_PARAMETERS_MAX {
        return Err(error(
            "at most 32 parameters or result columns are supported",
        ));
    }
    let mut names = std::collections::HashSet::new();
    for parameter in parameters {
        if parameter.name.is_empty() || !names.insert(&parameter.name) {
            return Err(error("invalid or duplicate parameter name"));
        }
        validate_type(&parameter.sql_type)?;
    }
    Ok(())
}

fn validate_type(sql_type: &reddb_types::SqlTypeName) -> RedDBResult<()> {
    DataType::from_sql_type_name(sql_type)
        .ok_or_else(|| error(format!("unknown function type '{sql_type}'")))?;
    Ok(())
}

pub(super) fn error(message: impl std::fmt::Display) -> RedDBError {
    RedDBError::Query(format!("stored function: {message}"))
}

pub(super) fn validate_statement(statement: &QueryExpr, effect: FunctionEffect) -> RedDBResult<()> {
    let mut pending = vec![(statement, 0usize)];
    while let Some((query, depth)) = pending.pop() {
        if depth > 32 {
            return Err(error("query nesting exceeds 32"));
        }
        let mut expressions = Vec::new();
        match query {
            QueryExpr::Table(table) => {
                if effect == FunctionEffect::Pure && (!super::query_exec::table_query_is_implicit_scalar_select(table)
                        || table.where_expr.is_some() || table.filter.is_some()
                        || table.having_expr.is_some() || table.having.is_some()
                        || !table.group_by_exprs.is_empty() || !table.order_by.is_empty()) {
                    return Err(error("PURE functions cannot read collections"));
                }
                if depth > 0 && (table.limit_param.is_some() || table.offset_param.is_some()) {
                    return Err(error("parameterized LIMIT/OFFSET in nested sources is not yet supported"));
                }
                match &table.source {
                    Some(TableSource::Subquery(query)) => pending.push((query, depth + 1)),
                    Some(TableSource::Name(_)) | None => {}
                    _ => return Err(error("table-valued function effects are not yet supported")),
                }
                if table.as_of.is_some() { return Err(error("function statements must share the caller snapshot; AS OF is unsupported")); }
                validate_projections(&table.columns)?;
                for filter in table.filter.iter().chain(table.having.iter()) { validate_expression(&reddb_rql::sql_lowering::filter_to_expr(filter))?; }
                if table.expand.is_some() { return Err(error("EXPAND is not yet supported in functions")); }
                for item in &table.select_items {
                    if let SelectItem::Expr { expr, .. } = item { expressions.push(expr); }
                }
                expressions.extend(table.where_expr.iter());
                expressions.extend(table.group_by_exprs.iter());
                expressions.extend(table.having_expr.iter());
                expressions.extend(table.order_by.iter().filter_map(|order| order.expr.as_ref()));
            }
            QueryExpr::Join(join) if effect != FunctionEffect::Pure => {
                pending.push((&join.left, depth + 1));
                pending.push((&join.right, depth + 1));
                validate_projections(&join.return_)?;
                if let Some(filter) = &join.filter { validate_expression(&reddb_rql::sql_lowering::filter_to_expr(filter))?; }
                for item in &join.return_items {
                    if let SelectItem::Expr { expr, .. } = item { expressions.push(expr); }
                }
                expressions.extend(join.order_by.iter().filter_map(|order| order.expr.as_ref()));
            }
            QueryExpr::Graph(graph) if effect != FunctionEffect::Pure => {
                validate_projections(&graph.return_)?;
                if let Some(filter) = &graph.filter { validate_expression(&reddb_rql::sql_lowering::filter_to_expr(filter))?; }
            }
            QueryExpr::Vector(vector) if effect != FunctionEffect::Pure => validate_vector(vector)?,
            QueryExpr::Hybrid(hybrid) if effect != FunctionEffect::Pure => {
                validate_vector(&hybrid.vector)?;
                pending.push((&hybrid.structured, depth + 1));
            }
            QueryExpr::Insert(insert) if effect == FunctionEffect::Write => {
                if insert.auto_embed.is_some() { return Err(error("external embedding calls are not admitted in functions")); }
                for row in &insert.value_exprs { expressions.extend(row.iter()); }
                if let Some(OnConflictClause { action: OnConflictAction::DoUpdate { assignments }, .. }) = &insert.on_conflict {
                    expressions.extend(assignments.iter().map(|(_, expression)| expression));
                }
            }
            QueryExpr::Update(update) if effect == FunctionEffect::Write => {
                if let Some(filter) = &update.filter { validate_expression(&reddb_rql::sql_lowering::filter_to_expr(filter))?; }
                expressions.extend(update.assignment_exprs.iter().map(|(_, expression)| expression));
                expressions.extend(update.where_expr.iter());
                expressions.extend(update.order_by.iter().filter_map(|order| order.expr.as_ref()));
            }
            QueryExpr::Delete(delete) if effect == FunctionEffect::Write => {
                if let Some(filter) = &delete.filter { validate_expression(&reddb_rql::sql_lowering::filter_to_expr(filter))?; }
                expressions.extend(delete.where_expr.iter());
            }
            QueryExpr::QueueCommand(QueueCommand::Push { .. }) if effect == FunctionEffect::Write => {}
            QueryExpr::KvCommand(KvCommand::Put { .. } | KvCommand::Delete { .. }) if effect == FunctionEffect::Write => {}
            _ => return Err(error("statement is outside the declared effect; only bounded data operations are supported")),
        }
        for expression in expressions {
            validate_expression_for_effect(expression, effect == FunctionEffect::Pure)?;
        }
    }
    Ok(())
}

fn validate_vector(vector: &VectorQuery) -> RedDBResult<()> {
    match &vector.query_vector {
        VectorSource::Literal(_) | VectorSource::Reference { .. } => Ok(()),
        _ => Err(error("vector functions require a literal vector or stored reference; embedding and subqueries are unsupported")),
    }
}

fn validate_projections(projections: &[Projection]) -> RedDBResult<()> {
    let mut pending: Vec<_> = projections
        .iter()
        .map(|projection| (projection, 0usize))
        .collect();
    while let Some((projection, depth)) = pending.pop() {
        if depth > 64 {
            return Err(error("projection nesting exceeds 64"));
        }
        match projection {
            Projection::Function(name, arguments) => {
                let name = name.split_once(':').map_or(name.as_str(), |(name, _)| name);
                // The legacy projection representation encodes operators as calls.
                if !matches!(
                    name,
                    "ADD" | "SUB" | "MUL" | "DIV" | "MOD" | "CONCAT" | "CAST" | "CASE"
                ) {
                    validate_callable(name)?;
                }
                pending.extend(arguments.iter().map(|argument| (argument, depth + 1)));
            }
            Projection::Expression(filter, _) => {
                validate_expression(&reddb_rql::sql_lowering::filter_to_expr(filter))?
            }
            Projection::Window { .. } => {
                return Err(error("window projections are not yet supported"))
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_callable(name: &str) -> RedDBResult<()> {
    let entries = reddb_types::function_catalog::lookup(name);
    if entries.is_empty()
        || entries
            .iter()
            .any(|entry| entry.kind == reddb_types::function_catalog::FunctionKind::Volatile)
    {
        return Err(error(format!(
            "callable '{name}' has no admitted pure effect"
        )));
    }
    Ok(())
}

pub(super) fn validate_expression(expression: &Expr) -> RedDBResult<()> {
    validate_expression_for_effect(expression, false)
}

fn validate_expression_for_effect(expression: &Expr, scalar_only: bool) -> RedDBResult<()> {
    let mut pending = vec![(expression, 0usize)];
    while let Some((expression, depth)) = pending.pop() {
        if depth > 64 {
            return Err(error("expression nesting exceeds 64"));
        }
        let mut push = |expression| pending.push((expression, depth + 1));
        match expression {
            Expr::Literal { .. } | Expr::Parameter { .. } | Expr::Column { .. } => {}
            Expr::BinaryOp { lhs, rhs, .. } => {
                push(lhs);
                push(rhs);
            }
            Expr::UnaryOp { operand, .. } | Expr::IsNull { operand, .. } => push(operand),
            Expr::Cast { inner, .. } => push(inner),
            Expr::Between {
                target, low, high, ..
            } => {
                push(target);
                push(low);
                push(high);
            }
            Expr::InList { target, values, .. } => {
                push(target);
                for value in values {
                    push(value);
                }
            }
            Expr::Case {
                branches, else_, ..
            } => {
                for (condition, value) in branches {
                    push(condition);
                    push(value);
                }
                if let Some(value) = else_ {
                    push(value);
                }
            }
            Expr::FunctionCall { name, args, .. } => {
                validate_callable(name)?;
                if scalar_only
                    && reddb_types::function_catalog::lookup(name)
                        .iter()
                        .any(|entry| {
                            entry.kind != reddb_types::function_catalog::FunctionKind::Scalar
                        })
                {
                    return Err(error("PURE functions cannot aggregate storage rows"));
                }
                for argument in args {
                    push(argument);
                }
            }
            _ => {
                return Err(error(
                    "subquery and window expressions are not yet supported in function bodies",
                ))
            }
        }
    }
    Ok(())
}

pub(super) fn normalize(
    context: &str,
    name: &str,
    sql_type: &reddb_types::SqlTypeName,
    value: Value,
) -> RedDBResult<Value> {
    crate::application::collection_contract_enforcer::normalize_declared_value(
        context, name, sql_type, value,
    )
}
