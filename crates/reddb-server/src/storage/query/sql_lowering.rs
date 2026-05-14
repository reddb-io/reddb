use crate::storage::engine::vector_metadata::MetadataFilter;
use crate::storage::query::ast::{
    BinOp, CompareOp, DeleteQuery, Expr, FieldRef, Filter, GraphQuery, InsertQuery, JoinQuery,
    PathQuery, Projection, SelectItem, Span, TableQuery, UnaryOp, UpdateQuery, VectorQuery,
};
use crate::storage::schema::Value;

pub const PARAMETER_PROJECTION_PREFIX: &str = "__user_param_projection__:";

pub fn expr_to_projection(expr: &Expr) -> Option<Projection> {
    match expr {
        Expr::Literal { value, .. } => projection_from_literal(value),
        Expr::Column { field, .. } => {
            if matches!(
                field,
                FieldRef::TableColumn { table, column } if table.is_empty() && column == "*"
            ) {
                Some(Projection::All)
            } else {
                Some(Projection::Field(field.clone(), None))
            }
        }
        Expr::Parameter { index, .. } => Some(Projection::Column(format!(
            "{PARAMETER_PROJECTION_PREFIX}{index}"
        ))),
        Expr::BinaryOp { op, lhs, rhs, .. } => match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Concat => {
                Some(Projection::Function(
                    projection_binop_name(*op).to_string(),
                    vec![expr_to_projection(lhs)?, expr_to_projection(rhs)?],
                ))
            }
            _ => Some(boolean_expr_projection(expr.clone())),
        },
        Expr::UnaryOp { op, operand, .. } => match op {
            UnaryOp::Neg => Some(Projection::Function(
                "SUB".to_string(),
                vec![
                    Projection::Column("LIT:0".to_string()),
                    expr_to_projection(operand)?,
                ],
            )),
            UnaryOp::Not => Some(boolean_expr_projection(expr.clone())),
        },
        Expr::Cast { inner, target, .. } => Some(Projection::Function(
            "CAST".to_string(),
            vec![
                expr_to_projection(inner)?,
                Projection::Column(format!("TYPE:{target}")),
            ],
        )),
        Expr::FunctionCall { name, args, .. } => Some(Projection::Function(
            name.to_uppercase(),
            args.iter()
                .map(expr_to_projection)
                .collect::<Option<Vec<_>>>()?,
        )),
        Expr::Case {
            branches, else_, ..
        } => {
            let mut args = Vec::with_capacity(branches.len() * 2 + usize::from(else_.is_some()));
            for (cond, value) in branches {
                args.push(case_condition_projection(cond.clone()));
                args.push(expr_to_projection(value)?);
            }
            if let Some(else_expr) = else_ {
                args.push(expr_to_projection(else_expr)?);
            }
            Some(Projection::Function("CASE".to_string(), args))
        }
        Expr::IsNull { .. }
        | Expr::InList { .. }
        | Expr::Between { .. }
        | Expr::Subquery { .. } => Some(boolean_expr_projection(expr.clone())),
    }
}

pub fn select_item_to_projection(item: &SelectItem) -> Option<Projection> {
    match item {
        SelectItem::Wildcard => Some(Projection::All),
        SelectItem::Expr { expr, alias } => {
            let projection = expr_to_projection(expr)?;
            let output_name = alias.clone().or_else(|| Some(render_expr_label(expr)));
            Some(attach_projection_alias(projection, output_name))
        }
    }
}

pub fn effective_table_projections(query: &TableQuery) -> Vec<Projection> {
    if !query.select_items.is_empty() {
        return query
            .select_items
            .iter()
            .filter_map(select_item_to_projection)
            .collect();
    }
    if query.columns.is_empty() {
        vec![Projection::All]
    } else {
        query.columns.clone()
    }
}

pub fn effective_table_filter(query: &TableQuery) -> Option<Filter> {
    query
        .filter
        .clone()
        .or_else(|| query.where_expr.as_ref().map(expr_to_filter))
        .map(|f| f.optimize()) // OR-of-Eq → In; AND/OR flatten; constant fold
}

