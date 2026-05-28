//! SLO descriptor catalog persisted in `red_config`.
//!
//! Issue #791 — Analytics v0. An SLO is declared over an existing
//! SLI-role metric descriptor (see `metric_descriptor_catalog`) with a
//! target (0 < target <= 1) and a window in milliseconds. The catalog
//! stores descriptor state only — burn-rate / error-budget evaluation
//! is deferred to later slices.

use crate::api::{RedDBError, RedDBResult};
use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, UnifiedStore};
use crate::utils::json::{parse_json, JsonValue};

use std::time::{SystemTime, UNIX_EPOCH};

const REGISTRY_KEY: &str = "red.analytics.slos.entries_json";

#[derive(Debug, Clone, PartialEq)]
pub struct SloDescriptor {
    pub path: String,
    pub metric_path: String,
    pub target: f64,
    pub window_ms: u64,
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
    metric_path: &str,
    target: f64,
    window_ms: u64,
) -> RedDBResult<SloDescriptor> {
    validate_path(path)?;
    validate_target(target)?;
    validate_window_ms(window_ms)?;

    // Target metric must exist *and* be role 'sli'. The catalog is the
    // sole source of truth — looking the metric up via the metric
    // descriptor catalog keeps the dependency explicit and the error
    // wording symmetric with CREATE METRIC validation.
    let metric_entries = super::metric_descriptor_catalog::list(store);
    let metric = metric_entries
        .iter()
        .find(|entry| entry.path == metric_path)
        .ok_or_else(|| {
            RedDBError::Query(format!(
                "SLO target metric '{metric_path}' does not exist in the metric \
                 descriptor catalog; declare it with CREATE METRIC first"
            ))
        })?;
    if metric.role != "sli" {
        return Err(RedDBError::Query(format!(
            "SLO target metric '{metric_path}' has role '{}', expected 'sli'; \
             update the metric descriptor's role with ALTER METRIC … SET ROLE sli \
             before declaring an SLO over it",
            metric.role
        )));
    }

    let mut entries = load(store);
    if entries.iter().any(|entry| entry.path == path) {
        return Err(RedDBError::Query(format!(
            "SLO descriptor '{path}' already exists"
        )));
    }

    let descriptor = SloDescriptor {
        path: path.to_string(),
        metric_path: metric_path.to_string(),
        target,
        window_ms,
        created_at_ms: now_ms(),
    };
    entries.push(descriptor.clone());
    save(store, &entries);
    Ok(descriptor)
}

pub fn list(store: &UnifiedStore) -> Vec<SloDescriptor> {
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
            "invalid SLO descriptor path '{path}': expected a dotted path like infra.api.availability"
        )));
    }
    Ok(())
}

fn valid_path_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'
}

fn validate_target(target: f64) -> RedDBResult<()> {
    if !target.is_finite() || target <= 0.0 || target > 1.0 {
        return Err(RedDBError::Query(format!(
            "invalid SLO target '{target}': expected a finite value in (0, 1]"
        )));
    }
    Ok(())
}

fn validate_window_ms(window_ms: u64) -> RedDBResult<()> {
    if window_ms == 0 {
        return Err(RedDBError::Query(
            "invalid SLO window: expected a positive duration (e.g. 30 DAYS)".to_string(),
        ));
    }
    Ok(())
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

fn load(store: &UnifiedStore) -> Vec<SloDescriptor> {
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
        let Some(metric_path) = lookup("metric_path").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(target) = lookup("target").and_then(JsonValue::as_f64) else {
            continue;
        };
        let Some(window_ms) = lookup("window_ms").and_then(JsonValue::as_f64) else {
            continue;
        };
        let Some(created_at_ms) = lookup("created_at_ms").and_then(JsonValue::as_f64) else {
            continue;
        };
        out.push(SloDescriptor {
            path: path.to_string(),
            metric_path: metric_path.to_string(),
            target,
            window_ms: window_ms as u64,
            created_at_ms: created_at_ms as u128,
        });
    }
    out
}

fn save(store: &UnifiedStore, entries: &[SloDescriptor]) {
    let arr = crate::serde_json::Value::Array(entries.iter().map(entry_to_json).collect());
    store.set_config_tree(
        REGISTRY_KEY,
        &crate::serde_json::Value::String(arr.to_string()),
    );
}

fn entry_to_json(entry: &SloDescriptor) -> crate::serde_json::Value {
    let mut obj = crate::serde_json::Map::new();
    obj.insert(
        "path".to_string(),
        crate::serde_json::Value::String(entry.path.clone()),
    );
    obj.insert(
        "metric_path".to_string(),
        crate::serde_json::Value::String(entry.metric_path.clone()),
    );
    obj.insert(
        "target".to_string(),
        crate::serde_json::Value::Number(entry.target),
    );
    obj.insert(
        "window_ms".to_string(),
        crate::serde_json::Value::Number(entry.window_ms as f64),
    );
    obj.insert(
        "created_at_ms".to_string(),
        crate::serde_json::Value::Number(entry.created_at_ms as f64),
    );
    crate::serde_json::Value::Object(obj)
}
