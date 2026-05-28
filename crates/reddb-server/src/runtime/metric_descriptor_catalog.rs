//! Metric descriptor catalog persisted in `red_config`.
//!
//! Analytics v0 starts with descriptor state only: CREATE METRIC stores a
//! catalog record and `red.analytics.metrics` projects it back for reads.

use crate::api::{RedDBError, RedDBResult};
use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, UnifiedStore};
use crate::utils::json::{parse_json, JsonValue};

use std::time::{SystemTime, UNIX_EPOCH};

const REGISTRY_KEY: &str = "red.analytics.metrics.entries_json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricDescriptor {
    pub path: String,
    pub kind: String,
    pub role: String,
    pub created_at_ms: u128,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub fn create(
    store: &UnifiedStore,
    path: &str,
    kind: &str,
    role: &str,
) -> RedDBResult<MetricDescriptor> {
    validate_path(path)?;
    validate_kind(kind)?;
    validate_role(role)?;

    let mut entries = load(store);
    if entries.iter().any(|entry| entry.path == path) {
        return Err(RedDBError::Query(format!(
            "metric descriptor '{path}' already exists"
        )));
    }

    let descriptor = MetricDescriptor {
        path: path.to_string(),
        kind: kind.to_string(),
        role: role.to_string(),
        created_at_ms: now_ms(),
    };
    entries.push(descriptor.clone());
    save(store, &entries);
    Ok(descriptor)
}

pub fn list(store: &UnifiedStore) -> Vec<MetricDescriptor> {
    load(store)
}

fn validate_path(path: &str) -> RedDBResult<()> {
    let segments: Vec<_> = path.split('.').collect();
    if segments.len() < 2
        || segments
            .iter()
            .any(|segment| segment.is_empty() || !segment.chars().all(valid_path_char))
    {
        return Err(RedDBError::Query(format!(
            "invalid metric descriptor path '{path}': expected a dotted path like infra.database.cpu.usage"
        )));
    }
    Ok(())
}

fn valid_path_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'
}

fn validate_kind(kind: &str) -> RedDBResult<()> {
    if matches!(
        kind,
        "counter" | "gauge" | "histogram" | "ratio" | "derived"
    ) {
        return Ok(());
    }
    Err(RedDBError::Query(format!(
        "invalid metric descriptor kind '{kind}': expected counter, gauge, histogram, ratio, or derived"
    )))
}

fn validate_role(role: &str) -> RedDBResult<()> {
    if matches!(role, "metric" | "operational" | "kpi" | "sli") {
        return Ok(());
    }
    Err(RedDBError::Query(format!(
        "invalid metric descriptor role '{role}': expected metric, operational, kpi, or sli"
    )))
}

fn read_latest_registry_json(store: &UnifiedStore) -> Option<String> {
    let manager = store.get_collection("red_config")?;
    let mut all = manager.query_all(|_| true);
    all.sort_by_key(|entity| std::cmp::Reverse(entity.id.raw()));
    for entity in all {
        let EntityData::Row(row) = &entity.data else {
            continue;
        };
        let Some(named) = &row.named else { continue };
        let matches = matches!(
            named.get("key"),
            Some(Value::Text(value)) if value.as_ref() == REGISTRY_KEY
        );
        if matches {
            if let Some(Value::Text(value)) = named.get("value") {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn load(store: &UnifiedStore) -> Vec<MetricDescriptor> {
    let raw = match read_latest_registry_json(store) {
        Some(raw) => raw,
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
        let lookup = |k: &str| obj.iter().find(|(key, _)| key == k).map(|(_, value)| value);
        let Some(path) = lookup("path").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(kind) = lookup("kind").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(role) = lookup("role").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(created_at_ms) = lookup("created_at_ms").and_then(JsonValue::as_f64) else {
            continue;
        };
        out.push(MetricDescriptor {
            path: path.to_string(),
            kind: kind.to_string(),
            role: role.to_string(),
            created_at_ms: created_at_ms as u128,
        });
    }
    out
}

fn save(store: &UnifiedStore, entries: &[MetricDescriptor]) {
    let arr = crate::serde_json::Value::Array(entries.iter().map(entry_to_json).collect());
    store.set_config_tree(
        REGISTRY_KEY,
        &crate::serde_json::Value::String(arr.to_string()),
    );
}

fn entry_to_json(entry: &MetricDescriptor) -> crate::serde_json::Value {
    let mut obj = crate::serde_json::Map::new();
    obj.insert(
        "path".to_string(),
        crate::serde_json::Value::String(entry.path.clone()),
    );
    obj.insert(
        "kind".to_string(),
        crate::serde_json::Value::String(entry.kind.clone()),
    );
    obj.insert(
        "role".to_string(),
        crate::serde_json::Value::String(entry.role.clone()),
    );
    obj.insert(
        "created_at_ms".to_string(),
        crate::serde_json::Value::Number(entry.created_at_ms as f64),
    );
    crate::serde_json::Value::Object(obj)
}
