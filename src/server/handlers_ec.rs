use super::transport::{json_response, HttpResponse};
use crate::json::{from_slice as json_from_slice, Map, Value as JsonValue};
use crate::runtime::RedDBRuntime;

pub(crate) fn handle_ec_mutate(
    runtime: &RedDBRuntime,
    collection: &str,
    field: &str,
    operation: &str,
    body: Vec<u8>,
) -> HttpResponse {
    let body: JsonValue = json_from_slice(&body).unwrap_or(JsonValue::Null);

    let id = match &body {
        JsonValue::Object(ref obj) => obj
            .get("id")
            .and_then(|v| match v {
                JsonValue::Number(n) => Some(*n as u64),
                JsonValue::String(s) => s.parse::<u64>().ok(),
                _ => None,
            })
            .unwrap_or(0),
        _ => 0,
    };

    let value = match &body {
        JsonValue::Object(ref obj) => obj
            .get("value")
            .and_then(|v| match v {
                JsonValue::Number(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0.0),
        _ => 0.0,
    };

    let source = match &body {
        JsonValue::Object(ref obj) => obj.get("source").and_then(|v| match v {
            JsonValue::String(s) => Some(s.clone()),
            _ => None,
        }),
        _ => None,
    };

    let result = match operation {
        "add" => runtime.ec_add(collection, field, id, value, source.as_deref()),
        "sub" => runtime.ec_sub(collection, field, id, value, source.as_deref()),
        "set" => runtime.ec_set(collection, field, id, value, source.as_deref()),
        _ => Err(crate::RedDBError::Query("unknown EC operation".into())),
    };

    match result {
        Ok(tx_id) => {
            let mut obj = Map::new();
            obj.insert("ok".to_string(), JsonValue::Bool(true));
            obj.insert(
                "transaction_id".to_string(),
                JsonValue::Number(tx_id as f64),
            );
            json_response(200, JsonValue::Object(obj))
        }
        Err(e) => {
            let mut obj = Map::new();
            obj.insert("ok".to_string(), JsonValue::Bool(false));
            obj.insert("error".to_string(), JsonValue::String(e.to_string()));
            json_response(400, JsonValue::Object(obj))
        }
    }
}

pub(crate) fn handle_ec_consolidate(
    runtime: &RedDBRuntime,
    collection: &str,
    field: &str,
) -> HttpResponse {
    crate::server::transport::run_use_case(
        || runtime.ec_consolidate(collection, field, None),
        |result| {
            let mut obj = Map::new();
            obj.insert("ok".to_string(), JsonValue::Bool(true));
            obj.insert(
                "records_consolidated".to_string(),
                JsonValue::Number(result.records_consolidated as f64),
            );
            obj.insert(
                "transactions_applied".to_string(),
                JsonValue::Number(result.transactions_applied as f64),
            );
            obj.insert(
                "errors".to_string(),
                JsonValue::Number(result.errors as f64),
            );
            JsonValue::Object(obj)
        },
    )
}

pub(crate) fn handle_ec_status(
    runtime: &RedDBRuntime,
    collection: &str,
    field: &str,
    query: &std::collections::BTreeMap<String, String>,
) -> HttpResponse {
    let id = query
        .get("id")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);

    let status = runtime.ec_status(collection, field, id);
    let mut obj = Map::new();
    obj.insert("ok".to_string(), JsonValue::Bool(true));
    obj.insert(
        "consolidated".to_string(),
        JsonValue::Number(status.consolidated),
    );
    obj.insert(
        "pending_value".to_string(),
        JsonValue::Number(status.pending_value),
    );
    obj.insert(
        "pending_transactions".to_string(),
        JsonValue::Number(status.pending_transactions as f64),
    );
    obj.insert(
        "has_pending_set".to_string(),
        JsonValue::Bool(status.has_pending_set),
    );
    obj.insert("field".to_string(), JsonValue::String(status.field));
    obj.insert(
        "collection".to_string(),
        JsonValue::String(status.collection),
    );
    obj.insert("reducer".to_string(), JsonValue::String(status.reducer));
    obj.insert("mode".to_string(), JsonValue::String(status.mode));
    json_response(200, JsonValue::Object(obj))
}

pub(crate) fn handle_ec_global_status(runtime: &RedDBRuntime) -> HttpResponse {
    let statuses = runtime.ec_global_status();
    let fields: Vec<JsonValue> = statuses
        .into_iter()
        .map(|s| {
            let mut obj = Map::new();
            obj.insert("collection".to_string(), JsonValue::String(s.collection));
            obj.insert("field".to_string(), JsonValue::String(s.field));
            obj.insert("reducer".to_string(), JsonValue::String(s.reducer));
            obj.insert("mode".to_string(), JsonValue::String(s.mode));
            obj.insert(
                "pending_transactions".to_string(),
                JsonValue::Number(s.pending_transactions as f64),
            );
            JsonValue::Object(obj)
        })
        .collect();

    let mut obj = Map::new();
    obj.insert("ok".to_string(), JsonValue::Bool(true));
    obj.insert(
        "total_fields".to_string(),
        JsonValue::Number(fields.len() as f64),
    );
    obj.insert("fields".to_string(), JsonValue::Array(fields));
    json_response(200, JsonValue::Object(obj))
}
