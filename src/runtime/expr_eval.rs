//! Runtime evaluator for the Fase 2 `ast::Expr` tree.
//!
//! This module evaluates a parsed `Expr` against a concrete
//! `UnifiedRecord`, returning `Option<Value>` where `None` means
//! "unresolvable" (null / missing field / unsupported op). The
//! evaluator intentionally mirrors the semantics of the legacy
//! `evaluate_runtime_filter` / `evaluate_scalar_function` paths so
//! call sites can swap in `evaluate_runtime_expr` without behavioural
//! drift.
//!
//! Scope today (Week 3):
//! - `OrderByClause.expr` — sort key evaluation for ORDER BY expr.
//!
//! Scope tomorrow (Week 3 continuation):
//! - `Filter::Compare` RHS once the variant grows an `Expr` slot.
//! - `Projection::Expression` once the planner flips from Filter
//!   to Expr for scalar projection bodies.

use super::join_filter::{compare_runtime_values, evaluate_runtime_filter, resolve_runtime_field};
use crate::storage::query::ast::{BinOp, Expr, Filter, UnaryOp};
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::Value;

/// Evaluate an `Expr` against a record and return its resulting
/// `Value`, or `None` if the expression cannot be resolved (missing
/// column, type mismatch, unsupported feature for this phase).
pub(super) fn evaluate_runtime_expr(
    expr: &Expr,
    record: &UnifiedRecord,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Option<Value> {
    match expr {
        Expr::Literal { value, .. } => Some(value.clone()),

        Expr::Column { field, .. } => resolve_runtime_field(record, field, table_name, table_alias),

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
            let v = evaluate_runtime_expr(operand, record, table_name, table_alias)?;
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
                    let l = evaluate_runtime_expr(lhs, record, table_name, table_alias)?;
                    if let Value::Boolean(false) = l {
                        return Some(Value::Boolean(false));
                    }
                    let r = evaluate_runtime_expr(rhs, record, table_name, table_alias)?;
                    match (l, r) {
                        (Value::Boolean(a), Value::Boolean(b)) => Some(Value::Boolean(a && b)),
                        _ => None,
                    }
                }
                BinOp::Or => {
                    let l = evaluate_runtime_expr(lhs, record, table_name, table_alias)?;
                    if let Value::Boolean(true) = l {
                        return Some(Value::Boolean(true));
                    }
                    let r = evaluate_runtime_expr(rhs, record, table_name, table_alias)?;
                    match (l, r) {
                        (Value::Boolean(a), Value::Boolean(b)) => Some(Value::Boolean(a || b)),
                        _ => None,
                    }
                }
                _ => {
                    let l = evaluate_runtime_expr(lhs, record, table_name, table_alias)?;
                    let r = evaluate_runtime_expr(rhs, record, table_name, table_alias)?;
                    apply_binop(*op, l, r)
                }
            }
        }

        Expr::Cast {
            inner,
            target,
            span: _,
        } => {
            let v = evaluate_runtime_expr(inner, record, table_name, table_alias)?;
            Some(runtime_cast(&v, *target))
        }

        Expr::FunctionCall {
            name,
            args,
            span: _,
        } => {
            // For Week 3 we route through the existing evaluate_scalar_function
            // dispatcher, which speaks the legacy Projection::Function
            // argument convention (Column("LIT:…"), Column("TYPE:…"), etc.).
            // Week 4 replaces this shim with a proper registry keyed on
            // Expr arguments directly.
            let mut arg_values: Vec<Value> = Vec::with_capacity(args.len());
            for arg in args {
                arg_values.push(
                    evaluate_runtime_expr(arg, record, table_name, table_alias)
                        .unwrap_or(Value::Null),
                );
            }
            // Uppercase the function name so CASE-insensitive lookups
            // match the legacy is_scalar_function table.
            let upper = name.to_uppercase();
            dispatch_builtin_function(&upper, &arg_values)
        }

        Expr::Case {
            branches,
            else_,
            span: _,
        } => {
            for (cond, then_val) in branches {
                let cond_val = evaluate_runtime_expr(cond, record, table_name, table_alias);
                if matches!(cond_val, Some(Value::Boolean(true))) {
                    return evaluate_runtime_expr(then_val, record, table_name, table_alias);
                }
            }
            if let Some(else_expr) = else_ {
                evaluate_runtime_expr(else_expr, record, table_name, table_alias)
            } else {
                Some(Value::Null)
            }
        }

        Expr::IsNull {
            operand,
            negated,
            span: _,
        } => {
            let v = evaluate_runtime_expr(operand, record, table_name, table_alias);
            let is_null = matches!(v, None | Some(Value::Null));
            Some(Value::Boolean(if *negated { !is_null } else { is_null }))
        }

        Expr::InList {
            target,
            values,
            negated,
            span: _,
        } => {
            let t = evaluate_runtime_expr(target, record, table_name, table_alias)?;
            let mut hit = false;
            for v in values {
                if let Some(candidate) = evaluate_runtime_expr(v, record, table_name, table_alias) {
                    if compare_runtime_values(
                        &t,
                        &candidate,
                        crate::storage::query::ast::CompareOp::Eq,
                    ) {
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
            let t = evaluate_runtime_expr(target, record, table_name, table_alias)?;
            let lo = evaluate_runtime_expr(low, record, table_name, table_alias)?;
            let hi = evaluate_runtime_expr(high, record, table_name, table_alias)?;
            let in_range =
                compare_runtime_values(&t, &lo, crate::storage::query::ast::CompareOp::Ge)
                    && compare_runtime_values(&t, &hi, crate::storage::query::ast::CompareOp::Le);
            Some(Value::Boolean(if *negated { !in_range } else { in_range }))
        }
    }
}

/// Evaluate a legacy `Filter` tree as an expression context. Used by
/// nodes that still produce `Filter` (WHERE clause today) while the
/// ORDER BY / projection paths flip to `Expr`. Bridges the two until
/// Week 3 finishes the Filter migration.
#[allow(dead_code)]
pub(super) fn evaluate_filter_as_bool(
    filter: &Filter,
    record: &UnifiedRecord,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    evaluate_runtime_filter(record, filter, table_name, table_alias)
}

fn negate_value(v: &Value) -> Option<Value> {
    match v {
        Value::Integer(n) => Some(Value::Integer(-n)),
        Value::BigInt(n) => Some(Value::BigInt(-n)),
        Value::Float(f) => Some(Value::Float(-f)),
        _ => None,
    }
}

fn apply_binop(op: BinOp, a: Value, b: Value) -> Option<Value> {
    use crate::storage::query::ast::CompareOp;
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => arith(op, a, b),
        BinOp::Concat => Some(Value::Text(format!(
            "{}{}",
            a.display_string(),
            b.display_string()
        ))),
        BinOp::Eq => Some(Value::Boolean(compare_runtime_values(
            &a,
            &b,
            CompareOp::Eq,
        ))),
        BinOp::Ne => Some(Value::Boolean(compare_runtime_values(
            &a,
            &b,
            CompareOp::Ne,
        ))),
        BinOp::Lt => Some(Value::Boolean(compare_runtime_values(
            &a,
            &b,
            CompareOp::Lt,
        ))),
        BinOp::Le => Some(Value::Boolean(compare_runtime_values(
            &a,
            &b,
            CompareOp::Le,
        ))),
        BinOp::Gt => Some(Value::Boolean(compare_runtime_values(
            &a,
            &b,
            CompareOp::Gt,
        ))),
        BinOp::Ge => Some(Value::Boolean(compare_runtime_values(
            &a,
            &b,
            CompareOp::Ge,
        ))),
        BinOp::And | BinOp::Or => None, // handled upstream (short-circuit)
    }
}

