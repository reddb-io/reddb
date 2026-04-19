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
use crate::storage::query::ast::{BinOp, Expr, FieldRef, Filter, UnaryOp};
use crate::storage::query::unified::UnifiedRecord;
use crate::storage::schema::Value;
use crate::storage::RedDB;

/// Evaluate an `Expr` against a record and return its resulting
/// `Value`, or `None` if the expression cannot be resolved (missing
/// column, type mismatch, unsupported feature for this phase).
pub(super) fn evaluate_runtime_expr(
    expr: &Expr,
    record: &UnifiedRecord,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Option<Value> {
    evaluate_runtime_expr_with_db(None, expr, record, table_name, table_alias)
}

pub(super) fn evaluate_runtime_expr_with_db(
    db: Option<&RedDB>,
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
            let v = evaluate_runtime_expr_with_db(db, operand, record, table_name, table_alias)?;
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
                    let l =
                        evaluate_runtime_expr_with_db(db, lhs, record, table_name, table_alias)?;
                    if let Value::Boolean(false) = l {
                        return Some(Value::Boolean(false));
                    }
                    let r =
                        evaluate_runtime_expr_with_db(db, rhs, record, table_name, table_alias)?;
                    match (l, r) {
                        (Value::Boolean(a), Value::Boolean(b)) => Some(Value::Boolean(a && b)),
                        _ => None,
                    }
                }
                BinOp::Or => {
                    let l =
                        evaluate_runtime_expr_with_db(db, lhs, record, table_name, table_alias)?;
                    if let Value::Boolean(true) = l {
                        return Some(Value::Boolean(true));
                    }
                    let r =
                        evaluate_runtime_expr_with_db(db, rhs, record, table_name, table_alias)?;
                    match (l, r) {
                        (Value::Boolean(a), Value::Boolean(b)) => Some(Value::Boolean(a || b)),
                        _ => None,
                    }
                }
                _ => {
                    let l =
                        evaluate_runtime_expr_with_db(db, lhs, record, table_name, table_alias)?;
                    let r =
                        evaluate_runtime_expr_with_db(db, rhs, record, table_name, table_alias)?;
                    apply_binop(*op, l, r)
                }
            }
        }

        Expr::Cast {
            inner,
            target,
            span: _,
        } => {
            let v = evaluate_runtime_expr_with_db(db, inner, record, table_name, table_alias)?;
            Some(runtime_cast(&v, *target))
        }

        Expr::FunctionCall {
            name,
            args,
            span: _,
        } => {
            let upper = name.to_uppercase();
            if upper == "CONFIG" {
                return evaluate_runtime_config_function(db, args, record, table_name, table_alias);
            }
            if upper == "KV" {
                return evaluate_runtime_kv_function(db, args, record, table_name, table_alias);
            }
            // For Week 3 we route through the existing evaluate_scalar_function
            // dispatcher, which speaks the legacy Projection::Function
            // argument convention (Column("LIT:…"), Column("TYPE:…"), etc.).
            // Week 4 replaces this shim with a proper registry keyed on
            // Expr arguments directly.
            let mut arg_values: Vec<Value> = Vec::with_capacity(args.len());
            for arg in args {
                arg_values.push(
                    evaluate_runtime_expr_with_db(db, arg, record, table_name, table_alias)
                        .unwrap_or(Value::Null),
                );
            }
            // Uppercase the function name so CASE-insensitive lookups
            // match the legacy is_scalar_function table.
            dispatch_builtin_function(&upper, &arg_values)
        }

        Expr::Case {
            branches,
            else_,
            span: _,
        } => {
            for (cond, then_val) in branches {
                let cond_val =
                    evaluate_runtime_expr_with_db(db, cond, record, table_name, table_alias);
                if matches!(cond_val, Some(Value::Boolean(true))) {
                    return evaluate_runtime_expr_with_db(
                        db,
                        then_val,
                        record,
                        table_name,
                        table_alias,
                    );
                }
            }
            if let Some(else_expr) = else_ {
                evaluate_runtime_expr_with_db(db, else_expr, record, table_name, table_alias)
            } else {
                Some(Value::Null)
            }
        }

        Expr::IsNull {
            operand,
            negated,
            span: _,
        } => {
            let v = evaluate_runtime_expr_with_db(db, operand, record, table_name, table_alias);
            let is_null = matches!(v, None | Some(Value::Null));
            Some(Value::Boolean(if *negated { !is_null } else { is_null }))
        }

        Expr::InList {
            target,
            values,
            negated,
            span: _,
        } => {
            let t = evaluate_runtime_expr_with_db(db, target, record, table_name, table_alias)?;
            let mut hit = false;
            for v in values {
                if let Some(candidate) =
                    evaluate_runtime_expr_with_db(db, v, record, table_name, table_alias)
                {
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
            let t = evaluate_runtime_expr_with_db(db, target, record, table_name, table_alias)?;
            let lo = evaluate_runtime_expr_with_db(db, low, record, table_name, table_alias)?;
            let hi = evaluate_runtime_expr_with_db(db, high, record, table_name, table_alias)?;
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

pub(super) fn lookup_latest_kv_value(db: &RedDB, collection: &str, key: &str) -> Option<Value> {
    let manager = db.store().get_collection(collection)?;
    let mut latest_id: u64 = 0;
    let mut latest_value: Option<Value> = None;
    // The parser rebuilds dotted paths by concatenating token display
    // strings; any segment that collides with a SQL keyword (DEFAULT,
    // LEFT, IN, …) comes back uppercase, but SET CONFIG stores keys
    // lowercase. Normalise here so CONFIG(red.ai.default.provider)
    // matches the key persisted by SET CONFIG.
    let key_lc = key.to_ascii_lowercase();
    manager.for_each_entity(|entity| {
        let Some(row) = entity.data.as_row() else {
            return true;
        };
        let entry_key = row.get_field("key").and_then(|value| match value {
            Value::Text(text) => Some(text.as_str()),
            _ => None,
        });
        if entry_key == Some(key_lc.as_str()) && entity.id.raw() >= latest_id {
            latest_id = entity.id.raw();
            latest_value = Some(row.get_field("value").cloned().unwrap_or(Value::Null));
        }
        true
    });
    latest_value
}

fn evaluate_runtime_config_function(
    db: Option<&RedDB>,
    args: &[Expr],
    record: &UnifiedRecord,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Option<Value> {
    let key = expr_path_text(args.first()?)?;
    if let Some(db) = db {
        if let Some(value) = lookup_latest_kv_value(db, "red_config", &key) {
            return Some(value);
        }
    }
    args.get(1)
        .and_then(|expr| special_default_expr_value(db, expr, record, table_name, table_alias))
        .or(Some(Value::Null))
}

fn evaluate_runtime_kv_function(
    db: Option<&RedDB>,
    args: &[Expr],
    record: &UnifiedRecord,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Option<Value> {
    let collection = expr_path_text(args.first()?)?;
    let key = expr_path_text(args.get(1)?)?;
    if let Some(db) = db {
        if let Some(value) = lookup_latest_kv_value(db, &collection, &key) {
            return Some(value);
        }
    }
    args.get(2)
        .and_then(|expr| special_default_expr_value(db, expr, record, table_name, table_alias))
        .or(Some(Value::Null))
}

fn special_default_expr_value(
    db: Option<&RedDB>,
    expr: &Expr,
    record: &UnifiedRecord,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Option<Value> {
    match expr {
        Expr::Column { field, .. } => field_ref_path_text(field).map(Value::Text),
        _ => evaluate_runtime_expr_with_db(db, expr, record, table_name, table_alias),
    }
}

fn expr_path_text(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Column { field, .. } => field_ref_path_text(field),
        Expr::Literal { value, .. } => literal_path_text(value),
        _ => None,
    }
}

fn field_ref_path_text(field: &FieldRef) -> Option<String> {
    match field {
        FieldRef::TableColumn { table, column } => Some(if table.is_empty() {
            column.clone()
        } else {
            format!("{table}.{column}")
        }),
        FieldRef::NodeProperty { alias, property } => Some(format!("{alias}.{property}")),
        FieldRef::EdgeProperty { alias, property } => Some(format!("{alias}.{property}")),
        FieldRef::NodeId { alias } => Some(format!("{alias}.id")),
    }
}

fn literal_path_text(value: &Value) -> Option<String> {
    if matches!(value, Value::Null) {
        None
    } else {
        Some(value.display_string())
    }
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
        "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" => match args.first()? {
            Value::Text(s) => Some(Value::Integer(s.chars().count() as i64)),
            Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
            Value::Array(a) => Some(Value::Integer(a.len() as i64)),
            _ => Some(Value::Null),
        },
        "OCTET_LENGTH" => match args.first()? {
            Value::Text(s) => Some(Value::Integer(s.len() as i64)),
            Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
            _ => Some(Value::Null),
        },
        "BIT_LENGTH" => match args.first()? {
            Value::Text(s) => Some(Value::Integer((s.len() * 8) as i64)),
            Value::Blob(b) => Some(Value::Integer((b.len() * 8) as i64)),
            _ => Some(Value::Null),
        },
        "SUBSTRING" | "SUBSTR" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            match args.get(1)? {
                Value::Text(pattern) if name == "SUBSTRING" && args.len() == 2 => {
                    Some(match substring_pattern_text(text, pattern) {
                        Some(matched) => Value::Text(matched),
                        None => Value::Null,
                    })
                }
                start_value => {
                    let start = value_as_i64(start_value)?;
                    let count = args.get(2).and_then(value_as_i64);
                    Some(Value::Text(substring_text(text, start, count)?))
                }
            }
        }
        "POSITION" => {
            let needle = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let haystack = match args.get(1)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            Some(Value::Integer(position_text(needle, haystack)))
        }
        "TRIM" | "BTRIM" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args.get(1) {
                None => None,
                Some(Value::Text(chars)) => Some(chars.as_str()),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::Text(trim_text(text, chars, true, true)))
        }
        "LTRIM" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args.get(1) {
                None => None,
                Some(Value::Text(chars)) => Some(chars.as_str()),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::Text(trim_text(text, chars, true, false)))
        }
        "RTRIM" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args.get(1) {
                None => None,
                Some(Value::Text(chars)) => Some(chars.as_str()),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::Text(trim_text(text, chars, false, true)))
        }
        "CONCAT" => Some(Value::Text(
            args.iter()
                .filter(|value| !matches!(value, Value::Null))
                .map(Value::display_string)
                .collect::<String>(),
        )),
        "CONCAT_WS" => {
            let separator = match args.first()? {
                Value::Null => return Some(Value::Null),
                Value::Text(text) => text.as_str(),
                other => {
                    return Some(Value::Text(
                        args.iter()
                            .skip(1)
                            .filter(|value| !matches!(value, Value::Null))
                            .map(Value::display_string)
                            .collect::<Vec<_>>()
                            .join(&other.display_string()),
                    ))
                }
            };
            Some(Value::Text(
                args.iter()
                    .skip(1)
                    .filter(|value| !matches!(value, Value::Null))
                    .map(Value::display_string)
                    .collect::<Vec<_>>()
                    .join(separator),
            ))
        }
        "REVERSE" => match args.first()? {
            Value::Text(text) => Some(Value::Text(text.chars().rev().collect())),
            _ => Some(Value::Null),
        },
        "LEFT" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count = value_as_i64(args.get(1)?)?;
            Some(Value::Text(slice_left_text(text, count)))
        }
        "RIGHT" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count = value_as_i64(args.get(1)?)?;
            Some(Value::Text(slice_right_text(text, count)))
        }
        "QUOTE_LITERAL" => match args.first()? {
            Value::Null => Some(Value::Null),
            Value::Text(text) => Some(Value::Text(quote_literal_text(text))),
            other => Some(Value::Text(quote_literal_text(&other.display_string()))),
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
        "PG_ADVISORY_LOCK" => {
            let key = value_as_i64(args.first()?)?;
            crate::auth::locks::global()
                .acquire(key, crate::runtime::impl_core::current_connection_id());
            Some(Value::Null)
        }
        "PG_TRY_ADVISORY_LOCK" => {
            let key = value_as_i64(args.first()?)?;
            Some(Value::Boolean(crate::auth::locks::global().try_acquire(
                key,
                crate::runtime::impl_core::current_connection_id(),
            )))
        }
        "PG_ADVISORY_UNLOCK" => {
            let key = value_as_i64(args.first()?)?;
            Some(Value::Boolean(crate::auth::locks::global().release(
                key,
                crate::runtime::impl_core::current_connection_id(),
            )))
        }
        "PG_ADVISORY_UNLOCK_ALL" => {
            let dropped = crate::auth::locks::global()
                .release_all(crate::runtime::impl_core::current_connection_id());
            Some(Value::Integer(dropped as i64))
        }
        "NOW" | "CURRENT_TIMESTAMP" => Some(Value::TimestampMs(current_unix_ms())),
        "CURRENT_DATE" => Some(Value::Date((current_unix_ms() / 86_400_000) as i32)),
        // Phase 2.5.3 multi-tenancy: reads the thread-local session
        // tenant installed by `SET TENANT 'id'` or transport
        // middleware. Returns NULL when no tenant is bound so RLS
        // policies like `USING (tenant_id = CURRENT_TENANT())` deny
        // every row for unauthenticated / unscoped sessions.
        "CURRENT_TENANT" => Some(
            crate::runtime::impl_core::current_tenant()
                .map(Value::Text)
                .unwrap_or(Value::Null),
        ),
        // Session identity scalars — `WITHIN ... USER '<u>' AS ROLE '<r>'`
        // overrides win over the transport-installed thread-local.
        // Anonymous callers with no override get NULL.
        "CURRENT_USER" | "SESSION_USER" | "USER" => Some(
            crate::runtime::impl_core::current_user_projected()
                .map(Value::Text)
                .unwrap_or(Value::Null),
        ),
        "CURRENT_ROLE" => Some(
            crate::runtime::impl_core::current_role_projected()
                .map(Value::Text)
                .unwrap_or(Value::Null),
        ),
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
        "MONEY" => money_from_args(args),
        "MONEY_ASSET" => match args.first()? {
            Value::Money { asset_code, .. } => Some(Value::AssetCode(asset_code.clone())),
            _ => Some(Value::Null),
        },
        "MONEY_MINOR" => match args.first()? {
            Value::Money { minor_units, .. } => Some(Value::BigInt(*minor_units)),
            _ => Some(Value::Null),
        },
        "MONEY_SCALE" => match args.first()? {
            Value::Money { scale, .. } => Some(Value::Integer(i64::from(*scale))),
            _ => Some(Value::Null),
        },
        // ─────────────────────────────────────────────────────────────
        // JSON functions (Phase 1.4 PG parity).
        //
        // Accepts Value::Json, Value::Text (parsed as JSON), or returns
        // Value::Null when the input is not valid JSON.
        // ─────────────────────────────────────────────────────────────
        "JSON_EXTRACT" => json_extract_impl(args.first()?, args.get(1)?, /*as_text=*/ false),
        "JSON_EXTRACT_TEXT" => {
            json_extract_impl(args.first()?, args.get(1)?, /*as_text=*/ true)
        }
        "JSON_SET" => json_set_impl(args.first()?, args.get(1)?, args.get(2)?),
        "JSON_ARRAY_LENGTH" => {
            let v = value_to_json(args.first()?)?;
            match v {
                crate::serde_json::Value::Array(a) => Some(Value::Integer(a.len() as i64)),
                _ => Some(Value::Null),
            }
        }
        "JSON_TYPEOF" => {
            let v = value_to_json(args.first()?)?;
            let name = match v {
                crate::serde_json::Value::Null => "null",
                crate::serde_json::Value::Bool(_) => "boolean",
                crate::serde_json::Value::Number(_) => "number",
                crate::serde_json::Value::String(_) => "string",
                crate::serde_json::Value::Array(_) => "array",
                crate::serde_json::Value::Object(_) => "object",
            };
            Some(Value::Text(name.to_string()))
        }
        "JSON_VALID" => {
            let text = match args.first()? {
                Value::Text(s) => s.clone(),
                Value::Json(b) => String::from_utf8_lossy(b).to_string(),
                _ => return Some(Value::Boolean(false)),
            };
            Some(Value::Boolean(
                crate::serde_json::from_str::<crate::serde_json::Value>(&text).is_ok(),
            ))
        }
        "JSON_ARRAY" => {
            let arr: Vec<crate::serde_json::Value> = args.iter().map(value_as_json).collect();
            let json = crate::serde_json::Value::Array(arr);
            Some(Value::Json(json.to_string_compact().into_bytes()))
        }
        "JSON_OBJECT" => {
            // Args come as interleaved (key, value, key, value, ...).
            if args.len() % 2 != 0 {
                return Some(Value::Null);
            }
            let mut map = crate::serde_json::Map::new();
            for pair in args.chunks_exact(2) {
                let key = match &pair[0] {
                    Value::Text(s) => s.clone(),
                    other => other.display_string(),
                };
                map.insert(key, value_as_json(&pair[1]));
            }
            let json = crate::serde_json::Value::Object(map);
            Some(Value::Json(json.to_string_compact().into_bytes()))
        }
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────
// JSON scalar helpers (Phase 1.4 PG parity)
// ─────────────────────────────────────────────────────────────────────

/// Parse a scalar argument into a JSON value. `Value::Json` bytes are
/// decoded; `Value::Text` is parsed. Anything else returns None so
/// callers propagate Null.
fn value_to_json(value: &Value) -> Option<crate::serde_json::Value> {
    match value {
        Value::Null => Some(crate::serde_json::Value::Null),
        Value::Json(bytes) => {
            let text = String::from_utf8_lossy(bytes);
            crate::serde_json::from_str(&text).ok()
        }
        Value::Text(s) => crate::serde_json::from_str(s).ok(),
        _ => None,
    }
}

/// Convert any scalar to the JSON representation used by JSON_ARRAY /
/// JSON_OBJECT / JSON_SET. Non-JSON-native types fall back to their
/// display string.
fn value_as_json(value: &Value) -> crate::serde_json::Value {
    match value {
        Value::Null => crate::serde_json::Value::Null,
        Value::Boolean(b) => crate::serde_json::Value::Bool(*b),
        Value::Integer(n) => crate::serde_json::Value::Number(*n as f64),
        Value::UnsignedInteger(n) => crate::serde_json::Value::Number(*n as f64),
        Value::BigInt(n) => crate::serde_json::Value::Number(*n as f64),
        Value::Float(n) => crate::serde_json::Value::Number(*n),
        Value::Text(s) => crate::serde_json::Value::String(s.clone()),
        Value::Json(bytes) => {
            let text = String::from_utf8_lossy(bytes);
            crate::serde_json::from_str(&text)
                .unwrap_or_else(|_| crate::serde_json::Value::String(text.into_owned()))
        }
        other => crate::serde_json::Value::String(other.display_string()),
    }
}

/// Parse a `$.a.b[0]` path into a sequence of (field | index) steps.
/// Returns None on unrecognised syntax.
enum JsonPathStep<'a> {
    Field(&'a str),
    Index(usize),
}

fn parse_json_path(path: &str) -> Option<Vec<JsonPathStep<'_>>> {
    let path = path.trim();
    let rest = path.strip_prefix('$').unwrap_or(path);
    let mut steps = Vec::new();
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'.' && bytes[i] != b'[' {
                    i += 1;
                }
                if start == i {
                    return None;
                }
                let field = std::str::from_utf8(&bytes[start..i]).ok()?;
                steps.push(JsonPathStep::Field(field));
            }
            b'[' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                if i >= bytes.len() {
                    return None;
                }
                let idx: usize = std::str::from_utf8(&bytes[start..i]).ok()?.parse().ok()?;
                i += 1; // skip ']'
                steps.push(JsonPathStep::Index(idx));
            }
            _ => return None,
        }
    }
    Some(steps)
}

