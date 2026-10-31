//! Canonical native scalar traversal with optional and fallible host adapters.
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

/// Fallible traversal for hosts whose scalar resolution performs storage I/O.
/// A missing value remains distinct from an operating error, including in lazy
/// defaults, CASE, boolean branches and function arguments.
pub trait FallibleRuntimeExpressionHost {
    type Error;
    fn secret_query_input(&self, value: Value) -> Result<Option<Value>, Self::Error>;
    fn resolve_field(&self, field: &FieldRef) -> Result<Option<Value>, Self::Error>;
    fn evaluate_special_function(
        &self,
        name: &str,
        arguments: &[Expr],
        evaluate: &dyn Fn(&Expr) -> Result<Option<Value>, Self::Error>,
    ) -> Result<Option<Value>, Self::Error>;
    fn evaluate_function(
        &self,
        name: &str,
        arguments: &[Value],
    ) -> Result<Option<Value>, Self::Error>;
}

struct OptionalExpressionHost<'a, H>(&'a H);
impl<H: RuntimeExpressionHost> FallibleRuntimeExpressionHost for OptionalExpressionHost<'_, H> {
    type Error = std::convert::Infallible;
    fn secret_query_input(&self, value: Value) -> Result<Option<Value>, Self::Error> {
        Ok(self.0.secret_query_input(value))
    }
    fn resolve_field(&self, field: &FieldRef) -> Result<Option<Value>, Self::Error> {
        Ok(self.0.resolve_field(field))
    }
    fn evaluate_special_function(
        &self,
        name: &str,
        arguments: &[Expr],
        evaluate: &dyn Fn(&Expr) -> Result<Option<Value>, Self::Error>,
    ) -> Result<Option<Value>, Self::Error> {
        Ok(self
            .0
            .evaluate_special_function(name, arguments, &|expression| match evaluate(expression) {
                Ok(value) => value,
                Err(never) => match never {},
            }))
    }
    fn evaluate_function(
        &self,
        name: &str,
        arguments: &[Value],
    ) -> Result<Option<Value>, Self::Error> {
        Ok(self.0.evaluate_function(name, arguments))
    }
}

pub fn evaluate_runtime_expr_with_host<H: RuntimeExpressionHost>(
    host: &H,
    expression: &Expr,
) -> Option<Value> {
    match try_evaluate_runtime_expr_with_host(&OptionalExpressionHost(host), expression) {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

pub fn try_evaluate_runtime_expr_with_host<H: FallibleRuntimeExpressionHost>(
    host: &H,
    expression: &Expr,
) -> Result<Option<Value>, H::Error> {
    evaluate_runtime_expr_at_depth(host, expression, 0)
}

fn evaluate_runtime_expr_at_depth<H: FallibleRuntimeExpressionHost>(
    host: &H,
    expr: &Expr,
    depth: u32,
) -> Result<Option<Value>, H::Error> {
    if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        return Ok(None);
    }
    macro_rules! resolved {
        ($value:expr) => {
            match $value {
                Some(value) => value,
                None => return Ok(None),
            }
        };
    }
    let evaluate = |expression: &Expr| evaluate_runtime_expr_at_depth(host, expression, depth + 1);
    match expr {
        Expr::Literal { value, .. } => host.secret_query_input(value.clone()),
        Expr::Column { field, .. } => {
            let value = resolved!(host.resolve_field(field)?);
            host.secret_query_input(value)
        }
        Expr::Parameter { .. } => Ok(None),
        Expr::UnaryOp { op, operand, .. } => {
            let value = resolved!(evaluate(operand)?);
            Ok(match op {
                UnaryOp::Neg => negate_value(&value),
                UnaryOp::Not => match value {
                    Value::Boolean(value) => Some(Value::Boolean(!value)),
                    _ => None,
                },
            })
        }
        Expr::BinaryOp { op, lhs, rhs, .. } => {
            let left = resolved!(evaluate(lhs)?);
            if matches!((op, &left), (BinOp::And, Value::Boolean(false))) {
                return Ok(Some(Value::Boolean(false)));
            }
            if matches!((op, &left), (BinOp::Or, Value::Boolean(true))) {
                return Ok(Some(Value::Boolean(true)));
            }
            let right = resolved!(evaluate(rhs)?);
            match op {
                BinOp::And | BinOp::Or => Ok(match (left, right) {
                    (Value::Boolean(left), Value::Boolean(right)) => {
                        Some(Value::Boolean(if *op == BinOp::And {
                            left && right
                        } else {
                            left || right
                        }))
                    }
                    _ => None,
                }),
                _ => apply_binop(host, *op, left, right),
            }
        }
        Expr::Cast { inner, target, .. } => {
            let value = resolved!(evaluate(inner)?);
            Ok(Some(runtime_cast(&value, *target)))
        }
        Expr::FunctionCall { name, args, .. } => {
            let upper = name.to_uppercase();
            if matches!(
                upper.as_str(),
                "CONFIG" | "KV" | "__SECRET_REF" | "__KV_REF"
            ) {
                return host.evaluate_special_function(&upper, args, &evaluate);
            }
            let mut values = Vec::with_capacity(args.len());
            for argument in args {
                values.push(evaluate(argument)?.unwrap_or(Value::Null));
            }
            host.evaluate_function(&upper, &values)
        }
        Expr::Case {
            branches, else_, ..
        } => {
            for (condition, value) in branches {
                if matches!(evaluate(condition)?, Some(Value::Boolean(true))) {
                    return evaluate(value);
                }
            }
            match else_ {
                Some(expression) => evaluate(expression),
                None => Ok(Some(Value::Null)),
            }
        }
        Expr::IsNull {
            operand, negated, ..
        } => {
            let value = evaluate(operand)?;
            let is_null = matches!(value, None | Some(Value::Null));
            Ok(Some(Value::Boolean(if *negated {
                !is_null
            } else {
                is_null
            })))
        }
        Expr::InList {
            target,
            values,
            negated,
            ..
        } => {
            let target = resolved!(evaluate(target)?);
            let mut hit = false;
            for value in values {
                if let Some(candidate) = evaluate(value)? {
                    if compare_values(host, &target, &candidate, CompareOp::Eq)? {
                        hit = true;
                        break;
                    }
                }
            }
            Ok(Some(Value::Boolean(if *negated { !hit } else { hit })))
        }
        Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } => {
            let target = resolved!(evaluate(target)?);
            let low = resolved!(evaluate(low)?);
            let high = resolved!(evaluate(high)?);
            let in_range = compare_values(host, &target, &low, CompareOp::Ge)?
                && compare_values(host, &target, &high, CompareOp::Le)?;
            Ok(Some(Value::Boolean(if *negated {
                !in_range
            } else {
                in_range
            })))
        }
        Expr::Subquery { .. } | Expr::WindowFunctionCall { .. } => Ok(None),
    }
}

