use std::collections::BTreeSet;

use crate::application::CreateEntityOutput;
use crate::json::{
    from_slice as json_from_slice, to_string as json_to_string, Map, Value as JsonValue,
};
use crate::runtime::ScanPage;
use crate::storage::schema::Value;
use crate::storage::{CrossRef, EntityData, EntityKind, UnifiedEntity};

pub(crate) fn created_entity_output_json(output: &CreateEntityOutput) -> JsonValue {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(true));
    object.insert("id".to_string(), JsonValue::Number(output.id.raw() as f64));
    object.insert(
        "entity".to_string(),
        output
            .entity
            .as_ref()
            .map(entity_json)
            .unwrap_or(JsonValue::Null),
    );
    JsonValue::Object(object)
}

pub(crate) fn scan_page_json(page: &ScanPage) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(page.collection.clone()),
    );
    object.insert("total".to_string(), JsonValue::Number(page.total as f64));
    object.insert(
        "next_offset".to_string(),
        match page.next {
            Some(cursor) => JsonValue::Number(cursor.offset as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "items".to_string(),
        JsonValue::Array(page.items.iter().map(entity_json).collect()),
    );
    JsonValue::Object(object)
}

pub(crate) fn entity_json(entity: &UnifiedEntity) -> JsonValue {
    let mut object = base_entity_object(entity);
    object.insert("identity".to_string(), entity_kind_json(&entity.kind));
    object.insert("data".to_string(), entity_data_json(&entity.data));
    object.insert(
        "cross_refs".to_string(),
        JsonValue::Array(entity.cross_refs().iter().map(cross_ref_json).collect()),
    );
    JsonValue::Object(object)
}

pub(crate) fn compact_entity_json(entity: &UnifiedEntity) -> JsonValue {
    let mut object = base_entity_object(entity);
    append_compact_entity_fields(&mut object, &entity.data);
    JsonValue::Object(object)
}

pub(crate) fn compact_entity_json_string(entity: &UnifiedEntity) -> String {
    json_to_string(&compact_entity_json(entity)).unwrap_or_else(|_| "{}".to_string())
}

pub(crate) fn storage_value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::Null => JsonValue::Null,
        Value::Integer(value) => JsonValue::Number(*value as f64),
        Value::UnsignedInteger(value) => JsonValue::Number(*value as f64),
        Value::Float(value) => JsonValue::Number(*value),
        Value::Text(value) => JsonValue::String(value.clone()),
        Value::Blob(value) => JsonValue::String(hex::encode(value)),
        Value::Boolean(value) => JsonValue::Bool(*value),
        Value::Timestamp(value) => JsonValue::Number(*value as f64),
        Value::Duration(value) => JsonValue::Number(*value as f64),
        Value::IpAddr(value) => JsonValue::String(value.to_string()),
        Value::MacAddr(value) => JsonValue::String(format_mac(value)),
        Value::Vector(value) => JsonValue::Array(
            value
                .iter()
                .map(|entry| JsonValue::Number(*entry as f64))
                .collect(),
        ),
        Value::Json(value) => json_from_slice::<JsonValue>(value)
            .unwrap_or_else(|_| JsonValue::String(hex::encode(value))),
        Value::Uuid(value) => JsonValue::String(hex::encode(value)),
        Value::NodeRef(value) => JsonValue::String(value.clone()),
        Value::EdgeRef(value) => JsonValue::String(value.clone()),
        Value::VectorRef(collection, id) => {
            let mut object = Map::new();
            object.insert(
                "collection".to_string(),
                JsonValue::String(collection.clone()),
            );
            object.insert("id".to_string(), JsonValue::Number(*id as f64));
            JsonValue::Object(object)
        }
        Value::RowRef(table, row_id) => {
            let mut object = Map::new();
            object.insert("table".to_string(), JsonValue::String(table.to_string()));
            object.insert("row_id".to_string(), JsonValue::Number(*row_id as f64));
            JsonValue::Object(object)
        }
        Value::Color([r, g, b]) => JsonValue::String(format!("#{:02X}{:02X}{:02X}", r, g, b)),
        Value::Email(s) => JsonValue::String(s.clone()),
        Value::Url(s) => JsonValue::String(s.clone()),
        Value::Phone(n) => JsonValue::Number(*n as f64),
        Value::Semver(packed) => JsonValue::String(format!(
            "{}.{}.{}",
            packed / 1_000_000,
            (packed / 1_000) % 1_000,
            packed % 1_000
        )),
        Value::Cidr(ip, prefix) => JsonValue::String(format!(
            "{}.{}.{}.{}/{}",
            (ip >> 24) & 0xFF,
            (ip >> 16) & 0xFF,
            (ip >> 8) & 0xFF,
            ip & 0xFF,
            prefix
        )),
        Value::Date(days) => JsonValue::Number(*days as f64),
        Value::Time(ms) => JsonValue::Number(*ms as f64),
        Value::Decimal(v) => JsonValue::Number(*v as f64 / 10_000.0),
        Value::EnumValue(i) => JsonValue::Number(*i as f64),
        Value::Array(elems) => JsonValue::Array(elems.iter().map(storage_value_to_json).collect()),
        Value::TimestampMs(ms) => JsonValue::Number(*ms as f64),
        Value::Ipv4(ip) => JsonValue::String(format!(
            "{}.{}.{}.{}",
            (ip >> 24) & 0xFF,
            (ip >> 16) & 0xFF,
            (ip >> 8) & 0xFF,
            ip & 0xFF
        )),
        Value::Ipv6(bytes) => JsonValue::String(format!("{}", std::net::Ipv6Addr::from(*bytes))),
        Value::Subnet(ip, mask) => {
            let prefix = mask.leading_ones();
            JsonValue::String(format!(
                "{}.{}.{}.{}/{}",
                (ip >> 24) & 0xFF,
                (ip >> 16) & 0xFF,
                (ip >> 8) & 0xFF,
                ip & 0xFF,
                prefix
            ))
        }
        Value::Port(p) => JsonValue::Number(*p as f64),
        Value::Latitude(micro) => JsonValue::Number(*micro as f64 / 1_000_000.0),
        Value::Longitude(micro) => JsonValue::Number(*micro as f64 / 1_000_000.0),
        Value::GeoPoint(lat, lon) => JsonValue::String(format!(
            "{:.6},{:.6}",
            *lat as f64 / 1_000_000.0,
            *lon as f64 / 1_000_000.0
        )),
        Value::Country2(c) => JsonValue::String(String::from_utf8_lossy(c).to_string()),
        Value::Country3(c) => JsonValue::String(String::from_utf8_lossy(c).to_string()),
        Value::Lang2(c) => JsonValue::String(String::from_utf8_lossy(c).to_string()),
        Value::Lang5(c) => JsonValue::String(String::from_utf8_lossy(c).to_string()),
        Value::Currency(c) => JsonValue::String(String::from_utf8_lossy(c).to_string()),
        Value::ColorAlpha([r, g, b, a]) => {
            JsonValue::String(format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a))
        }
        Value::BigInt(v) => JsonValue::Number(*v as f64),
        Value::KeyRef(col, key) => {
            let mut object = Map::new();
            object.insert("collection".to_string(), JsonValue::String(col.clone()));
            object.insert("key".to_string(), JsonValue::String(key.clone()));
            JsonValue::Object(object)
        }
        Value::DocRef(col, id) => {
            let mut object = Map::new();
            object.insert("collection".to_string(), JsonValue::String(col.clone()));
            object.insert("id".to_string(), JsonValue::Number(*id as f64));
            JsonValue::Object(object)
        }
        Value::TableRef(name) => JsonValue::String(name.clone()),
        Value::PageRef(page_id) => JsonValue::Number(*page_id as f64),
        // Secrets and passwords are always masked in JSON output.
        // SELECT decryption (if the vault is unsealed) happens earlier
        // in the query pipeline and replaces Value::Secret with
        // Value::Text before serialization.
        Value::Secret(_) => JsonValue::String("***".to_string()),
        Value::Password(_) => JsonValue::String("***".to_string()),
    }
}

