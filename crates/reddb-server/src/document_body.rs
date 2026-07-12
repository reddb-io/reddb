//! Native binary document-body container wiring (PRD-1398, ADR-0063).
//!
//! DOCUMENT collections keep the canonical document under a `body` field.
//! Behind the `storage.binary_document_body` flag, writes serialise that body into the
//! native binary container ([`reddb_types::document_body_codec`]) instead of
//! plain UTF-8 JSON. The container is self-describing — it starts with the
//! `RDOC` magic — so every read path decodes it back to JSON transparently,
//! regardless of the flag, keeping the wire/clients on JSON (no driver change).
//!
//! The binary form lives only inside the stored `Value::Json` bytes; the rest
//! of the engine continues to see a JSON body in memory. This is the
//! binary write + binary-to-JSON read path; bare field filters/projections
//! offset-read from the body after the production cutover.

use reddb_types::document_body_codec;

use crate::application::entity::json_to_storage_value;
use crate::json::{to_vec as json_to_vec, Map, Value as JsonValue};
use crate::presentation::entity_json::storage_value_to_json;
use crate::storage::schema::Value;
use crate::{RedDBError, RedDBResult};

/// True when `bytes` begin with the document-body container magic.
///
/// A legacy UTF-8 JSON document body is an object and starts with `{`, so this
/// magic check never collides with a JSON body.
pub(crate) fn is_binary_container(bytes: &[u8]) -> bool {
    bytes.starts_with(document_body_codec::MAGIC)
}

/// Decode a binary document-body container back to its JSON object.
///
/// Returns `None` when `bytes` is not a container — so callers fall back to
/// their existing JSON parse — or when a container fails to decode.
pub(crate) fn decode_container_to_json(bytes: &[u8]) -> Option<JsonValue> {
    if !is_binary_container(bytes) {
        return None;
    }
    let fields = document_body_codec::decode(bytes).ok()?;
    let mut map = Map::new();
    for (key, value) in fields {
        let json = match &value {
            Value::Integer(n) => JsonValue::Integer(*n),
            other => storage_value_to_json(other),
        };
        map.insert(key, json);
    }
    Some(JsonValue::Object(map))
}

/// Offset-read a single top-level field from a stored document body.
///
/// Returns the field's storage [`Value`] decoded directly from the binary
/// container by offset ([`document_body_codec::read_field_by_name`]) — no
/// other field is touched. Returns `None` when `bytes` is not a binary
/// container or the field is absent.
///
/// This is the read seam that makes the body the single source of truth: a
/// `WHERE`/projection on a top-level field routes here to read it straight
/// from the body.
pub(crate) fn read_body_field(bytes: &[u8], name: &str) -> Option<Value> {
    if !is_binary_container(bytes) {
        return None;
    }
    document_body_codec::read_field_by_name(bytes, name)
        .ok()
        .flatten()
}

/// Decode every top-level field of a binary document body as storage values.
///
/// Returns `None` when `bytes` is not a binary container. Used to expand a
/// `SELECT *` over a single-source document back into its top-level fields.
pub(crate) fn body_fields(bytes: &[u8]) -> Option<Vec<(String, Value)>> {
    if !is_binary_container(bytes) {
        return None;
    }
    document_body_codec::decode(bytes).ok()
}

/// List the top-level field names of a binary document body (offset-read of
/// the keys section only). Returns `None` for non-container bytes.
pub(crate) fn container_field_names(bytes: &[u8]) -> Option<Vec<String>> {
    if !is_binary_container(bytes) {
        return None;
    }
    document_body_codec::field_names(bytes).ok()
}