fn compare_values<H: FallibleRuntimeExpressionHost>(
    host: &H,
    left: &Value,
    right: &Value,
    op: CompareOp,
) -> Result<bool, H::Error> {
    let mut left = std::borrow::Cow::Borrowed(left);
    let mut right = std::borrow::Cow::Borrowed(right);
    for _ in 0..=reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        if !matches!(left.as_ref(), Value::Secret(_)) && !matches!(right.as_ref(), Value::Secret(_))
        {
            return Ok(super::values::compare_runtime_values(
                &left, &right, op, &Some,
            ));
        }
        let Some(value) = host.secret_query_input(left.into_owned())? else {
            return Ok(false);
        };
        left = std::borrow::Cow::Owned(value);
        let Some(value) = host.secret_query_input(right.into_owned())? else {
            return Ok(false);
        };
        right = std::borrow::Cow::Owned(value);
    }
    Ok(false)
}

fn negate_value(v: &Value) -> Option<Value> {
    match v {
        Value::Integer(n) => n.checked_neg().map(Value::Integer),
        Value::BigInt(n) => n.checked_neg().map(Value::BigInt),
        Value::Float(f) => Some(Value::Float(-f)),
        _ => None,
    }
}

fn apply_binop<H: FallibleRuntimeExpressionHost>(
    host: &H,
    op: BinOp,
    a: Value,
    b: Value,
) -> Result<Option<Value>, H::Error> {
    let comparison = match op {
        BinOp::Eq => Some(CompareOp::Eq),
        BinOp::Ne => Some(CompareOp::Ne),
        BinOp::Lt => Some(CompareOp::Lt),
        BinOp::Le => Some(CompareOp::Le),
        BinOp::Gt => Some(CompareOp::Gt),
        BinOp::Ge => Some(CompareOp::Ge),
        _ => None,
    };
    if let Some(comparison) = comparison {
        return Ok(Some(Value::Boolean(compare_values(
            host, &a, &b, comparison,
        )?)));
    }
    Ok(match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => arith(op, a, b),
        BinOp::Concat => Some(Value::text(format!("{}{}", a.plain_text(), b.plain_text()))),
        _ => None,
    })
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
