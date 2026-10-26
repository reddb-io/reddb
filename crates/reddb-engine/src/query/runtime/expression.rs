//! Canonical native optional-value Expr traversal used by runtime ORDER BY.
//! This preserves legacy evaluation rules; it is distinct from the typed evaluator.
use reddb_rql::ast::{BinOp, CompareOp, Expr, FieldRef, UnaryOp};
use reddb_types::Value;

/// A host binds the source record and retains secret policy, scalar effects,
/// catalog/context access and value-function dispatch. Special CONFIG/KV/reference
/// calls receive raw arguments; their evaluated defaults must use the supplied
/// callback so nesting limits and lazy evaluation remain in the same walk.
pub trait RuntimeExpressionHost {
    fn secret_query_input(&self, value: Value) -> Option<Value>;
    fn resolve_field(&self, field: &FieldRef) -> Option<Value>;
    fn evaluate_special_function(
        &self,
        name: &str,
        arguments: &[Expr],
        evaluate: &dyn Fn(&Expr) -> Option<Value>,
    ) -> Option<Value>;
    fn evaluate_function(&self, name: &str, arguments: &[Value]) -> Option<Value>;
}

pub fn evaluate_runtime_expr_with_host<H: RuntimeExpressionHost>(
    host: &H,
    expression: &Expr,
) -> Option<Value> {
    evaluate_runtime_expr_at_depth(host, expression, 0)
}

fn evaluate_runtime_expr_at_depth<H: RuntimeExpressionHost>(
    host: &H,
    expr: &Expr,
    depth: u32,
) -> Option<Value> {
    if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        return None;
    }
    let evaluate = |expression: &Expr| evaluate_runtime_expr_at_depth(host, expression, depth + 1);
    match expr {
        Expr::Literal { value, .. } => host.secret_query_input(value.clone()),

        Expr::Column { field, .. } => host
            .resolve_field(field)
            .and_then(|value| host.secret_query_input(value)),

        Expr::Parameter { .. } => {
            // Parameter placeholders only appear in prepared-statement
            // plans; they must be bound to concrete values before the
            // runtime sees them. Hitting this arm means the bind phase
            // skipped a slot, which is a bug further upstream.
            None
        }

        Expr::UnaryOp {
            op,
            operand,
            span: _,
        } => {
            let v = evaluate(operand)?;
            match op {
                UnaryOp::Neg => negate_value(&v),
                UnaryOp::Not => match v {
                    Value::Boolean(b) => Some(Value::Boolean(!b)),
                    _ => None,
                },
            }
        }

        Expr::BinaryOp {
            op,
            lhs,
            rhs,
            span: _,
        } => {
            // Short-circuit AND/OR on boolean LHS first so expensive
            // RHS subtrees (function calls, nested arithmetic) are
            // skipped when the result is already determined.
            match op {
                BinOp::And => {
                    let l = evaluate(lhs)?;
                    if let Value::Boolean(false) = l {
                        return Some(Value::Boolean(false));
                    }
                    let r = evaluate(rhs)?;
                    match (l, r) {
                        (Value::Boolean(a), Value::Boolean(b)) => Some(Value::Boolean(a && b)),
                        _ => None,
                    }
                }
                BinOp::Or => {
                    let l = evaluate(lhs)?;
                    if let Value::Boolean(true) = l {
                        return Some(Value::Boolean(true));
                    }
                    let r = evaluate(rhs)?;
                    match (l, r) {
                        (Value::Boolean(a), Value::Boolean(b)) => Some(Value::Boolean(a || b)),
                        _ => None,
                    }
                }
                _ => {
                    let l = evaluate(lhs)?;
                    let r = evaluate(rhs)?;
                    apply_binop(host, *op, l, r)
                }
            }
        }

        Expr::Cast {
            inner,
            target,
            span: _,
        } => {
            let v = evaluate(inner)?;
            Some(runtime_cast(&v, *target))
        }

        Expr::FunctionCall { name, args, .. } => {
            let upper = name.to_uppercase();
            if matches!(
                upper.as_str(),
                "CONFIG" | "KV" | "__SECRET_REF" | "__KV_REF"
            ) {
                return host.evaluate_special_function(&upper, args, &evaluate);
            }
            // Preserve native eager, left-to-right argument evaluation. A miss
            // is passed to the host as SQL NULL rather than aborting the call.
            let mut values = Vec::with_capacity(args.len());
            for argument in args {
                values.push(evaluate(argument).unwrap_or(Value::Null));
            }
            host.evaluate_function(&upper, &values)
        }

        Expr::Case {
            branches,
            else_,
            span: _,
        } => {
            for (cond, then_val) in branches {
                let cond_val = evaluate(cond);
                if matches!(cond_val, Some(Value::Boolean(true))) {
                    return evaluate(then_val);
                }
            }
            if let Some(else_expr) = else_ {
                evaluate(else_expr)
            } else {
                Some(Value::Null)
            }
        }

        Expr::IsNull {
            operand,
            negated,
            span: _,
        } => {
            let v = evaluate(operand);
            let is_null = matches!(v, None | Some(Value::Null));
            Some(Value::Boolean(if *negated { !is_null } else { is_null }))
        }

        Expr::InList {
            target,
            values,
            negated,
            span: _,
        } => {
            let t = evaluate(target)?;
            let mut hit = false;
            for v in values {
                if let Some(candidate) = evaluate(v) {
                    if compare_values(host, &t, &candidate, reddb_rql::ast::CompareOp::Eq) {
                        hit = true;
                        break;
                    }
                }
            }
            Some(Value::Boolean(if *negated { !hit } else { hit }))
        }

        Expr::Between {
            target,
            low,
            high,
            negated,
            span: _,
        } => {
            let t = evaluate(target)?;
            let lo = evaluate(low)?;
            let hi = evaluate(high)?;
            let in_range = compare_values(host, &t, &lo, reddb_rql::ast::CompareOp::Ge)
                && compare_values(host, &t, &hi, reddb_rql::ast::CompareOp::Le);
            Some(Value::Boolean(if *negated { !in_range } else { in_range }))
        }

        Expr::Subquery { .. } => None,
        // Window output belongs to the host window phase; a standalone
        // window expression remains unresolvable in this scalar walk.
        Expr::WindowFunctionCall { .. } => None,
    }
}