pub fn effective_table_group_by_exprs(query: &TableQuery) -> Vec<Expr> {
    if !query.group_by_exprs.is_empty() {
        query.group_by_exprs.clone()
    } else {
        query
            .group_by
            .iter()
            .map(|column| Expr::Column {
                field: FieldRef::TableColumn {
                    table: String::new(),
                    column: column.clone(),
                },
                span: Span::synthetic(),
            })
            .collect()
    }
}

pub fn effective_table_having_filter(query: &TableQuery) -> Option<Filter> {
    query
        .having
        .clone()
        .or_else(|| query.having_expr.as_ref().map(expr_to_filter))
}

pub fn effective_update_filter(query: &UpdateQuery) -> Option<Filter> {
    query
        .filter
        .clone()
        .or_else(|| query.where_expr.as_ref().map(expr_to_filter))
}

pub fn effective_insert_rows(query: &InsertQuery) -> Result<Vec<Vec<Value>>, String> {
    if !query.value_exprs.is_empty() {
        return query
            .value_exprs
            .iter()
            .cloned()
            .map(|row| row.into_iter().map(fold_expr_to_value).collect())
            .collect();
    }
    Ok(query.values.clone())
}

pub fn effective_delete_filter(query: &DeleteQuery) -> Option<Filter> {
    query
        .filter
        .clone()
        .or_else(|| query.where_expr.as_ref().map(expr_to_filter))
}

pub fn effective_join_filter(query: &JoinQuery) -> Option<Filter> {
    query.filter.clone()
}

pub fn effective_graph_filter(query: &GraphQuery) -> Option<Filter> {
    query.filter.clone()
}

pub fn effective_graph_projections(query: &GraphQuery) -> Vec<Projection> {
    query.return_.clone()
}

pub fn effective_path_filter(query: &PathQuery) -> Option<Filter> {
    query.filter.clone()
}

pub fn effective_path_projections(query: &PathQuery) -> Vec<Projection> {
    query.return_.clone()
}

pub fn effective_vector_filter(query: &VectorQuery) -> Option<MetadataFilter> {
    query.filter.clone()
}

pub fn projection_to_expr(projection: &Projection) -> Option<(Expr, Option<String>)> {
    match projection {
        Projection::All => Some((
            Expr::Column {
                field: FieldRef::TableColumn {
                    table: String::new(),
                    column: "*".to_string(),
                },
                span: Span::synthetic(),
            },
            None,
        )),
        Projection::Column(column) => Some((projection_column_to_expr(column), None)),
        Projection::Alias(column, alias) => {
            Some((projection_column_to_expr(column), Some(alias.clone())))
        }
        Projection::Function(name, args) => {
            let (name, alias) = split_projection_function_alias(name);
            let args = args
                .iter()
                .map(projection_to_expr)
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .map(|(expr, _)| expr)
                .collect();
            Some((
                Expr::FunctionCall {
                    name,
                    args,
                    span: Span::synthetic(),
                },
                alias,
            ))
        }
        Projection::Expression(filter, alias) => Some((filter_to_expr(filter), alias.clone())),
        Projection::Field(field, alias) => Some((
            Expr::Column {
                field: field.clone(),
                span: Span::synthetic(),
            },
            alias.clone(),
        )),
    }
}

fn projection_column_to_expr(column: &str) -> Expr {
    if let Some(value) = projection_literal_value(column) {
        return Expr::Literal {
            value,
            span: Span::synthetic(),
        };
    }

    Expr::Column {
        field: FieldRef::TableColumn {
            table: String::new(),
            column: column.to_string(),
        },
        span: Span::synthetic(),
    }
}

fn projection_literal_value(column: &str) -> Option<Value> {
    let literal = column.strip_prefix("LIT:")?;
    if literal.is_empty() {
        return Some(Value::Null);
    }
    if let Ok(value) = literal.parse::<i64>() {
        return Some(Value::Integer(value));
    }
    if let Ok(value) = literal.parse::<f64>() {
        return Some(Value::Float(value));
    }
    Some(Value::text(literal.to_string()))
}

