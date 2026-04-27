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

use super::join_filter::{compare_runtime_values, resolve_runtime_field};
use crate::storage::query::ast::{BinOp, Expr, FieldRef, UnaryOp};
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
            // ML and semantic-cache scalars live on the `RedDB` handle
            // (model registry, shared cache). Route them before the
            // db-less builtin dispatcher so the values land with full
            // context.
            if matches!(
                upper.as_str(),
                "ML_CLASSIFY"
                    | "ML_PREDICT_PROBA"
                    | "SEMANTIC_CACHE_GET"
                    | "SEMANTIC_CACHE_PUT"
                    | "EMBED"
            ) {
                if let Some(db) = db {
                    return dispatch_ml_function(db, &upper, &arg_values);
                }
                return None;
            }
            if matches!(
                upper.as_str(),
                "CA_REGISTER" | "CA_DROP" | "CA_STATE" | "CA_LIST" | "CA_REFRESH" | "CA_QUERY"
            ) {
                if let Some(db) = db {
                    return dispatch_ca_function(db, &upper, &arg_values);
                }
                return None;
            }
            if matches!(
                upper.as_str(),
                "LIST_HYPERTABLES" | "LIST_MODELS" | "SHOW_HYPERTABLES" | "SHOW_MODELS"
            ) {
                if let Some(db) = db {
                    return dispatch_introspection_function(db, &upper);
                }
                return None;
            }
            if upper.as_str() == "HYPERTABLE_PRUNE_CHUNKS" {
                if let Some(db) = db {
                    return dispatch_hypertable_prune(db, &arg_values);
                }
                return None;
            }
            if matches!(
                upper.as_str(),
                "HYPERTABLE_DROP_CHUNKS_BEFORE"
                    | "HYPERTABLE_SWEEP_EXPIRED"
                    | "HYPERTABLE_SHOW_CHUNKS"
                    | "HYPERTABLE_SWEEP_ALL_EXPIRED"
                    | "HYPERTABLE_SET_TTL"
                    | "HYPERTABLE_GET_TTL"
                    | "HYPERTABLE_CHUNKS_EXPIRING_WITHIN"
            ) {
                if let Some(db) = db {
                    return dispatch_hypertable_retention(db, &upper, &arg_values);
                }
                return None;
            }
            if matches!(upper.as_str(), "MODEL_REGISTER" | "MODEL_DROP") {
                if let Some(db) = db {
                    return dispatch_model_function(db, &upper, &arg_values);
                }
                return None;
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
            Value::Text(text) => Some(text.as_ref()),
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
        Expr::Column { field, .. } => field_ref_path_text(field).map(Value::text),
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
        BinOp::Concat => Some(Value::text(format!(
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
fn feature_vector_from_value(value: &Value) -> Option<Vec<f32>> {
    match value {
        Value::Vector(v) => Some(v.clone()),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let n = match item {
                    Value::Integer(n) | Value::BigInt(n) => *n as f32,
                    Value::UnsignedInteger(n) => *n as f32,
                    Value::Float(f) => *f as f32,
                    _ => return None,
                };
                out.push(n);
            }
            Some(out)
        }
        _ => None,
    }
}

fn model_kind_from_json(hyperparams_json: &str) -> String {
    crate::serde_json::from_str::<crate::serde_json::Value>(hyperparams_json)
        .ok()
        .as_ref()
        .and_then(|v| v.get("kind"))
        .and_then(|k| k.as_str())
        .unwrap_or("logreg")
        .to_ascii_lowercase()
}

fn dispatch_ml_function(db: &RedDB, name: &str, args: &[Value]) -> Option<Value> {
    match name {
        "ML_CLASSIFY" | "ML_PREDICT_PROBA" => {
            let model_name = match args.first()? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let features = feature_vector_from_value(args.get(1)?)?;

            let version = db.ml_runtime().registry().get_active(&model_name).ok()??;
            let kind = model_kind_from_json(&version.hyperparams_json);
            let weights_json = std::str::from_utf8(&version.weights_blob).ok()?;

            use crate::storage::ml::classifier::IncrementalClassifier;
            let (class, probs) = match kind.as_str() {
                "nb" | "naive_bayes" => {
                    let m = crate::storage::ml::classifier::MultinomialNaiveBayes::from_json(
                        weights_json,
                    )?;
                    (m.predict(&features), m.predict_proba(&features))
                }
                _ => {
                    let m = crate::storage::ml::classifier::LogisticRegression::from_json(
                        weights_json,
                    )?;
                    (m.predict(&features), m.predict_proba(&features))
                }
            };

            if name == "ML_PREDICT_PROBA" {
                Some(Value::Array(
                    probs.into_iter().map(|p| Value::Float(p as f64)).collect(),
                ))
            } else {
                Some(
                    class
                        .map(|c| Value::Integer(c as i64))
                        .unwrap_or(Value::Null),
                )
            }
        }
        "SEMANTIC_CACHE_GET" => {
            let _ns = args.first()?;
            let embedding = feature_vector_from_value(args.get(1)?)?;
            Some(match db.semantic_cache().lookup(&embedding) {
                Some(resp) => Value::text(resp),
                None => Value::Null,
            })
        }
        "SEMANTIC_CACHE_PUT" => {
            let _ns = args.first()?;
            let prompt = match args.get(1)? {
                Value::Text(s) => s.to_string(),
                other => format!("{:?}", other),
            };
            let response = match args.get(2)? {
                Value::Text(s) => s.to_string(),
                other => format!("{:?}", other),
            };
            let embedding = feature_vector_from_value(args.get(3)?)?;
            db.semantic_cache()
                .insert(prompt, response, embedding, None);
            Some(Value::Boolean(true))
        }
        "EMBED" => {
            let text = match args.first()? {
                Value::Text(s) => s.to_string(),
                other => other.display_string(),
            };
            let provider_hint = args.get(1).and_then(|v| match v {
                Value::Text(s) => Some(s.to_string()),
                _ => None,
            });
            embed_text(db, &text, provider_hint.as_deref())
        }
        _ => None,
    }
}

pub(super) fn embed_text_public(
    db: &RedDB,
    text: &str,
    provider_hint: Option<&str>,
) -> Option<Value> {
    embed_text(db, text, provider_hint)
}

fn embed_text(db: &RedDB, text: &str, provider_hint: Option<&str>) -> Option<Value> {
    // Resolve provider from explicit hint, then fall back to the
    // runtime-wide default captured in `red_config`. Ollama and other
    // OpenAI-compatible endpoints share the `openai_embeddings` call;
    // only the api_base and model differ.
    let kv_getter = |k: &str| -> Result<Option<String>, crate::RedDBError> {
        Ok(
            lookup_latest_kv_value(db, "red_config", k).and_then(|v| match v {
                Value::Text(s) => Some(s.to_string()),
                _ => None,
            }),
        )
    };
    let provider = match provider_hint {
        Some(name) => crate::ai::parse_provider(name).ok()?,
        None => crate::ai::resolve_default_provider(&kv_getter),
    };
    if !provider.is_openai_compatible() {
        // Anthropic / providers without embedding parity fall out —
        // caller will see Null. A later sprint widens this.
        return None;
    }

    let api_key = crate::ai::resolve_api_key(&provider, None, |kv_key| {
        Ok(
            lookup_latest_kv_value(db, "red_config", kv_key).and_then(|v| match v {
                Value::Text(s) => Some(s.to_string()),
                _ => None,
            }),
        )
    })
    .ok()?;

    let api_base = provider.resolve_api_base_with_kv("default", &kv_getter);
    let model = provider.default_embedding_model().to_string();

    let request = crate::ai::OpenAiEmbeddingRequest {
        api_key,
        model,
        inputs: vec![text.to_string()],
        dimensions: None,
        api_base,
    };
    match crate::ai::openai_embeddings(request) {
        Ok(resp) => resp.embeddings.into_iter().next().map(Value::Vector),
        Err(_) => None,
    }
}

pub(super) fn dispatch_model_function_public(
    db: &RedDB,
    name: &str,
    args: &[Value],
) -> Option<Value> {
    dispatch_model_function(db, name, args)
}

/// `MODEL_REGISTER(name, kind, weights_json [, hyperparams_json, metrics_json])` —
/// appends a new version to the model registry and activates it.
/// `kind` is `'logreg'` or `'nb'` and is stored inside
/// `hyperparams_json` for ML_CLASSIFY to decode.
///
/// `MODEL_DROP(name)` — archives every version and clears activation.
fn dispatch_model_function(db: &RedDB, name: &str, args: &[Value]) -> Option<Value> {
    use crate::storage::ml::ModelVersion;

    match name {
        "MODEL_REGISTER" => {
            let model_name = match args.first()? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let kind = match args.get(1)? {
                Value::Text(s) => s.to_ascii_lowercase(),
                _ => return Some(Value::Null),
            };
            let weights_json = match args.get(2)? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let hyperparams_json = match args.get(3) {
                Some(Value::Text(s)) => {
                    // Caller-supplied hyperparams; stamp `kind` in if
                    // not already there so downstream decoding picks
                    // the right classifier.
                    if s.contains("\"kind\"") {
                        s.to_string()
                    } else if s.trim() == "{}" || s.trim().is_empty() {
                        format!("{{\"kind\":\"{kind}\"}}")
                    } else {
                        let trimmed = s.trim_start_matches('{').to_string();
                        format!("{{\"kind\":\"{kind}\",{trimmed}")
                    }
                }
                _ => format!("{{\"kind\":\"{kind}\"}}"),
            };
            let metrics_json = match args.get(4) {
                Some(Value::Text(s)) => s.to_string(),
                _ => "{}".to_string(),
            };

            let version = ModelVersion {
                model: model_name.clone(),
                version: 0,
                weights_blob: weights_json.into_bytes(),
                hyperparams_json,
                metrics_json,
                training_data_hash: None,
                training_sql: None,
                parent_version: None,
                created_at_ms: 0,
                created_by: None,
                archived: false,
            };
            let v = db
                .ml_runtime()
                .registry()
                .register_version(model_name, version, /*make_active=*/ true)
                .ok()?;
            Some(Value::Integer(v as i64))
        }
        "MODEL_DROP" => {
            let model_name = match args.first()? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let reg = db.ml_runtime().registry();
            let versions = reg.list_versions(&model_name).ok()?;
            for v in versions {
                let _ = reg.archive_version(&model_name, v.version);
            }
            Some(Value::Boolean(true))
        }
        _ => None,
    }
}

pub(super) fn dispatch_introspection_function_public(db: &RedDB, name: &str) -> Option<Value> {
    dispatch_introspection_function(db, name)
}

/// `HYPERTABLE_PRUNE_CHUNKS(name, lo_ns, hi_ns)` — returns the chunk
/// start timestamps that overlap `[lo_ns, hi_ns)` for the given
/// hypertable. Exposes the partition-pruning primitive over real
/// allocated chunks. Uses RANGE semantics (the only kind hypertables
/// have today).
pub(super) fn dispatch_hypertable_prune_public(db: &RedDB, args: &[Value]) -> Option<Value> {
    dispatch_hypertable_prune(db, args)
}

pub(super) fn dispatch_hypertable_retention_public(
    db: &RedDB,
    name: &str,
    args: &[Value],
) -> Option<Value> {
    dispatch_hypertable_retention(db, name, args)
}

/// Retention + introspection scalars on top of HypertableRegistry.
///
/// * `HYPERTABLE_SHOW_CHUNKS(name)` — array of `"name:start_ns"` for
///   every chunk of the hypertable.
/// * `HYPERTABLE_DROP_CHUNKS_BEFORE(name, cutoff_ns)` — drops every
///   chunk whose `max_ts_ns <= cutoff`. Returns the drop count.
/// * `HYPERTABLE_SWEEP_EXPIRED(name [, now_ns])` — drops every chunk
///   whose effective TTL has fired. Returns the count.
fn dispatch_hypertable_retention(db: &RedDB, name: &str, args: &[Value]) -> Option<Value> {
    let registry = db.hypertables();

    // HYPERTABLE_SWEEP_ALL_EXPIRED takes an optional now_ns and
    // doesn't need a hypertable name — handle before the per-name
    // dispatch path.
    if name == "HYPERTABLE_SWEEP_ALL_EXPIRED" {
        let now_ns = args
            .first()
            .and_then(|v| match v {
                Value::Integer(n) | Value::BigInt(n) => Some(*n as u64),
                Value::UnsignedInteger(n) => Some(*n),
                _ => None,
            })
            .unwrap_or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0)
            });
        let dropped = registry.sweep_all_expired(now_ns).len();
        return Some(Value::Integer(dropped as i64));
    }

    let ht_name = match args.first()? {
        Value::Text(s) => s.to_string(),
        _ => return Some(Value::Null),
    };
    match name {
        "HYPERTABLE_SHOW_CHUNKS" => Some(Value::Array(
            registry
                .show_chunks(&ht_name)
                .into_iter()
                .map(|c| Value::text(format!("{}:{}", c.id.hypertable, c.id.start_ns)))
                .collect(),
        )),
        "HYPERTABLE_DROP_CHUNKS_BEFORE" => {
            let cutoff = match args.get(1)? {
                Value::Integer(n) | Value::BigInt(n) => *n as u64,
                Value::UnsignedInteger(n) => *n,
                _ => return Some(Value::Null),
            };
            let dropped = registry.drop_chunks_before(&ht_name, cutoff).len();
            Some(Value::Integer(dropped as i64))
        }
        "HYPERTABLE_SWEEP_EXPIRED" => {
            let now_ns = args
                .get(1)
                .and_then(|v| match v {
                    Value::Integer(n) | Value::BigInt(n) => Some(*n as u64),
                    Value::UnsignedInteger(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0)
                });
            let dropped = registry.sweep_expired(&ht_name, now_ns).len();
            Some(Value::Integer(dropped as i64))
        }
        "HYPERTABLE_SET_TTL" => {
            // HYPERTABLE_SET_TTL(name, duration | null)
            let ttl_ns = match args.get(1)? {
                Value::Null => None,
                Value::Text(s) => {
                    Some(crate::storage::timeseries::retention::parse_duration_ns(s)?)
                }
                Value::Integer(n) | Value::BigInt(n) if *n >= 0 => Some(*n as u64),
                Value::UnsignedInteger(n) => Some(*n),
                _ => return Some(Value::Null),
            };
            Some(Value::Boolean(
                registry.set_default_ttl_ns(&ht_name, ttl_ns),
            ))
        }
        "HYPERTABLE_GET_TTL" => {
            let spec = registry.get(&ht_name)?;
            Some(match spec.default_ttl_ns {
                Some(n) => Value::Integer(n as i64),
                None => Value::Null,
            })
        }
        "HYPERTABLE_CHUNKS_EXPIRING_WITHIN" => {
            // HYPERTABLE_CHUNKS_EXPIRING_WITHIN(name, now_ns, horizon_ns)
            let now_ns = match args.get(1)? {
                Value::Integer(n) | Value::BigInt(n) => *n as u64,
                Value::UnsignedInteger(n) => *n,
                _ => return Some(Value::Null),
            };
            let horizon_ns = match args.get(2)? {
                Value::Integer(n) | Value::BigInt(n) => *n as u64,
                Value::UnsignedInteger(n) => *n,
                _ => return Some(Value::Null),
            };
            let expiring = registry.chunks_expiring_within(&ht_name, now_ns, horizon_ns);
            Some(Value::Array(
                expiring
                    .into_iter()
                    .map(|c| Value::text(format!("{}:{}", c.id.hypertable, c.id.start_ns)))
                    .collect(),
            ))
        }
        _ => None,
    }
}