/// Traverse a JSON value along the path. Returns None if any step misses.
fn json_path_get<'a>(
    root: &'a crate::serde_json::Value,
    steps: &[JsonPathStep<'_>],
) -> Option<&'a crate::serde_json::Value> {
    let mut cur = root;
    for step in steps {
        match (step, cur) {
            (JsonPathStep::Field(name), crate::serde_json::Value::Object(map)) => {
                cur = map.get(*name)?;
            }
            (JsonPathStep::Index(idx), crate::serde_json::Value::Array(arr)) => {
                cur = arr.get(*idx)?;
            }
            _ => return None,
        }
    }
    Some(cur)
}

fn json_extract_impl(input: &Value, path: &Value, as_text: bool) -> Option<Value> {
    let path_str = match path {
        Value::Text(s) => s.clone(),
        _ => return Some(Value::Null),
    };
    let json = value_to_json(input)?;
    let steps = parse_json_path(&path_str)?;
    let Some(target) = json_path_get(&json, &steps) else {
        // Missing path → SQL NULL, not function failure.
        return Some(Value::Null);
    };
    if as_text {
        // Unquoted scalar text, JSON for containers.
        match target {
            crate::serde_json::Value::String(s) => Some(Value::Text(s.clone())),
            crate::serde_json::Value::Null => Some(Value::Null),
            crate::serde_json::Value::Bool(b) => Some(Value::Text(b.to_string())),
            crate::serde_json::Value::Number(n) => Some(Value::Text(n.to_string())),
            other => Some(Value::Text(other.to_string_compact())),
        }
    } else {
        // JSON-serialised representation (strings come back quoted).
        Some(Value::Text(target.to_string_compact()))
    }
}

