//! DML insert-support and value-conversion helpers extracted from `impl_dml`.
//!
//! Behaviour-preserving move (issue #1632): INSERT metadata / WITH clauses /
//! column-value extraction, timeseries insert helpers, and metadata/value
//! conversion. Names and behaviour are unchanged from `impl_dml`; the only
//! adjustment is `pub(super)` visibility so the sibling `impl_dml` module can
//! keep calling these helpers by their bare names.

use super::*;

use crate::application::entity::metadata_from_json;
use crate::application::ttl_payload::has_internal_ttl_metadata;
use crate::storage::unified::MetadataValue;
use std::collections::HashMap;

// Symbols that remain in `impl_dml` but are still referenced by these helpers
// (not part of this extraction slice, issue #1632).
use super::impl_dml::{
    canonicalize_sql_ttl_metadata, resolve_sql_ttl_metadata_key, TREE_CHILD_EDGE_LABEL,
    TREE_METADATA_PREFIX,
};

pub(super) fn split_insert_metadata(
    runtime: &RedDBRuntime,
    columns: &[String],
    values: &[Value],
) -> RedDBResult<(Vec<(String, Value)>, Vec<(String, MetadataValue)>)> {
    let mut fields = Vec::new();
    let mut metadata = Vec::new();

    for (column, value) in columns.iter().zip(values.iter()) {
        // Still support legacy _ttl columns for backward compat
        if let Some(metadata_key) = resolve_sql_ttl_metadata_key(column) {
            let raw_value = sql_literal_to_metadata_value(metadata_key, value)?;
            let (canonical_key, canonical_value) =
                canonicalize_sql_ttl_metadata(metadata_key, raw_value);
            metadata.push((canonical_key.to_string(), canonical_value));
            continue;
        }
        fields.push((
            column.clone(),
            runtime.resolve_crypto_sentinel(value.clone())?,
        ));
    }

    Ok((fields, metadata))
}

/// Merge structured WITH TTL, WITH EXPIRES AT, and WITH METADATA clauses into metadata entries.
pub(super) fn merge_with_clauses(
    metadata: &mut Vec<(String, MetadataValue)>,
    ttl_ms: Option<u64>,
    expires_at_ms: Option<u64>,
    with_metadata: &[(String, Value)],
) {
    if let Some(ms) = ttl_ms {
        metadata.push((
            "_ttl_ms".to_string(),
            if ms <= i64::MAX as u64 {
                MetadataValue::Int(ms as i64)
            } else {
                MetadataValue::Timestamp(ms)
            },
        ));
    }
    if let Some(ms) = expires_at_ms {
        metadata.push(("_expires_at".to_string(), MetadataValue::Timestamp(ms)));
    }
    for (key, value) in with_metadata {
        let meta_value = match value {
            Value::Text(s) => MetadataValue::String(s.to_string()),
            Value::Integer(n) => MetadataValue::Int(*n),
            Value::Float(n) => MetadataValue::Float(*n),
            Value::Boolean(b) => MetadataValue::Bool(*b),
            _ => MetadataValue::String(value.to_string()),
        };
        metadata.push((key.clone(), meta_value));
    }
}

pub(super) fn merge_vector_metadata_column(
    metadata: &mut Vec<(String, MetadataValue)>,
    columns: &[String],
    values: &[Value],
) -> RedDBResult<()> {
    let Some(value) = columns
        .iter()
        .position(|column| column.eq_ignore_ascii_case("metadata"))
        .map(|index| &values[index])
    else {
        return Ok(());
    };
    let json = match value {
        Value::Null => return Ok(()),
        Value::Json(bytes) => crate::json::from_slice(bytes).map_err(|err| {
            RedDBError::Query(format!("column 'metadata' invalid JSON object: {err}"))
        })?,
        Value::Text(text) => crate::json::from_str(text).map_err(|err| {
            RedDBError::Query(format!("column 'metadata' invalid JSON object: {err}"))
        })?,
        other => {
            return Err(RedDBError::Query(format!(
                "column 'metadata' expected JSON object, got {other:?}"
            )))
        }
    };
    let parsed = metadata_from_json(&json)?;
    for (key, value) in parsed.iter() {
        metadata.push((key.clone(), value.clone()));
    }
    Ok(())
}

