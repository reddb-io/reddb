use crate::ast::{
    BinOp, CompareOp, DeleteQuery, Expr, FieldRef, Filter, GraphQuery, InsertQuery, JoinQuery,
    PathQuery, Projection, SelectItem, Span, TableQuery, UnaryOp, UpdateQuery, VectorQuery,
};
use reddb_types::types::Value;
use reddb_types::vector_metadata::MetadataFilter;

pub const PARAMETER_PROJECTION_PREFIX: &str = "__user_param_projection__:";

/// Recursively check whether `expr` contains any `Expr::Parameter` node.
/// Used by the INSERT parser to know when to defer literal folding to
/// the user_params binder.
pub fn expr_contains_parameter(expr: &Expr) -> bool {
    match expr {
        Expr::Parameter { .. } => true,
        Expr::Literal { .. } | Expr::Column { .. } => false,
        Expr::BinaryOp { lhs, rhs, .. } => {
            expr_contains_parameter(lhs) || expr_contains_parameter(rhs)
        }
        Expr::UnaryOp { operand, .. } => expr_contains_parameter(operand),
        Expr::Cast { inner, .. } => expr_contains_parameter(inner),
        Expr::FunctionCall { args, .. } => args.iter().any(expr_contains_parameter),
        Expr::Case {
            branches, else_, ..
        } => {
            branches
                .iter()
                .any(|(c, v)| expr_contains_parameter(c) || expr_contains_parameter(v))
                || else_.as_deref().is_some_and(expr_contains_parameter)
        }
        Expr::IsNull { operand, .. } => expr_contains_parameter(operand),
        Expr::InList { target, values, .. } => {
            expr_contains_parameter(target) || values.iter().any(expr_contains_parameter)
        }
        Expr::Between {
            target, low, high, ..
        } => {
            expr_contains_parameter(target)
                || expr_contains_parameter(low)
                || expr_contains_parameter(high)
        }
        Expr::Subquery { .. } => false,
        Expr::WindowFunctionCall { args, window, .. } => {
            args.iter().any(expr_contains_parameter)
                || window.partition_by.iter().any(expr_contains_parameter)
                || window
                    .order_by
                    .iter()
                    .any(|o| expr_contains_parameter(&o.expr))
        }
    }
}

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
        Expr::WindowFunctionCall {
            name, args, window, ..
        } => {
            let lowered_args = args
                .iter()
                .map(expr_to_projection)
                .collect::<Option<Vec<_>>>()?;
            Some(crate::ast::Projection::Window {
                name: name.to_uppercase(),
                args: lowered_args,
                window: Box::new(window.clone()),
                alias: None,
            })
        }
    }
}