/// Serialise a document body for storage in the `body` field.
///
/// With `binary` set and an object body, produce the native binary container;
/// otherwise (or for a non-object body, which the container cannot represent)
/// fall back to UTF-8 JSON bytes. The caller wraps the result in `Value::Json`.
pub(crate) fn serialize_document_body(body: &JsonValue, binary: bool) -> RedDBResult<Vec<u8>> {
    if binary {
        if let JsonValue::Object(map) = body {
            let typed: Vec<(String, Value)> = map
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_storage_value(value)?)))
                .collect::<RedDBResult<_>>()?;
            let refs: Vec<(&str, &Value)> = typed
                .iter()
                .map(|(key, value)| (key.as_str(), value))
                .collect();
            let mut out = Vec::new();
            document_body_codec::encode(&refs, &mut out).map_err(|err| {
                RedDBError::Query(format!("failed to encode binary document body: {err}"))
            })?;
            return Ok(out);
        }
    }
    json_to_vec(body)
        .map_err(|err| RedDBError::Query(format!("failed to serialize document body: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> JsonValue {
        crate::json::from_str(text).expect("valid JSON fixture")
    }

    fn body() -> JsonValue {
        parse(
            r#"{"name":"Alice","age":30,"email":"alice@example.com",
               "tags":["admin","ops"],"profile":{"city":"SP","active":true}}"#,
        )
    }

    #[test]
    fn binary_off_emits_plain_json_bytes() {
        let bytes = serialize_document_body(&body(), false).expect("serialize");
        assert!(!is_binary_container(&bytes), "flag off must stay JSON");
        assert_eq!(bytes.first(), Some(&b'{'));
        assert!(decode_container_to_json(&bytes).is_none());
    }

    #[test]
    fn binary_on_emits_container_that_decodes_to_equal_json() {
        let original = body();
        let bytes = serialize_document_body(&original, true).expect("serialize");
        assert!(is_binary_container(&bytes), "flag on must produce RDOC");
        let decoded = decode_container_to_json(&bytes).expect("decode");
        assert_eq!(
            decoded, original,
            "binary body must round-trip to equal JSON"
        );
    }

    #[test]
    fn non_object_body_falls_back_to_json_even_with_binary_on() {
        let scalar = JsonValue::String("just-a-string".to_string());
        let bytes = serialize_document_body(&scalar, true).expect("serialize");
        assert!(!is_binary_container(&bytes));
        assert_eq!(
            json_to_vec(&scalar).unwrap(),
            bytes,
            "non-object body must serialise as plain JSON"
        );
    }

    #[test]
    fn rich_semantic_string_types_survive_round_trip() {
        // On the JSON wire these are strings; the container must round-trip the
        // exact JSON the client sent (Email/Ipv4/Subnet/Color string forms).
        let original = parse(
            r##"{"email":"user@example.com","ipv4":"127.0.0.1",
               "subnet":"10.0.0.0/8","color":"#DEADBE","url":"https://reddb.io"}"##,
        );
        let bytes = serialize_document_body(&original, true).expect("serialize");
        let decoded = decode_container_to_json(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn read_body_field_offset_reads_from_binary_body() {
        let bytes = serialize_document_body(&body(), true).expect("serialize");
        assert_eq!(read_body_field(&bytes, "name"), Some(Value::text("Alice")));
        assert_eq!(read_body_field(&bytes, "age"), Some(Value::Integer(30)));
        assert_eq!(read_body_field(&bytes, "missing"), None);
    }

    #[test]
    fn body_field_helpers_ignore_plain_json_bodies() {
        let bytes = serialize_document_body(&body(), false).expect("serialize");
        assert_eq!(read_body_field(&bytes, "name"), None);
        assert_eq!(body_fields(&bytes), None);
        assert_eq!(container_field_names(&bytes), None);
    }

    #[test]
    fn body_fields_and_names_cover_top_level_keys() {
        let bytes = serialize_document_body(&body(), true).expect("serialize");
        let names = container_field_names(&bytes).expect("names");
        for key in ["name", "age", "email", "tags", "profile"] {
            assert!(names.contains(&key.to_string()), "missing {key}");
        }
        let fields = body_fields(&bytes).expect("fields");
        assert_eq!(fields.len(), names.len());
    }

    #[test]
    fn empty_object_round_trips() {
        let original = parse("{}");
        let bytes = serialize_document_body(&original, true).expect("serialize");
        assert!(is_binary_container(&bytes));
        assert_eq!(decode_container_to_json(&bytes), Some(original));
    }
}