fn base_entity_object(entity: &UnifiedEntity) -> Map<String, JsonValue> {
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
                    JsonValue::Object(
                        named
                            .iter()
                            .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
                            .collect(),
                    ),
                );
            }
        }
        EntityData::Node(node) => {
            object.insert(
                "properties".to_string(),
                JsonValue::Object(
                    node.properties
                        .iter()
                        .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
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
                        .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
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
            object.insert("payload".to_string(), storage_value_to_json(&msg.payload));
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

fn row_is_kv(row: &crate::storage::RowData) -> bool {
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

fn entity_kind_json(kind: &EntityKind) -> JsonValue {
    let mut object = Map::new();
    match kind {
        EntityKind::TableRow { table, row_id } => {
            object.insert("table".to_string(), JsonValue::String(table.to_string()));
            object.insert("row_id".to_string(), JsonValue::Number(*row_id as f64));
        }
        EntityKind::GraphNode(ref node) => {
            object.insert("label".to_string(), JsonValue::String(node.label.clone()));
            object.insert(
                "node_type".to_string(),
                JsonValue::String(node.node_type.clone()),
            );
        }
        EntityKind::GraphEdge(ref edge) => {
            object.insert("label".to_string(), JsonValue::String(edge.label.clone()));
            object.insert(
                "from_node".to_string(),
                JsonValue::String(edge.from_node.clone()),
            );
            object.insert(
                "to_node".to_string(),
                JsonValue::String(edge.to_node.clone()),
            );
            object.insert("weight".to_string(), JsonValue::Number(edge.weight as f64));
        }
        EntityKind::Vector { collection } => {
            object.insert(
                "collection".to_string(),
                JsonValue::String(collection.clone()),
            );
        }
        EntityKind::TimeSeriesPoint(ref ts) => {
            object.insert("series".to_string(), JsonValue::String(ts.series.clone()));
            object.insert("metric".to_string(), JsonValue::String(ts.metric.clone()));
        }
        EntityKind::QueueMessage { queue, position } => {
            object.insert("queue".to_string(), JsonValue::String(queue.clone()));
            object.insert("position".to_string(), JsonValue::Number(*position as f64));
        }
    }
    JsonValue::Object(object)
}

fn entity_data_json(data: &EntityData) -> JsonValue {
    let mut object = Map::new();
    match data {
        EntityData::Row(row) => {
            object.insert(
                "columns".to_string(),
                JsonValue::Array(row.columns.iter().map(storage_value_to_json).collect()),
            );
            object.insert(
                "named".to_string(),
                match &row.named {
                    Some(named) => JsonValue::Object(
                        named
                            .iter()
                            .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
                            .collect(),
                    ),
                    None => JsonValue::Null,
                },
            );
        }
        EntityData::Node(node) => {
            object.insert(
                "properties".to_string(),
                JsonValue::Object(
                    node.properties
                        .iter()
                        .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
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
                        .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
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
            object.insert(
                "sparse".to_string(),
                match &vector.sparse {
                    Some(sparse) => {
                        let mut sparse_object = Map::new();
                        sparse_object.insert(
                            "indices".to_string(),
                            JsonValue::Array(
                                sparse
                                    .indices
                                    .iter()
                                    .map(|value| JsonValue::Number(*value as f64))
                                    .collect(),
                            ),
                        );
                        sparse_object.insert(
                            "values".to_string(),
                            JsonValue::Array(
                                sparse
                                    .values
                                    .iter()
                                    .map(|value| JsonValue::Number(*value as f64))
                                    .collect(),
                            ),
                        );
                        JsonValue::Object(sparse_object)
                    }
                    None => JsonValue::Null,
                },
            );
            object.insert(
                "content".to_string(),
                match &vector.content {
                    Some(content) => JsonValue::String(content.clone()),
                    None => JsonValue::Null,
                },
            );
        }
        EntityData::TimeSeries(ts) => {
            object.insert("metric".to_string(), JsonValue::String(ts.metric.clone()));
            object.insert(
                "timestamp_ns".to_string(),
                JsonValue::Number(ts.timestamp_ns as f64),
            );
            object.insert("value".to_string(), JsonValue::Number(ts.value));
            object.insert(
                "tags".to_string(),
                JsonValue::Object(
                    ts.tags
                        .iter()
                        .map(|(k, v)| (k.clone(), JsonValue::String(v.clone())))
                        .collect(),
                ),
            );
        }
        EntityData::QueueMessage(msg) => {
            object.insert("payload".to_string(), storage_value_to_json(&msg.payload));
            if let Some(priority) = msg.priority {
                object.insert("priority".to_string(), JsonValue::Number(priority as f64));
            }
            object.insert(
                "enqueued_at_ns".to_string(),
                JsonValue::Number(msg.enqueued_at_ns as f64),
            );
            object.insert(
                "attempts".to_string(),
                JsonValue::Number(msg.attempts as f64),
            );
            object.insert(
                "max_attempts".to_string(),
                JsonValue::Number(msg.max_attempts as f64),
            );
            object.insert("acked".to_string(), JsonValue::Bool(msg.acked));
        }
    }
    JsonValue::Object(object)
}

fn cross_ref_json(cross_ref: &CrossRef) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "source".to_string(),
        JsonValue::Number(cross_ref.source.raw() as f64),
    );
    object.insert(
        "target".to_string(),
        JsonValue::Number(cross_ref.target.raw() as f64),
    );
    object.insert(
        "target_collection".to_string(),
        JsonValue::String(cross_ref.target_collection.clone()),
    );
    object.insert(
        "ref_type".to_string(),
        JsonValue::String(format!("{:?}", cross_ref.ref_type)),
    );
    object.insert(
        "weight".to_string(),
        JsonValue::Number(cross_ref.weight as f64),
    );
    object.insert(
        "created_at".to_string(),
        JsonValue::Number(cross_ref.created_at as f64),
    );
    JsonValue::Object(object)
}

fn format_mac(bytes: &[u8; 6]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}