pub(super) fn apply_collection_default_ttl_metadata(
    runtime: &RedDBRuntime,
    collection: &str,
    metadata: &mut Vec<(String, MetadataValue)>,
) {
    if has_internal_ttl_metadata(metadata) {
        return;
    }

    let Some(default_ttl_ms) = runtime.db().collection_default_ttl_ms(collection) else {
        return;
    };

    metadata.push((
        "_ttl_ms".to_string(),
        if default_ttl_ms <= i64::MAX as u64 {
            MetadataValue::Int(default_ttl_ms as i64)
        } else {
            MetadataValue::Timestamp(default_ttl_ms)
        },
    ));
}

pub(super) fn ensure_non_tree_reserved_metadata_entries(
    metadata: &[(String, MetadataValue)],
) -> RedDBResult<()> {
    for (key, _) in metadata {
        ensure_non_tree_reserved_metadata_key(key)?;
    }
    Ok(())
}

pub(super) fn ensure_non_tree_reserved_metadata_key(key: &str) -> RedDBResult<()> {
    if key.starts_with(TREE_METADATA_PREFIX) {
        return Err(RedDBError::Query(format!(
            "metadata key '{}' is reserved for managed trees",
            key
        )));
    }
    Ok(())
}

pub(super) fn ensure_non_tree_structural_edge_label(label: &str) -> RedDBResult<()> {
    if label.eq_ignore_ascii_case(TREE_CHILD_EDGE_LABEL) {
        return Err(RedDBError::Query(format!(
            "edge label '{}' is reserved for managed trees",
            TREE_CHILD_EDGE_LABEL
        )));
    }
    Ok(())
}

pub(super) fn pairwise_columns_values(pairs: &[(String, Value)]) -> (Vec<String>, Vec<Value>) {
    let mut columns = Vec::with_capacity(pairs.len());
    let mut values = Vec::with_capacity(pairs.len());

    for (column, value) in pairs {
        columns.push(column.clone());
        values.push(value.clone());
    }

    (columns, values)
}

/// Find a required column value and return it as-is.
pub(super) fn find_column_value(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<Value> {
    for (i, col) in columns.iter().enumerate() {
        if col.eq_ignore_ascii_case(name) {
            return Ok(values[i].clone());
        }
    }
    Err(RedDBError::Query(format!(
        "required column '{name}' not found in INSERT"
    )))
}

/// Find a required column value and coerce to String.
pub(super) fn find_column_value_string(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<String> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Text(s) => Ok(s.to_string()),
        Value::Integer(n) => Ok(n.to_string()),
        Value::Float(n) => Ok(n.to_string()),
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected text, got {other:?}"
        ))),
    }
}

pub(super) fn find_document_body_json(
    columns: &[String],
    values: &[Value],
) -> RedDBResult<crate::json::Value> {
    let val = find_column_value(columns, values, "body")?;
    match val {
        Value::Json(bytes) | Value::Blob(bytes) => {
            if let Some(body) = crate::document_body::decode_container_to_json(&bytes) {
                Ok(body)
            } else {
                crate::json::from_slice(&bytes)
                    .map_err(|err| RedDBError::Query(format!("invalid JSON body: {err}")))
            }
        }
        // A JSON-position array literal parses losslessly into `Value::Array`
        // (issue #1708); resolve it to a JSON array here.
        Value::Array(_) => Ok(crate::presentation::entity_json::storage_value_to_json(
            &val,
        )),
        // ADR 0067 (#1721): a document body is an inline strict-JSON literal.
        // A runtime string bound through a parameter (`DOCUMENT VALUES ($1)`)
        // is no longer silently coerced — wrap it with `JSON_PARSE(<expr>)`.
        Value::Text(_) => Err(RedDBError::Query(
            "document body must be an inline strict-JSON literal \
             (e.g. `DOCUMENT VALUES ({\"level\": \"info\"})`); wrap a runtime \
             string with `JSON_PARSE(<expr>)` (ADR 0067)"
                .to_string(),
        )),
        other => Err(RedDBError::Query(format!(
            "document body must be an inline strict-JSON literal or `JSON_PARSE(<expr>)`, \
             got {other:?} (ADR 0067)"
        ))),
    }
}

