//! Analytics source profile catalog persisted in `red_config`.
//!
//! Source profiles describe how ordinary table/document collections represent
//! event-shaped facts. They do not create or redirect raw event storage.

use crate::api::{RedDBError, RedDBResult};
use crate::catalog::CollectionModel;
use crate::physical::CollectionContract;
use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, UnifiedStore};
use crate::utils::json::{parse_json, JsonValue};

use std::time::{SystemTime, UNIX_EPOCH};

const REGISTRY_KEY: &str = "red.analytics.sources.entries_json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticsSourceProfile {
    pub name: String,
    pub collection: String,
    pub time_field: String,
    pub event_field: String,
    pub actor_field: String,
    pub session_field: Option<String>,
    pub properties_field: Option<String>,
    pub created_at_ms: u128,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateAnalyticsSourceProfile {
    pub name: String,
    pub collection: String,
    pub time_field: String,
    pub event_field: String,
    pub actor_field: String,
    pub session_field: Option<String>,
    pub properties_field: Option<String>,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub fn parse_create_statement(sql: &str) -> RedDBResult<Option<CreateAnalyticsSourceProfile>> {
    let trimmed = sql.trim().trim_end_matches(';');
    let tokens: Vec<&str> = trimmed.split_whitespace().collect();
    if tokens.len() < 3
        || !tokens[0].eq_ignore_ascii_case("CREATE")
        || !tokens[1].eq_ignore_ascii_case("ANALYTICS")
    {
        return Ok(None);
    }
    if !tokens[2].eq_ignore_ascii_case("SOURCE") {
        // Issue #789 — Analytics v0 explicitly excludes generic
        // `CREATE ANALYTICS …` objects (PRD #782 non-goal). The only
        // supported form starting with `CREATE ANALYTICS` is the
        // source-profile path `CREATE ANALYTICS SOURCE …` over an
        // ordinary backing collection. Surface the v0 boundary in the
        // message so accidental use does not look like a syntax glitch.
        return Err(RedDBError::Query(
            "CREATE ANALYTICS is not supported in Analytics v0 — \
             use CREATE METRIC <dotted.path> for the metric-centric \
             catalog, or CREATE ANALYTICS SOURCE … to register a \
             source profile over an ordinary collection \
             (PRD #782 non-goal)"
                .to_string(),
        ));
    }

    let mut cursor = 3;
    let name = take_ident(&tokens, &mut cursor, "analytics source name")?;
    expect_keyword(&tokens, &mut cursor, "ON")?;
    let collection = take_ident(&tokens, &mut cursor, "backing collection")?;

    let mut time_field = None;
    let mut event_field = None;
    let mut actor_field = None;
    let mut session_field = None;
    let mut properties_field = None;
    while cursor < tokens.len() {
        if consume_keyword(&tokens, &mut cursor, "TIME") {
            expect_keyword(&tokens, &mut cursor, "FIELD")?;
            time_field = Some(take_ident(&tokens, &mut cursor, "time field")?);
        } else if consume_keyword(&tokens, &mut cursor, "EVENT") {
            expect_keyword(&tokens, &mut cursor, "FIELD")?;
            event_field = Some(take_ident(&tokens, &mut cursor, "event field")?);
        } else if consume_keyword(&tokens, &mut cursor, "ACTOR") {
            expect_keyword(&tokens, &mut cursor, "FIELD")?;
            actor_field = Some(take_ident(&tokens, &mut cursor, "actor field")?);
        } else if consume_keyword(&tokens, &mut cursor, "SESSION") {
            expect_keyword(&tokens, &mut cursor, "FIELD")?;
            session_field = Some(take_ident(&tokens, &mut cursor, "session field")?);
        } else if consume_keyword(&tokens, &mut cursor, "PROPERTIES") {
            expect_keyword(&tokens, &mut cursor, "FIELD")?;
            properties_field = Some(take_ident(&tokens, &mut cursor, "properties field")?);
        } else {
            return Err(RedDBError::Query(format!(
                "analytics source DDL has unexpected token '{}'",
                tokens[cursor]
            )));
        }
    }

    Ok(Some(CreateAnalyticsSourceProfile {
        name,
        collection,
        time_field: time_field
            .ok_or_else(|| RedDBError::Query("analytics source requires TIME FIELD".to_string()))?,
        event_field: event_field.ok_or_else(|| {
            RedDBError::Query("analytics source requires EVENT FIELD".to_string())
        })?,
        actor_field: actor_field.ok_or_else(|| {
            RedDBError::Query("analytics source requires ACTOR FIELD".to_string())
        })?,
        session_field,
        properties_field,
    }))
}

pub fn create(
    store: &UnifiedStore,
    contracts: &[CollectionContract],
    input: CreateAnalyticsSourceProfile,
) -> RedDBResult<AnalyticsSourceProfile> {
    validate_identifier(&input.name, "analytics source name")?;
    validate_identifier(&input.collection, "backing collection")?;
    validate_identifier(&input.time_field, "time field")?;
    validate_identifier(&input.event_field, "event field")?;
    validate_identifier(&input.actor_field, "actor field")?;
    if let Some(field) = &input.session_field {
        validate_identifier(field, "session field")?;
    }
    if let Some(field) = &input.properties_field {
        validate_identifier(field, "properties field")?;
    }
    validate_collection(contracts, &input)?;

    let mut entries = load(store);
    if entries.iter().any(|entry| entry.name == input.name) {
        return Err(RedDBError::Query(format!(
            "analytics source '{}' already exists",
            input.name
        )));
    }

    let profile = AnalyticsSourceProfile {
        name: input.name,
        collection: input.collection,
        time_field: input.time_field,
        event_field: input.event_field,
        actor_field: input.actor_field,
        session_field: input.session_field,
        properties_field: input.properties_field,
        created_at_ms: now_ms(),
    };
    entries.push(profile.clone());
    save(store, &entries);
    Ok(profile)
}

pub fn list(store: &UnifiedStore) -> Vec<AnalyticsSourceProfile> {
    load(store)
}

fn validate_collection(
    contracts: &[CollectionContract],
    input: &CreateAnalyticsSourceProfile,
) -> RedDBResult<()> {
    let Some(contract) = contracts
        .iter()
        .find(|contract| contract.name == input.collection)
    else {
        return Err(RedDBError::Query(format!(
            "analytics source backing collection '{}' does not exist",
            input.collection
        )));
    };
    if !matches!(
        contract.declared_model,
        CollectionModel::Table | CollectionModel::Document
    ) {
        return Err(RedDBError::Query(format!(
            "analytics source backing collection '{}' must be a table or document collection",
            input.collection
        )));
    }

    if contract.declared_model == CollectionModel::Table {
        for field in required_fields(input) {
            if !contract
                .declared_columns
                .iter()
                .any(|column| column.name == field)
            {
                return Err(RedDBError::Query(format!(
                    "analytics source field '{field}' does not exist on backing collection '{}'",
                    input.collection
                )));
            }
        }
    }
    Ok(())
}

fn required_fields(input: &CreateAnalyticsSourceProfile) -> Vec<&str> {
    let mut fields = vec![
        input.time_field.as_str(),
        input.event_field.as_str(),
        input.actor_field.as_str(),
    ];
    if let Some(field) = &input.session_field {
        fields.push(field.as_str());
    }
    if let Some(field) = &input.properties_field {
        fields.push(field.as_str());
    }
    fields
}

fn validate_identifier(value: &str, label: &str) -> RedDBResult<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(RedDBError::Query(format!(
            "invalid analytics source {label} '{value}'"
        )));
    }
    Ok(())
}

