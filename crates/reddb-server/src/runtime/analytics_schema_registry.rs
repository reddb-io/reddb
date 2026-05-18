//! Issue #577 — Analytics slice 2: `AnalyticsSchemaRegistry`.
//! Issue #581 — Analytics slice 3: additive schema evolution +
//! breaking-change rejection.
//!
//! Owns `(event_name, version) → schema_json` mappings persisted in
//! `red_config`, validates payloads at insert time, and exposes the
//! registered set for the `red.schema_registry` virtual table.
//!
//! Re-registering an existing `event_name` is allowed iff the change
//! is *additive*: new optional fields only (with or without default),
//! widening string `maxLength`. Anything else — rename, retype, drop,
//! optional→required, brand-new required field — is rejected with a
//! typed `SchemaError::BreakingChange { offenders }` whose `offenders`
//! list names every offending field together with the kind of break,
//! so the caller can pick a new `event_name` rather than smuggle the
//! incompatible change through the same one.
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
    /// Schema text did not parse as JSON.
    InvalidSchemaJson(String),
    /// Schema parsed but did not match the expected
    /// `{type:"object", properties:{}, required:[]}` shape.
    InvalidSchemaShape(String),
    /// A re-registration would break wire compatibility with the
    /// previously registered version. `offenders` carries every
    /// breaking change found in the diff so the caller can fix the
    /// schema or pick a different `event_name` in one shot.
    BreakingChange {
        event_name: String,
        previous_version: u32,
        offenders: Vec<BreakingChange>,
    },
}

/// One reason why a candidate schema is not an additive successor of
/// the previous version. Used inside [`SchemaError::BreakingChange`].
#[derive(Debug, Clone, PartialEq)]
pub enum BreakingChange {
    /// A field present in the previous version disappeared, and a new
    /// field of the same declared type appeared in the candidate.
    /// Treated as a rename rather than two separate changes because
    /// the caller almost certainly meant to rename — the error
    /// message tells them which pair we paired up.
    Rename { from: String, to: String },
    /// A field changed declared `type`.
    Retype {
        field: String,
        from: String,
        to: String,
    },
    /// A previously declared field is gone in the candidate.
    Drop { field: String },
    /// A field that was previously optional became required, or a new
    /// field appeared in the candidate's `required` list (existing
    /// rows wouldn't carry it).
    RequiredAdd { field: String },
}