pub(super) fn find_column_value_f64(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<f64> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Float(n) => Ok(n),
        Value::Integer(n) => Ok(n as f64),
        Value::UnsignedInteger(n) => Ok(n as f64),
        Value::Text(s) => s
            .parse::<f64>()
            .map_err(|_| RedDBError::Query(format!("column '{name}' expected number, got '{s}'"))),
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected number, got {other:?}"
        ))),
    }
}

/// Find an optional column value as String.
pub(super) fn find_column_value_opt_string(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> Option<String> {
    for (i, col) in columns.iter().enumerate() {
        if col.eq_ignore_ascii_case(name) {
            return match &values[i] {
                Value::Null => None,
                Value::Text(s) => Some(s.to_string()),
                Value::Integer(n) => Some(n.to_string()),
                Value::Float(n) => Some(n.to_string()),
                _ => None,
            };
        }
    }
    None
}

/// Resolve an EDGE endpoint (`from`/`to`) to a numeric entity id.
///
/// Accepts integer literals, decimal strings, and node labels resolved via
/// the per-collection graph label index (same source of truth that
/// `GRAPH NEIGHBORHOOD` / `GRAPH TRAVERSE` use at query time). Ambiguous
/// labels error so callers can fall back to the numeric id form.
pub(super) fn resolve_edge_endpoint(
    store: &crate::storage::unified::UnifiedStore,
    collection: &str,
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<u64> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Integer(n) => Ok(n as u64),
        Value::UnsignedInteger(n) => Ok(n),
        Value::Text(s) => {
            if let Ok(n) = s.parse::<u64>() {
                return Ok(n);
            }
            let matches = store.lookup_graph_nodes_by_label_in(collection, &s);
            match matches.len() {
                0 => Err(RedDBError::Query(format!(
                    "column '{name}': no graph node with label '{s}' in collection '{collection}'"
                ))),
                1 => Ok(matches[0].raw()),
                n => Err(RedDBError::Query(format!(
                    "column '{name}': ambiguous label '{s}' matches {n} nodes in collection '{collection}'; use the numeric id"
                ))),
            }
        }
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected integer or node label, got {other:?}"
        ))),
    }
}

pub(super) fn resolve_edge_endpoint_any(
    store: &crate::storage::unified::UnifiedStore,
    collection: &str,
    columns: &[String],
    values: &[Value],
    names: &[&str],
) -> RedDBResult<u64> {
    for name in names {
        if columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case(name))
        {
            return resolve_edge_endpoint(store, collection, columns, values, name);
        }
    }

    Err(RedDBError::Query(format!(
        "required column '{}' not found in INSERT",
        names.first().copied().unwrap_or("from_rid")
    )))
}

/// Find a required column value and coerce to u64.
pub(super) fn find_column_value_u64(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<u64> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Integer(n) => Ok(n as u64),
        Value::UnsignedInteger(n) => Ok(n),
        Value::Text(s) => s
            .parse::<u64>()
            .map_err(|_| RedDBError::Query(format!("column '{name}' expected integer, got '{s}'"))),
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected integer, got {other:?}"
        ))),
    }
}