fn compare_values<H: RuntimeExpressionHost>(
    host: &H,
    left: &Value,
    right: &Value,
    op: CompareOp,
) -> bool {
    super::values::compare_runtime_values(left, right, op, &|value| host.secret_query_input(value))
}

fn negate_value(v: &Value) -> Option<Value> {
    match v {
        Value::Integer(n) => n.checked_neg().map(Value::Integer),
        Value::BigInt(n) => n.checked_neg().map(Value::BigInt),
        Value::Float(f) => Some(Value::Float(-f)),
        _ => None,
    }
}

fn apply_binop<H: RuntimeExpressionHost>(host: &H, op: BinOp, a: Value, b: Value) -> Option<Value> {
    use reddb_rql::ast::CompareOp;
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => arith(op, a, b),
        BinOp::Concat => Some(Value::text(format!("{}{}", a.plain_text(), b.plain_text()))),
        BinOp::Eq => Some(Value::Boolean(compare_values(host, &a, &b, CompareOp::Eq))),
        BinOp::Ne => Some(Value::Boolean(compare_values(host, &a, &b, CompareOp::Ne))),
        BinOp::Lt => Some(Value::Boolean(compare_values(host, &a, &b, CompareOp::Lt))),
        BinOp::Le => Some(Value::Boolean(compare_values(host, &a, &b, CompareOp::Le))),
        BinOp::Gt => Some(Value::Boolean(compare_values(host, &a, &b, CompareOp::Gt))),
        BinOp::Ge => Some(Value::Boolean(compare_values(host, &a, &b, CompareOp::Ge))),
        BinOp::And | BinOp::Or => None, // handled upstream (short-circuit)
    }
}

