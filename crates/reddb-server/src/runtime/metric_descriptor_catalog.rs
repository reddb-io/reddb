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
    /// Issue #790 — derived metric descriptors.
    ///
    /// A descriptor is "derived" when at least one of these fields is set.
    /// They name the inputs that *would* feed a future execution layer:
    /// `source` references an `red.analytics.sources` profile,
    /// `query` is a free-form expression string (opaque at v0),
    /// `window_ms` is the evaluation window in milliseconds, and
    /// `time_field` overrides the source's time column. v0 stores them
    /// verbatim — there is no execution engine yet; see [`read_output_unsupported`].
    pub source: Option<String>,
    pub query: Option<String>,
    pub window_ms: Option<u64>,
    pub time_field: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivedSpec {
    pub source: Option<String>,
    pub query: Option<String>,
    pub window_ms: Option<u64>,
    pub time_field: Option<String>,
}

impl DerivedSpec {
    pub fn is_empty(&self) -> bool {
        self.source.is_none()
            && self.query.is_none()
            && self.window_ms.is_none()
            && self.time_field.is_none()
    }
}

/// Build the standard "metric output reads are not yet implemented" error.
///
/// Analytics v0 ships descriptor state only — there is no execution engine
/// that turns a derived metric definition into a value. Callers asking for
/// the *output* (current value, sample series, etc.) get a structured error
/// that names the path and explains why, so downstream tooling does not
/// confuse "not yet built" with "metric does not exist".
/// Parse `READ METRIC <dotted.path>` into the requested path.
///
/// Returns `Ok(None)` for any statement that does not start with
/// `READ METRIC`, leaving the regular SQL pipeline untouched. Recognising
/// the form here (rather than in the grammar) keeps the intercept narrow
/// and avoids polluting the executor planner pipe-patterns just to surface
/// a structured "not yet implemented" error.
pub fn parse_read_metric_statement(sql: &str) -> Option<String> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let mut tokens = trimmed.split_whitespace();
    let head = tokens.next()?;
    let next = tokens.next()?;
    if !head.eq_ignore_ascii_case("READ") || !next.eq_ignore_ascii_case("METRIC") {
        return None;
    }
    let path = tokens.next()?.to_string();
    if tokens.next().is_some() {
        // Reject trailing tokens — keep the surface narrow so users get
        // a clear "unsupported" error rather than silently dropped args.
        return Some(path + " <trailing>");
    }
    Some(path)
}

pub fn read_output_unsupported(path: &str) -> RedDBError {
    RedDBError::Query(format!(
        "metric output read for '{path}' is not yet implemented: \
         Analytics v0 persists derived metric descriptors only — the \
         execution engine that materializes KPI/SLI values from \
         source/query/window definitions has not shipped yet. The \
         descriptor itself remains readable via \
         red.analytics.metrics."
    ))
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
    derived: DerivedSpec,
) -> RedDBResult<MetricDescriptor> {
    validate_path(path)?;
    validate_kind(kind)?;
    validate_role(role)?;
    validate_derived(&derived)?;

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
        source: derived.source,
        query: derived.query,
        window_ms: derived.window_ms,
        time_field: derived.time_field,
    };
    entries.push(descriptor.clone());
    save(store, &entries);
    Ok(descriptor)
}

pub fn list(store: &UnifiedStore) -> Vec<MetricDescriptor> {
    load(store)
}

/// Update mutable fields of an existing metric descriptor.
///
/// v0 mutability rules:
/// - `set_role`: mutable. Role is a semantic label (operational/kpi/sli)
///   that does not change how stored samples are interpreted.
/// - `attempted_kind`: rejected. Changing kind (counter ⇄ gauge ⇄
///   histogram, etc.) silently changes the mathematical meaning of any
///   already-stored or future samples; the safe path is DROP + CREATE.
/// - `attempted_path`: rejected. Path is the descriptor's identity; an
///   "in-place" rename would invalidate every consumer that addresses
///   the metric by dotted path.
pub fn update(
    store: &UnifiedStore,
    path: &str,
    set_role: Option<&str>,
    attempted_kind: Option<&str>,
    attempted_path: Option<&str>,
) -> RedDBResult<MetricDescriptor> {
    if let Some(kind) = attempted_kind {
        return Err(RedDBError::Query(format!(
            "metric descriptor field 'kind' cannot be changed (attempted '{kind}'): \
             changing the metric kind alters the mathematical meaning of \
             stored samples; drop and recreate the descriptor instead"
        )));
    }
    if let Some(new_path) = attempted_path {
        return Err(RedDBError::Query(format!(
            "metric descriptor field 'path' cannot be changed (attempted '{new_path}'): \
             the dotted path is the descriptor's identity; create a new \
             descriptor at the desired path instead"
        )));
    }

    let mut entries = load(store);
    let idx = entries
        .iter()
        .position(|entry| entry.path == path)
        .ok_or_else(|| RedDBError::Query(format!("metric descriptor '{path}' does not exist")))?;

    if let Some(role) = set_role {
        validate_role(role)?;
        entries[idx].role = role.to_string();
    }

    let updated = entries[idx].clone();
    save(store, &entries);
    Ok(updated)
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

fn validate_derived(derived: &DerivedSpec) -> RedDBResult<()> {
    if let Some(source) = &derived.source {
        validate_identifier(source, "derived metric source")?;
    }
    if let Some(field) = &derived.time_field {
        validate_identifier(field, "derived metric time_field")?;
    }
    if let Some(query) = &derived.query {
        if query.trim().is_empty() {
            return Err(RedDBError::Query(
                "derived metric QUERY must not be empty".to_string(),
            ));
        }
    }
    if derived.window_ms == Some(0) {
        return Err(RedDBError::Query(
            "derived metric WINDOW must be greater than zero".to_string(),
        ));
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> RedDBResult<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(RedDBError::Query(format!(
            "invalid {label} '{value}': expected an alphanumeric/underscore identifier"
        )));
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
        let source = lookup("source")
            .and_then(JsonValue::as_str)
            .map(str::to_string);
        let query = lookup("query")
            .and_then(JsonValue::as_str)
            .map(str::to_string);
        let window_ms = lookup("window_ms")
            .and_then(JsonValue::as_f64)
            .map(|n| n as u64);
        let time_field = lookup("time_field")
            .and_then(JsonValue::as_str)
            .map(str::to_string);
        out.push(MetricDescriptor {
            path: path.to_string(),
            kind: kind.to_string(),
            role: role.to_string(),
            created_at_ms: created_at_ms as u128,
            source,
            query,
            window_ms,
            time_field,
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
    if let Some(source) = &entry.source {
        obj.insert(
            "source".to_string(),
            crate::serde_json::Value::String(source.clone()),
        );
    }
    if let Some(query) = &entry.query {
        obj.insert(
            "query".to_string(),
            crate::serde_json::Value::String(query.clone()),
        );
    }
    if let Some(window_ms) = entry.window_ms {
        obj.insert(
            "window_ms".to_string(),
            crate::serde_json::Value::Number(window_ms as f64),
        );
    }
    if let Some(time_field) = &entry.time_field {
        obj.insert(
            "time_field".to_string(),
            crate::serde_json::Value::String(time_field.clone()),
        );
    }
    crate::serde_json::Value::Object(obj)
}