pub fn projection_to_select_item(projection: &Projection) -> Option<SelectItem> {
    match projection {
        Projection::All => Some(SelectItem::Wildcard),
        other => {
            let (expr, alias) = projection_to_expr(other)?;
            Some(SelectItem::Expr { expr, alias })
        }
    }
}

pub fn effective_join_projections(query: &JoinQuery) -> Vec<Projection> {
    if !query.return_items.is_empty() {
        return query
            .return_items
            .iter()
            .filter_map(select_item_to_projection)
            .collect();
    }
    query.return_.clone()
}

pub fn expr_to_filter(expr: &Expr) -> Filter {
    match expr {
        Expr::BinaryOp { op, lhs, rhs, .. } => match op {
            BinOp::And => Filter::And(Box::new(expr_to_filter(lhs)), Box::new(expr_to_filter(rhs))),
            BinOp::Or => Filter::Or(Box::new(expr_to_filter(lhs)), Box::new(expr_to_filter(rhs))),
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                try_specialized_compare_filter(lhs, *op, rhs).unwrap_or_else(|| {
                    Filter::CompareExpr {
                        lhs: lhs.as_ref().clone(),
                        op: binop_to_compare_op(*op),
                        rhs: rhs.as_ref().clone(),
                    }
                })
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Concat => {
                Filter::CompareExpr {
                    lhs: expr.clone(),
                    op: CompareOp::Eq,
                    rhs: Expr::lit(Value::Boolean(true)),
                }
            }
        },
        Expr::UnaryOp {
            op: UnaryOp::Not,
            operand,
            ..
        } => Filter::Not(Box::new(expr_to_filter(operand))),
        Expr::IsNull {
            operand, negated, ..
        } => match operand.as_ref() {
            Expr::Column { field, .. } => {
                if *negated {
                    Filter::IsNotNull(field.clone())
                } else {
                    Filter::IsNull(field.clone())
                }
            }
            _ => Filter::CompareExpr {
                lhs: expr.clone(),
                op: CompareOp::Eq,
                rhs: Expr::lit(Value::Boolean(true)),
            },
        },
        Expr::InList {
            target,
            values,
            negated,
            ..
        } => match (target.as_ref(), all_literal_values(values)) {
            (Expr::Column { field, .. }, Some(values)) if !negated => Filter::In {
                field: field.clone(),
                values,
            },
            _ => Filter::CompareExpr {
                lhs: expr.clone(),
                op: CompareOp::Eq,
                rhs: Expr::lit(Value::Boolean(true)),
            },
        },
        Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } => match (
            target.as_ref(),
            literal_expr_value(low),
            literal_expr_value(high),
        ) {
            (Expr::Column { field, .. }, Some(low), Some(high)) if !negated => Filter::Between {
                field: field.clone(),
                low,
                high,
            },
            _ => Filter::CompareExpr {
                lhs: expr.clone(),
                op: CompareOp::Eq,
                rhs: Expr::lit(Value::Boolean(true)),
            },
        },
        Expr::Subquery { .. } => Filter::CompareExpr {
            lhs: expr.clone(),
            op: CompareOp::Eq,
            rhs: Expr::lit(Value::Boolean(true)),
        },
        _ => Filter::CompareExpr {
            lhs: expr.clone(),
            op: CompareOp::Eq,
            rhs: Expr::lit(Value::Boolean(true)),
        },
    }
}

pub fn boolean_expr_projection(expr: Expr) -> Projection {
    Projection::Expression(
        Box::new(Filter::CompareExpr {
            lhs: expr,
            op: CompareOp::Eq,
            rhs: Expr::Literal {
                value: Value::Boolean(true),
                span: Span::synthetic(),
            },
        }),
        None,
    )
}

