//! Projection-value evaluation and projection scalar helpers.
use super::*;

pub(in crate::runtime) fn eval_projection_value(
    proj: &Projection,
    source: &UnifiedRecord,
) -> Option<Value> {
    reddb_engine::query::runtime::projection_value::eval_projection_value_with_host(
        &RuntimeProjectionValueHost,
        proj,
        source,
    )
}

struct RuntimeProjectionValueHost;
impl reddb_engine::query::runtime::projection_value::ProjectionValueHost
    for RuntimeProjectionValueHost
{
    fn try_expression(
        &self,
        expression: &reddb_rql::ast::Expr,
        source: &UnifiedRecord,
    ) -> Option<Value> {
        let row = RecordRow {
            db: None,
            record: source,
            table_name: None,
            table_alias: None,
        };
        crate::runtime::expr_eval::evaluate_typed_expr_with_secret_literals(expression, &row).ok()
    }
    fn fallback_function(
        &self,
        name: &str,
        arguments: &[Projection],
        source: &UnifiedRecord,
    ) -> Option<Value> {
        evaluate_scalar_function(name, arguments, source)
    }
    fn fallback_filter(&self, filter: &Filter, source: &UnifiedRecord) -> bool {
        evaluate_runtime_filter(source, filter, None, None)
    }
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        crate::runtime::execution_context::secret_query_input(value)
    }
}

pub(in crate::runtime) fn eval_projection_value_with_db(
    db: Option<&RedDB>,
    proj: &Projection,
    source: &UnifiedRecord,
) -> Option<Value> {
    match proj {
        Projection::Function(name, inner_args) => {
            evaluate_scalar_function_with_db(db, name, inner_args, source)
        }
        Projection::Expression(filter, _) => Some(Value::Boolean(evaluate_runtime_filter_with_db(
            db, source, filter, None, None,
        ))),
        _ => eval_projection_value(proj, source),
    }
}

/// Handle ML_CLASSIFY / ML_PREDICT_PROBA / SEMANTIC_CACHE_* scalars.
///
/// Calling convention:
/// - `ML_CLASSIFY(model_name, features)` — `features` is either a
///   `Value::Vector(Vec<f32>)` or a `Value::Array(Vec<numeric>)`.
///   Returns the predicted class id as `Value::Integer`, or `Null`
///   when the model is unknown / features shape mismatch.
/// - `ML_PREDICT_PROBA(model_name, features)` — same shapes; returns
///   a `Value::Array(Vec<Float>)` of per-class probabilities.
/// - `SEMANTIC_CACHE_GET(namespace, embedding)` — returns the cached
///   response `Value::Text` if cosine similarity ≥ the cache's
///   configured threshold; `Value::Null` otherwise. `namespace` is
///   reserved for future per-tenant isolation; currently shared.
/// - `SEMANTIC_CACHE_PUT(namespace, prompt, response, embedding)` —
///   inserts. Returns `Value::Boolean(true)` on success.
pub(in crate::runtime::join_filter) fn evaluate_ml_scalar(
    db: &RedDB,
    name: &str,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    match name {
        "ML_CLASSIFY" => ml_classify(db, args, source, /*probas=*/ false),
        "ML_PREDICT_PROBA" => ml_classify(db, args, source, /*probas=*/ true),
        "SEMANTIC_CACHE_GET" => semantic_cache_get(db, args, source),
        "SEMANTIC_CACHE_PUT" => semantic_cache_put(db, args, source),
        "EMBED" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(s) => s.to_string(),
                other => other.display_string(),
            };
            let provider_hint = args.get(1).and_then(|_| {
                resolve_scalar_arg(args, 1, source).and_then(|v| match v {
                    Value::Text(s) => Some(s.to_string()),
                    _ => None,
                })
            });
            super::expr_eval::embed_text_public(db, &text, provider_hint.as_deref())
        }
        _ => None,
    }
}

pub(in crate::runtime::join_filter) fn resolve_feature_vector(
    args: &[Projection],
    idx: usize,
    source: &UnifiedRecord,
) -> Option<Vec<f32>> {
    let val = resolve_scalar_arg(args, idx, source)?;
    match val {
        Value::Vector(v) => Some(v),
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let n = value_as_number(&item)?;
                out.push(n.as_f64() as f32);
            }
            Some(out)
        }
        _ => None,
    }
}

pub(in crate::runtime::join_filter) fn ml_classify(
    db: &RedDB,
    args: &[Projection],
    source: &UnifiedRecord,
    probas: bool,
) -> Option<Value> {
    let model_name = match resolve_scalar_arg(args, 0, source)? {
        Value::Text(s) => s.to_string(),
        _ => return None,
    };
    let features = resolve_feature_vector(args, 1, source)?;

    let version = db.ml_runtime().registry().get_active(&model_name).ok()??;
    // Model kind is stamped into `hyperparams_json` as `{"kind":"logreg"|"nb", ...}`.
    // Fall back to `logreg` when unset (pre-existing models only ever
    // registered logreg).
    let kind = parse_model_kind(&version.hyperparams_json);
    let weights_json = std::str::from_utf8(&version.weights_blob).ok()?;

    use crate::storage::ml::classifier::IncrementalClassifier;
    let (class, probs) = match kind.as_str() {
        "nb" | "naive_bayes" => {
            let m = crate::storage::ml::classifier::MultinomialNaiveBayes::from_json(weights_json)?;
            (m.predict(&features), m.predict_proba(&features))
        }
        _ => {
            let m = crate::storage::ml::classifier::LogisticRegression::from_json(weights_json)?;
            (m.predict(&features), m.predict_proba(&features))
        }
    };

    if probas {
        Some(Value::Array(
            probs.into_iter().map(|p| Value::Float(p as f64)).collect(),
        ))
    } else {
        class.map(|c| Value::Integer(c as i64))
    }
}

