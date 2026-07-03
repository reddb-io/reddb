use super::*;

pub(super) fn relation_scopes_for_query(query: &QueryExpr) -> Vec<String> {
    let mut scopes = Vec::new();
    collect_relation_scopes(query, &mut scopes);
    scopes.sort();
    scopes.dedup();
    scopes
}

fn collect_relation_scopes(query: &QueryExpr, scopes: &mut Vec<String>) {
    match query {
        QueryExpr::Table(table) => {
            if !table.table.is_empty() {
                scopes.push(table.table.clone());
            }
            if let Some(alias) = &table.alias {
                scopes.push(alias.clone());
            }
        }
        QueryExpr::Join(join) => {
            collect_relation_scopes(&join.left, scopes);
            collect_relation_scopes(&join.right, scopes);
        }
        _ => {}
    }
}

pub(super) fn query_references_outer_scope(query: &QueryExpr, outer_scopes: &[String]) -> bool {
    let inner_scopes = relation_scopes_for_query(query);
    query_expr_references_outer_scope(query, outer_scopes, &inner_scopes)
}

fn query_expr_references_outer_scope(
    query: &QueryExpr,
    outer_scopes: &[String],
    inner_scopes: &[String],
) -> bool {
    match query {
        QueryExpr::Table(table) => {
            table.select_items.iter().any(|item| match item {
                crate::storage::query::ast::SelectItem::Wildcard => false,
                crate::storage::query::ast::SelectItem::Expr { expr, .. } => {
                    expr_references_outer_scope(expr, outer_scopes, inner_scopes)
                }
            }) || table
                .where_expr
                .as_ref()
                .is_some_and(|expr| expr_references_outer_scope(expr, outer_scopes, inner_scopes))
                || table.filter.as_ref().is_some_and(|filter| {
                    filter_references_outer_scope(filter, outer_scopes, inner_scopes)
                })
                || table.having_expr.as_ref().is_some_and(|expr| {
                    expr_references_outer_scope(expr, outer_scopes, inner_scopes)
                })
                || table.having.as_ref().is_some_and(|filter| {
                    filter_references_outer_scope(filter, outer_scopes, inner_scopes)
                })
                || table
                    .group_by_exprs
                    .iter()
                    .any(|expr| expr_references_outer_scope(expr, outer_scopes, inner_scopes))
                || table.order_by.iter().any(|clause| {
                    clause.expr.as_ref().is_some_and(|expr| {
                        expr_references_outer_scope(expr, outer_scopes, inner_scopes)
                    })
                })
        }
        QueryExpr::Join(join) => {
            query_expr_references_outer_scope(&join.left, outer_scopes, inner_scopes)
                || query_expr_references_outer_scope(&join.right, outer_scopes, inner_scopes)
                || join.filter.as_ref().is_some_and(|filter| {
                    filter_references_outer_scope(filter, outer_scopes, inner_scopes)
                })
                || join.return_items.iter().any(|item| match item {
                    crate::storage::query::ast::SelectItem::Wildcard => false,
                    crate::storage::query::ast::SelectItem::Expr { expr, .. } => {
                        expr_references_outer_scope(expr, outer_scopes, inner_scopes)
                    }
                })
        }
        _ => false,
    }
}

fn filter_references_outer_scope(
    filter: &crate::storage::query::ast::Filter,
    outer_scopes: &[String],
    inner_scopes: &[String],
) -> bool {
    use crate::storage::query::ast::Filter;
    match filter {
        Filter::Compare { field, .. }
        | Filter::IsNull(field)
        | Filter::IsNotNull(field)
        | Filter::In { field, .. }
        | Filter::Between { field, .. }
        | Filter::Like { field, .. }
        | Filter::StartsWith { field, .. }
        | Filter::EndsWith { field, .. }
        | Filter::Contains { field, .. } => {
            field_ref_references_outer_scope(field, outer_scopes, inner_scopes)
        }
        Filter::CompareFields { left, right, .. } => {
            field_ref_references_outer_scope(left, outer_scopes, inner_scopes)
                || field_ref_references_outer_scope(right, outer_scopes, inner_scopes)
        }
        Filter::CompareExpr { lhs, rhs, .. } => {
            expr_references_outer_scope(lhs, outer_scopes, inner_scopes)
                || expr_references_outer_scope(rhs, outer_scopes, inner_scopes)
        }
        Filter::And(left, right) | Filter::Or(left, right) => {
            filter_references_outer_scope(left, outer_scopes, inner_scopes)
                || filter_references_outer_scope(right, outer_scopes, inner_scopes)
        }
        Filter::Not(inner) => filter_references_outer_scope(inner, outer_scopes, inner_scopes),
    }
}