pub fn filter_to_expr(filter: &Filter) -> Expr {
    match filter {
        Filter::Compare { field, op, value } => Expr::BinaryOp {
            op: compare_op_to_binop(*op),
            lhs: Box::new(Expr::Column {
                field: field.clone(),
                span: Span::synthetic(),
            }),
            rhs: Box::new(Expr::Literal {
                value: value.clone(),
                span: Span::synthetic(),
            }),
            span: Span::synthetic(),
        },
        Filter::CompareFields { left, op, right } => Expr::BinaryOp {
            op: compare_op_to_binop(*op),
            lhs: Box::new(Expr::Column {
                field: left.clone(),
                span: Span::synthetic(),
            }),
            rhs: Box::new(Expr::Column {
                field: right.clone(),
                span: Span::synthetic(),
            }),
            span: Span::synthetic(),
        },
        Filter::CompareExpr { lhs, op, rhs } => Expr::BinaryOp {
            op: compare_op_to_binop(*op),
            lhs: Box::new(lhs.clone()),
            rhs: Box::new(rhs.clone()),
            span: Span::synthetic(),
        },
        Filter::And(left, right) => Expr::BinaryOp {
            op: BinOp::And,
            lhs: Box::new(filter_to_expr(left)),
            rhs: Box::new(filter_to_expr(right)),
            span: Span::synthetic(),
        },
        Filter::Or(left, right) => Expr::BinaryOp {
            op: BinOp::Or,
            lhs: Box::new(filter_to_expr(left)),
            rhs: Box::new(filter_to_expr(right)),
            span: Span::synthetic(),
        },
        Filter::Not(inner) => Expr::UnaryOp {
            op: UnaryOp::Not,
            operand: Box::new(filter_to_expr(inner)),
            span: Span::synthetic(),
        },
        Filter::IsNull(field) => Expr::IsNull {
            operand: Box::new(Expr::Column {
                field: field.clone(),
                span: Span::synthetic(),
            }),
            negated: false,
            span: Span::synthetic(),
        },
        Filter::IsNotNull(field) => Expr::IsNull {
            operand: Box::new(Expr::Column {
                field: field.clone(),
                span: Span::synthetic(),
            }),
            negated: true,
            span: Span::synthetic(),
        },
        Filter::In { field, values } => Expr::InList {
            target: Box::new(Expr::Column {
                field: field.clone(),
                span: Span::synthetic(),
            }),
            values: values
                .iter()
                .cloned()
                .map(|value| Expr::Literal {
                    value,
                    span: Span::synthetic(),
                })
                .collect(),
            negated: false,
            span: Span::synthetic(),
        },
        Filter::Between { field, low, high } => Expr::Between {
            target: Box::new(Expr::Column {
                field: field.clone(),
                span: Span::synthetic(),
            }),
            low: Box::new(Expr::Literal {
                value: low.clone(),
                span: Span::synthetic(),
            }),
            high: Box::new(Expr::Literal {
                value: high.clone(),
                span: Span::synthetic(),
            }),
            negated: false,
            span: Span::synthetic(),
        },
        Filter::Like { field, pattern } => Expr::FunctionCall {
            name: "LIKE".to_string(),
            args: vec![
                Expr::Column {
                    field: field.clone(),
                    span: Span::synthetic(),
                },
                Expr::Literal {
                    value: Value::text(pattern.clone()),
                    span: Span::synthetic(),
                },
            ],
            span: Span::synthetic(),
        },
        Filter::StartsWith { field, prefix } => Expr::FunctionCall {
            name: "STARTS_WITH".to_string(),
            args: vec![
                Expr::Column {
                    field: field.clone(),
                    span: Span::synthetic(),
                },
                Expr::Literal {
                    value: Value::text(prefix.clone()),
                    span: Span::synthetic(),
                },
            ],
            span: Span::synthetic(),
        },
        Filter::EndsWith { field, suffix } => Expr::FunctionCall {
            name: "ENDS_WITH".to_string(),
            args: vec![
                Expr::Column {
                    field: field.clone(),
                    span: Span::synthetic(),
                },
                Expr::Literal {
                    value: Value::text(suffix.clone()),
                    span: Span::synthetic(),
                },
            ],
            span: Span::synthetic(),
        },
        Filter::Contains { field, substring } => Expr::FunctionCall {
            name: "CONTAINS".to_string(),
            args: vec![
                Expr::Column {
                    field: field.clone(),
                    span: Span::synthetic(),
                },
                Expr::Literal {
                    value: Value::text(substring.clone()),
                    span: Span::synthetic(),
                },
            ],
            span: Span::synthetic(),
        },
    }
}

