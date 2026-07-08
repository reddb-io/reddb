//! Projection-value evaluation and projection scalar helpers.
use super::*;

pub(in crate::runtime) fn eval_projection_value(
    proj: &Projection,
    source: &UnifiedRecord,
) -> Option<Value> {
    match proj {
        Projection::Column(col) => {
            if let Some(lit_val) = col.strip_prefix("LIT:") {
                if lit_val.is_empty() {
                    return Some(Value::Null);
                }
                // Composite sentinel — Array/Vector/Blob roundtrip via
                // JSON-ish encoding (see `serialize_value_json` in
                // `sql_lowering`). Preserves shape across the
                // Projection-based legacy scalar dispatcher.
                if let Some(payload) = lit_val.strip_prefix("@RL:") {
                    if let Some(v) = parse_rl_literal(payload) {
                        return Some(v);
                    }
                }
                if let Ok(n) = lit_val.parse::<i64>() {
                    return Some(Value::Integer(n));
                }
                if let Ok(n) = lit_val.parse::<f64>() {
                    return Some(Value::Float(n));
                }
                return Some(Value::text(lit_val.to_string()));
            }
            source.get(col.as_str()).cloned()
        }
        Projection::Alias(col, _) => {
            eval_projection_value(&Projection::Column(col.clone()), source)
        }
        Projection::Field(field, _) => resolve_runtime_field(source, field, None, None),
        Projection::Function(name, inner_args) => {
            crate::storage::query::sql_lowering::projection_to_expr(proj)
                .and_then(|(expr, _)| {
                    let row = RecordRow {
                        record: source,
                        table_name: None,
                        table_alias: None,
                    };
                    crate::storage::query::evaluator::evaluate(&expr, &row).ok()
                })
                .or_else(|| evaluate_scalar_function(name, inner_args, source))
        }
        Projection::Expression(filter, _) => {
            crate::storage::query::sql_lowering::projection_to_expr(proj)
                .and_then(|(expr, _)| {
                    let row = RecordRow {
                        record: source,
                        table_name: None,
                        table_alias: None,
                    };
                    crate::storage::query::evaluator::evaluate(&expr, &row).ok()
                })
                .or_else(|| {
                    Some(Value::Boolean(evaluate_runtime_filter(
                        source, filter, None, None,
                    )))
                })
        }
        Projection::All => None,
        // Slice 7b (#590): window output is pre-materialised on the
        // record under the alias by `runtime::window_phase::apply`.
        Projection::Window { name, alias, .. } => {
            let label: String = alias.clone().unwrap_or_else(|| name.clone());
            source.get(label.as_str()).cloned()
        }
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
    if crate::runtime::impl_core::current_secret_value(&key).is_some() {
        Some(Value::text("***"))
    } else {
        Some(Value::Null)
    }
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

pub(in crate::runtime::join_filter) fn resolve_time_bucket_duration(
    args: &[Projection],
    index: usize,
) -> Option<u64> {
    let Projection::Column(column) = args.get(index)? else {
        return None;
    };
    let literal = column.strip_prefix("LIT:")?;
    crate::storage::timeseries::retention::parse_duration_ns(literal)
}

pub(in crate::runtime::join_filter) fn resolve_time_bucket_timestamp(
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<u64> {
    if let Some(value) = args
        .get(1)
        .and_then(|_| resolve_scalar_arg(args, 1, source))
    {
        return value_to_bucket_timestamp_ns(&value);
    }

    source
        .get("timestamp_ns")
        .and_then(value_to_bucket_timestamp_ns)
        .or_else(|| {
            source
                .get("timestamp_ms")
                .and_then(value_to_bucket_timestamp_ns)
        })
        .or_else(|| {
            source
                .get("timestamp")
                .and_then(value_to_bucket_timestamp_ns)
        })
}

pub(in crate::runtime::join_filter) fn value_to_bucket_timestamp_ns(value: &Value) -> Option<u64> {
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

pub(in crate::runtime::join_filter) fn substring_text(
    text: &str,
    start: i64,
    count: Option<i64>,
) -> Option<String> {
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

pub(in crate::runtime::join_filter) fn substring_pattern_text(
    text: &str,
    pattern: &str,
) -> Option<String> {
    let regex = regex::Regex::new(pattern).ok()?;
    let captures = regex.captures(text)?;
    if captures.len() > 1 {
        return captures.get(1).map(|capture| capture.as_str().to_string());
    }
    captures.get(0).map(|capture| capture.as_str().to_string())
}

pub(in crate::runtime::join_filter) fn position_text(needle: &str, haystack: &str) -> i64 {
    if needle.is_empty() {
        return 1;
    }
    haystack
        .find(needle)
        .map(|byte_idx| haystack[..byte_idx].chars().count() as i64 + 1)
        .unwrap_or(0)
}

pub(in crate::runtime::join_filter) fn slice_left_text(text: &str, count: i64) -> String {
    let chars: Vec<char> = text.chars().collect();
    let take = normalized_slice_len(chars.len(), count);
    chars.into_iter().take(take).collect()
}

pub(in crate::runtime::join_filter) fn slice_right_text(text: &str, count: i64) -> String {
    let chars: Vec<char> = text.chars().collect();
    let take = normalized_slice_len(chars.len(), count);
    let len = chars.len();
    chars.into_iter().skip(len.saturating_sub(take)).collect()
}

pub(in crate::runtime::join_filter) fn normalized_slice_len(len: usize, count: i64) -> usize {
    if count >= 0 {
        usize::try_from(count).unwrap_or(usize::MAX).min(len)
    } else {
        len.saturating_sub(count.unsigned_abs() as usize)
    }
}

pub(in crate::runtime::join_filter) fn quote_literal_text(text: &str) -> String {
    let escaped = text.replace('\'', "''");
    if text.contains('\\') {
        format!("E'{}'", escaped.replace('\\', "\\\\"))
    } else {
        format!("'{escaped}'")
    }
}

pub(in crate::runtime::join_filter) fn trim_text(
    text: &str,
    chars: Option<&str>,
    left: bool,
    right: bool,
) -> String {
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

/// Resolve two geographic points from function arguments.
/// Supports: (column, POINT(lat, lon)) or (col1, col2)
pub(in crate::runtime::join_filter) fn resolve_two_geo_points(
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<(f64, f64, f64, f64)> {
    match args {
        [left, right] => {
            let (lat1, lon1) = resolve_geo_arg(left, source)?;
            let (lat2, lon2) = resolve_geo_arg(right, source)?;
            Some((lat1, lon1, lat2, lon2))
        }
        [left, lat, lon] => {
            let (lat1, lon1) = resolve_geo_arg(left, source)?;
            let lat2 = resolve_geo_number(lat, source)?;
            let lon2 = resolve_geo_number(lon, source)?;
            Some((lat1, lon1, lat2, lon2))
        }
        [lat1, lon1, lat2, lon2] => Some((
            resolve_geo_number(lat1, source)?,
            resolve_geo_number(lon1, source)?,
            resolve_geo_number(lat2, source)?,
            resolve_geo_number(lon2, source)?,
        )),
        _ => None,
    }
}

/// Resolve a single geo argument — either a column (GeoPoint/Latitude/Longitude) or POINT literal.
pub(in crate::runtime::join_filter) fn resolve_geo_arg(
    arg: &Projection,
    source: &UnifiedRecord,
) -> Option<(f64, f64)> {
    match arg {
        Projection::Column(col) => {
            // POINT:lat:lon literal
            if let Some(rest) = col.strip_prefix("POINT:") {
                let parts: Vec<&str> = rest.splitn(2, ':').collect();
                if parts.len() == 2 {
                    let lat: f64 = parts[0].parse().ok()?;
                    let lon: f64 = parts[1].parse().ok()?;
                    return Some((lat, lon));
                }
            }
            // Column reference → look up in record values
            let val = source
                .get(col.as_str())
                .cloned()
                .or_else(|| resolve_runtime_document_path(source, col))?;
            match &val {
                value if crate::geo::recognize_geo_value(value).is_some() => {
                    crate::geo::recognize_geo_value(value)
                }
                Value::Float(f) => {
                    // Could be a lat or lon — check for "lat"/"lon" sibling columns
                    let lat_keys = ["lat", "latitude"];
                    let lon_keys = ["lon", "longitude", "lng"];
                    if lat_keys.contains(&col.as_str()) {
                        let lon =
                            lon_keys
                                .iter()
                                .find_map(|k| source.get(k))
                                .and_then(|v| match v {
                                    Value::Float(f) => Some(*f),
                                    Value::Integer(n) => Some(*n as f64),
                                    _ => None,
                                })?;
                        Some((*f, lon))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        _ => {
            let value = eval_projection_value(arg, source)?;
            crate::geo::recognize_geo_value(&value)
        }
    }
}

fn resolve_geo_number(arg: &Projection, source: &UnifiedRecord) -> Option<f64> {
    let value = eval_projection_value(arg, source)?;
    value_as_number(&value).map(NumOperand::as_f64)
}