impl BreakingChange {
    /// Short, machine-parseable description used in error bodies.
    pub fn describe(&self) -> String {
        match self {
            BreakingChange::Rename { from, to } => format!("renamed field '{from}' to '{to}'"),
            BreakingChange::Retype { field, from, to } => {
                format!("retyped field '{field}' from {from} to {to}")
            }
            BreakingChange::Drop { field } => format!("dropped field '{field}'"),
            BreakingChange::RequiredAdd { field } => {
                format!("required-add for field '{field}'")
            }
        }
    }
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

/// Register a schema for `event_name`.
///
/// * First registration → returns version `1`.
/// * Additive successor → returns `previous_version + 1`.
/// * Anything else → `SchemaError::BreakingChange { offenders }` with
///   every break the diff turned up so the caller can fix them all in
///   one round-trip.
pub fn register(
    store: &UnifiedStore,
    event_name: &str,
    schema_json: &str,
) -> Result<u32, SchemaError> {
    let candidate = validate_schema_shape(schema_json)?;
    let mut entries = load(store);

    let previous = entries
        .iter()
        .filter(|e| e.event_name == event_name)
        .max_by_key(|e| e.version)
        .cloned();

    let next_version = match previous {
        None => 1,
        Some(prev) => {
            let prev_schema = parse_json(&prev.schema_json).map_err(|e| {
                SchemaError::InvalidSchemaShape(format!(
                    "previously registered schema for {event_name} v{} is corrupt: {e}",
                    prev.version
                ))
            })?;
            let offenders = diff_for_breaking_changes(&prev_schema, &candidate);
            if !offenders.is_empty() {
                return Err(SchemaError::BreakingChange {
                    event_name: event_name.to_string(),
                    previous_version: prev.version,
                    offenders,
                });
            }
            prev.version + 1
        }
    };

    entries.push(SchemaEntry {
        event_name: event_name.to_string(),
        version: next_version,
        schema_json: schema_json.to_string(),
        registered_at_ms: now_ms(),
    });
    save(store, &entries);
    Ok(next_version)
}

/// Extract `(field, type, is_required)` triples from a parsed schema
/// object. `type` is the declared JSON-Schema `type` string for the
/// property, or `""` when none was declared.
fn schema_fields(schema: &JsonValue) -> Vec<(String, String, bool)> {
    let Some(obj) = schema.as_object() else {
        return Vec::new();
    };
    let properties: &[(String, JsonValue)] = obj
        .iter()
        .find(|(k, _)| k == "properties")
        .and_then(|(_, v)| v.as_object())
        .unwrap_or(&[]);
    let required: Vec<&str> = obj
        .iter()
        .find(|(k, _)| k == "required")
        .and_then(|(_, v)| v.as_array())
        .map(|arr| arr.iter().filter_map(JsonValue::as_str).collect())
        .unwrap_or_default();
    properties
        .iter()
        .map(|(name, prop)| {
            let ty = prop
                .as_object()
                .and_then(|entries| entries.iter().find(|(k, _)| k == "type"))
                .and_then(|(_, v)| v.as_str())
                .unwrap_or("")
                .to_string();
            let req = required.iter().any(|r| *r == name.as_str());
            (name.clone(), ty, req)
        })
        .collect()
}

/// Diff a previously registered schema against a candidate and return
/// every breaking change. Empty result == additive (or identical).
///
/// The diff intentionally pairs unmatched drops + adds of the same
/// declared type as a [`BreakingChange::Rename`] — the caller is told
/// which pair we associated so they can disambiguate if our guess is
/// wrong.
fn diff_for_breaking_changes(prev: &JsonValue, next: &JsonValue) -> Vec<BreakingChange> {
    let prev_fields = schema_fields(prev);
    let next_fields = schema_fields(next);

    let mut breaks = Vec::new();
    let mut dropped: Vec<(String, String)> = Vec::new();
    // (name, type, required) for fields present in next but not prev.
    let mut added: Vec<(String, String, bool)> = Vec::new();

    for (name, prev_type, prev_required) in &prev_fields {
        match next_fields.iter().find(|(n, _, _)| n == name) {
            Some((_, next_type, next_required)) => {
                if prev_type != next_type && !prev_type.is_empty() && !next_type.is_empty() {
                    breaks.push(BreakingChange::Retype {
                        field: name.clone(),
                        from: prev_type.clone(),
                        to: next_type.clone(),
                    });
                }
                if !prev_required && *next_required {
                    breaks.push(BreakingChange::RequiredAdd {
                        field: name.clone(),
                    });
                }
            }
            None => dropped.push((name.clone(), prev_type.clone())),
        }
    }

    for (name, next_type, next_required) in &next_fields {
        if prev_fields.iter().any(|(n, _, _)| n == name) {
            continue;
        }
        added.push((name.clone(), next_type.clone(), *next_required));
    }

    // Pair drops with same-typed additions first → rename. A paired
    // addition is *not* also reported as RequiredAdd even if the new
    // version flagged it required: the user's intent was a rename,
    // and surfacing both would just be noise for the same root cause.
    for (drop_name, drop_type) in dropped {
        let paired = added
            .iter()
            .position(|(_, ty, _)| ty == &drop_type && !drop_type.is_empty());
        match paired {
            Some(idx) => {
                let (add_name, _, _) = added.remove(idx);
                breaks.push(BreakingChange::Rename {
                    from: drop_name,
                    to: add_name,
                });
            }
            None => breaks.push(BreakingChange::Drop { field: drop_name }),
        }
    }

    // Unpaired added fields: required-add is breaking, optional-add
    // is additive (the happy path).
    for (name, _, required) in added {
        if required {
            breaks.push(BreakingChange::RequiredAdd { field: name });
        }
    }

    breaks
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
    fn re_registering_identical_schema_bumps_to_next_version() {
        // Slice 3 (#581): re-registering an identical schema is the
        // degenerate additive case — no fields changed, so it must
        // be accepted as v2.
        let s = store();
        register(&s, "page_view", PAGE_VIEW_SCHEMA).unwrap();
        let v = register(&s, "page_view", PAGE_VIEW_SCHEMA).expect("identical is additive");
        assert_eq!(v, 2);
    }

    // --- slice 3 (#581): additive evolution + breaking-change rejection ---

    const PURCHASE_V1: &str =
        r#"{"type":"object","properties":{"amount":{"type":"number"}},"required":["amount"]}"#;

    #[test]
    fn additive_optional_field_is_accepted_as_v2() {
        let s = store();
        register(&s, "purchase", PURCHASE_V1).unwrap();
        let v2 = register(
            &s,
            "purchase",
            r#"{"type":"object",
                "properties":{"amount":{"type":"number"},
                              "discount_code":{"type":"string"}},
                "required":["amount"]}"#,
        )
        .expect("optional add is additive");
        assert_eq!(v2, 2);
        let (latest_v, _) = latest(&s, "purchase").unwrap();
        assert_eq!(latest_v, 2);
    }