fn arith(op: BinOp, a: Value, b: Value) -> Option<Value> {
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

/// Tuple `(f64 value, is_float_literally)`. The second element lets
/// the caller decide whether to preserve integer type after the op.
fn value_as_number(v: &Value) -> Option<(f64, bool)> {
    match v {
        Value::Integer(n) | Value::BigInt(n) => Some((*n as f64, false)),
        Value::UnsignedInteger(n) => Some((*n as f64, false)),
        Value::Float(f) => Some((*f, true)),
        Value::Decimal(d) => Some((*d as f64 / 10_000.0, true)),
        Value::Text(s) => s
            .parse::<i64>()
            .map(|n| (n as f64, false))
            .or_else(|_| s.parse::<f64>().map(|f| (f, true)))
            .ok(),
        _ => None,
    }
}

fn runtime_cast(src: &Value, target: crate::storage::schema::types::DataType) -> Value {
    use crate::storage::schema::types::DataType as DT;
    match (src, target) {
        (v, DT::Text) => Value::Text(v.display_string()),
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
        (Value::Text(s), t) => match crate::storage::schema::coerce::coerce(s, t, None) {
            Ok(v) => v,
            Err(_) => Value::Null,
        },
        (v, t) => match crate::storage::schema::coerce::coerce(&v.display_string(), t, None) {
            Ok(v) => v,
            Err(_) => Value::Null,
        },
    }
}

/// Minimal built-in function dispatcher used by `Expr::FunctionCall`.
/// For Week 3 we cover only the pure-scalar functions that can be
/// evaluated from a `&[Value]` argument list — the geo / time-bucket
/// functions that require row-level access stay on the legacy
/// Projection::Function path for now. Week 4 folds them into a
/// proper registry keyed on Expr.
fn dispatch_builtin_function(name: &str, args: &[Value]) -> Option<Value> {
    match name {
        "UPPER" => match args.first()? {
            Value::Text(s) => Some(Value::Text(s.to_uppercase())),
            other => Some(other.clone()),
        },
        "LOWER" => match args.first()? {
            Value::Text(s) => Some(Value::Text(s.to_lowercase())),
            other => Some(other.clone()),
        },
        "LENGTH" => match args.first()? {
            Value::Text(s) => Some(Value::Integer(s.len() as i64)),
            Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
            Value::Array(a) => Some(Value::Integer(a.len() as i64)),
            _ => Some(Value::Null),
        },
        "ABS" => match args.first()? {
            Value::Integer(n) => Some(Value::Integer(n.abs())),
            Value::Float(f) => Some(Value::Float(f.abs())),
            _ => Some(Value::Null),
        },
        "ROUND" => match args.first()? {
            Value::Float(f) => Some(Value::Float(f.round())),
            other => Some(other.clone()),
        },
        "FLOOR" => match args.first()? {
            Value::Float(f) => Some(Value::Float(f.floor())),
            other => Some(other.clone()),
        },
        "CEIL" => match args.first()? {
            Value::Float(f) => Some(Value::Float(f.ceil())),
            other => Some(other.clone()),
        },
        "COALESCE" => args
            .iter()
            .find(|v| !matches!(v, Value::Null))
            .cloned()
            .or(Some(Value::Null)),
        "NOW" | "CURRENT_TIMESTAMP" => Some(Value::TimestampMs(current_unix_ms())),
        "CURRENT_DATE" => Some(Value::Date((current_unix_ms() / 86_400_000) as i32)),
        "TIME_BUCKET" => {
            let bucket_ns = time_bucket_duration(args.first()?)?;
            let timestamp_ns = args.get(1).and_then(value_to_bucket_timestamp_ns)?;
            let bucket_start = if bucket_ns == 0 {
                timestamp_ns
            } else {
                (timestamp_ns / bucket_ns) * bucket_ns
            };
            Some(Value::UnsignedInteger(bucket_start))
        }
        "GEO_DISTANCE" | "HAVERSINE" => {
            let (lat1, lon1, lat2, lon2) = geo_args(args)?;
            Some(Value::Float(crate::geo::haversine_km(
                lat1, lon1, lat2, lon2,
            )))
        }
        "VINCENTY" => {
            let (lat1, lon1, lat2, lon2) = geo_args(args)?;
            Some(Value::Float(crate::geo::vincenty_km(
                lat1, lon1, lat2, lon2,
            )))
        }
        "GEO_BEARING" => {
            let (lat1, lon1, lat2, lon2) = geo_args(args)?;
            Some(Value::Float(crate::geo::bearing(lat1, lon1, lat2, lon2)))
        }
        "POWER" => {
            let base = value_as_f64(args.first()?)?;
            let exp = value_as_f64(args.get(1)?)?;
            Some(Value::Float(base.powf(exp)))
        }
        "VERIFY_PASSWORD" => {
            let stored = args.first()?;
            let candidate = args.get(1)?;
            let hash = match stored {
                Value::Password(hash) | Value::Text(hash) => hash,
                _ => return Some(Value::Boolean(false)),
            };
            let plain = match candidate {
                Value::Text(plain) => plain,
                _ => return Some(Value::Boolean(false)),
            };
            Some(Value::Boolean(crate::auth::store::verify_password(
                plain, hash,
            )))
        }
        _ => None,
    }
}

fn current_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn time_bucket_duration(value: &Value) -> Option<u64> {
    match value {
        Value::Text(text) => crate::storage::timeseries::retention::parse_duration_ns(text),
        Value::UnsignedInteger(value) => Some(*value),
        Value::Integer(value) if *value >= 0 => Some(*value as u64),
        _ => None,
    }
}

fn value_to_bucket_timestamp_ns(value: &Value) -> Option<u64> {
    match value {
        Value::UnsignedInteger(v) => Some(*v),
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        Value::Float(v) if *v >= 0.0 => Some(*v as u64),
        Value::Timestamp(v) if *v >= 0 => Some((*v as u64) * 1_000_000_000),
        Value::TimestampMs(v) if *v >= 0 => Some((*v as u64) * 1_000_000),
        _ => None,
    }
}

fn geo_args(args: &[Value]) -> Option<(f64, f64, f64, f64)> {
    match args {
        [left, right] => {
            let (lat1, lon1) = geo_point(left)?;
            let (lat2, lon2) = geo_point(right)?;
            Some((lat1, lon1, lat2, lon2))
        }
        [lat1, lon1, lat2, lon2] => Some((
            value_as_f64(lat1)?,
            value_as_f64(lon1)?,
            value_as_f64(lat2)?,
            value_as_f64(lon2)?,
        )),
        _ => None,
    }
}

fn geo_point(value: &Value) -> Option<(f64, f64)> {
    match value {
        Value::GeoPoint(lat, lon) => Some((
            crate::geo::micro_to_deg(*lat),
            crate::geo::micro_to_deg(*lon),
        )),
        _ => None,
    }
}

fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Float(value) => Some(*value),
        Value::Integer(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        _ => None,
    }
}
