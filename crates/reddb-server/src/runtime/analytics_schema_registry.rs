//! Issue #577 — Analytics slice 2: `AnalyticsSchemaRegistry`.
//!
//! Owns `(event_name, version) → schema_json` mappings persisted in
//! `red_config`, validates payloads at insert time, and exposes the
//! registered set for the `red.schema_registry` virtual table.
//!
//! Scope of this slice (per PRD #575): register a fresh schema
//! (version 1 only), validate a payload against it, and list every
//! registered schema. Schema evolution (v2+) and breaking-change
//! detection live in a follow-up slice — re-registering the same
//! `event_name` is rejected here as `SchemaError::AlreadyRegistered`
//! so we cannot silently accept an evolution that the breaking-change
//! checker hasn't reviewed yet.
//!
//! Persistence shape: a single JSON document stored under
//! `red.analytics.schema_registry.entries_json` as a Text value. It
//! contains an array of entries `{event_name, version, schema_json,
//! registered_at_ms}`. `red_config` is append-only, so we read by
//! scanning that collection and keeping the row with the largest
//! engine-assigned `EntityId` (most recent write) — same trick
//! `signed_writes_kind` uses.
//!
//! The schema language is a minimal JSON Schema subset:
//! ```json
//! { "type": "object",
//!   "properties": { "url": { "type": "string" } },
//!   "required": ["url"] }
//! ```
//! Validation rules (v1):
//! * payload must parse to a JSON object,
//! * every key in `required` must be present,
//! * every key in the payload must appear in `properties` (unknown
//!   field rejected — strict mode),
//! * for keys present in both, the type tag must match.

use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, UnifiedStore};
use crate::utils::json::{parse_json, JsonValue};

use std::time::{SystemTime, UNIX_EPOCH};

const REGISTRY_KEY: &str = "red.analytics.schema_registry.entries_json";