fn consume_keyword(tokens: &[&str], cursor: &mut usize, expected: &str) -> bool {
    if tokens
        .get(*cursor)
        .is_some_and(|token| token.eq_ignore_ascii_case(expected))
    {
        *cursor += 1;
        return true;
    }
    false
}

fn expect_keyword(tokens: &[&str], cursor: &mut usize, expected: &str) -> RedDBResult<()> {
    if consume_keyword(tokens, cursor, expected) {
        return Ok(());
    }
    Err(RedDBError::Query(format!(
        "analytics source DDL expected {expected}"
    )))
}

fn take_ident(tokens: &[&str], cursor: &mut usize, label: &str) -> RedDBResult<String> {
    let Some(token) = tokens.get(*cursor) else {
        return Err(RedDBError::Query(format!(
            "analytics source DDL expected {label}"
        )));
    };
    *cursor += 1;
    Ok((*token).to_string())
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

fn load(store: &UnifiedStore) -> Vec<AnalyticsSourceProfile> {
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
        let Some(name) = lookup("name").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(collection) = lookup("collection").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(time_field) = lookup("time_field").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(event_field) = lookup("event_field").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(actor_field) = lookup("actor_field").and_then(JsonValue::as_str) else {
            continue;
        };
        let Some(created_at_ms) = lookup("created_at_ms").and_then(JsonValue::as_f64) else {
            continue;
        };
        out.push(AnalyticsSourceProfile {
            name: name.to_string(),
            collection: collection.to_string(),
            time_field: time_field.to_string(),
            event_field: event_field.to_string(),
            actor_field: actor_field.to_string(),
            session_field: lookup("session_field")
                .and_then(JsonValue::as_str)
                .map(str::to_string),
            properties_field: lookup("properties_field")
                .and_then(JsonValue::as_str)
                .map(str::to_string),
            created_at_ms: created_at_ms as u128,
        });
    }
    out
}

fn save(store: &UnifiedStore, entries: &[AnalyticsSourceProfile]) {
    let arr = crate::serde_json::Value::Array(entries.iter().map(entry_to_json).collect());
    store.set_config_tree(
        REGISTRY_KEY,
        &crate::serde_json::Value::String(arr.to_string()),
    );
}

fn entry_to_json(entry: &AnalyticsSourceProfile) -> crate::serde_json::Value {
    let mut obj = crate::serde_json::Map::new();
    obj.insert(
        "name".to_string(),
        crate::serde_json::Value::String(entry.name.clone()),
    );
    obj.insert(
        "collection".to_string(),
        crate::serde_json::Value::String(entry.collection.clone()),
    );
    obj.insert(
        "time_field".to_string(),
        crate::serde_json::Value::String(entry.time_field.clone()),
    );
    obj.insert(
        "event_field".to_string(),
        crate::serde_json::Value::String(entry.event_field.clone()),
    );
    obj.insert(
        "actor_field".to_string(),
        crate::serde_json::Value::String(entry.actor_field.clone()),
    );
    if let Some(field) = &entry.session_field {
        obj.insert(
            "session_field".to_string(),
            crate::serde_json::Value::String(field.clone()),
        );
    }
    if let Some(field) = &entry.properties_field {
        obj.insert(
            "properties_field".to_string(),
            crate::serde_json::Value::String(field.clone()),
        );
    }
    obj.insert(
        "created_at_ms".to_string(),
        crate::serde_json::Value::Number(entry.created_at_ms as f64),
    );
    crate::serde_json::Value::Object(obj)
}