pub fn projection_from_literal(value: &Value) -> Option<Projection> {
    match value {
        Value::Boolean(_) => Some(boolean_expr_projection(Expr::Literal {
            value: value.clone(),
            span: Span::synthetic(),
        })),
        _ => Some(Projection::Column(format!(
            "LIT:{}",
            render_projection_literal(value)
        ))),
    }
}

pub fn case_condition_projection(condition: Expr) -> Projection {
    Projection::Expression(
        Box::new(Filter::CompareExpr {
            lhs: condition,
            op: CompareOp::Eq,
            rhs: Expr::Literal {
                value: Value::Boolean(true),
                span: Span::synthetic(),
            },
        }),
        None,
    )
}

pub fn fold_expr_to_value(expr: Expr) -> Result<Value, String> {
    match expr {
        Expr::Literal { value, .. } => Ok(value),
        Expr::FunctionCall { name, args, .. } => {
            if (name.eq_ignore_ascii_case("PASSWORD") || name.eq_ignore_ascii_case("SECRET"))
                && args.len() == 1
            {
                let plaintext = match fold_expr_to_value(args.into_iter().next().unwrap())? {
                    Value::Text(text) => text,
                    other => {
                        return Err(format!(
                            "{name}() expects a string literal argument, got {other:?}"
                        ))
                    }
                };
                return Ok(if name.eq_ignore_ascii_case("PASSWORD") {
                    Value::Password(format!("@@plain@@{plaintext}"))
                } else {
                    Value::Secret(format!("@@plain@@{plaintext}").into_bytes())
                });
            }
            Err(format!(
                "expression is not a foldable literal: FunctionCall({name})"
            ))
        }
        Expr::UnaryOp { op, operand, .. } => {
            let inner = fold_expr_to_value(*operand)?;
            match (op, inner) {
                (UnaryOp::Neg, Value::Integer(n)) => Ok(Value::Integer(-n)),
                (UnaryOp::Neg, Value::UnsignedInteger(n)) => Ok(Value::Integer(-(n as i64))),
                (UnaryOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
                (UnaryOp::Not, Value::Boolean(b)) => Ok(Value::Boolean(!b)),
                (other_op, other) => Err(format!(
                    "unary `{other_op:?}` cannot fold to literal Value (operand: {other:?})"
                )),
            }
        }
        Expr::Cast { inner, .. } => fold_expr_to_value(*inner),
        other => Err(format!("expression is not a foldable literal: {other:?}")),
    }
}

fn projection_binop_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "ADD",
        BinOp::Sub => "SUB",
        BinOp::Mul => "MUL",
        BinOp::Div => "DIV",
        BinOp::Mod => "MOD",
        BinOp::Concat => "CONCAT",
        BinOp::Eq
        | BinOp::Ne
        | BinOp::Lt
        | BinOp::Le
        | BinOp::Gt
        | BinOp::Ge
        | BinOp::And
        | BinOp::Or => {
            unreachable!("boolean operators are lowered through Projection::Expression")
        }
    }
}

fn render_expr_label(expr: &Expr) -> String {
    render_expr_label_prec(expr, 0)
}