/// Find an optional column value as f32.
pub(super) fn find_column_value_f32_opt(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> Option<f32> {
    for (i, col) in columns.iter().enumerate() {
        if col.eq_ignore_ascii_case(name) {
            return match &values[i] {
                Value::Float(n) => Some(*n as f32),
                Value::Integer(n) => Some(*n as f32),
                Value::Null => None,
                _ => None,
            };
        }
    }
    None
}

/// Find a required column value and coerce to Vec<f32> (from Value::Vector).
///
/// Array literals now parse losslessly into `Value::Array` (issue #1708), so a
/// vector-typed column position resolves that array to `Vec<f32>` here rather
/// than the parser committing to an f32 vector before the target is known.
pub(super) fn find_column_value_vec_f32(
    columns: &[String],
    values: &[Value],
    name: &str,
) -> RedDBResult<Vec<f32>> {
    let val = find_column_value(columns, values, name)?;
    match val {
        Value::Vector(v) => Ok(v),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::Float(f) => Ok(*f as f32),
                Value::Integer(n) | Value::BigInt(n) => Ok(*n as f32),
                Value::UnsignedInteger(n) => Ok(*n as f32),
                other => Err(RedDBError::Query(format!(
                    "column '{name}' vector array accepts only numeric values, got {other:?}"
                ))),
            })
            .collect(),
        Value::Json(bytes) => {
            // Try to parse as JSON array of numbers
            let s = std::str::from_utf8(&bytes).map_err(|_| {
                RedDBError::Query(format!("column '{name}' contains invalid UTF-8"))
            })?;
            let arr: Vec<f32> = crate::json::from_str(s).map_err(|e| {
                RedDBError::Query(format!("column '{name}' invalid vector JSON: {e}"))
            })?;
            Ok(arr)
        }
        other => Err(RedDBError::Query(format!(
            "column '{name}' expected vector, got {other:?}"
        ))),
    }
}

pub(super) fn find_column_value_vec_f32_any(
    columns: &[String],
    values: &[Value],
    names: &[&str],
) -> RedDBResult<Vec<f32>> {
    for name in names {
        if columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case(name))
        {
            return find_column_value_vec_f32(columns, values, name);
        }
    }
    Err(RedDBError::Query(format!(
        "required vector column '{}' not found in INSERT",
        names.join("' or '")
    )))
}

/// Extract remaining properties (all columns not in the exclusion list).
pub(super) fn extract_remaining_properties(
    columns: &[String],
    values: &[Value],
    exclude: &[&str],
) -> Vec<(String, Value)> {
    columns
        .iter()
        .zip(values.iter())
        .filter(|(col, _)| !exclude.iter().any(|e| col.eq_ignore_ascii_case(e)))
        .map(|(col, val)| (col.clone(), val.clone()))
        .collect()
}

pub(super) fn validate_timeseries_insert_columns(columns: &[String]) -> RedDBResult<()> {
    let mut invalid = Vec::new();
    for column in columns {
        if !is_timeseries_insert_column(column) && resolve_sql_ttl_metadata_key(column).is_none() {
            invalid.push(column.clone());
        }
    }

    if invalid.is_empty() {
        Ok(())
    } else {
        Err(RedDBError::Query(format!(
            "timeseries INSERT only accepts metric, value, tags, timestamp, timestamp_ns, or time columns; got {}",
            invalid.join(", ")
        )))
    }
}

pub(super) fn is_timeseries_insert_column(column: &str) -> bool {
    matches!(
        column.to_ascii_lowercase().as_str(),
        "metric"
            | "value"
            | "tags"
            | "timestamp"
            | "timestamp_ns"
            | "time"
            // Analytics-event extension (#577): an analytics row carries
            // an `event_name` + JSON `payload`. The payload is validated
            // against the AnalyticsSchemaRegistry inside
            // `insert_timeseries_point` before the row lands.
            | "event_name"
            | "payload"
    )
}

