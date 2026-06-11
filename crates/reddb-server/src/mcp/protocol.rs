//! JSON-RPC protocol handling for MCP server.
//!
//! The Content-Length framed JSON-RPC 2.0 transport used by the Model Context
//! Protocol over stdio is a message-framing wire codec and lives in the
//! protocol-authority crate (`reddb_wire::jsonrpc`), per ADR 0046. This module
//! binds the server's JSON value type to that codec via [`ServerJson`] and
//! re-exports the framing functions, so call sites keep using
//! `protocol::read_payload`, `protocol::build_result_message`, etc. unchanged.

use crate::json::{Map, Value as JsonValue};
use reddb_wire::jsonrpc::{self, JsonRpcSerializer};

pub use reddb_wire::jsonrpc::{read_payload, write_message};

/// Binds `reddb-server`'s JSON value type to the wire crate's framed JSON-RPC
/// envelope builders.
///
/// Objects are backed by [`Map`] (a `BTreeMap`), which sorts keys on compact
/// serialization — that sort order is what pins the emitted field order.
pub struct ServerJson;

impl JsonRpcSerializer for ServerJson {
    type Value = JsonValue;

    fn null() -> JsonValue {
        JsonValue::Null
    }

    fn string(value: &str) -> JsonValue {
        JsonValue::String(value.to_string())
    }

    fn number(value: i64) -> JsonValue {
        JsonValue::Number(value as f64)
    }

    fn object(entries: Vec<(&'static str, JsonValue)>) -> JsonValue {
        let mut object = Map::new();
        for (key, value) in entries {
            object.insert(key.to_string(), value);
        }
        JsonValue::Object(object)
    }

    fn to_compact_string(value: &JsonValue) -> String {
        value.to_string_compact()
    }
}

/// Build a JSON-RPC 2.0 result message.
pub fn build_result_message(id: Option<&JsonValue>, result: JsonValue) -> String {
    jsonrpc::build_result_message::<ServerJson>(id, result)
}

/// Build a JSON-RPC 2.0 error message.
pub fn build_error_message(id: Option<&JsonValue>, code: i64, message: &str) -> String {
    jsonrpc::build_error_message::<ServerJson>(id, code, message)
}

/// Build a JSON-RPC 2.0 notification (no id, no response expected).
pub fn build_notification(method: &str, params: JsonValue) -> String {
    jsonrpc::build_notification::<ServerJson>(method, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::from_str;

    #[test]
    fn test_build_result_message() {
        let id = JsonValue::Number(1.0);
        let result = JsonValue::Bool(true);
        let msg = build_result_message(Some(&id), result);
        let parsed: JsonValue = from_str(&msg).unwrap();
        assert_eq!(parsed.get("jsonrpc").and_then(|v| v.as_str()), Some("2.0"));
        assert_eq!(parsed.get("id").and_then(|v| v.as_f64()), Some(1.0));
        // Field order is pinned by the BTreeMap-backed serializer.
        assert_eq!(msg, r#"{"id":1,"jsonrpc":"2.0","result":true}"#);
    }

    #[test]
    fn test_build_error_message() {
        let id = JsonValue::Number(2.0);
        let msg = build_error_message(Some(&id), -32601, "method not found");
        let parsed: JsonValue = from_str(&msg).unwrap();
        assert_eq!(parsed.get("jsonrpc").and_then(|v| v.as_str()), Some("2.0"));
        let error = parsed.get("error").unwrap();
        assert_eq!(error.get("code").and_then(|v| v.as_f64()), Some(-32601.0));
        assert_eq!(
            error.get("message").and_then(|v| v.as_str()),
            Some("method not found")
        );
        assert_eq!(
            msg,
            r#"{"error":{"code":-32601,"message":"method not found"},"id":2,"jsonrpc":"2.0"}"#
        );
    }

    #[test]
    fn test_build_notification() {
        let msg = build_notification("test/event", JsonValue::Null);
        let parsed: JsonValue = from_str(&msg).unwrap();
        assert_eq!(
            parsed.get("method").and_then(|v| v.as_str()),
            Some("test/event")
        );
        assert!(parsed.get("id").is_none());
        assert_eq!(
            msg,
            r#"{"jsonrpc":"2.0","method":"test/event","params":null}"#
        );
    }

    #[test]
    fn test_read_payload_basic() {
        let body = r#"{"id":1}"#;
        let msg = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = std::io::BufReader::new(msg.as_bytes());
        let payload = read_payload(&mut reader).unwrap();
        assert_eq!(payload, Some(body.to_string()));
    }

    #[test]
    fn test_read_payload_eof() {
        let input = b"";
        let mut reader = std::io::BufReader::new(&input[..]);
        let payload = read_payload(&mut reader).unwrap();
        assert!(payload.is_none());
    }
}
