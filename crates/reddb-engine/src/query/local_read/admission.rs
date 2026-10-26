use super::{LocalReadError, LocalReadError::Unsupported};
use crate::catalog::schema::ResolvedColumnRule;
use reddb_rql::ast::{Expr, FieldRef, QueryExpr, SelectItem, TableQuery, TableSource};
use reddb_types::{function_catalog::FUNCTION_CATALOG, DataType};

pub(super) fn query(sql: &str) -> Result<TableQuery, LocalReadError> {
    let statement = reddb_rql::parse_frontend(sql).map_err(LocalReadError::Parse)?;
    let QueryExpr::Table(query) = statement.into_query_expr() else {
        return Err(Unsupported(
            "local database supports read-only table SELECT statements",
        ));
    };
    if query.select_items.is_empty()
        || crate::query::runtime::is_universal_entity_source(&query.table)
        || query.table.starts_with('_')
        || query.table.starts_with("red.")
        || matches!(&query.source, Some(source) if !matches!(source, TableSource::Name(name) if name == &query.table))
        || !query.group_by_exprs.is_empty()
        || !query.group_by.is_empty()
        || query.having_expr.is_some()
        || query.having.is_some()
        || query.limit_param.is_some()
        || query.offset_param.is_some()
        || query.expand.is_some()
        || query.as_of.is_some()
        || query.sessionize.is_some()
        || query.distinct
        || (query.filter.is_some() && query.where_expr.is_none())
    {
        return Err(Unsupported("local SELECT supports a named table, scalar projections, WHERE, ORDER BY and literal LIMIT/OFFSET"));
    }
    crate::query::planning::aggregation::validate_aggregate_planning_depth(&query)
        .map_err(|message| LocalReadError::Query(message.into()))?;
    Ok(query)
}

pub(crate) fn expressions(
    query: &TableQuery,
    columns: &[ResolvedColumnRule],
) -> Result<(), LocalReadError> {
    let mut pending: Vec<&Expr> = query
        .select_items
        .iter()
        .filter_map(|item| match item {
            SelectItem::Expr { expr, .. } => Some(expr),
            SelectItem::Wildcard => None,
        })
        .chain(query.where_expr.iter())
        .chain(
            query
                .order_by
                .iter()
                .filter_map(|order| order.expr.as_ref()),
        )
        .collect();
    for order in &query.order_by {
        if matches!(order.expr, Some(Expr::Literal { .. })) {
            return Err(Unsupported(
                "ordinal or literal ORDER BY is not yet supported by local SELECT",
            ));
        }
        if order.expr.is_none() {
            field(&order.field, query, columns)?;
        }
    }
    // Source depth has already been checked iteratively before lowering/planning.
    while let Some(expression) = pending.pop() {
        match expression {
            Expr::Literal { .. } => {}
            Expr::Column {
                field: reference, ..
            } => field(reference, query, columns)?,
            Expr::UnaryOp { operand, .. } | Expr::IsNull { operand, .. } => pending.push(operand),
            Expr::Cast { inner, .. } => pending.push(inner),
            Expr::BinaryOp { lhs, rhs, .. } => {
                pending.push(lhs);
                pending.push(rhs);
            }
            Expr::Case {
                branches, else_, ..
            } => {
                for (condition, value) in branches {
                    pending.push(condition);
                    pending.push(value);
                }
                pending.extend(else_.iter().map(|value| value.as_ref()));
            }
            Expr::InList { target, values, .. } => {
                pending.push(target);
                pending.extend(values);
            }
            Expr::Between {
                target, low, high, ..
            } => {
                pending.extend([target.as_ref(), low.as_ref(), high.as_ref()]);
            }
            Expr::FunctionCall { name, args, .. } => {
                if !matches!(
                    name.to_ascii_uppercase().as_str(),
                    "UPPER"
                        | "LOWER"
                        | "LENGTH"
                        | "CHAR_LENGTH"
                        | "CHARACTER_LENGTH"
                        | "OCTET_LENGTH"
                        | "TRIM"
                        | "LTRIM"
                        | "RTRIM"
                        | "CONCAT"
                        | "COALESCE"
                        | "ABS"
                        | "JSON_PARSE"
                        | "JSON_EXTRACT"
                        | "JSON_EXTRACT_TEXT"
                        | "CONTAINS"
                ) || !FUNCTION_CATALOG.iter().any(|entry| {
                    entry.name.eq_ignore_ascii_case(name)
                        && (entry.variadic && !args.is_empty()
                            || entry.arg_types.len() == args.len())
                }) {
                    return Err(Unsupported(
                        "function is not supported by local table SELECT",
                    ));
                }
                pending.extend(args);
            }
            Expr::Parameter { .. } | Expr::Subquery { .. } | Expr::WindowFunctionCall { .. } => {
                return Err(Unsupported("parameters, subqueries, aggregates and windows are not yet supported by local SELECT"));
            }
        }
    }
    Ok(())
}

fn field(
    reference: &FieldRef,
    query: &TableQuery,
    columns: &[ResolvedColumnRule],
) -> Result<(), LocalReadError> {
    let FieldRef::TableColumn { table, column } = reference else {
        return Err(Unsupported("local SELECT requires table column references"));
    };
    let qualified = if table.is_empty() {
        column.clone()
    } else {
        format!("{table}.{column}")
    };
    let name = qualified
        .strip_prefix(&format!(
            "{}.",
            query.alias.as_deref().unwrap_or(&query.table)
        ))
        .or_else(|| qualified.strip_prefix(&format!("{}.", query.table)))
        .unwrap_or(&qualified);
    let root = name
        .split('.')
        .next()
        .expect("split yields at least one element");
    if matches!(name, "rid" | "entity_id" | "created_at" | "updated_at") {
        return Ok(());
    }
    let Some(column) = columns.iter().find(|column| column.name == root) else {
        return Err(LocalReadError::Query(format!("unknown column {qualified}")));
    };
    if matches!(column.data_type, DataType::Secret | DataType::Password) {
        return Err(Unsupported(
            "secret columns are redacted by SELECT * and cannot be used in local expressions",
        ));
    }
    if root.starts_with('_') {
        return Err(Unsupported(
            "internal columns are not exposed by local SELECT",
        ));
    }
    Ok(())
}