pub(super) fn find_timeseries_timestamp_ns(
    columns: &[String],
    values: &[Value],
) -> RedDBResult<Option<u64>> {
    let mut found = None;

    for alias in ["timestamp_ns", "timestamp", "time"] {
        for (index, column) in columns.iter().enumerate() {
            if !column.eq_ignore_ascii_case(alias) {
                continue;
            }

            if found.is_some() {
                return Err(RedDBError::Query(
                    "timeseries INSERT accepts only one timestamp column".to_string(),
                ));
            }

            found = Some(coerce_value_to_non_negative_u64(&values[index], alias)?);
        }
    }

    Ok(found)
}

pub(super) fn find_timeseries_tags(
    columns: &[String],
    values: &[Value],
) -> RedDBResult<std::collections::HashMap<String, String>> {
    for (index, column) in columns.iter().enumerate() {
        if column.eq_ignore_ascii_case("tags") {
            return parse_timeseries_tags(&values[index]);
        }
    }
    Ok(std::collections::HashMap::new())
}

pub(super) fn parse_timeseries_tags(
    value: &Value,
) -> RedDBResult<std::collections::HashMap<String, String>> {
    match value {
        Value::Null => Ok(std::collections::HashMap::new()),
        Value::Json(bytes) => parse_timeseries_tags_json(bytes),
        Value::Text(text) => parse_timeseries_tags_json(text.as_bytes()),
        other => Err(RedDBError::Query(format!(
            "timeseries tags must be a JSON object or JSON text, got {other:?}"
        ))),
    }
}

pub(super) fn parse_timeseries_tags_json(
    bytes: &[u8],
) -> RedDBResult<std::collections::HashMap<String, String>> {
    let json: crate::json::Value = crate::json::from_slice(bytes)
        .map_err(|err| RedDBError::Query(format!("timeseries tags must be valid JSON: {err}")))?;

    let object = match json {
        crate::json::Value::Object(object) => object,
        other => {
            return Err(RedDBError::Query(format!(
                "timeseries tags must be a JSON object, got {other:?}"
            )))
        }
    };

    let mut tags = std::collections::HashMap::with_capacity(object.len());
    for (key, value) in object {
        tags.insert(key, json_tag_value_to_string(&value));
    }
    Ok(tags)
}

/// Encode a tag value for storage so the original JSON type can be
/// recovered on read (issue #543).
///
/// Time-series tags are stored as `HashMap<String, String>` on the
/// physical record (see [`crate::storage::TimeSeriesData`]) so that
/// the segment codec, WAL and gRPC mirrors don't need a new value
/// variant. To preserve the original JSON type across that
/// string-only channel we prepend the
/// [`crate::runtime::query_exec::TIMESERIES_TAG_JSON_PREFIX`] marker
/// and serialize the value as compact JSON text. The read paths
/// (`timeseries_tags_json_value` / `timeseries_tags_value`) detect
/// the marker, parse the suffix, and recover a real JSON value.
/// Tags written through other channels (Prometheus remote write,
/// metrics handlers, legacy on-disk data) lack the marker and are
/// returned as `JsonValue::String(raw)` exactly as before.
pub(super) fn json_tag_value_to_string(value: &crate::json::Value) -> String {
    let mut buf = String::with_capacity(value.to_string_compact().len() + 1);
    buf.push(crate::runtime::query_exec::TIMESERIES_TAG_JSON_PREFIX);
    buf.push_str(&value.to_string_compact());
    buf
}

pub(super) fn coerce_value_to_non_negative_u64(value: &Value, column: &str) -> RedDBResult<u64> {
    match value {
        Value::UnsignedInteger(value) => Ok(*value),
        Value::Integer(value) if *value >= 0 => Ok(*value as u64),
        Value::Float(value) if *value >= 0.0 => Ok(*value as u64),
        Value::Text(value) => value.parse::<u64>().map_err(|_| {
            RedDBError::Query(format!(
                "column '{column}' expected a non-negative integer timestamp, got '{value}'"
            ))
        }),
        other => Err(RedDBError::Query(format!(
            "column '{column}' expected a non-negative integer timestamp, got {other:?}"
        ))),
    }
}

