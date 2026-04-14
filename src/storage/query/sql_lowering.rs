use crate::storage::query::ast::{
    BinOp, CompareOp, Expr, FieldRef, Filter, Projection, SelectItem, Span, TableQuery, UnaryOp,
};
use crate::storage::schema::Value;

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
        Expr::Parameter { .. } => None,
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
        Expr::IsNull { .. } | Expr::InList { .. } | Expr::Between { .. } => {
            Some(boolean_expr_projection(expr.clone()))
        }
    }
}

pub fn select_item_to_projection(item: &SelectItem) -> Option<Projection> {
    match item {
        SelectItem::Wildcard => Some(Projection::All),
        SelectItem::Expr { expr, alias } => {
            let projection = expr_to_projection(expr)?;
            Some(match alias {
                Some(alias) => attach_projection_alias(projection, Some(alias.clone())),
                None => projection,
            })
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
                    value: Value::Text(pattern.clone()),
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
                    value: Value::Text(prefix.clone()),
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
                    value: Value::Text(suffix.clone()),
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
                    value: Value::Text(substring.clone()),
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
        Value::Text(v) => v.clone(),
        Value::Boolean(true) => "true".to_string(),
        Value::Boolean(false) => "false".to_string(),
        other => other.to_string(),
    }
}
