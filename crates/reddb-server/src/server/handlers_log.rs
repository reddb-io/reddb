use super::transport::{json_response, HttpResponse};
use crate::json::{from_slice as json_from_slice, Map, Value as JsonValue};
use crate::log::id::LogId;
use crate::log::store::{LogCollection, LogCollectionConfig, LogRetention};
use crate::runtime::RedDBRuntime;
use crate::storage::schema::Value;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) fn handle_log_append(
    runtime: &RedDBRuntime,
    collection: &str,
    body: Vec<u8>,
) -> HttpResponse {
    let body: JsonValue = json_from_slice(&body).unwrap_or(JsonValue::Null);

    let fields = match &body {
        JsonValue::Object(obj) => {
            let mut map = HashMap::new();
            for (k, v) in obj {
                map.insert(k.clone(), json_to_value(v));
            }
            map
        }
        _ => return json_response(400, err("provide JSON object with log fields")),
    };

    let log = get_or_create_log(runtime, collection);
    let id = log.append(fields);

    let mut out = Map::new();
    out.insert("ok".to_string(), JsonValue::Bool(true));
    out.insert("id".to_string(), JsonValue::Number(id.raw() as f64));
    out.insert(
        "timestamp_ms".to_string(),
        JsonValue::Number(id.timestamp_ms() as f64),
    );
    json_response(200, JsonValue::Object(out))
}

pub(crate) fn handle_log_query(
    runtime: &RedDBRuntime,
    collection: &str,
    query: &std::collections::BTreeMap<String, String>,
) -> HttpResponse {
    let log = get_or_create_log(runtime, collection);

    let limit = query
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100);

    let entries = if let Some(since) = query.get("since").and_then(|v| v.parse::<u64>().ok()) {
        let from = LogId(since);
        let to = LogId(u64::MAX);
        log.range(from, to, limit)
    } else {
        log.recent(limit)
    };

    let items: Vec<JsonValue> = entries
        .iter()
        .map(|entry| {
            let mut obj = Map::new();
            obj.insert("id".to_string(), JsonValue::Number(entry.id.raw() as f64));
            obj.insert(
                "timestamp_ms".to_string(),
                JsonValue::Number(entry.id.timestamp_ms() as f64),
            );
            for (k, v) in &entry.fields {
                obj.insert(k.clone(), value_to_json(v));
            }
            JsonValue::Object(obj)
        })
        .collect();

    let mut out = Map::new();
    out.insert("ok".to_string(), JsonValue::Bool(true));
    out.insert("count".to_string(), JsonValue::Number(items.len() as f64));
    out.insert("entries".to_string(), JsonValue::Array(items));
    json_response(200, JsonValue::Object(out))
}

pub(crate) fn handle_log_retention(runtime: &RedDBRuntime, collection: &str) -> HttpResponse {
    let log = get_or_create_log(runtime, collection);
    let deleted = log.apply_retention();

    let mut out = Map::new();
    out.insert("ok".to_string(), JsonValue::Bool(true));
    out.insert("deleted".to_string(), JsonValue::Number(deleted as f64));
    out.insert("remaining".to_string(), JsonValue::Number(log.len() as f64));
    json_response(200, JsonValue::Object(out))
}

fn get_or_create_log(runtime: &RedDBRuntime, collection: &str) -> LogCollection {
    let store = runtime.db().store();
    let config = LogCollectionConfig::new(collection);
    LogCollection::new(store, config)
}

fn json_to_value(v: &JsonValue) -> Value {
    match v {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Boolean(*b),
        JsonValue::Number(n) => Value::Float(*n),
        JsonValue::String(s) => Value::text(s.clone()),
        JsonValue::Array(arr) => Value::Array(arr.iter().map(json_to_value).collect()),
        JsonValue::Object(_) => Value::text(format!("{:?}", v)),
    }
}

fn value_to_json(v: &Value) -> JsonValue {
    match v {
        Value::Null => JsonValue::Null,
        Value::Boolean(b) => JsonValue::Bool(*b),
        Value::Integer(n) => JsonValue::Number(*n as f64),
        Value::UnsignedInteger(n) => JsonValue::Number(*n as f64),
        Value::Float(f) => JsonValue::Number(*f),
        Value::Text(s) => JsonValue::String(s.to_string()),
        _ => JsonValue::String(format!("{:?}", v)),
    }
}

fn err(msg: &str) -> JsonValue {
    let mut obj = Map::<String, JsonValue>::new();
    obj.insert("ok".to_string(), JsonValue::Bool(false));
    obj.insert("error".to_string(), JsonValue::String(msg.to_string()));
    JsonValue::Object(obj)
}