pub(super) fn current_unix_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u128::from(u64::MAX)) as u64
}

pub(super) fn metadata_value_to_json(value: &MetadataValue) -> crate::json::Value {
    use crate::json::{Map, Value as JV};
    match value {
        MetadataValue::Null => JV::Null,
        MetadataValue::Bool(value) => JV::Bool(*value),
        MetadataValue::Int(value) => JV::Number(*value as f64),
        MetadataValue::Float(value) => JV::Number(*value),
        MetadataValue::String(value) => JV::String(value.clone()),
        MetadataValue::Bytes(value) => JV::Array(
            value
                .iter()
                .map(|value| JV::Number(*value as f64))
                .collect(),
        ),
        MetadataValue::Timestamp(value) => JV::Number(*value as f64),
        MetadataValue::Array(values) => {
            JV::Array(values.iter().map(metadata_value_to_json).collect())
        }
        MetadataValue::Object(object) => {
            let entries = object
                .iter()
                .map(|(key, value)| (key.clone(), metadata_value_to_json(value)))
                .collect();
            JV::Object(entries)
        }
        MetadataValue::Geo { lat, lon } => {
            let mut object = Map::new();
            object.insert("lat".to_string(), JV::Number(*lat));
            object.insert("lon".to_string(), JV::Number(*lon));
            JV::Object(object)
        }
        MetadataValue::Reference(target) => {
            let mut object = Map::new();
            object.insert(
                "collection".to_string(),
                JV::String(target.collection().to_string()),
            );
            object.insert(
                "entity_id".to_string(),
                JV::Number(target.entity_id().raw() as f64),
            );
            JV::Object(object)
        }
        MetadataValue::References(values) => {
            let refs = values
                .iter()
                .map(|target| {
                    let mut object = Map::new();
                    object.insert(
                        "collection".to_string(),
                        JV::String(target.collection().to_string()),
                    );
                    object.insert(
                        "entity_id".to_string(),
                        JV::Number(target.entity_id().raw() as f64),
                    );
                    JV::Object(object)
                })
                .collect();
            JV::Array(refs)
        }
    }
}

pub(super) fn storage_value_to_metadata_value(value: &Value) -> MetadataValue {
    match value {
        Value::Null => MetadataValue::Null,
        Value::Boolean(value) => MetadataValue::Bool(*value),
        Value::Integer(value) => MetadataValue::Int(*value),
        Value::UnsignedInteger(value) => metadata_u64_to_value(*value),
        Value::Float(value) => MetadataValue::Float(*value),
        Value::Text(value) => MetadataValue::String(value.to_string()),
        Value::Blob(value) => MetadataValue::Bytes(value.clone()),
        Value::Timestamp(value) => {
            if *value >= 0 {
                metadata_u64_to_value(*value as u64)
            } else {
                MetadataValue::Int(*value)
            }
        }
        Value::TimestampMs(value) => {
            if *value >= 0 {
                metadata_u64_to_value(*value as u64)
            } else {
                MetadataValue::Int(*value)
            }
        }
        Value::Json(value) => MetadataValue::String(String::from_utf8_lossy(value).into_owned()),
        Value::Uuid(value) => MetadataValue::String(format!("{value:?}")),
        Value::Date(value) => MetadataValue::String(value.to_string()),
        Value::Time(value) => MetadataValue::String(value.to_string()),
        Value::Decimal(value) => MetadataValue::String(value.to_string()),
        Value::Ipv4(value) => MetadataValue::String(format!(
            "{}.{}.{}.{}",
            (value >> 24) & 0xFF,
            (value >> 16) & 0xFF,
            (value >> 8) & 0xFF,
            value & 0xFF
        )),
        Value::Port(value) => MetadataValue::Int(i64::from(*value)),
        Value::Latitude(value) => MetadataValue::Float(*value as f64 / 1_000_000.0),
        Value::Longitude(value) => MetadataValue::Float(*value as f64 / 1_000_000.0),
        Value::GeoPoint(lat, lon) => MetadataValue::Geo {
            lat: *lat as f64 / 1_000_000.0,
            lon: *lon as f64 / 1_000_000.0,
        },
        Value::BigInt(value) => MetadataValue::String(value.to_string()),
        Value::TableRef(value) => MetadataValue::String(value.clone()),
        Value::PageRef(value) => MetadataValue::Int(*value as i64),
        Value::Password(value) => MetadataValue::String(value.clone()),
        Value::Array(values) => {
            MetadataValue::Array(values.iter().map(storage_value_to_metadata_value).collect())
        }
        _ => MetadataValue::String(value.to_string()),
    }
}