fn dispatch_hypertable_prune(db: &RedDB, args: &[Value]) -> Option<Value> {
    use crate::storage::query::planner::partition_pruning::{
        prune_range, PruneKind, PruneOp, PrunePartitioning, PrunePredicate, PruneValue, RangeChild,
    };

    let ht_name = match args.first()? {
        Value::Text(s) => s.to_string(),
        _ => return Some(Value::Null),
    };
    let lo = match args.get(1)? {
        Value::Integer(n) | Value::BigInt(n) => *n as u64,
        Value::UnsignedInteger(n) => *n,
        _ => return Some(Value::Null),
    };
    let hi = match args.get(2)? {
        Value::Integer(n) | Value::BigInt(n) => *n as u64,
        Value::UnsignedInteger(n) => *n,
        _ => return Some(Value::Null),
    };

    let registry = db.hypertables();
    let spec = registry.get(&ht_name)?;
    let chunks = registry.show_chunks(&ht_name);

    // Build RangeChild list — one per chunk — then feed the predicate
    // `spec.time_column >= lo AND spec.time_column < hi` to the
    // existing pruner primitive.
    let children: Vec<RangeChild> = chunks
        .iter()
        .map(|c| RangeChild {
            name: format!("{}:{}", c.id.hypertable, c.id.start_ns),
            low: Some(PruneValue::Int(c.id.start_ns as i64)),
            high_exclusive: Some(PruneValue::Int(c.end_ns_exclusive as i64)),
        })
        .collect();

    let partitioning = PrunePartitioning {
        kind: PruneKind::Range,
        column: spec.time_column.clone(),
    };
    let pred = PrunePredicate::And(vec![
        PrunePredicate::Compare {
            column: spec.time_column.clone(),
            op: PruneOp::GtEq,
            value: PruneValue::Int(lo as i64),
        },
        PrunePredicate::Compare {
            column: spec.time_column.clone(),
            op: PruneOp::Lt,
            value: PruneValue::Int(hi as i64),
        },
    ]);
    let kept = prune_range(&partitioning, &children, &pred);
    Some(Value::Array(kept.into_iter().map(Value::text).collect()))
}