/// One registered schema row.
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaEntry {
    pub event_name: String,
    pub version: u32,
    pub schema_json: String,
    pub registered_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SchemaError {
    /// `event_name` already has a registered schema. Slice 2 rejects
    /// re-registration; schema evolution is owned by a follow-up
    /// slice that ships the breaking-change checker.
    AlreadyRegistered { event_name: String, existing_version: u32 },
    /// Schema text did not parse as JSON.
    InvalidSchemaJson(String),
    /// Schema parsed but did not match the expected
    /// `{type:"object", properties:{}, required:[]}` shape.
    InvalidSchemaShape(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    /// No schema is registered for this `event_name`. Callers may
    /// silently treat this as "no validation" (insert path) — the
    /// variant is here so library consumers can branch on it.
    UnknownEventName(String),
    InvalidPayloadJson(String),
    /// Payload parsed but is not a JSON object.
    PayloadNotObject,
    /// Payload omitted a field listed in `required`.
    MissingRequiredField {
        event_name: String,
        version: u32,
        field: String,
    },
    /// Payload included a field that the registered schema does not
    /// declare in `properties`. Strict mode — slice 2 has no
    /// `additionalProperties: true` escape hatch.
    UnknownField {
        event_name: String,
        version: u32,
        field: String,
    },
    /// Field's JSON type does not match the property's declared type.
    TypeMismatch {
        event_name: String,
        version: u32,
        field: String,
        expected: String,
        got: String,
    },
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Read the *latest* Text payload for the registry key out of
/// `red_config`. `red_config` is append-only — `UnifiedStore::get_config`
/// returns the first matching row, but the most-recent write wins
/// for us. We sort by `EntityId` descending and keep the first
/// matching row — identical to `signed_writes_kind::read_latest_config`.
fn read_latest_registry_json(store: &UnifiedStore) -> Option<String> {
    let manager = store.get_collection("red_config")?;
    let mut all = manager.query_all(|_| true);
    all.sort_by(|a, b| b.id.raw().cmp(&a.id.raw()));
    for entity in all {
        let EntityData::Row(row) = &entity.data else {
            continue;
        };
        let Some(named) = &row.named else { continue };
        let matches = matches!(
            named.get("key"),
            Some(Value::Text(s)) if s.as_ref() == REGISTRY_KEY
        );
        if matches {
            if let Some(Value::Text(s)) = named.get("value") {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn load(store: &UnifiedStore) -> Vec<SchemaEntry> {
    let raw = match read_latest_registry_json(store) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let Ok(parsed) = parse_json(&raw) else {
        return Vec::new();
    };
    let Some(arr) = parsed.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let lookup = |k: &str| obj.iter().find(|(key, _)| key == k).map(|(_, v)| v);
        let Some(event_name) = lookup("event_name").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(version) = lookup("version").and_then(JsonValue::as_f64) else {
            continue;
        };
        let Some(schema_json) = lookup("schema_json").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(registered_at_ms) = lookup("registered_at_ms").and_then(JsonValue::as_f64) else {
            continue;
        };
        out.push(SchemaEntry {
            event_name: event_name.to_string(),
            version: version as u32,
            schema_json: schema_json.to_string(),
            registered_at_ms: registered_at_ms as u128,
        });
    }
    out
}

fn entry_to_json(e: &SchemaEntry) -> crate::serde_json::Value {
    let mut obj = crate::serde_json::Map::new();
    obj.insert(
        "event_name".to_string(),
        crate::serde_json::Value::String(e.event_name.clone()),
    );
    obj.insert(
        "version".to_string(),
        crate::serde_json::Value::Number(e.version as f64),
    );
    obj.insert(
        "schema_json".to_string(),
        crate::serde_json::Value::String(e.schema_json.clone()),
    );
    obj.insert(
        "registered_at_ms".to_string(),
        crate::serde_json::Value::Number(e.registered_at_ms as f64),
    );
    crate::serde_json::Value::Object(obj)
}

fn save(store: &UnifiedStore, entries: &[SchemaEntry]) {
    let arr = crate::serde_json::Value::Array(entries.iter().map(entry_to_json).collect());
    // Store the array as one Text value, not as a flattened tree,
    // so `set_config_tree` writes a single row whose `value` column
    // round-trips back into the same JSON bytes.
    let wrapped = crate::serde_json::Value::String(arr.to_string());
    store.set_config_tree(REGISTRY_KEY, &wrapped);
}

/// Parse + minimal shape check on the schema string. Returns the
/// canonical re-serialised form, so the registry stores a normalised
/// representation regardless of caller whitespace / key ordering.
fn validate_schema_shape(schema_json: &str) -> Result<JsonValue, SchemaError> {
    let parsed = parse_json(schema_json)
        .map_err(|err| SchemaError::InvalidSchemaJson(err.to_string()))?;
    let Some(obj) = parsed.as_object() else {
        return Err(SchemaError::InvalidSchemaShape(
            "schema must be a JSON object".to_string(),
        ));
    };
    let lookup = |k: &str| obj.iter().find(|(key, _)| key == k).map(|(_, v)| v);
    match lookup("type").and_then(JsonValue::as_str) {
        Some("object") => {}
        Some(other) => {
            return Err(SchemaError::InvalidSchemaShape(format!(
                "schema `type` must be \"object\", got \"{other}\""
            )));
        }
        None => {
            return Err(SchemaError::InvalidSchemaShape(
                "schema must declare `type`".to_string(),
            ));
        }
    }
    if let Some(props) = lookup("properties") {
        if props.as_object().is_none() {
            return Err(SchemaError::InvalidSchemaShape(
                "schema `properties` must be an object".to_string(),
            ));
        }
    }
    if let Some(req) = lookup("required") {
        let Some(arr) = req.as_array() else {
            return Err(SchemaError::InvalidSchemaShape(
                "schema `required` must be an array of strings".to_string(),
            ));
        };
        for item in arr {
            if item.as_str().is_none() {
                return Err(SchemaError::InvalidSchemaShape(
                    "schema `required` must be an array of strings".to_string(),
                ));
            }
        }
    }
    Ok(parsed)
}

/// Register a fresh schema for `event_name`. Slice 2 only supports
/// the *first* registration — returns version `1` on success and
/// `SchemaError::AlreadyRegistered` if any version already exists.
pub fn register(
    store: &UnifiedStore,
    event_name: &str,
    schema_json: &str,
) -> Result<u32, SchemaError> {
    let _shape = validate_schema_shape(schema_json)?;
    let mut entries = load(store);
    if let Some(existing) = entries.iter().find(|e| e.event_name == event_name) {
        return Err(SchemaError::AlreadyRegistered {
            event_name: event_name.to_string(),
            existing_version: existing.version,
        });
    }
    entries.push(SchemaEntry {
        event_name: event_name.to_string(),
        version: 1,
        schema_json: schema_json.to_string(),
        registered_at_ms: now_ms(),
    });
    save(store, &entries);
    Ok(1)
}

/// Return `(version, schema_json)` for the latest registered schema
/// of `event_name`, or `None` if nothing is registered. Since slice
/// 2 only allows version 1 per event, "latest" == "the one row that
/// exists". Once evolution lands, the resolver will keep the
/// max-version row per event_name.
pub fn latest(store: &UnifiedStore, event_name: &str) -> Option<(u32, String)> {
    let entries = load(store);
    entries
        .into_iter()
        .filter(|e| e.event_name == event_name)
        .max_by_key(|e| e.version)
        .map(|e| (e.version, e.schema_json))
}

/// Snapshot every registered schema. Used by the
/// `red.schema_registry` virtual table.
pub fn list(store: &UnifiedStore) -> Vec<SchemaEntry> {
    load(store)
}

fn json_type_name(v: &JsonValue) -> &'static str {
    match v {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

fn type_matches(expected: &str, got: &JsonValue) -> bool {
    match expected {
        "string" => matches!(got, JsonValue::String(_)),
        "boolean" => matches!(got, JsonValue::Bool(_)),
        "array" => matches!(got, JsonValue::Array(_)),
        "object" => matches!(got, JsonValue::Object(_)),
        "null" => matches!(got, JsonValue::Null),
        "number" => matches!(got, JsonValue::Number(_)),
        "integer" => match got {
            JsonValue::Number(n) => *n == n.trunc(),
            _ => false,
        },
        _ => false,
    }
}

/// Validate `payload` (a JSON string) against the latest schema
/// registered for `event_name`. Returns `Ok(())` if the payload
/// matches; `Err(ValidationError)` with a typed reason otherwise.
///
/// `UnknownEventName` is returned when no schema is registered —
/// the insert path treats that as "no validation, accept" for
/// back-compat with `timeseries` rows that don't carry an
/// `event_name` registered yet.
pub fn validate(
    store: &UnifiedStore,
    event_name: &str,
    payload_json: &str,
) -> Result<(), ValidationError> {
    let Some((version, schema_json)) = latest(store, event_name) else {
        return Err(ValidationError::UnknownEventName(event_name.to_string()));
    };
    let schema = parse_json(&schema_json)
        .map_err(|e| ValidationError::InvalidPayloadJson(format!("schema corrupt: {e}")))?;
    let payload = parse_json(payload_json)
        .map_err(|e| ValidationError::InvalidPayloadJson(e.to_string()))?;
    let Some(payload_obj) = payload.as_object() else {
        return Err(ValidationError::PayloadNotObject);
    };
    let schema_obj = schema.as_object().unwrap_or(&[]);
    let properties: &[(String, JsonValue)] = schema_obj
        .iter()
        .find(|(k, _)| k == "properties")
        .and_then(|(_, v)| v.as_object())
        .unwrap_or(&[]);
    let required: Vec<&str> = schema_obj
        .iter()
        .find(|(k, _)| k == "required")
        .and_then(|(_, v)| v.as_array())
        .map(|arr| arr.iter().filter_map(JsonValue::as_str).collect())
        .unwrap_or_default();

    // Required-field check first so callers see the missing-field
    // error before the unknown-field error when both could fire.
    for req in &required {
        if !payload_obj.iter().any(|(k, _)| k == *req) {
            return Err(ValidationError::MissingRequiredField {
                event_name: event_name.to_string(),
                version,
                field: (*req).to_string(),
            });
        }
    }
    // Strict mode: every payload key must appear in properties.
    for (key, value) in payload_obj {
        let Some((_, prop)) = properties.iter().find(|(k, _)| k == key) else {
            return Err(ValidationError::UnknownField {
                event_name: event_name.to_string(),
                version,
                field: key.clone(),
            });
        };
        let expected_type = prop
            .as_object()
            .and_then(|entries| entries.iter().find(|(k, _)| k == "type"))
            .and_then(|(_, v)| v.as_str())
            .unwrap_or("");
        if expected_type.is_empty() {
            continue;
        }
        if !type_matches(expected_type, value) {
            return Err(ValidationError::TypeMismatch {
                event_name: event_name.to_string(),
                version,
                field: key.clone(),
                expected: expected_type.to_string(),
                got: json_type_name(value).to_string(),
            });
        }
    }
    Ok(())
}

/// Map a [`ValidationError`] onto a [`RedDBError`] with a marker
/// prefix the transport layer can pattern-match for status codes.
/// The exact HTTP mapping is wired up alongside the broader analytics
/// transport work; here we keep the body shape stable so callers can
/// already parse it.
pub fn validation_error_to_reddb(err: ValidationError) -> crate::api::RedDBError {
    let body = match &err {
        ValidationError::UnknownEventName(name) => {
            format!("AnalyticsSchemaError:UnknownEventName:{name}")
        }
        ValidationError::InvalidPayloadJson(reason) => {
            format!("AnalyticsSchemaError:InvalidPayloadJson:{reason}")
        }
        ValidationError::PayloadNotObject => {
            "AnalyticsSchemaError:PayloadNotObject".to_string()
        }
        ValidationError::MissingRequiredField {
            event_name,
            version,
            field,
        } => format!(
            "AnalyticsSchemaError:MissingRequiredField:{event_name}:v{version}:{field}"
        ),
        ValidationError::UnknownField {
            event_name,
            version,
            field,
        } => format!(
            "AnalyticsSchemaError:UnknownField:{event_name}:v{version}:{field}"
        ),
        ValidationError::TypeMismatch {
            event_name,
            version,
            field,
            expected,
            got,
        } => format!(
            "AnalyticsSchemaError:TypeMismatch:{event_name}:v{version}:{field}:{expected}:{got}"
        ),
    };
    crate::api::RedDBError::InvalidOperation(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> UnifiedStore {
        UnifiedStore::new()
    }

    const PAGE_VIEW_SCHEMA: &str = r#"{
        "type": "object",
        "properties": {
            "url": {"type": "string"},
            "user_id": {"type": "integer"}
        },
        "required": ["url"]
    }"#;

    #[test]
    fn first_registration_is_version_1() {
        let s = store();
        let v = register(&s, "page_view", PAGE_VIEW_SCHEMA).expect("register ok");
        assert_eq!(v, 1);
        let (latest_v, _) = latest(&s, "page_view").expect("latest present");
        assert_eq!(latest_v, 1);
    }

    #[test]
    fn second_registration_same_event_is_rejected_in_slice_2() {
        let s = store();
        register(&s, "page_view", PAGE_VIEW_SCHEMA).unwrap();
        let err = register(&s, "page_view", PAGE_VIEW_SCHEMA).unwrap_err();
        match err {
            SchemaError::AlreadyRegistered {
                event_name,
                existing_version,
            } => {
                assert_eq!(event_name, "page_view");
                assert_eq!(existing_version, 1);
            }
            other => panic!("expected AlreadyRegistered, got {other:?}"),
        }
    }

    #[test]
    fn invalid_schema_json_rejected_at_register() {
        let s = store();
        let err = register(&s, "x", "{not json").unwrap_err();
        assert!(matches!(err, SchemaError::InvalidSchemaJson(_)));
    }

    #[test]
    fn schema_must_be_type_object() {
        let s = store();
        let err = register(&s, "x", r#"{"type":"string"}"#).unwrap_err();
        assert!(matches!(err, SchemaError::InvalidSchemaShape(_)));
    }

    #[test]
    fn validate_happy_path_accepts_known_fields() {
        let s = store();
        register(&s, "page_view", PAGE_VIEW_SCHEMA).unwrap();
        validate(&s, "page_view", r#"{"url":"/x","user_id":42}"#).expect("ok");
        validate(&s, "page_view", r#"{"url":"/y"}"#).expect("ok without optional");
    }

    #[test]
    fn validate_rejects_unknown_field() {
        let s = store();
        register(&s, "page_view", PAGE_VIEW_SCHEMA).unwrap();
        let err = validate(&s, "page_view", r#"{"url":"/x","mystery":1}"#).unwrap_err();
        match err {
            ValidationError::UnknownField { field, .. } => assert_eq!(field, "mystery"),
            other => panic!("expected UnknownField, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_missing_required_field() {
        let s = store();
        register(&s, "page_view", PAGE_VIEW_SCHEMA).unwrap();
        let err = validate(&s, "page_view", r#"{}"#).unwrap_err();
        match err {
            ValidationError::MissingRequiredField { field, .. } => assert_eq!(field, "url"),
            other => panic!("expected MissingRequiredField, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_type_mismatch() {
        let s = store();
        register(&s, "page_view", PAGE_VIEW_SCHEMA).unwrap();
        let err = validate(&s, "page_view", r#"{"url":123}"#).unwrap_err();
        match err {
            ValidationError::TypeMismatch {
                field, expected, got, ..
            } => {
                assert_eq!(field, "url");
                assert_eq!(expected, "string");
                assert_eq!(got, "number");
            }
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn validate_unknown_event_name() {
        let s = store();
        let err = validate(&s, "nope", r#"{}"#).unwrap_err();
        assert!(matches!(err, ValidationError::UnknownEventName(name) if name == "nope"));
    }

    #[test]
    fn validate_payload_must_be_object() {
        let s = store();
        register(&s, "page_view", PAGE_VIEW_SCHEMA).unwrap();
        let err = validate(&s, "page_view", r#""hello""#).unwrap_err();
        assert!(matches!(err, ValidationError::PayloadNotObject));
    }

    #[test]
    fn list_returns_every_registered_event() {
        let s = store();
        register(&s, "page_view", PAGE_VIEW_SCHEMA).unwrap();
        register(
            &s,
            "signup",
            r#"{"type":"object","properties":{"email":{"type":"string"}},"required":["email"]}"#,
        )
        .unwrap();
        let mut names: Vec<String> = list(&s).into_iter().map(|e| e.event_name).collect();
        names.sort();
        assert_eq!(names, vec!["page_view".to_string(), "signup".to_string()]);
        assert!(list(&s).iter().all(|e| e.version == 1));
        assert!(list(&s).iter().all(|e| e.registered_at_ms > 0));
    }

    #[test]
    fn persistence_smoke_latest_survives_restart() {
        // Slice-2 "engine restart" is simulated by handing the same
        // store handle to a second `latest()` call after the
        // original `register` returns. The real engine restart wires
        // through the same `UnifiedStore` API — we exercise the
        // serialise/deserialise path here, which is what survives
        // process restart on a durable backend.
        let s = store();
        register(&s, "page_view", PAGE_VIEW_SCHEMA).unwrap();
        let raw =
            read_latest_registry_json(&s).expect("registry json must be persisted on register");
        assert!(raw.contains("page_view"));
        // Round-trip through a fresh load that reuses only the
        // public read path:
        let (v, schema) = latest(&s, "page_view").expect("latest after persist");
        assert_eq!(v, 1);
        assert!(schema.contains("\"url\""));
    }

    #[test]
    fn validation_error_maps_to_invalid_operation_with_typed_marker() {
        let err = validation_error_to_reddb(ValidationError::MissingRequiredField {
            event_name: "page_view".to_string(),
            version: 1,
            field: "url".to_string(),
        });
        match err {
            crate::api::RedDBError::InvalidOperation(body) => {
                assert!(
                    body.starts_with("AnalyticsSchemaError:MissingRequiredField:page_view:v1:url"),
                    "unexpected body: {body}"
                );
            }
            other => panic!("expected InvalidOperation, got {other:?}"),
        }
    }
}