pub(super) fn sql_literal_to_metadata_value(
    field: &str,
    value: &Value,
) -> RedDBResult<MetadataValue> {
    match value {
        Value::Null => Ok(MetadataValue::Null),
        Value::Integer(value) if *value >= 0 => Ok(metadata_u64_to_value(*value as u64)),
        Value::Integer(_) => Err(RedDBError::Query(format!(
            "column '{field}' must be non-negative for TTL metadata"
        ))),
        Value::UnsignedInteger(value) => Ok(metadata_u64_to_value(*value)),
        Value::Float(value) if value.is_finite() => {
            if value.fract().abs() >= f64::EPSILON {
                return Err(RedDBError::Query(format!(
                    "column '{field}' must be an integer (TTL metadata must be an integer)"
                )));
            }
            if *value < 0.0 {
                return Err(RedDBError::Query(format!(
                    "column '{field}' must be non-negative for TTL metadata"
                )));
            }
            if *value > u64::MAX as f64 {
                return Err(RedDBError::Query(format!(
                    "column '{field}' value is too large"
                )));
            }
            Ok(metadata_u64_to_value(*value as u64))
        }
        Value::Float(_) => Err(RedDBError::Query(format!(
            "column '{field}' must be a finite number"
        ))),
        Value::Text(value) => {
            let value = value.trim();
            if let Ok(value) = value.parse::<u64>() {
                Ok(metadata_u64_to_value(value))
            } else if let Ok(value) = value.parse::<i64>() {
                if value < 0 {
                    return Err(RedDBError::Query(format!(
                        "column '{field}' must be non-negative for TTL metadata"
                    )));
                }
                Ok(metadata_u64_to_value(value as u64))
            } else if let Ok(value) = value.parse::<f64>() {
                if !value.is_finite() {
                    return Err(RedDBError::Query(format!(
                        "column '{field}' must be a finite number"
                    )));
                }
                if value.fract().abs() >= f64::EPSILON {
                    return Err(RedDBError::Query(format!(
                        "column '{field}' must be an integer (TTL metadata must be an integer)"
                    )));
                }
                if value < 0.0 {
                    return Err(RedDBError::Query(format!(
                        "column '{field}' must be non-negative for TTL metadata"
                    )));
                }
                if value > u64::MAX as f64 {
                    return Err(RedDBError::Query(format!(
                        "column '{field}' value is too large"
                    )));
                }
                Ok(metadata_u64_to_value(value as u64))
            } else {
                Err(RedDBError::Query(format!(
                    "column '{field}' expects a numeric value for TTL metadata"
                )))
            }
        }
        _ => Err(RedDBError::Query(format!(
            "column '{field}' expects a numeric value for TTL metadata"
        ))),
    }
}

pub(super) fn metadata_u64_to_value(value: u64) -> MetadataValue {
    if value <= i64::MAX as u64 {
        MetadataValue::Int(value as i64)
    } else {
        MetadataValue::Timestamp(value)
    }
}