fn render_expr_label_prec(expr: &Expr, parent_prec: u8) -> String {
    match expr {
        Expr::Literal { value, .. } => render_sql_literal_label(value),
        Expr::Column { field, .. } => render_field_label(field),
        Expr::Parameter { index, .. } => format!("${index}"),
        Expr::BinaryOp { op, lhs, rhs, .. } => {
            let prec = op.precedence();
            let rendered = format!(
                "{} {} {}",
                render_expr_label_prec(lhs, prec),
                render_binop_label(*op),
                render_expr_label_prec(rhs, prec + 1)
            );
            if prec < parent_prec {
                format!("({rendered})")
            } else {
                rendered
            }
        }
        Expr::UnaryOp { op, operand, .. } => match op {
            UnaryOp::Neg => format!("-{}", render_expr_label_prec(operand, u8::MAX)),
            UnaryOp::Not => format!("NOT {}", render_expr_label_prec(operand, u8::MAX)),
        },
        Expr::Cast { inner, target, .. } => {
            format!("CAST({} AS {target})", render_expr_label(inner))
        }
        Expr::FunctionCall { name, args, .. } => {
            let args = args
                .iter()
                .map(render_expr_label)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}({args})")
        }
        Expr::Case {
            branches, else_, ..
        } => {
            let mut out = String::from("CASE");
            for (condition, value) in branches {
                out.push_str(" WHEN ");
                out.push_str(&render_expr_label(condition));
                out.push_str(" THEN ");
                out.push_str(&render_expr_label(value));
            }
            if let Some(else_expr) = else_ {
                out.push_str(" ELSE ");
                out.push_str(&render_expr_label(else_expr));
            }
            out.push_str(" END");
            out
        }
        Expr::IsNull {
            operand, negated, ..
        } => {
            let op = if *negated { "IS NOT NULL" } else { "IS NULL" };
            format!("{} {op}", render_expr_label_prec(operand, u8::MAX))
        }
        Expr::InList {
            target,
            values,
            negated,
            ..
        } => {
            let op = if *negated { "NOT IN" } else { "IN" };
            let values = values
                .iter()
                .map(render_expr_label)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} {op} ({values})", render_expr_label(target))
        }
        Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } => {
            let op = if *negated { "NOT BETWEEN" } else { "BETWEEN" };
            format!(
                "{} {op} {} AND {}",
                render_expr_label(target),
                render_expr_label(low),
                render_expr_label(high)
            )
        }
        Expr::Subquery { .. } => "subquery".to_string(),
    }
}

fn render_binop_label(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Mod => "%",
        BinOp::Concat => "||",
        BinOp::Eq => "=",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "AND",
        BinOp::Or => "OR",
    }
}

fn render_field_label(field: &FieldRef) -> String {
    match field {
        FieldRef::TableColumn { table, column } => {
            if table.is_empty() {
                column.clone()
            } else {
                format!("{table}.{column}")
            }
        }
        FieldRef::NodeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::EdgeProperty { alias, property } => format!("{alias}.{property}"),
        FieldRef::NodeId { alias } => format!("{alias}.id"),
    }
}

fn render_sql_literal_label(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Text(value) => format!("'{}'", value.replace('\'', "''")),
        Value::Boolean(value) => value.to_string(),
        Value::Integer(value) => value.to_string(),
        Value::UnsignedInteger(value) => value.to_string(),
        Value::Float(value) => {
            if value.fract().abs() < f64::EPSILON {
                (*value as i64).to_string()
            } else {
                value.to_string()
            }
        }
        other => other.to_string(),
    }
}

fn binop_to_compare_op(op: BinOp) -> CompareOp {
    match op {
        BinOp::Eq => CompareOp::Eq,
        BinOp::Ne => CompareOp::Ne,
        BinOp::Lt => CompareOp::Lt,
        BinOp::Le => CompareOp::Le,
        BinOp::Gt => CompareOp::Gt,
        BinOp::Ge => CompareOp::Ge,
        other => unreachable!("non-compare binop cannot lower to CompareOp: {other:?}"),
    }
}

fn compare_op_to_binop(op: CompareOp) -> BinOp {
    match op {
        CompareOp::Eq => BinOp::Eq,
        CompareOp::Ne => BinOp::Ne,
        CompareOp::Lt => BinOp::Lt,
        CompareOp::Le => BinOp::Le,
        CompareOp::Gt => BinOp::Gt,
        CompareOp::Ge => BinOp::Ge,
    }
}