fn dispatch_introspection_function(db: &RedDB, name: &str) -> Option<Value> {
    match name {
        "LIST_HYPERTABLES" | "SHOW_HYPERTABLES" => {
            let names: Vec<Value> = db
                .hypertables()
                .list()
                .into_iter()
                .map(|s| Value::text(s.name))
                .collect();
            Some(Value::Array(names))
        }
        "LIST_MODELS" | "SHOW_MODELS" => {
            let summaries = db.ml_runtime().registry().summaries().ok()?;
            Some(Value::Array(
                summaries.into_iter().map(|s| Value::text(s.name)).collect(),
            ))
        }
        _ => None,
    }
}

pub(super) fn dispatch_ca_function_public(db: &RedDB, name: &str, args: &[Value]) -> Option<Value> {
    dispatch_ca_function(db, name, args)
}

fn dispatch_ca_function(db: &RedDB, name: &str, args: &[Value]) -> Option<Value> {
    use crate::storage::timeseries::continuous_aggregate::{
        ContinuousAggregateColumn, ContinuousAggregateSpec,
    };
    use crate::storage::timeseries::AggregationType;

    let engine = db.continuous_aggregates();

    match name {
        "CA_REGISTER" => {
            // CA_REGISTER(name, source, bucket_duration, alias, agg, field
            //             [, refresh_lag, max_interval])
            let ca_name = match args.first()? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let source = match args.get(1)? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let bucket = match args.get(2)? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let alias = match args.get(3)? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let agg = match args.get(4)? {
                Value::Text(s) => AggregationType::from_str(s)?,
                _ => return Some(Value::Null),
            };
            let field = match args.get(5)? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let lag = args
                .get(6)
                .and_then(|v| match v {
                    Value::Text(s) => Some(s.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| "0s".to_string());
            let max_interval = args
                .get(7)
                .and_then(|v| match v {
                    Value::Text(s) => Some(s.to_string()),
                    _ => None,
                })
                .unwrap_or_else(|| "365d".to_string());

            let spec = ContinuousAggregateSpec::from_durations(
                ca_name,
                source,
                &bucket,
                vec![ContinuousAggregateColumn {
                    alias,
                    source_column: field,
                    agg,
                }],
                &lag,
                &max_interval,
            )?;
            engine.register(spec);
            Some(Value::Boolean(true))
        }
        "CA_DROP" => {
            let ca_name = match args.first()? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            engine.drop_aggregate(&ca_name);
            Some(Value::Boolean(true))
        }
        "CA_STATE" => {
            // Returns a JSON-ish summary — buckets count + last refreshed.
            let ca_name = match args.first()? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            match engine.state(&ca_name) {
                Some(state) => Some(Value::text(format!(
                    "{{\"last_refreshed_bucket_ns\":{},\"bucket_count\":{}}}",
                    state.last_refreshed_bucket_ns(),
                    state.bucket_count()
                ))),
                None => Some(Value::Null),
            }
        }
        "CA_LIST" => {
            let names: Vec<Value> = engine
                .list()
                .into_iter()
                .map(|s| Value::text(s.name))
                .collect();
            Some(Value::Array(names))
        }
        "CA_REFRESH" => {
            // CA_REFRESH(name [, now_ns]) — scans the source collection
            // for row entities, extracts the timestamp column from the
            // spec and every aggregated source_column, folds into
            // buckets. `now_ns` defaults to wall-clock now.
            let ca_name = match args.first()? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let now_ns = args
                .get(1)
                .and_then(|v| match v {
                    Value::Integer(n) | Value::BigInt(n) => Some(*n as u64),
                    Value::UnsignedInteger(n) => Some(*n),
                    _ => None,
                })
                .unwrap_or_else(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0)
                });

            let specs = engine.list();
            let spec = specs.into_iter().find(|s| s.name == ca_name)?;
            let store = db.store();
            let time_col = "ts".to_string();
            let columns = spec.columns.clone();
            let source_name = spec.source.clone();
            let source_cb: crate::storage::timeseries::continuous_aggregate::ContinuousAggregateSource =
                std::sync::Arc::new(move |src: &str, start: u64, end: u64| {
                    let mut out = Vec::new();
                    let Some(mgr) = store.get_collection(src) else {
                        return out;
                    };
                    for entity in mgr.query_all(|_| true) {
                        let crate::storage::unified::entity::EntityData::Row(row) = &entity.data
                        else {
                            continue;
                        };
                        let ts = match row.get_field(&time_col) {
                            Some(Value::Integer(n) | Value::BigInt(n)) => *n as u64,
                            Some(Value::UnsignedInteger(n)) => *n,
                            _ => continue,
                        };
                        if ts < start || ts >= end {
                            continue;
                        }
                        let mut values = std::collections::HashMap::new();
                        for col in &columns {
                            let v = row.get_field(&col.source_column).and_then(|v| match v {
                                Value::Integer(n) | Value::BigInt(n) => Some(*n as f64),
                                Value::UnsignedInteger(n) => Some(*n as f64),
                                Value::Float(f) => Some(*f),
                                _ => None,
                            });
                            if let Some(f) = v {
                                values.insert(col.alias.clone(), f);
                            }
                        }
                        out.push(
                            crate::storage::timeseries::continuous_aggregate::RefreshPoint {
                                ts_ns: ts,
                                values,
                            },
                        );
                    }
                    out
                });
            let _ = source_name;
            let absorbed = engine.refresh(&ca_name, now_ns, &source_cb);
            Some(Value::Integer(absorbed as i64))
        }
        "CA_QUERY" => {
            // CA_QUERY(name, bucket_start_ns, alias) — returns the
            // aggregated value for a single bucket using the aggregator
            // declared at registration time.
            let ca_name = match args.first()? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let bucket_start = match args.get(1)? {
                Value::Integer(n) | Value::BigInt(n) => *n as u64,
                Value::UnsignedInteger(n) => *n,
                _ => return Some(Value::Null),
            };
            let alias = match args.get(2)? {
                Value::Text(s) => s.to_string(),
                _ => return Some(Value::Null),
            };
            let state = engine.state(&ca_name)?;
            let spec = engine.list().into_iter().find(|s| s.name == ca_name)?;
            let agg = spec
                .columns
                .iter()
                .find(|c| c.alias == alias)
                .map(|c| c.agg)?;
            state
                .query(bucket_start, &alias, agg)
                .map(Value::Float)
                .or(Some(Value::Null))
        }
        _ => None,
    }
}