/// Phase 2 PG parity: inspect a column value and return `true` when
/// the dotted `tail` path is already present under it. Used by the
/// tenant auto-fill so rows that already carry an explicit value
/// (bulk import, admin insert on behalf of a tenant) are not
/// double-stamped with the session's current_tenant().
pub(super) fn dotted_tail_already_set(value: &Value, tail: &str) -> bool {
    let json = match value {
        Value::Null => return false,
        Value::Json(bytes) | Value::Blob(bytes) => {
            match crate::json::from_slice::<crate::json::Value>(bytes) {
                Ok(v) => v,
                Err(_) => return false,
            }
        }
        Value::Text(s) => {
            let trimmed = s.trim_start();
            if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
                return false;
            }
            match crate::json::from_str::<crate::json::Value>(s) {
                Ok(v) => v,
                Err(_) => return false,
            }
        }
        _ => return false,
    };
    let mut cursor = &json;
    for seg in tail.split('.') {
        match cursor {
            crate::json::Value::Object(map) => match map.iter().find(|(k, _)| *k == seg) {
                Some((_, v)) => cursor = v,
                None => return false,
            },
            _ => return false,
        }
    }
    !matches!(cursor, crate::json::Value::Null)
}

/// Phase 2 PG parity: take a column value (possibly Null / Text /
/// Json) and return a `Value::Json` with the dotted `tail` path set
/// to `tenant_id`. Preserves every pre-existing key.
///
/// Accepts:
/// * `Value::Null`  → fresh `{tail: tenant_id}` object
/// * `Value::Json(bytes)` → parse, navigate / create path, re-serialize
/// * `Value::text(s)` if `s` is valid JSON → same as Json
/// * anything else → error (user supplied a scalar where we need
///   a JSON container)
pub(super) fn merge_dotted_tenant(
    current: Value,
    tail: &str,
    tenant_id: &str,
) -> RedDBResult<Value> {
    let mut root = match current {
        Value::Null => crate::json::Value::Object(Default::default()),
        Value::Json(bytes) | Value::Blob(bytes) => {
            crate::json::from_slice(&bytes).map_err(|err| {
                RedDBError::Query(format!(
                    "tenant auto-fill: root column is not valid JSON ({err})"
                ))
            })?
        }
        Value::Text(s) => {
            if s.trim().is_empty() {
                crate::json::Value::Object(Default::default())
            } else {
                crate::json::from_str::<crate::json::Value>(&s).map_err(|err| {
                    RedDBError::Query(format!(
                        "tenant auto-fill: text root is not valid JSON ({err})"
                    ))
                })?
            }
        }
        other => {
            return Err(RedDBError::Query(format!(
                "tenant auto-fill: root column must be JSON / NULL, got {other:?}"
            )));
        }
    };

    // Navigate path segments, creating intermediate objects on demand.
    let segments: Vec<&str> = tail.split('.').collect();
    let mut cursor: &mut crate::json::Value = &mut root;
    for (i, seg) in segments.iter().enumerate() {
        let is_last = i + 1 == segments.len();
        let map = match cursor {
            crate::json::Value::Object(m) => m,
            _ => {
                return Err(RedDBError::Query(format!(
                    "tenant auto-fill: segment '{seg}' is not inside an object"
                )));
            }
        };
        if is_last {
            map.insert(
                seg.to_string(),
                crate::json::Value::String(tenant_id.to_string()),
            );
            break;
        }
        cursor = map
            .entry(seg.to_string())
            .or_insert_with(|| crate::json::Value::Object(Default::default()));
    }

    let bytes = crate::json::to_vec(&root).map_err(|err| {
        RedDBError::Query(format!(
            "tenant auto-fill: failed to re-serialize JSON ({err})"
        ))
    })?;
    Ok(Value::Json(bytes))
}