fn attach_projection_alias(proj: Projection, alias: Option<String>) -> Projection {
    let Some(alias) = alias else { return proj };
    match proj {
        Projection::Field(f, _) => Projection::Field(f, Some(alias)),
        Projection::Expression(filter, _) => Projection::Expression(filter, Some(alias)),
        Projection::Function(name, args) => {
            if name.contains(':') {
                Projection::Function(name, args)
            } else {
                Projection::Function(format!("{name}:{alias}"), args)
            }
        }
        Projection::Column(c) => Projection::Alias(c, alias),
        other => other,
    }
}

fn split_projection_function_alias(name: &str) -> (String, Option<String>) {
    match name.split_once(':') {
        Some((function, alias)) if !function.is_empty() && !alias.is_empty() => {
            (function.to_string(), Some(alias.to_string()))
        }
        _ => (name.to_string(), None),
    }
}

fn render_projection_literal(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Integer(v) => v.to_string(),
        Value::UnsignedInteger(v) => v.to_string(),
        Value::Float(v) => {
            if v.fract().abs() < f64::EPSILON {
                (*v as i64).to_string()
            } else {
                v.to_string()
            }
        }
        Value::Text(v) => v.to_string(),
        Value::Boolean(true) => "true".to_string(),
        Value::Boolean(false) => "false".to_string(),
        // Composite values (arrays, vectors, blobs) would lose fidelity
        // going through `Display` — `Vec<Value>` turns into
        // "<vector dim=N>". Use a JSON sentinel so the reader in
        // `eval_projection_value` can round-trip the exact Value.
        Value::Array(_) | Value::Vector(_) | Value::Json(_) | Value::Blob(_) => {
            format!("@RL:{}", serialize_value_json(value))
        }
        other => other.to_string(),
    }
}

fn serialize_value_json(value: &Value) -> String {
    // Uses `crate::serde_json` which is already a workspace dep.
    match value {
        Value::Array(items) => {
            let mut out = String::from("[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serialize_value_json(item));
            }
            out.push(']');
            out
        }
        Value::Vector(items) => {
            let mut out = String::from("V[");
            for (i, f) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&f.to_string());
            }
            out.push(']');
            out
        }
        Value::Integer(n) | Value::BigInt(n) => n.to_string(),
        Value::UnsignedInteger(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Text(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Boolean(b) => b.to_string(),
        Value::Null => "null".to_string(),
        other => format!("\"{}\"", other.to_string().replace('"', "\\\"")),
    }
}

fn try_specialized_compare_filter(lhs: &Expr, op: BinOp, rhs: &Expr) -> Option<Filter> {
    let op = binop_to_compare_op(op);
    match (lhs, rhs) {
        (Expr::Column { field, .. }, Expr::Literal { value, .. }) => Some(Filter::Compare {
            field: field.clone(),
            op,
            value: value.clone(),
        }),
        (Expr::Literal { value, .. }, Expr::Column { field, .. }) => Some(Filter::Compare {
            field: field.clone(),
            op: flipped_compare_op(op),
            value: value.clone(),
        }),
        (Expr::Column { field: left, .. }, Expr::Column { field: right, .. }) => {
            Some(Filter::CompareFields {
                left: left.clone(),
                op,
                right: right.clone(),
            })
        }
        _ => None,
    }
}

fn flipped_compare_op(op: CompareOp) -> CompareOp {
    match op {
        CompareOp::Eq => CompareOp::Eq,
        CompareOp::Ne => CompareOp::Ne,
        CompareOp::Lt => CompareOp::Gt,
        CompareOp::Le => CompareOp::Ge,
        CompareOp::Gt => CompareOp::Lt,
        CompareOp::Ge => CompareOp::Le,
    }
}

fn literal_expr_value(expr: &Expr) -> Option<Value> {
    match expr {
        Expr::Literal { value, .. } => Some(value.clone()),
        _ => None,
    }
}

fn all_literal_values(values: &[Expr]) -> Option<Vec<Value>> {
    values.iter().map(literal_expr_value).collect()
}