fn dispatch_builtin_function(name: &str, args: &[Value]) -> Option<Value> {
    match name {
        "UPPER" => match args.first()? {
            Value::Text(s) => Some(Value::text(s.to_uppercase())),
            other => Some(other.clone()),
        },
        "LOWER" => match args.first()? {
            Value::Text(s) => Some(Value::text(s.to_lowercase())),
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
                        Some(matched) => Value::text(matched),
                        None => Value::Null,
                    })
                }
                start_value => {
                    let start = value_as_i64(start_value)?;
                    let count = args.get(2).and_then(value_as_i64);
                    Some(Value::text(substring_text(text, start, count)?))
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
                Some(Value::Text(chars)) => Some(chars.as_ref()),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(text, chars, true, true)))
        }
        "LTRIM" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args.get(1) {
                None => None,
                Some(Value::Text(chars)) => Some(chars.as_ref()),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(text, chars, true, false)))
        }
        "RTRIM" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args.get(1) {
                None => None,
                Some(Value::Text(chars)) => Some(chars.as_ref()),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(text, chars, false, true)))
        }
        "CONCAT" => Some(Value::text(
            args.iter()
                .filter(|value| !matches!(value, Value::Null))
                .map(Value::display_string)
                .collect::<String>(),
        )),
        "CONCAT_WS" => {
            let separator = match args.first()? {
                Value::Null => return Some(Value::Null),
                Value::Text(text) => text.as_ref(),
                other => {
                    return Some(Value::text(
                        args.iter()
                            .skip(1)
                            .filter(|value| !matches!(value, Value::Null))
                            .map(Value::display_string)
                            .collect::<Vec<_>>()
                            .join(&other.display_string()),
                    ))
                }
            };
            Some(Value::text(
                args.iter()
                    .skip(1)
                    .filter(|value| !matches!(value, Value::Null))
                    .map(Value::display_string)
                    .collect::<Vec<_>>()
                    .join(separator),
            ))
        }
        "REVERSE" => match args.first()? {
            Value::Text(text) => Some(Value::text(text.chars().rev().collect::<String>())),
            _ => Some(Value::Null),
        },
        "LEFT" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count = value_as_i64(args.get(1)?)?;
            Some(Value::text(slice_left_text(text, count)))
        }
        "RIGHT" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count = value_as_i64(args.get(1)?)?;
            Some(Value::text(slice_right_text(text, count)))
        }
        "QUOTE_LITERAL" => match args.first()? {
            Value::Null => Some(Value::Null),
            Value::Text(text) => Some(Value::text(quote_literal_text(text))),
            other => Some(Value::text(quote_literal_text(&other.display_string()))),
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
                .map(Value::text)
                .unwrap_or(Value::Null),
        ),
        // Session identity scalars — `WITHIN ... USER '<u>' AS ROLE '<r>'`
        // overrides win over the transport-installed thread-local.
        // Anonymous callers with no override get NULL.
        "CURRENT_USER" | "SESSION_USER" | "USER" => Some(
            crate::runtime::impl_core::current_user_projected()
                .map(Value::text)
                .unwrap_or(Value::Null),
        ),
        "CURRENT_ROLE" => Some(
            crate::runtime::impl_core::current_role_projected()
                .map(Value::text)
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
            let hash: &str = match stored {
                Value::Password(hash) => hash.as_str(),
                Value::Text(hash) => hash.as_ref(),
                _ => return Some(Value::Boolean(false)),
            };
            let plain: &str = match candidate {
                Value::Text(plain) => plain.as_ref(),
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
            Some(Value::text(name.to_string()))
        }
        "JSON_VALID" => {
            let text: String = match args.first()? {
                Value::Text(s) => s.to_string(),
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
                let key: String = match &pair[0] {
                    Value::Text(s) => s.to_string(),
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
        Value::Text(s) => crate::serde_json::Value::String(s.to_string()),
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
            crate::serde_json::Value::String(s) => Some(Value::text(s.clone())),
            crate::serde_json::Value::Null => Some(Value::Null),
            crate::serde_json::Value::Bool(b) => Some(Value::text(b.to_string())),
            crate::serde_json::Value::Number(n) => Some(Value::text(n.to_string())),
            other => Some(Value::text(other.to_string_compact())),
        }
    } else {
        // JSON-serialised representation (strings come back quoted).
        Some(Value::text(target.to_string_compact()))
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
        Value::Text(text) => Some(text.to_string()),
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
                Value::text("0.125".to_string()),
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
        Value::text(s.to_string())
    }

    #[test]
    fn json_extract_scalar_and_nested() {
        let doc = json_text(r#"{"a":1,"b":{"c":"hello","d":[10,20,30]}}"#);
        // Top-level scalar.
        assert_eq!(
            dispatch_builtin_function("JSON_EXTRACT", &[doc.clone(), json_text("$.a")]).unwrap(),
            Value::text("1".to_string())
        );
        // Nested string (quoted for JSON_EXTRACT).
        assert_eq!(
            dispatch_builtin_function("JSON_EXTRACT", &[doc.clone(), json_text("$.b.c")]).unwrap(),
            Value::text("\"hello\"".to_string())
        );
        // Unquoted via JSON_EXTRACT_TEXT.
        assert_eq!(
            dispatch_builtin_function("JSON_EXTRACT_TEXT", &[doc.clone(), json_text("$.b.c")])
                .unwrap(),
            Value::text("hello".to_string())
        );
        // Array index.
        assert_eq!(
            dispatch_builtin_function("JSON_EXTRACT_TEXT", &[doc.clone(), json_text("$.b.d[1]")])
                .unwrap(),
            Value::text("20".to_string())
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
            Value::text("array".to_string())
        );
        assert_eq!(
            dispatch_builtin_function("JSON_TYPEOF", &[json_text(r#"{"k":1}"#)]).unwrap(),
            Value::text("object".to_string())
        );
        assert_eq!(
            dispatch_builtin_function("JSON_TYPEOF", &[json_text("null")]).unwrap(),
            Value::text("null".to_string())
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
                Value::text("x".to_string()),
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
                Value::text("k1".to_string()),
                Value::Integer(1),
                Value::text("k2".to_string()),
                Value::text("v".to_string()),
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
                Value::text("new".to_string()),
            ],
        )
        .unwrap();
        if let Value::Json(bytes) = out {
            let text = String::from_utf8(bytes).unwrap();
            // JSON_EXTRACT_TEXT on result must return "new".
            let extracted = dispatch_builtin_function(
                "JSON_EXTRACT_TEXT",
                &[Value::text(text), json_text("$.b.c")],
            )
            .unwrap();
            assert_eq!(extracted, Value::text("new".to_string()));
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
                &[Value::text(text), json_text("$.new.deep")],
            )
            .unwrap();
            assert_eq!(extracted, Value::text("42".to_string()));
        } else {
            panic!("expected Json");
        }
    }
}