pub fn select_item_to_projection(item: &SelectItem) -> Option<Projection> {
    match item {
        SelectItem::Wildcard => Some(Projection::All),
        SelectItem::Expr { expr, alias } => {
            let projection = expr_to_projection(expr)?;
            // Attach ONLY an explicit alias here. The previous
            // `.or_else(|| Some(render_expr_label(expr)))` synthesized an implicit
            // output label from the expression text and baked it into the legacy
            // Projection — mangling function names (`CAST` → `CAST:CAST(.. AS ..)`),
            // wrapping bare columns in a redundant `Alias(name, name)`, and thereby
            // breaking render→parse→render idempotency. The default output-column
            // label for an un-aliased projection is derived at render time from the
            // SelectItem (which keeps `alias: None`), not from this lowering.
            Some(attach_projection_alias(projection, alias.clone()))
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
        Projection::Window {
            name,
            args,
            window,
            alias,
        } => {
            let args = args
                .iter()
                .map(projection_to_expr)
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .map(|(expr, _)| expr)
                .collect();
            Some((
                Expr::WindowFunctionCall {
                    name: name.clone(),
                    args,
                    window: (**window).clone(),
                    span: Span::synthetic(),
                },
                alias.clone(),
            ))
        }
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
        // Reverse-lower the string-predicate FunctionCall forms emitted by
        // `filter_to_expr` (`LIKE`, `STARTS_WITH`, `ENDS_WITH`, `CONTAINS`)
        // back to the typed `Filter` variants. The runtime filter
        // evaluators (`runtime::join_filter`, virtual `red.*` reads) only
        // understand the typed variants; without this round-trip step a
        // `WHERE path STARTS WITH 'infra'` clause survives the parser as
        // `Filter::StartsWith` but is reduced to a `where_expr`-only
        // `FunctionCall` after subquery resolution clears `table.filter`,
        // and `effective_table_filter` would then fall through to a
        // generic `CompareExpr(FunctionCall, =, true)` that no virtual
        // table can evaluate. Refs #785.
        Expr::FunctionCall { name, args, .. } => string_predicate_from_function_call(name, args)
            .unwrap_or_else(|| Filter::CompareExpr {
                lhs: expr.clone(),
                op: CompareOp::Eq,
                rhs: Expr::lit(Value::Boolean(true)),
            }),
        _ => Filter::CompareExpr {
            lhs: expr.clone(),
            op: CompareOp::Eq,
            rhs: Expr::lit(Value::Boolean(true)),
        },
    }
}

fn string_predicate_from_function_call(name: &str, args: &[Expr]) -> Option<Filter> {
    if args.len() != 2 {
        return None;
    }
    let field = match &args[0] {
        Expr::Column { field, .. } => field.clone(),
        _ => return None,
    };
    let text = match &args[1] {
        Expr::Literal {
            value: Value::Text(value),
            ..
        } => value.as_ref().to_string(),
        _ => return None,
    };
    if name.eq_ignore_ascii_case("LIKE") {
        Some(Filter::Like {
            field,
            pattern: text,
        })
    } else if name.eq_ignore_ascii_case("STARTS_WITH") {
        Some(Filter::StartsWith {
            field,
            prefix: text,
        })
    } else if name.eq_ignore_ascii_case("ENDS_WITH") {
        Some(Filter::EndsWith {
            field,
            suffix: text,
        })
    } else if name.eq_ignore_ascii_case("CONTAINS") {
        Some(Filter::Contains {
            field,
            substring: text,
        })
    } else {
        None
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
            // ADR 0067 (#1721): `JSON_PARSE('{…}')` is the sanctioned escape
            // hatch for writing JSON from a runtime string. Fold a literal
            // string argument to a `Value::Json` here so it is accepted in
            // INSERT VALUES positions, using the same parse+canonicalize
            // pipeline as an inline JSON literal.
            if name.eq_ignore_ascii_case("JSON_PARSE") && args.len() == 1 {
                let raw = match fold_expr_to_value(args.into_iter().next().unwrap())? {
                    Value::Text(text) => text,
                    other => {
                        return Err(format!(
                            "JSON_PARSE() expects a string literal argument, got {other:?}"
                        ))
                    }
                };
                let parsed = reddb_types::utils::json::parse_json(raw.as_ref())
                    .map_err(|err| format!("JSON_PARSE failed to parse JSON: {err}"))?;
                let canonical = reddb_types::serde_json::Value::from(parsed);
                let bytes = reddb_types::json::to_vec(&canonical)
                    .map_err(|err| format!("JSON_PARSE failed to encode JSON: {err}"))?;
                return Ok(Value::Json(bytes));
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

// SQL-text label rendering for projection expressions. Retained verbatim from
// the pre-move module (no live caller today); kept for the upcoming Fase 2
// projection-aliasing work rather than dropped during the byte-faithful move.
#[allow(dead_code)]
fn render_expr_label(expr: &Expr) -> String {
    render_expr_label_prec(expr, 0)
}

#[allow(dead_code)]
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
        Expr::WindowFunctionCall { name, args, .. } => {
            let args = args
                .iter()
                .map(render_expr_label)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}({args}) OVER (...)")
        }
    }
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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

#[allow(dead_code)]
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
        Projection::Window {
            name, args, window, ..
        } => Projection::Window {
            name,
            args,
            window,
            alias: Some(alias),
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{
        GraphPattern, GraphQuery, JoinCondition, JoinQuery, NodeSelector, OrderByClause, PathQuery,
        QueryExpr, VectorQuery, VectorSource, WindowOrderItem, WindowSpec,
    };

    fn field(name: &str) -> FieldRef {
        FieldRef::column("", name)
    }

    fn col(name: &str) -> Expr {
        Expr::Column {
            field: field(name),
            span: Span::synthetic(),
        }
    }

    fn lit(value: Value) -> Expr {
        Expr::Literal {
            value,
            span: Span::synthetic(),
        }
    }

    fn parameter(index: usize) -> Expr {
        Expr::Parameter {
            index,
            span: Span::synthetic(),
        }
    }

    fn bin(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
        Expr::BinaryOp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span: Span::synthetic(),
        }
    }

    #[test]
    fn expr_contains_parameter_walks_nested_expression_shapes() {
        assert!(expr_contains_parameter(&bin(
            BinOp::Add,
            col("age"),
            parameter(1)
        )));

        let case = Expr::Case {
            branches: vec![(col("active"), lit(Value::Integer(1)))],
            else_: Some(Box::new(parameter(2))),
            span: Span::synthetic(),
        };
        assert!(expr_contains_parameter(&case));

        let window = Expr::WindowFunctionCall {
            name: "row_number".to_string(),
            args: Vec::new(),
            window: WindowSpec {
                partition_by: vec![col("tenant_id")],
                order_by: vec![WindowOrderItem {
                    expr: parameter(3),
                    ascending: true,
                    nulls_first: false,
                }],
                frame: None,
            },
            span: Span::synthetic(),
        };
        assert!(expr_contains_parameter(&window));

        let no_parameter = Expr::FunctionCall {
            name: "lower".to_string(),
            args: vec![col("name")],
            span: Span::synthetic(),
        };
        assert!(!expr_contains_parameter(&no_parameter));
    }

    #[test]
    fn expressions_lower_to_legacy_projections_with_aliases_preserved() {
        assert!(matches!(
            expr_to_projection(&col("*")),
            Some(Projection::All)
        ));

        let param_projection = expr_to_projection(&parameter(7)).unwrap();
        assert!(matches!(
            param_projection,
            Projection::Column(value) if value == format!("{PARAMETER_PROJECTION_PREFIX}7")
        ));

        let arithmetic = bin(BinOp::Add, col("age"), lit(Value::Integer(1)));
        let projection = select_item_to_projection(&SelectItem::Expr {
            expr: arithmetic,
            alias: Some("age_plus_one".to_string()),
        })
        .unwrap();
        assert!(matches!(
            projection,
            Projection::Function(ref name, ref args)
                if name == "ADD:age_plus_one" && args.len() == 2
        ));

        let negated = Expr::UnaryOp {
            op: UnaryOp::Neg,
            operand: Box::new(col("age")),
            span: Span::synthetic(),
        };
        assert!(matches!(
            expr_to_projection(&negated),
            Some(Projection::Function(name, args)) if name == "SUB" && args.len() == 2
        ));

        let cast = Expr::Cast {
            inner: Box::new(col("age")),
            target: reddb_types::types::DataType::Text,
            span: Span::synthetic(),
        };
        assert!(matches!(
            expr_to_projection(&cast),
            Some(Projection::Function(name, args)) if name == "CAST" && args.len() == 2
        ));

        let window = Expr::WindowFunctionCall {
            name: "sum".to_string(),
            args: vec![col("amount")],
            window: WindowSpec::default(),
            span: Span::synthetic(),
        };
        assert!(matches!(
            select_item_to_projection(&SelectItem::Expr {
                expr: window,
                alias: Some("running_sum".to_string()),
            }),
            Some(Projection::Window { name, alias, .. })
                if name == "SUM" && alias.as_deref() == Some("running_sum")
        ));
    }

    #[test]
    fn projections_raise_back_to_select_items_and_expression_nodes() {
        assert!(matches!(
            projection_to_select_item(&Projection::All),
            Some(SelectItem::Wildcard)
        ));

        let literal = projection_to_expr(&Projection::Column("LIT:42".to_string())).unwrap();
        assert!(matches!(
            literal,
            (
                Expr::Literal {
                    value: Value::Integer(42),
                    ..
                },
                None
            )
        ));

        let float_literal = projection_to_expr(&Projection::Column("LIT:3.5".to_string())).unwrap();
        assert!(matches!(
            float_literal,
            (Expr::Literal { value: Value::Float(v), .. }, None) if (v - 3.5).abs() < f64::EPSILON
        ));

        let null_literal = projection_to_expr(&Projection::Column("LIT:".to_string())).unwrap();
        assert!(matches!(
            null_literal,
            (
                Expr::Literal {
                    value: Value::Null,
                    ..
                },
                None
            )
        ));

        let function = Projection::Function(
            "LOWER:lower_name".to_string(),
            vec![Projection::Field(field("name"), None)],
        );
        let (expr, alias) = projection_to_expr(&function).unwrap();
        assert_eq!(alias.as_deref(), Some("lower_name"));
        assert!(
            matches!(expr, Expr::FunctionCall { name, args, .. } if name == "LOWER" && args.len() == 1)
        );

        let window = Projection::Window {
            name: "ROW_NUMBER".to_string(),
            args: Vec::new(),
            window: Box::new(WindowSpec::default()),
            alias: Some("rn".to_string()),
        };
        let (expr, alias) = projection_to_expr(&window).unwrap();
        assert_eq!(alias.as_deref(), Some("rn"));
        assert!(matches!(expr, Expr::WindowFunctionCall { name, .. } if name == "ROW_NUMBER"));
    }

    #[test]
    fn filters_round_trip_through_expression_forms() {
        let filters = vec![
            Filter::Compare {
                field: field("age"),
                op: CompareOp::Ge,
                value: Value::Integer(18),
            },
            Filter::CompareFields {
                left: field("updated_at"),
                op: CompareOp::Gt,
                right: field("created_at"),
            },
            Filter::And(
                Box::new(Filter::IsNotNull(field("email"))),
                Box::new(Filter::Like {
                    field: field("email"),
                    pattern: "%@example.com".to_string(),
                }),
            ),
            Filter::Or(
                Box::new(Filter::StartsWith {
                    field: field("path"),
                    prefix: "infra/".to_string(),
                }),
                Box::new(Filter::EndsWith {
                    field: field("path"),
                    suffix: ".log".to_string(),
                }),
            ),
            Filter::Not(Box::new(Filter::Contains {
                field: field("body"),
                substring: "secret".to_string(),
            })),
            Filter::IsNull(field("deleted_at")),
            Filter::In {
                field: field("status"),
                values: vec![Value::text("open"), Value::text("pending")],
            },
            Filter::Between {
                field: field("score"),
                low: Value::Integer(10),
                high: Value::Integer(20),
            },
        ];

        for filter in filters {
            let expr = filter_to_expr(&filter);
            assert_eq!(expr_to_filter(&expr), filter);
        }
    }

    #[test]
    fn expression_filters_specialize_common_predicates_and_fallbacks() {
        let flipped = expr_to_filter(&bin(BinOp::Lt, lit(Value::Integer(10)), col("age")));
        assert_eq!(
            flipped,
            Filter::Compare {
                field: field("age"),
                op: CompareOp::Gt,
                value: Value::Integer(10),
            }
        );

        let field_to_field = expr_to_filter(&bin(BinOp::Eq, col("lhs"), col("rhs")));
        assert_eq!(
            field_to_field,
            Filter::CompareFields {
                left: field("lhs"),
                op: CompareOp::Eq,
                right: field("rhs"),
            }
        );

        let arithmetic = expr_to_filter(&bin(BinOp::Add, col("age"), lit(Value::Integer(1))));
        assert!(matches!(
            arithmetic,
            Filter::CompareExpr {
                op: CompareOp::Eq,
                rhs: Expr::Literal {
                    value: Value::Boolean(true),
                    ..
                },
                ..
            }
        ));

        let negated_in = Expr::InList {
            target: Box::new(col("status")),
            values: vec![lit(Value::text("closed"))],
            negated: true,
            span: Span::synthetic(),
        };
        assert!(matches!(
            expr_to_filter(&negated_in),
            Filter::CompareExpr {
                op: CompareOp::Eq,
                ..
            }
        ));
    }

    #[test]
    fn table_effective_helpers_prefer_canonical_expr_fields() {
        let mut query = TableQuery::new("users");
        query.select_items = vec![
            SelectItem::Expr {
                expr: col("name"),
                alias: Some("display_name".to_string()),
            },
            SelectItem::Expr {
                expr: bin(BinOp::Add, col("age"), lit(Value::Integer(1))),
                alias: Some("next_age".to_string()),
            },
        ];
        query.where_expr = Some(bin(BinOp::Eq, col("active"), lit(Value::Boolean(true))));
        query.group_by_exprs = vec![col("name")];
        query.group_by = vec!["legacy_group".to_string()];
        query.having_expr = Some(bin(BinOp::Gt, col("age"), lit(Value::Integer(18))));

        let projections = effective_table_projections(&query);
        assert_eq!(projections.len(), 2);
        assert!(matches!(
            &projections[0],
            Projection::Field(FieldRef::TableColumn { column, .. }, Some(alias))
                if column == "name" && alias == "display_name"
        ));

        assert!(matches!(
            effective_table_filter(&query),
            Some(Filter::Compare {
                field: FieldRef::TableColumn { column, .. },
                op: CompareOp::Eq,
                value: Value::Boolean(true)
            }) if column == "active"
        ));
        assert_eq!(effective_table_group_by_exprs(&query), vec![col("name")]);
        assert!(matches!(
            effective_table_having_filter(&query),
            Some(Filter::Compare {
                field: FieldRef::TableColumn { column, .. },
                op: CompareOp::Gt,
                value: Value::Integer(18)
            }) if column == "age"
        ));

        let mut legacy = TableQuery::new("users");
        legacy.columns = vec![Projection::column("id")];
        legacy.group_by = vec!["tenant_id".to_string()];
        assert!(matches!(
            effective_table_projections(&legacy).as_slice(),
            [Projection::Column(column)] if column == "id"
        ));
        assert_eq!(
            effective_table_group_by_exprs(&legacy),
            vec![Expr::Column {
                field: field("tenant_id"),
                span: Span::synthetic(),
            }]
        );

        let default_projection = TableQuery::new("users");
        assert!(matches!(
            effective_table_projections(&default_projection).as_slice(),
            [Projection::All]
        ));
    }

    #[test]
    fn non_table_effective_helpers_preserve_existing_query_fields() {
        let mut join = JoinQuery::new(
            QueryExpr::Table(TableQuery::new("users")),
            QueryExpr::Graph(GraphQuery::new(GraphPattern::new())),
            JoinCondition::new(field("id"), FieldRef::node_id("n")),
        );
        join.filter = Some(Filter::IsNotNull(field("id")));
        join.return_items = vec![SelectItem::Expr {
            expr: col("name"),
            alias: Some("display_name".to_string()),
        }];
        join.return_ = vec![Projection::Column("legacy_name".to_string())];

        assert_eq!(
            effective_join_filter(&join),
            Some(Filter::IsNotNull(field("id")))
        );
        assert!(matches!(
            effective_join_projections(&join).as_slice(),
            [Projection::Field(FieldRef::TableColumn { column, .. }, Some(alias))]
                if column == "name" && alias == "display_name"
        ));

        join.return_items.clear();
        assert_eq!(
            effective_join_projections(&join),
            vec![Projection::Column("legacy_name".to_string())]
        );

        let graph_filter = Filter::StartsWith {
            field: FieldRef::node_prop("n", "path"),
            prefix: "infra/".to_string(),
        };
        let graph_return = vec![Projection::Field(FieldRef::node_prop("n", "name"), None)];
        let mut graph = GraphQuery::new(GraphPattern::new());
        graph.filter = Some(graph_filter.clone());
        graph.return_ = graph_return.clone();
        assert_eq!(effective_graph_filter(&graph), Some(graph_filter));
        assert_eq!(effective_graph_projections(&graph), graph_return);

        let path_filter = Filter::Contains {
            field: FieldRef::edge_prop("e", "label"),
            substring: "depends".to_string(),
        };
        let path_return = vec![Projection::Column("path".to_string())];
        let mut path = PathQuery::new(NodeSelector::by_id("start"), NodeSelector::by_id("end"));
        path.filter = Some(path_filter.clone());
        path.return_ = path_return.clone();
        assert_eq!(effective_path_filter(&path), Some(path_filter));
        assert_eq!(effective_path_projections(&path), path_return);

        let mut vector = VectorQuery::new("embeddings", VectorSource::literal(vec![0.1, 0.2]));
        assert!(effective_vector_filter(&vector).is_none());
        vector.filter = Some(MetadataFilter::eq("source", "nmap"));
        assert!(matches!(
            effective_vector_filter(&vector),
            Some(MetadataFilter::Eq(key, reddb_types::vector_metadata::MetadataValue::String(value)))
                if key == "source" && value == "nmap"
        ));
    }

    #[test]
    fn insert_update_delete_helpers_fold_canonical_expressions() {
        let insert = InsertQuery {
            table: "users".to_string(),
            entity_type: crate::ast::InsertEntityType::Row,
            columns: vec!["name".to_string(), "password".to_string()],
            value_exprs: vec![vec![
                lit(Value::text("ada")),
                Expr::FunctionCall {
                    name: "PASSWORD".to_string(),
                    args: vec![lit(Value::text("pw"))],
                    span: Span::synthetic(),
                },
            ]],
            values: Vec::new(),
            returning: None,
            ttl_ms: None,
            expires_at_ms: None,
            with_metadata: Vec::new(),
            auto_embed: None,
            suppress_events: false,
        };
        let rows = effective_insert_rows(&insert).unwrap();
        assert!(matches!(
            rows.as_slice(),
            [row] if row[0] == Value::text("ada")
                && matches!(&row[1], Value::Password(value) if value == "@@plain@@pw")
        ));

        let update = UpdateQuery {
            table: "users".to_string(),
            target: crate::ast::UpdateTarget::Rows,
            assignment_exprs: Vec::new(),
            compound_assignment_ops: Vec::new(),
            assignments: Vec::new(),
            where_expr: Some(bin(BinOp::Eq, col("id"), lit(Value::Integer(1)))),
            filter: None,
            ttl_ms: None,
            expires_at_ms: None,
            with_metadata: Vec::new(),
            returning: None,
            order_by: vec![OrderByClause::asc(field("id"))],
            limit: Some(1),
            claim_limit: None,
            claim_exact: false,
            suppress_events: false,
        };
        assert!(matches!(
            effective_update_filter(&update),
            Some(Filter::Compare {
                field: FieldRef::TableColumn { column, .. },
                value: Value::Integer(1),
                ..
            }) if column == "id"
        ));

        let delete = DeleteQuery {
            table: "users".to_string(),
            where_expr: Some(Expr::IsNull {
                operand: Box::new(col("deleted_at")),
                negated: false,
                span: Span::synthetic(),
            }),
            filter: None,
            returning: None,
            suppress_events: false,
        };
        assert!(matches!(
            effective_delete_filter(&delete),
            Some(Filter::IsNull(FieldRef::TableColumn { column, .. })) if column == "deleted_at"
        ));
    }

    #[test]
    fn fold_expr_to_value_handles_secret_constructors_unary_and_errors() {
        assert_eq!(
            fold_expr_to_value(Expr::UnaryOp {
                op: UnaryOp::Neg,
                operand: Box::new(lit(Value::UnsignedInteger(7))),
                span: Span::synthetic(),
            })
            .unwrap(),
            Value::Integer(-7)
        );
        assert_eq!(
            fold_expr_to_value(Expr::UnaryOp {
                op: UnaryOp::Not,
                operand: Box::new(lit(Value::Boolean(false))),
                span: Span::synthetic(),
            })
            .unwrap(),
            Value::Boolean(true)
        );

        let secret = fold_expr_to_value(Expr::FunctionCall {
            name: "SECRET".to_string(),
            args: vec![lit(Value::text("token"))],
            span: Span::synthetic(),
        })
        .unwrap();
        assert!(matches!(secret, Value::Secret(bytes) if bytes == b"@@plain@@token"));

        let casted = fold_expr_to_value(Expr::Cast {
            inner: Box::new(lit(Value::Integer(5))),
            target: reddb_types::types::DataType::Text,
            span: Span::synthetic(),
        })
        .unwrap();
        assert_eq!(casted, Value::Integer(5));

        assert!(fold_expr_to_value(bin(BinOp::Add, col("age"), lit(Value::Integer(1)))).is_err());
        assert!(fold_expr_to_value(Expr::FunctionCall {
            name: "PASSWORD".to_string(),
            args: vec![lit(Value::Integer(1))],
            span: Span::synthetic(),
        })
        .is_err());
    }

    #[test]
    fn render_label_and_literal_helpers_cover_private_round_trip_paths() {
        assert_eq!(
            render_expr_label(&lit(Value::text("O'Reilly"))),
            "'O''Reilly'"
        );
        assert_eq!(render_expr_label(&parameter(4)), "$4");
        assert_eq!(
            render_expr_label(&bin(
                BinOp::Mul,
                bin(BinOp::Add, col("a"), col("b")),
                col("c")
            )),
            "(a + b) * c"
        );
        assert_eq!(
            render_expr_label(&Expr::UnaryOp {
                op: UnaryOp::Not,
                operand: Box::new(col("active")),
                span: Span::synthetic(),
            }),
            "NOT active"
        );
        assert_eq!(
            render_expr_label(&Expr::Cast {
                inner: Box::new(col("age")),
                target: reddb_types::types::DataType::Text,
                span: Span::synthetic(),
            }),
            "CAST(age AS TEXT)"
        );
        assert_eq!(
            render_expr_label(&Expr::FunctionCall {
                name: "lower".to_string(),
                args: vec![col("name")],
                span: Span::synthetic(),
            }),
            "lower(name)"
        );
        assert_eq!(
            render_expr_label(&Expr::Case {
                branches: vec![(col("active"), lit(Value::text("yes")))],
                else_: Some(Box::new(lit(Value::text("no")))),
                span: Span::synthetic(),
            }),
            "CASE WHEN active THEN 'yes' ELSE 'no' END"
        );
        assert_eq!(
            render_expr_label(&Expr::IsNull {
                operand: Box::new(col("deleted_at")),
                negated: true,
                span: Span::synthetic(),
            }),
            "deleted_at IS NOT NULL"
        );
        assert_eq!(
            render_expr_label(&Expr::InList {
                target: Box::new(col("status")),
                values: vec![lit(Value::text("closed"))],
                negated: true,
                span: Span::synthetic(),
            }),
            "status NOT IN ('closed')"
        );
        assert_eq!(
            render_expr_label(&Expr::Between {
                target: Box::new(col("age")),
                low: Box::new(lit(Value::Integer(18))),
                high: Box::new(lit(Value::Integer(65))),
                negated: false,
                span: Span::synthetic(),
            }),
            "age BETWEEN 18 AND 65"
        );
        assert_eq!(
            render_expr_label(&Expr::Subquery {
                query: crate::ast::ExprSubquery {
                    query: Box::new(QueryExpr::Table(TableQuery::new("users"))),
                },
                span: Span::synthetic(),
            }),
            "subquery"
        );
        assert_eq!(
            render_expr_label(&Expr::WindowFunctionCall {
                name: "sum".to_string(),
                args: vec![col("amount")],
                window: WindowSpec::default(),
                span: Span::synthetic(),
            }),
            "sum(amount) OVER (...)"
        );

        assert_eq!(render_field_label(&FieldRef::node_id("n")), "n.id");
        assert_eq!(
            render_field_label(&FieldRef::edge_prop("e", "weight")),
            "e.weight"
        );
        assert_eq!(render_binop_label(BinOp::Concat), "||");
        assert_eq!(render_sql_literal_label(&Value::Float(3.0)), "3");
        assert_eq!(
            render_projection_literal(&Value::Array(vec![Value::Integer(1), Value::text("x"),])),
            "@RL:[1,\"x\"]"
        );
        assert_eq!(
            render_projection_literal(&Value::Vector(vec![1.0, 2.5])),
            "@RL:V[1,2.5]"
        );
        assert_eq!(
            render_projection_literal(&Value::Json(br#"{"a":1}"#.to_vec())),
            "@RL:\"<json 7 bytes>\""
        );
        assert!(matches!(
            projection_from_literal(&Value::Boolean(true)),
            Some(Projection::Expression(_, None))
        ));
        assert_eq!(
            split_projection_function_alias("LOWER:name").1.as_deref(),
            Some("name")
        );
        assert_eq!(split_projection_function_alias(":bad").1, None);
    }

    #[test]
    fn lowering_fallbacks_cover_alias_legacy_and_non_specialized_paths() {
        for (op, name) in [
            (BinOp::Sub, "SUB"),
            (BinOp::Div, "DIV"),
            (BinOp::Mod, "MOD"),
            (BinOp::Concat, "CONCAT"),
        ] {
            assert!(matches!(
                expr_to_projection(&bin(op, col("lhs"), col("rhs"))),
                Some(Projection::Function(function, args))
                    if function == name && args.len() == 2
            ));
        }

        assert!(matches!(
            select_item_to_projection(&SelectItem::Expr {
                expr: lit(Value::Integer(1)),
                alias: Some("one".to_string()),
            }),
            Some(Projection::Alias(column, alias)) if column == "LIT:1" && alias == "one"
        ));
        assert!(matches!(
            select_item_to_projection(&SelectItem::Expr {
                expr: Expr::UnaryOp {
                    op: UnaryOp::Not,
                    operand: Box::new(col("active")),
                    span: Span::synthetic(),
                },
                alias: Some("inactive".to_string()),
            }),
            Some(Projection::Expression(_, Some(alias))) if alias == "inactive"
        ));

        let legacy_insert = InsertQuery {
            table: "users".to_string(),
            entity_type: crate::ast::InsertEntityType::Row,
            columns: vec!["id".to_string()],
            value_exprs: Vec::new(),
            values: vec![vec![Value::Integer(1)]],
            returning: None,
            ttl_ms: None,
            expires_at_ms: None,
            with_metadata: Vec::new(),
            auto_embed: None,
            suppress_events: false,
        };
        assert_eq!(
            effective_insert_rows(&legacy_insert).unwrap(),
            vec![vec![Value::Integer(1)]]
        );

        assert!(matches!(
            expr_to_filter(&Expr::IsNull {
                operand: Box::new(lit(Value::Null)),
                negated: false,
                span: Span::synthetic(),
            }),
            Filter::CompareExpr { .. }
        ));
        assert!(matches!(
            expr_to_filter(&Expr::InList {
                target: Box::new(col("status")),
                values: vec![col("other_status")],
                negated: false,
                span: Span::synthetic(),
            }),
            Filter::CompareExpr { .. }
        ));
        assert!(matches!(
            expr_to_filter(&Expr::Between {
                target: Box::new(col("age")),
                low: Box::new(lit(Value::Integer(18))),
                high: Box::new(lit(Value::Integer(65))),
                negated: true,
                span: Span::synthetic(),
            }),
            Filter::CompareExpr { .. }
        ));
        assert!(matches!(
            expr_to_filter(&Expr::FunctionCall {
                name: "UNKNOWN".to_string(),
                args: vec![col("name"), lit(Value::text("a"))],
                span: Span::synthetic(),
            }),
            Filter::CompareExpr { .. }
        ));
        assert!(matches!(
            expr_to_filter(&Expr::FunctionCall {
                name: "LIKE".to_string(),
                args: vec![lit(Value::text("not_field")), lit(Value::text("a"))],
                span: Span::synthetic(),
            }),
            Filter::CompareExpr { .. }
        ));

        assert_eq!(
            fold_expr_to_value(Expr::UnaryOp {
                op: UnaryOp::Neg,
                operand: Box::new(lit(Value::Float(1.5))),
                span: Span::synthetic(),
            })
            .unwrap(),
            Value::Float(-1.5)
        );
        assert!(fold_expr_to_value(Expr::UnaryOp {
            op: UnaryOp::Not,
            operand: Box::new(lit(Value::Integer(1))),
            span: Span::synthetic(),
        })
        .is_err());
        assert!(fold_expr_to_value(Expr::FunctionCall {
            name: "LOWER".to_string(),
            args: vec![lit(Value::text("Ada"))],
            span: Span::synthetic(),
        })
        .is_err());

        assert_eq!(render_projection_literal(&Value::Null), "");
        assert_eq!(render_projection_literal(&Value::UnsignedInteger(7)), "7");
        assert_eq!(render_projection_literal(&Value::Boolean(false)), "false");
        assert_eq!(
            render_projection_literal(&Value::Blob(vec![1, 2, 3])),
            "@RL:\"<blob 3 bytes>\""
        );
        assert_eq!(serialize_value_json(&Value::Null), "null");
    }
}