pub(in crate::runtime::join_filter) fn parse_model_kind(hyperparams_json: &str) -> String {
    crate::serde_json::from_str::<crate::serde_json::Value>(hyperparams_json)
        .ok()
        .as_ref()
        .and_then(|v| v.get("kind"))
        .and_then(|k| k.as_str())
        .unwrap_or("logreg")
        .to_ascii_lowercase()
}

pub(in crate::runtime::join_filter) fn semantic_cache_get(
    db: &RedDB,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    // args[0] = namespace (reserved, currently ignored)
    let _ns = resolve_scalar_arg(args, 0, source)?;
    let embedding = resolve_feature_vector(args, 1, source)?;
    match db.semantic_cache().lookup(&embedding) {
        Some(response) => Some(Value::text(response)),
        None => Some(Value::Null),
    }
}

pub(in crate::runtime::join_filter) fn semantic_cache_put(
    db: &RedDB,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    let _ns = resolve_scalar_arg(args, 0, source)?;
    let prompt = match resolve_scalar_arg(args, 1, source)? {
        Value::Text(s) => s.to_string(),
        other => other.display_string(),
    };
    let response = match resolve_scalar_arg(args, 2, source)? {
        Value::Text(s) => s.to_string(),
        other => other.display_string(),
    };
    let embedding = resolve_feature_vector(args, 3, source)?;
    db.semantic_cache()
        .insert(prompt, response, embedding, None);
    Some(Value::Boolean(true))
}

pub(in crate::runtime::join_filter) fn evaluate_projection_config_function(
    db: Option<&RedDB>,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    let key = projection_path_text(args.first()?)?;
    if let Some(value) = crate::runtime::impl_core::current_config_value(&key) {
        return Some(value);
    }
    if let Some(db) = db {
        // #1743 — gate the raw `red_config` fallback on `config:read`, matching
        // the expression-path resolver.
        if crate::runtime::impl_core::config_read_permitted(&key) {
            // `$config.<path>` desugars to CONFIG("red.config/<path>") but SET CONFIG
            // stores under the bare key — try the stripped key too (#1370). This is
            // the WHERE-clause / projection legacy path (evaluate_scalar_function_with_db).
            let key_str: &str = key.as_ref();
            let bare = key_str.strip_prefix("red.config/").unwrap_or(key_str);
            if let Some(value) = super::expr_eval::lookup_latest_kv_value(db, "red_config", &key)
                .or_else(|| super::expr_eval::lookup_latest_kv_value(db, "red_config", bare))
            {
                return Some(value);
            }
        }
    }
    args.get(1)
        .and_then(|arg| projection_default_value_with_db(db, arg, source))
        .or(Some(Value::Null))
}

pub(in crate::runtime::join_filter) fn evaluate_projection_kv_function(
    db: Option<&RedDB>,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    let collection = projection_path_text(args.first()?)?;
    let key = projection_path_text(args.get(1)?)?;
    if let Some(db) = db {
        // #1743 — config-collection `KV()` reads are gated on `config:read`.
        if crate::runtime::impl_core::kv_read_permitted(&collection, &key) {
            if let Some(value) = super::expr_eval::lookup_latest_kv_value(db, &collection, &key) {
                return Some(value);
            }
        }
    }
    args.get(2)
        .and_then(|arg| projection_default_value_with_db(db, arg, source))
        .or(Some(Value::Null))
}

pub(in crate::runtime::join_filter) fn evaluate_projection_secret_ref(
    args: &[Projection],
) -> Option<Value> {
    let key = projection_path_text(args.first()?)?.to_ascii_lowercase();
    crate::runtime::impl_core::current_secret_value(&key).or(Some(Value::Null))
}

/// Resolve `$kv.*` in a projection. Unlike secrets, plain KV values are
/// not masked — the resolver already enforces `kv:read`, and denied/absent
/// keys fall through to NULL (#1602).
pub(in crate::runtime::join_filter) fn evaluate_projection_kv_ref(
    args: &[Projection],
) -> Option<Value> {
    let key = projection_path_text(args.first()?)?.to_ascii_lowercase();
    crate::runtime::impl_core::current_kv_value(&key)
        .map(Value::text)
        .or(Some(Value::Null))
}

pub(in crate::runtime::join_filter) fn projection_path_text(
    projection: &Projection,
) -> Option<String> {
    match projection {
        Projection::Field(field, _) => Some(field_ref_name(field)),
        Projection::Column(column) => column.strip_prefix("LIT:").map(|text| text.to_string()),
        Projection::Alias(column, _) => Some(column.clone()),
        _ => None,
    }
}

pub(in crate::runtime::join_filter) fn projection_default_value_with_db(
    db: Option<&RedDB>,
    projection: &Projection,
    source: &UnifiedRecord,
) -> Option<Value> {
    match projection {
        Projection::Field(field, _) => Some(Value::text(field_ref_name(field))),
        _ => eval_projection_value_with_db(db, projection, source),
    }
}

pub(in crate::runtime::join_filter) fn resolve_geo_arg(
    argument: &Projection,
    source: &UnifiedRecord,
) -> Option<(f64, f64)> {
    reddb_engine::query::runtime::geographic::resolve_geo_arg(argument, source, &|argument| {
        eval_projection_value(argument, source)
    })
}