fn expr_references_outer_scope(
    expr: &crate::storage::query::ast::Expr,
    outer_scopes: &[String],
    inner_scopes: &[String],
) -> bool {
    use crate::storage::query::ast::Expr;
    match expr {
        Expr::Column { field, .. } => {
            field_ref_references_outer_scope(field, outer_scopes, inner_scopes)
        }
        Expr::BinaryOp { lhs, rhs, .. } => {
            expr_references_outer_scope(lhs, outer_scopes, inner_scopes)
                || expr_references_outer_scope(rhs, outer_scopes, inner_scopes)
        }
        Expr::UnaryOp { operand, .. }
        | Expr::Cast { inner: operand, .. }
        | Expr::IsNull { operand, .. } => {
            expr_references_outer_scope(operand, outer_scopes, inner_scopes)
        }
        Expr::FunctionCall { args, .. } => args
            .iter()
            .any(|arg| expr_references_outer_scope(arg, outer_scopes, inner_scopes)),
        Expr::Case {
            branches, else_, ..
        } => {
            branches.iter().any(|(cond, value)| {
                expr_references_outer_scope(cond, outer_scopes, inner_scopes)
                    || expr_references_outer_scope(value, outer_scopes, inner_scopes)
            }) || else_
                .as_ref()
                .is_some_and(|expr| expr_references_outer_scope(expr, outer_scopes, inner_scopes))
        }
        Expr::InList { target, values, .. } => {
            expr_references_outer_scope(target, outer_scopes, inner_scopes)
                || values
                    .iter()
                    .any(|value| expr_references_outer_scope(value, outer_scopes, inner_scopes))
        }
        Expr::Between {
            target, low, high, ..
        } => {
            expr_references_outer_scope(target, outer_scopes, inner_scopes)
                || expr_references_outer_scope(low, outer_scopes, inner_scopes)
                || expr_references_outer_scope(high, outer_scopes, inner_scopes)
        }
        Expr::Subquery { query, .. } => query_references_outer_scope(&query.query, inner_scopes),
        Expr::Literal { .. } | Expr::Parameter { .. } => false,
        Expr::WindowFunctionCall { args, window, .. } => {
            args.iter()
                .any(|arg| expr_references_outer_scope(arg, outer_scopes, inner_scopes))
                || window
                    .partition_by
                    .iter()
                    .any(|e| expr_references_outer_scope(e, outer_scopes, inner_scopes))
                || window
                    .order_by
                    .iter()
                    .any(|o| expr_references_outer_scope(&o.expr, outer_scopes, inner_scopes))
        }
    }
}

fn field_ref_references_outer_scope(
    field: &crate::storage::query::ast::FieldRef,
    outer_scopes: &[String],
    inner_scopes: &[String],
) -> bool {
    match field {
        crate::storage::query::ast::FieldRef::TableColumn { table, .. } if !table.is_empty() => {
            outer_scopes.iter().any(|scope| scope == table)
                && !inner_scopes.iter().any(|scope| scope == table)
        }
        _ => false,
    }
}

pub(super) fn first_column_values(
    result: crate::storage::query::unified::UnifiedResult,
) -> RedDBResult<Vec<Value>> {
    if result.columns.len() > 1 {
        return Err(RedDBError::Query(
            "expression subquery must return exactly one column".to_string(),
        ));
    }
    let fallback_column = result
        .records
        .first()
        .and_then(|record| record.column_names().into_iter().next())
        .map(|name| name.to_string());
    let column = result.columns.first().cloned().or(fallback_column);
    let Some(column) = column else {
        return Ok(Vec::new());
    };
    Ok(result
        .records
        .iter()
        .map(|record| record.get(column.as_str()).cloned().unwrap_or(Value::Null))
        .collect())
}