/// Walk along a JSON path, mutably creating missing container nodes. The
/// final step writes `new_value` into its parent (object field or array
/// slot). Arrays grow with nulls when the index exceeds the current length.
fn json_set_impl(input: &Value, path: &Value, new_value: &Value) -> Option<Value> {
    let path_str = match path {
        Value::Text(s) => s.clone(),
        _ => return Some(Value::Null),
    };
    let mut json = value_to_json(input).unwrap_or(crate::serde_json::Value::Null);
    let steps = parse_json_path(&path_str)?;
    if steps.is_empty() {
        // Root replacement.
        let replaced = value_as_json(new_value);
        return Some(Value::Json(replaced.to_string_compact().into_bytes()));
    }
    // Walk + insert; use a simple recursive helper so we can own the mutation path.
    fn walk(
        node: &mut crate::serde_json::Value,
        steps: &[JsonPathStep<'_>],
        idx: usize,
        new_value: &crate::serde_json::Value,
    ) -> bool {
        if idx == steps.len() {
            *node = new_value.clone();
            return true;
        }
        match (&steps[idx], node) {
            (JsonPathStep::Field(name), crate::serde_json::Value::Object(map)) => {
                let entry = map
                    .entry(name.to_string())
                    .or_insert(crate::serde_json::Value::Null);
                walk(entry, steps, idx + 1, new_value)
            }
            (JsonPathStep::Field(name), other) => {
                // Coerce non-object into object to keep the path alive.
                let mut new_map = crate::serde_json::Map::new();
                new_map.insert(name.to_string(), crate::serde_json::Value::Null);
                *other = crate::serde_json::Value::Object(new_map);
                if let crate::serde_json::Value::Object(map) = other {
                    let entry = map.get_mut(*name).unwrap();
                    walk(entry, steps, idx + 1, new_value)
                } else {
                    false
                }
            }
            (JsonPathStep::Index(i), crate::serde_json::Value::Array(arr)) => {
                while arr.len() <= *i {
                    arr.push(crate::serde_json::Value::Null);
                }
                walk(&mut arr[*i], steps, idx + 1, new_value)
            }
            (JsonPathStep::Index(i), other) => {
                let mut arr = Vec::with_capacity(i + 1);
                arr.resize(*i + 1, crate::serde_json::Value::Null);
                *other = crate::serde_json::Value::Array(arr);
                if let crate::serde_json::Value::Array(arr) = other {
                    walk(&mut arr[*i], steps, idx + 1, new_value)
                } else {
                    false
                }
            }
        }
    }
    let new_json = value_as_json(new_value);
    if !walk(&mut json, &steps, 0, &new_json) {
        return Some(Value::Null);
    }
    Some(Value::Json(json.to_string_compact().into_bytes()))
}

fn money_from_args(args: &[Value]) -> Option<Value> {
    let input = match args {
        [single] => money_arg_text(single)?,
        [left, right] => format!("{} {}", money_arg_text(left)?, money_arg_text(right)?),
        _ => return Some(Value::Null),
    };
    match crate::storage::schema::coerce::coerce(
        &input,
        crate::storage::schema::DataType::Money,
        None,
    ) {
        Ok(value) => Some(value),
        Err(_) if args.len() == 2 => {
            let reversed = format!(
                "{} {}",
                money_arg_text(&args[1])?,
                money_arg_text(&args[0])?
            );
            crate::storage::schema::coerce::coerce(
                &reversed,
                crate::storage::schema::DataType::Money,
                None,
            )
            .ok()
        }
        Err(_) => Some(Value::Null),
    }
}

fn money_arg_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Text(text) => Some(text.clone()),
        Value::AssetCode(code) => Some(code.clone()),
        Value::Currency(code) => Some(String::from_utf8_lossy(code).to_string()),
        other => Some(other.display_string()),
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
        Value::BigInt(value) if *value >= 0 => Some(*value as u64),
        _ => None,
    }
}

