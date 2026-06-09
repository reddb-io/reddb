//! RedWire data-plane JSON payload contracts.
//!
//! The server owns execution. This module owns only the wire-visible
//! request/reply envelopes carried by BulkInsert, Get, Delete, BulkOk,
//! and DeleteOk frames.

use serde_json::{Map as JsonMap, Value as JsonValue};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct InsertDispatchPayload {
    pub collection: String,
    pub payload: Option<JsonValue>,
    pub payloads: Option<Vec<JsonValue>>,
    pub idempotency_key: Option<String>,
    pub batch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPayload {
    pub collection: String,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkOkPayload {
    pub affected: u64,
    pub rids: Vec<String>,
    pub ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationPayloadError {
    InvalidJson { op: &'static str, message: String },
    ExpectedObject { op: &'static str },
    MissingCollection { op: &'static str },
    MissingId { op: &'static str },
    TruncatedBulkOkCount,
}

impl fmt::Display for OperationPayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson { op, message } => write!(f, "{op}: invalid JSON: {message}"),
            Self::ExpectedObject { op } => write!(f, "{op}: payload must be a JSON object"),
            Self::MissingCollection { op } => {
                write!(f, "{op}: missing 'collection' string")
            }
            Self::MissingId { op } => write!(f, "{op}: missing 'id' string"),
            Self::TruncatedBulkOkCount => write!(f, "BulkOk truncated: expected 8-byte count"),
        }
    }
}

impl std::error::Error for OperationPayloadError {}

pub fn encode_insert_payload(collection: &str, payload: JsonValue) -> Vec<u8> {
    let mut obj = JsonMap::new();
    obj.insert(
        "collection".into(),
        JsonValue::String(collection.to_string()),
    );
    obj.insert("payload".into(), payload);
    serde_json::to_vec(&JsonValue::Object(obj)).expect("insert payload JSON is serializable")
}

pub fn encode_bulk_insert_payload(collection: &str, payloads: Vec<JsonValue>) -> Vec<u8> {
    let mut obj = JsonMap::new();
    obj.insert(
        "collection".into(),
        JsonValue::String(collection.to_string()),
    );
    obj.insert("payloads".into(), JsonValue::Array(payloads));
    serde_json::to_vec(&JsonValue::Object(obj)).expect("bulk insert payload JSON is serializable")
}

pub fn decode_insert_dispatch_payload(
    bytes: &[u8],
) -> Result<InsertDispatchPayload, OperationPayloadError> {
    let obj = object_from_payload("Insert", bytes)?;
    let collection = required_collection("Insert", &obj)?;
    let payload = obj.get("payload").cloned();
    let payloads = obj
        .get("payloads")
        .and_then(JsonValue::as_array)
        .map(|items| items.to_vec());
    let idempotency_key = obj
        .get("idempotency_key")
        .and_then(JsonValue::as_str)
        .map(String::from);
    let batch = obj
        .get("batch")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    Ok(InsertDispatchPayload {
        collection,
        payload,
        payloads,
        idempotency_key,
        batch,
    })
}

pub fn encode_key_payload(collection: &str, id: &str) -> Vec<u8> {
    let mut obj = JsonMap::new();
    obj.insert(
        "collection".into(),
        JsonValue::String(collection.to_string()),
    );
    obj.insert("id".into(), JsonValue::String(id.to_string()));
    serde_json::to_vec(&JsonValue::Object(obj)).expect("key payload JSON is serializable")
}

pub fn decode_get_payload(bytes: &[u8]) -> Result<KeyPayload, OperationPayloadError> {
    decode_key_payload("Get", bytes)
}

pub fn decode_delete_payload(bytes: &[u8]) -> Result<KeyPayload, OperationPayloadError> {
    decode_key_payload("Delete", bytes)
}

pub fn encode_query_result_summary_payload(statement: &str, affected: u64) -> Vec<u8> {
    let mut obj = JsonMap::new();
    obj.insert("ok".into(), JsonValue::Bool(true));
    obj.insert("statement".into(), JsonValue::String(statement.to_string()));
    obj.insert("affected".into(), JsonValue::Number(affected.into()));
    serde_json::to_vec(&JsonValue::Object(obj)).expect("query result payload JSON is serializable")
}

pub fn decode_query_result_payload(bytes: &[u8]) -> Result<JsonValue, OperationPayloadError> {
    json_value_from_payload("QueryResult", bytes)
}

pub fn encode_get_result_payload(found: bool) -> Vec<u8> {
    let mut obj = JsonMap::new();
    obj.insert("ok".into(), JsonValue::Bool(true));
    obj.insert("found".into(), JsonValue::Bool(found));
    serde_json::to_vec(&JsonValue::Object(obj)).expect("get result payload JSON is serializable")
}

pub fn decode_get_result_payload(bytes: &[u8]) -> Result<JsonValue, OperationPayloadError> {
    json_value_from_payload("GetResult", bytes)
}

pub fn encode_bulk_ok_payload(affected: u64, ids: Vec<JsonValue>) -> Vec<u8> {
    let mut obj = JsonMap::new();
    obj.insert("affected".into(), JsonValue::Number(affected.into()));
    obj.insert("ids".into(), JsonValue::Array(ids));
    serde_json::to_vec(&JsonValue::Object(obj)).expect("bulk ok payload JSON is serializable")
}

pub fn encode_bulk_ok_payload_from_json_ids_bytes(affected: u64, ids: &[u8]) -> Vec<u8> {
    let ids = match serde_json::from_slice::<JsonValue>(ids) {
        Ok(JsonValue::Array(items)) => items,
        _ => Vec::new(),
    };
    encode_bulk_ok_payload(affected, ids)
}

pub fn encode_bulk_ok_payload_from_json_id_literals<I, S>(affected: u64, ids: I) -> Vec<u8>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let ids = ids
        .into_iter()
        .map(|id| {
            serde_json::from_str::<JsonValue>(id.as_ref())
                .unwrap_or_else(|_| JsonValue::String(id.as_ref().to_string()))
        })
        .collect();
    encode_bulk_ok_payload(affected, ids)
}

pub fn decode_bulk_ok_payload(bytes: &[u8]) -> Result<BulkOkPayload, OperationPayloadError> {
    let obj = object_from_payload("BulkOk", bytes)?;
    let affected = obj.get("affected").and_then(JsonValue::as_u64).unwrap_or(0);
    let rids: Vec<String> = obj
        .get("rids")
        .or_else(|| obj.get("ids"))
        .and_then(JsonValue::as_array)
        .map(|items| items.iter().filter_map(json_id_to_string).collect())
        .unwrap_or_default();
    let ids: Vec<String> = obj
        .get("ids")
        .and_then(JsonValue::as_array)
        .map(|items| items.iter().filter_map(json_id_to_string).collect())
        .unwrap_or_else(|| rids.clone());
    Ok(BulkOkPayload {
        affected,
        rids,
        ids,
    })
}

pub fn encode_bulk_ok_count_payload(count: u64) -> Vec<u8> {
    count.to_le_bytes().to_vec()
}

pub fn decode_bulk_ok_count_payload(bytes: &[u8]) -> Result<u64, OperationPayloadError> {
    if bytes.len() < 8 {
        return Err(OperationPayloadError::TruncatedBulkOkCount);
    }
    let mut count = [0u8; 8];
    count.copy_from_slice(&bytes[..8]);
    Ok(u64::from_le_bytes(count))
}

pub fn decode_delete_ok_affected(bytes: &[u8]) -> Result<u64, OperationPayloadError> {
    let obj = object_from_payload("DeleteOk", bytes)?;
    Ok(obj.get("affected").and_then(JsonValue::as_u64).unwrap_or(0))
}

pub fn encode_delete_ok_payload(affected: u64) -> Vec<u8> {
    let mut obj = JsonMap::new();
    obj.insert("affected".into(), JsonValue::Number(affected.into()));
    serde_json::to_vec(&JsonValue::Object(obj)).expect("delete ok payload JSON is serializable")
}

fn decode_key_payload(op: &'static str, bytes: &[u8]) -> Result<KeyPayload, OperationPayloadError> {
    let obj = object_from_payload(op, bytes)?;
    let collection = required_collection(op, &obj)?;
    let id = match obj.get("id").and_then(JsonValue::as_str) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => return Err(OperationPayloadError::MissingId { op }),
    };
    Ok(KeyPayload { collection, id })
}

fn json_value_from_payload(
    op: &'static str,
    bytes: &[u8],
) -> Result<JsonValue, OperationPayloadError> {
    let value: JsonValue =
        serde_json::from_slice(bytes).map_err(|err| OperationPayloadError::InvalidJson {
            op,
            message: err.to_string(),
        })?;
    match value {
        JsonValue::Object(_) => Ok(value),
        _ => Err(OperationPayloadError::ExpectedObject { op }),
    }
}

fn object_from_payload(
    op: &'static str,
    bytes: &[u8],
) -> Result<JsonMap<String, JsonValue>, OperationPayloadError> {
    let value: JsonValue =
        serde_json::from_slice(bytes).map_err(|err| OperationPayloadError::InvalidJson {
            op,
            message: err.to_string(),
        })?;
    match value {
        JsonValue::Object(obj) => Ok(obj),
        _ => Err(OperationPayloadError::ExpectedObject { op }),
    }
}

fn required_collection(
    op: &'static str,
    obj: &JsonMap<String, JsonValue>,
) -> Result<String, OperationPayloadError> {
    match obj.get("collection").and_then(JsonValue::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(OperationPayloadError::MissingCollection { op }),
    }
}

fn json_id_to_string(value: &JsonValue) -> Option<String> {
    value
        .as_str()
        .map(String::from)
        .or_else(|| value.as_u64().map(|n| n.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_payload_round_trips_single_and_bulk_shapes() {
        let single = decode_insert_dispatch_payload(&encode_insert_payload(
            "users",
            serde_json::json!({"name":"Ada"}),
        ))
        .unwrap();
        assert_eq!(single.collection, "users");
        assert_eq!(single.payload.unwrap(), serde_json::json!({"name":"Ada"}));
        assert!(single.payloads.is_none());

        let bulk = decode_insert_dispatch_payload(&encode_bulk_insert_payload(
            "users",
            vec![serde_json::json!({"name":"Ada"})],
        ))
        .unwrap();
        assert_eq!(bulk.collection, "users");
        assert_eq!(bulk.payloads.unwrap().len(), 1);
        assert!(bulk.payload.is_none());
    }

    #[test]
    fn key_payload_round_trips_get_and_delete_contracts() {
        let bytes = encode_key_payload("users", "42");
        assert_eq!(
            decode_get_payload(&bytes).unwrap(),
            KeyPayload {
                collection: "users".into(),
                id: "42".into(),
            }
        );
        assert_eq!(
            decode_delete_payload(&bytes).unwrap(),
            KeyPayload {
                collection: "users".into(),
                id: "42".into(),
            }
        );
    }

    #[test]
    fn bulk_ok_decodes_ids_and_affected_count() {
        let payload = encode_bulk_ok_payload(2, vec![JsonValue::Number(1.into()), "2".into()]);
        assert_eq!(
            decode_bulk_ok_payload(&payload).unwrap(),
            BulkOkPayload {
                affected: 2,
                rids: vec!["1".into(), "2".into()],
                ids: vec!["1".into(), "2".into()],
            }
        );

        let payload = encode_bulk_ok_payload_from_json_ids_bytes(2, br#"[1,"2"]"#);
        assert_eq!(decode_bulk_ok_payload(&payload).unwrap().ids.len(), 2);

        let payload = encode_bulk_ok_payload_from_json_id_literals(2, ["1", r#""2""#]);
        assert_eq!(
            decode_bulk_ok_payload(&payload).unwrap().ids,
            vec!["1".to_string(), "2".to_string()]
        );
    }

    #[test]
    fn operation_reply_payloads_encode_wire_visible_json_contracts() {
        let query =
            decode_query_result_payload(&encode_query_result_summary_payload("INSERT", 3)).unwrap();
        assert_eq!(query["ok"], JsonValue::Bool(true));
        assert_eq!(query["statement"], JsonValue::String("INSERT".into()));
        assert_eq!(query["affected"], JsonValue::Number(3.into()));

        let get = decode_get_result_payload(&encode_get_result_payload(false)).unwrap();
        assert_eq!(get["ok"], JsonValue::Bool(true));
        assert_eq!(get["found"], JsonValue::Bool(false));

        assert_eq!(
            decode_delete_ok_affected(&encode_delete_ok_payload(7)).unwrap(),
            7
        );
    }

    #[test]
    fn bulk_ok_count_payload_round_trips_legacy_binary_shape() {
        let payload = encode_bulk_ok_count_payload(42);
        assert_eq!(payload.len(), 8);
        assert_eq!(decode_bulk_ok_count_payload(&payload).unwrap(), 42);
        assert_eq!(
            decode_bulk_ok_count_payload(&payload[..7])
                .unwrap_err()
                .to_string(),
            "BulkOk truncated: expected 8-byte count"
        );
    }
}
