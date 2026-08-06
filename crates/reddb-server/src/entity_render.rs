//! Entity JSON rendering shared by the engine and the presentation layer.
//!
//! Lives at the crate root rather than under `presentation` so runtime code
//! can render an entity without importing the presentation layer (Spec #2111:
//! the dependency arrow points one way). `presentation::entity_json`
//! re-exports these so its own surface is unchanged.

use std::collections::{BTreeSet, HashMap};

use crate::json::{to_string as json_to_string, Map, Value as JsonValue};
use reddb_types::Value;
use crate::storage::{EntityData, EntityKind, RowData, UnifiedEntity};

pub(crate) fn compact_entity_json(entity: &UnifiedEntity) -> JsonValue {
    let mut object = base_entity_object(entity);
    append_compact_entity_fields(&mut object, &entity.data);
    JsonValue::Object(object)
}

pub(crate) fn compact_entity_json_string(entity: &UnifiedEntity) -> String {
    json_to_string(&compact_entity_json(entity)).unwrap_or_else(|_| "{}".to_string())
}

pub(crate) fn base_entity_object(entity: &UnifiedEntity) -> Map<String, JsonValue> {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::Number(entity.id.raw() as f64));
    object.insert(
        "kind".to_string(),
        JsonValue::String(entity.kind.storage_type().to_string()),
    );
    object.insert(
        "collection".to_string(),
        JsonValue::String(entity.kind.collection().to_string()),
    );
    object.insert(
        "red_entity_type".to_string(),
        JsonValue::String(entity_type(entity).to_string()),
    );
    object.insert(
        "red_capabilities".to_string(),
        JsonValue::Array(
            entity_capabilities(entity)
                .into_iter()
                .map(JsonValue::String)
                .collect(),
        ),
    );
    object
}

fn append_compact_entity_fields(object: &mut Map<String, JsonValue>, data: &EntityData) {
    match data {
        EntityData::Row(row) => {
            if let Some(named) = &row.named {
                object.insert(
                    "row".to_string(),
                    JsonValue::Object(named_fields_json(named)),
                );
            }
        }
        EntityData::Node(node) => {
            object.insert(
                "properties".to_string(),
                JsonValue::Object(
                    node.properties
                        .iter()
                        .map(|(key, value)| (key.clone(), value.to_json()))
                        .collect(),
                ),
            );
        }
        EntityData::Edge(edge) => {
            object.insert("weight".to_string(), JsonValue::Number(edge.weight as f64));
            object.insert(
                "properties".to_string(),
                JsonValue::Object(
                    edge.properties
                        .iter()
                        .map(|(key, value)| (key.clone(), value.to_json()))
                        .collect(),
                ),
            );
        }
        EntityData::Vector(vector) => {
            object.insert(
                "dense".to_string(),
                JsonValue::Array(
                    vector
                        .dense
                        .iter()
                        .map(|value| JsonValue::Number(*value as f64))
                        .collect(),
                ),
            );
            if let Some(content) = &vector.content {
                object.insert("content".to_string(), JsonValue::String(content.clone()));
            }
        }
        EntityData::TimeSeries(ts) => {
            object.insert("metric".to_string(), JsonValue::String(ts.metric.clone()));
            object.insert(
                "timestamp_ns".to_string(),
                JsonValue::Number(ts.timestamp_ns as f64),
            );
            object.insert("value".to_string(), JsonValue::Number(ts.value));
        }
        EntityData::QueueMessage(msg) => {
            object.insert("payload".to_string(), msg.payload.to_json());
            object.insert(
                "attempts".to_string(),
                JsonValue::Number(msg.attempts as f64),
            );
            object.insert("acked".to_string(), JsonValue::Bool(msg.acked));
        }
    }
}

fn entity_type(entity: &UnifiedEntity) -> &'static str {
    match (&entity.kind, &entity.data) {
        (EntityKind::TableRow { .. }, EntityData::Row(row)) if row_is_kv(row) => "kv",
        (EntityKind::TableRow { .. }, EntityData::Row(_)) => "table",
        (EntityKind::GraphNode(_), EntityData::Node(_)) => "graph_node",
        (EntityKind::GraphEdge(_), EntityData::Edge(_)) => "graph_edge",
        (EntityKind::Vector { .. }, EntityData::Vector(_)) => "vector",
        (EntityKind::TimeSeriesPoint(_), EntityData::TimeSeries(_)) => "timeseries",
        _ => "unknown",
    }
}

fn entity_capabilities(entity: &UnifiedEntity) -> Vec<String> {
    let capabilities: BTreeSet<String> = match (&entity.kind, &entity.data) {
        (EntityKind::TableRow { .. }, EntityData::Row(row)) => {
            let mut values = BTreeSet::from(["table".to_string(), "structured".to_string()]);
            if row_is_kv(row) {
                values.insert("kv".to_string());
            }
            let is_document_like = row
                .named
                .as_ref()
                .map(|named| named.values().any(documentish_value))
                .unwrap_or(false)
                || row.columns.iter().any(documentish_value);
            if is_document_like {
                values.insert("document".to_string());
            }
            values
        }
        (EntityKind::GraphNode(_), EntityData::Node(_)) => {
            BTreeSet::from(["graph".to_string(), "graph_node".to_string()])
        }
        (EntityKind::GraphEdge(_), EntityData::Edge(_)) => {
            BTreeSet::from(["graph".to_string(), "graph_edge".to_string()])
        }
        (EntityKind::Vector { .. }, EntityData::Vector(_)) => BTreeSet::from([
            "vector".to_string(),
            "similarity".to_string(),
            "embedding".to_string(),
        ]),
        (EntityKind::TimeSeriesPoint(_), EntityData::TimeSeries(_)) => BTreeSet::from([
            "document".to_string(),
            "timeseries".to_string(),
            "metric".to_string(),
            "temporal".to_string(),
        ]),
        _ => BTreeSet::new(),
    };
    capabilities.into_iter().collect()
}

fn documentish_value(value: &Value) -> bool {
    matches!(value, Value::Json(_) | Value::Blob(_))
}

fn row_is_kv(row: &RowData) -> bool {
    let Some(named) = row.named.as_ref() else {
        return false;
    };

    if named.len() == 2 {
        named.contains_key("key") && named.contains_key("value")
    } else if named.len() == 1 {
        named.contains_key("key") || named.contains_key("value")
    } else {
        false
    }
}

pub(crate) fn named_fields_json(named: &HashMap<String, Value>) -> Map<String, JsonValue> {
    let mut out: Map<String, JsonValue> = named
        .iter()
        .map(|(key, value)| (key.clone(), value.to_json()))
        .collect();
    if let Some(Value::Json(bytes)) = named.get("body") {
        if let Some(fields) = crate::document_body::body_fields(bytes) {
            for (key, value) in fields {
                out.entry(key).or_insert_with(|| value.to_json());
            }
        }
    }
    out
}