fn value_to_bucket_timestamp_ns(value: &Value) -> Option<u64> {
    match value {
        Value::UnsignedInteger(v) => Some(*v),
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        Value::BigInt(v) if *v >= 0 => Some(*v as u64),
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
        Value::BigInt(value) => Some(*value as f64),
        _ => None,
    }
}

fn value_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) | Value::BigInt(value) => Some(*value),
        Value::UnsignedInteger(value) => i64::try_from(*value).ok(),
        _ => None,
    }
}

fn substring_text(text: &str, start: i64, count: Option<i64>) -> Option<String> {
    if count.is_some_and(|count| count < 0) {
        return None;
    }

    let chars: Vec<char> = text.chars().collect();
    let start_idx = if start <= 1 {
        0
    } else {
        usize::try_from(start - 1).ok()?
    };

    if start_idx >= chars.len() {
        return Some(String::new());
    }

    let end_idx = match count {
        Some(count) => start_idx.saturating_add(count as usize).min(chars.len()),
        None => chars.len(),
    };

    Some(chars[start_idx..end_idx].iter().collect())
}

fn substring_pattern_text(text: &str, pattern: &str) -> Option<String> {
    let regex = regex::Regex::new(pattern).ok()?;
    let captures = regex.captures(text)?;
    if captures.len() > 1 {
        return captures.get(1).map(|capture| capture.as_str().to_string());
    }
    captures.get(0).map(|capture| capture.as_str().to_string())
}