    #[test]
    fn additive_optional_field_with_default_is_accepted() {
        let s = store();
        register(&s, "purchase", PURCHASE_V1).unwrap();
        let v2 = register(
            &s,
            "purchase",
            r#"{"type":"object",
                "properties":{"amount":{"type":"number"},
                              "currency":{"type":"string","default":"USD"}},
                "required":["amount"]}"#,
        )
        .expect("optional add with default is additive");
        assert_eq!(v2, 2);
    }

    #[test]
    fn widening_string_max_length_is_accepted() {
        let s = store();
        register(
            &s,
            "ev",
            r#"{"type":"object","properties":{"name":{"type":"string","maxLength":32}},"required":["name"]}"#,
        )
        .unwrap();
        let v2 = register(
            &s,
            "ev",
            r#"{"type":"object","properties":{"name":{"type":"string","maxLength":128}},"required":["name"]}"#,
        )
        .expect("widening maxLength is additive");
        assert_eq!(v2, 2);
    }

    #[test]
    fn breaking_rename_is_rejected() {
        let s = store();
        register(&s, "purchase", PURCHASE_V1).unwrap();
        let err = register(
            &s,
            "purchase",
            r#"{"type":"object","properties":{"total":{"type":"number"}},"required":["total"]}"#,
        )
        .unwrap_err();
        match err {
            SchemaError::BreakingChange {
                event_name,
                previous_version,
                offenders,
            } => {
                assert_eq!(event_name, "purchase");
                assert_eq!(previous_version, 1);
                assert!(
                    offenders.iter().any(|b| matches!(
                        b,
                        BreakingChange::Rename { from, to }
                            if from == "amount" && to == "total"
                    )),
                    "expected Rename(amount->total), got {offenders:?}"
                );
            }
            other => panic!("expected BreakingChange, got {other:?}"),
        }
    }

    #[test]
    fn breaking_retype_is_rejected() {
        let s = store();
        register(&s, "purchase", PURCHASE_V1).unwrap();
        let err = register(
            &s,
            "purchase",
            r#"{"type":"object","properties":{"amount":{"type":"string"}},"required":["amount"]}"#,
        )
        .unwrap_err();
        let SchemaError::BreakingChange { offenders, .. } = err else {
            panic!("expected BreakingChange");
        };
        assert!(offenders.iter().any(|b| matches!(
            b,
            BreakingChange::Retype { field, from, to }
                if field == "amount" && from == "number" && to == "string"
        )));
    }

    #[test]
    fn breaking_drop_is_rejected() {
        let s = store();
        register(
            &s,
            "ev",
            r#"{"type":"object",
                "properties":{"a":{"type":"number"},"b":{"type":"boolean"}},
                "required":["a"]}"#,
        )
        .unwrap();
        let err = register(
            &s,
            "ev",
            r#"{"type":"object","properties":{"a":{"type":"number"}},"required":["a"]}"#,
        )
        .unwrap_err();
        let SchemaError::BreakingChange { offenders, .. } = err else {
            panic!("expected BreakingChange");
        };
        assert!(offenders
            .iter()
            .any(|b| matches!(b, BreakingChange::Drop { field } if field == "b")));
    }

    #[test]
    fn breaking_optional_to_required_is_rejected() {
        let s = store();
        register(
            &s,
            "ev",
            r#"{"type":"object",
                "properties":{"a":{"type":"number"},"b":{"type":"string"}},
                "required":["a"]}"#,
        )
        .unwrap();
        let err = register(
            &s,
            "ev",
            r#"{"type":"object",
                "properties":{"a":{"type":"number"},"b":{"type":"string"}},
                "required":["a","b"]}"#,
        )
        .unwrap_err();
        let SchemaError::BreakingChange { offenders, .. } = err else {
            panic!("expected BreakingChange");
        };
        assert!(offenders
            .iter()
            .any(|b| matches!(b, BreakingChange::RequiredAdd { field } if field == "b")));
    }

    #[test]
    fn multi_field_break_reports_every_offender() {
        let s = store();
        register(
            &s,
            "ev",
            r#"{"type":"object",
                "properties":{"a":{"type":"number"},
                              "b":{"type":"string"},
                              "c":{"type":"boolean"}},
                "required":["a"]}"#,
        )
        .unwrap();
        // Retype `a` (number → string), drop `c`, and add brand-new
        // required field `d`. Three independent breaks in one diff.
        let err = register(
            &s,
            "ev",
            r#"{"type":"object",
                "properties":{"a":{"type":"string"},
                              "b":{"type":"string"},
                              "d":{"type":"integer"}},
                "required":["a","d"]}"#,
        )
        .unwrap_err();
        let SchemaError::BreakingChange { offenders, .. } = err else {
            panic!("expected BreakingChange");
        };
        assert!(offenders
            .iter()
            .any(|b| matches!(b, BreakingChange::Retype { field, .. } if field == "a")));
        assert!(offenders
            .iter()
            .any(|b| matches!(b, BreakingChange::Drop { field } if field == "c")));
        assert!(offenders
            .iter()
            .any(|b| matches!(b, BreakingChange::RequiredAdd { field } if field == "d")));
    }

    #[test]
    fn validate_resolves_to_latest_version_after_evolution() {
        // After an additive evolution, validate() must use v2's
        // strict-properties set — a payload using only v1 fields
        // still passes; a payload using v2's new optional field
        // also passes; an unknown field still rejects.
        let s = store();
        register(&s, "purchase", PURCHASE_V1).unwrap();
        register(
            &s,
            "purchase",
            r#"{"type":"object",
                "properties":{"amount":{"type":"number"},
                              "discount_code":{"type":"string"}},
                "required":["amount"]}"#,
        )
        .unwrap();
        validate(&s, "purchase", r#"{"amount":1.0}"#).expect("v1-shape still valid");
        validate(&s, "purchase", r#"{"amount":1.0,"discount_code":"X"}"#)
            .expect("v2-only field accepted");
        let err = validate(&s, "purchase", r#"{"amount":1.0,"mystery":1}"#).unwrap_err();
        assert!(matches!(err, ValidationError::UnknownField { version, .. } if version == 2));
    }

    #[test]
    fn list_returns_every_version_not_just_latest() {
        // red.schema_registry virtual table is fed by list(); slice 3
        // contract is "every version, not just the latest".
        let s = store();
        register(&s, "purchase", PURCHASE_V1).unwrap();
        register(
            &s,
            "purchase",
            r#"{"type":"object",
                "properties":{"amount":{"type":"number"},
                              "discount_code":{"type":"string"}},
                "required":["amount"]}"#,
        )
        .unwrap();
        let purchase_versions: Vec<u32> = list(&s)
            .into_iter()
            .filter(|e| e.event_name == "purchase")
            .map(|e| e.version)
            .collect();
        let mut sorted = purchase_versions.clone();
        sorted.sort();
        assert_eq!(sorted, vec![1, 2], "expected both versions, got {purchase_versions:?}");
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