fn arith(op: BinOp, a: Value, b: Value) -> Option<Value> {
    if let Some(value) = timestamp_ms_arith(op, &a, &b) {
        return Some(value);
    }

    let (la, lb) = (value_as_number(&a)?, value_as_number(&b)?);
    let force_float = matches!(op, BinOp::Div) || la.1 || lb.1;
    let out = match op {
        BinOp::Add => la.0 + lb.0,
        BinOp::Sub => la.0 - lb.0,
        BinOp::Mul => la.0 * lb.0,
        BinOp::Div => {
            if lb.0 == 0.0 {
                return None;
            }
            la.0 / lb.0
        }
        BinOp::Mod => {
            if lb.0 == 0.0 {
                return None;
            }
            la.0 % lb.0
        }
        _ => return None,
    };
    Some(if force_float {
        Value::Float(out)
    } else {
        Value::Integer(out as i64)
    })
}

fn timestamp_ms_arith(op: BinOp, a: &Value, b: &Value) -> Option<Value> {
    match (op, a, b) {
        (BinOp::Add, Value::TimestampMs(ts), rhs) => Some(Value::TimestampMs(
            ts.checked_add(duration_ms_operand(rhs)?)?,
        )),
        (BinOp::Add, lhs, Value::TimestampMs(ts)) => Some(Value::TimestampMs(
            ts.checked_add(duration_ms_operand(lhs)?)?,
        )),
        (BinOp::Sub, Value::TimestampMs(ts), rhs) => Some(Value::TimestampMs(
            ts.checked_sub(duration_ms_operand(rhs)?)?,
        )),
        _ => None,
    }
}

fn duration_ms_operand(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) | Value::BigInt(value) | Value::Duration(value) => Some(*value),
        Value::UnsignedInteger(value) => i64::try_from(*value).ok(),
        _ => None,
    }
}

/// Tuple `(f64 value, is_float_literally)` preserves native arithmetic result types.
fn value_as_number(v: &Value) -> Option<(f64, bool)> {
    match v {
        Value::Integer(n) | Value::BigInt(n) => Some((*n as f64, false)),
        Value::UnsignedInteger(n) => Some((*n as f64, false)),
        Value::Float(f) => Some((*f, true)),
        Value::Decimal(d) => Some((reddb_types::types::decimal_to_f64(*d), true)),
        Value::Text(s) => s
            .parse::<i64>()
            .map(|n| (n as f64, false))
            .or_else(|_| s.parse::<f64>().map(|f| (f, true)))
            .ok(),
        _ => None,
    }
}

fn runtime_cast(src: &Value, target: reddb_types::DataType) -> Value {
    use reddb_types::types::DataType as DT;
    match (src, target) {
        (v, DT::Text) => Value::text(v.display_string()),
        (Value::Integer(n), DT::Float) => Value::Float(*n as f64),
        (Value::Integer(n), DT::BigInt) => Value::BigInt(*n),
        (Value::Integer(n), DT::UnsignedInteger) if *n >= 0 => Value::UnsignedInteger(*n as u64),
        (Value::UnsignedInteger(n), DT::Integer) if *n <= i64::MAX as u64 => {
            Value::Integer(*n as i64)
        }
        (Value::UnsignedInteger(n), DT::Float) => Value::Float(*n as f64),
        (Value::Float(f), DT::Integer) => Value::Integer(*f as i64),
        (Value::Float(f), DT::UnsignedInteger) if *f >= 0.0 => Value::UnsignedInteger(*f as u64),
        (Value::Boolean(b), DT::Integer) => Value::Integer(if *b { 1 } else { 0 }),
        (Value::Integer(n), DT::Boolean) => Value::Boolean(*n != 0),
        (Value::Text(s), t) => match reddb_types::coerce::coerce(s, t, None) {
            Ok(v) => v,
            Err(_) => Value::Null,
        },
        (v, t) => match reddb_types::coerce::coerce(&v.display_string(), t, None) {
            Ok(v) => v,
            Err(_) => Value::Null,
        },
    }
}