fn position_text(needle: &str, haystack: &str) -> i64 {
    if needle.is_empty() {
        return 1;
    }
    haystack
        .find(needle)
        .map(|byte_idx| haystack[..byte_idx].chars().count() as i64 + 1)
        .unwrap_or(0)
}

fn slice_left_text(text: &str, count: i64) -> String {
    let chars: Vec<char> = text.chars().collect();
    let take = normalized_slice_len(chars.len(), count);
    chars.into_iter().take(take).collect()
}

fn slice_right_text(text: &str, count: i64) -> String {
    let chars: Vec<char> = text.chars().collect();
    let take = normalized_slice_len(chars.len(), count);
    let len = chars.len();
    chars.into_iter().skip(len.saturating_sub(take)).collect()
}

fn normalized_slice_len(len: usize, count: i64) -> usize {
    if count >= 0 {
        usize::try_from(count).unwrap_or(usize::MAX).min(len)
    } else {
        len.saturating_sub(count.unsigned_abs() as usize)
    }
}

fn quote_literal_text(text: &str) -> String {
    let escaped = text.replace('\'', "''");
    if text.contains('\\') {
        format!("E'{}'", escaped.replace('\\', "\\\\"))
    } else {
        format!("'{escaped}'")
    }
}

fn trim_text(text: &str, chars: Option<&str>, left: bool, right: bool) -> String {
    match chars {
        Some(chars) => {
            let predicate = |ch| chars.contains(ch);
            match (left, right) {
                (true, true) => text.trim_matches(predicate).to_string(),
                (true, false) => text.trim_start_matches(predicate).to_string(),
                (false, true) => text.trim_end_matches(predicate).to_string(),
                (false, false) => text.to_string(),
            }
        }
        None => match (left, right) {
            (true, true) => text.trim().to_string(),
            (true, false) => text.trim_start().to_string(),
            (false, true) => text.trim_end().to_string(),
            (false, false) => text.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::dispatch_builtin_function;
    use crate::storage::schema::Value;

    #[test]
    fn test_money_constructor_two_args() {
        let value = dispatch_builtin_function(
            "MONEY",
            &[
                Value::AssetCode("BTC".to_string()),
                Value::Text("0.125".to_string()),
            ],
        )
        .unwrap();
        assert_eq!(
            value,
            Value::Money {
                asset_code: "BTC".to_string(),
                minor_units: 125,
                scale: 3,
            }
        );
    }

    #[test]
    fn test_money_extractors() {
        let money = Value::Money {
            asset_code: "USDT".to_string(),
            minor_units: 12345,
            scale: 2,
        };
        assert_eq!(
            dispatch_builtin_function("MONEY_ASSET", std::slice::from_ref(&money)).unwrap(),
            Value::AssetCode("USDT".to_string())
        );
        assert_eq!(
            dispatch_builtin_function("MONEY_MINOR", std::slice::from_ref(&money)).unwrap(),
            Value::BigInt(12345)
        );
        assert_eq!(
            dispatch_builtin_function("MONEY_SCALE", std::slice::from_ref(&money)).unwrap(),
            Value::Integer(2)
        );
    }

    // ─────────────────────────────────────────────────────────────
    // JSON functions (Phase 1.4 PG parity)
    // ─────────────────────────────────────────────────────────────

    fn json_text(s: &str) -> Value {
        Value::Text(s.to_string())
    }

    #[test]
    fn json_extract_scalar_and_nested() {
        let doc = json_text(r#"{"a":1,"b":{"c":"hello","d":[10,20,30]}}"#);
        // Top-level scalar.
        assert_eq!(
            dispatch_builtin_function("JSON_EXTRACT", &[doc.clone(), json_text("$.a")]).unwrap(),
            Value::Text("1".to_string())
        );
        // Nested string (quoted for JSON_EXTRACT).
        assert_eq!(
            dispatch_builtin_function("JSON_EXTRACT", &[doc.clone(), json_text("$.b.c")]).unwrap(),
            Value::Text("\"hello\"".to_string())
        );
        // Unquoted via JSON_EXTRACT_TEXT.
        assert_eq!(
            dispatch_builtin_function("JSON_EXTRACT_TEXT", &[doc.clone(), json_text("$.b.c")])
                .unwrap(),
            Value::Text("hello".to_string())
        );
        // Array index.
        assert_eq!(
            dispatch_builtin_function("JSON_EXTRACT_TEXT", &[doc.clone(), json_text("$.b.d[1]")])
                .unwrap(),
            Value::Text("20".to_string())
        );
        // Missing path → Null.
        assert_eq!(
            dispatch_builtin_function("JSON_EXTRACT", &[doc, json_text("$.missing")]).unwrap(),
            Value::Null
        );
    }

    #[test]
    fn json_array_length_and_typeof() {
        let arr = json_text(r#"[1,2,3,4]"#);
        assert_eq!(
            dispatch_builtin_function("JSON_ARRAY_LENGTH", &[arr.clone()]).unwrap(),
            Value::Integer(4)
        );
        assert_eq!(
            dispatch_builtin_function("JSON_TYPEOF", &[arr]).unwrap(),
            Value::Text("array".to_string())
        );
        assert_eq!(
            dispatch_builtin_function("JSON_TYPEOF", &[json_text(r#"{"k":1}"#)]).unwrap(),
            Value::Text("object".to_string())
        );
        assert_eq!(
            dispatch_builtin_function("JSON_TYPEOF", &[json_text("null")]).unwrap(),
            Value::Text("null".to_string())
        );
    }

    #[test]
    fn json_valid_accepts_and_rejects() {
        assert_eq!(
            dispatch_builtin_function("JSON_VALID", &[json_text(r#"{"a":1}"#)]).unwrap(),
            Value::Boolean(true)
        );
        assert_eq!(
            dispatch_builtin_function("JSON_VALID", &[json_text("not json")]).unwrap(),
            Value::Boolean(false)
        );
    }

    #[test]
    fn json_array_and_object_builders() {
        // JSON_ARRAY(1, "x", true) → [1,"x",true]
        let arr = dispatch_builtin_function(
            "JSON_ARRAY",
            &[
                Value::Integer(1),
                Value::Text("x".to_string()),
                Value::Boolean(true),
            ],
        )
        .unwrap();
        if let Value::Json(bytes) = arr {
            let text = String::from_utf8(bytes).unwrap();
            assert_eq!(text, r#"[1,"x",true]"#);
        } else {
            panic!("expected Json value");
        }

        // JSON_OBJECT("k1", 1, "k2", "v") → {"k1":1,"k2":"v"}
        let obj = dispatch_builtin_function(
            "JSON_OBJECT",
            &[
                Value::Text("k1".to_string()),
                Value::Integer(1),
                Value::Text("k2".to_string()),
                Value::Text("v".to_string()),
            ],
        )
        .unwrap();
        if let Value::Json(bytes) = obj {
            let text = String::from_utf8(bytes).unwrap();
            // Map keeps insertion order via BTreeMap (sorted alphabetically).
            assert_eq!(text, r#"{"k1":1,"k2":"v"}"#);
        } else {
            panic!("expected Json value");
        }
    }

    #[test]
    fn json_set_updates_existing_and_creates_new() {
        let doc = json_text(r#"{"a":1,"b":{"c":"x"}}"#);

        // Update existing nested field.
        let out = dispatch_builtin_function(
            "JSON_SET",
            &[
                doc.clone(),
                json_text("$.b.c"),
                Value::Text("new".to_string()),
            ],
        )
        .unwrap();
        if let Value::Json(bytes) = out {
            let text = String::from_utf8(bytes).unwrap();
            // JSON_EXTRACT_TEXT on result must return "new".
            let extracted = dispatch_builtin_function(
                "JSON_EXTRACT_TEXT",
                &[Value::Text(text), json_text("$.b.c")],
            )
            .unwrap();
            assert_eq!(extracted, Value::Text("new".to_string()));
        } else {
            panic!("expected Json");
        }

        // Create a new nested path.
        let out = dispatch_builtin_function(
            "JSON_SET",
            &[doc, json_text("$.new.deep"), Value::Integer(42)],
        )
        .unwrap();
        if let Value::Json(bytes) = out {
            let text = String::from_utf8(bytes).unwrap();
            let extracted = dispatch_builtin_function(
                "JSON_EXTRACT_TEXT",
                &[Value::Text(text), json_text("$.new.deep")],
            )
            .unwrap();
            assert_eq!(extracted, Value::Text("42".to_string()));
        } else {
            panic!("expected Json");
        }
    }
}
