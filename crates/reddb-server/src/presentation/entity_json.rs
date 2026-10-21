use std::collections::BTreeSet;

use crate::application::CreateEntityOutput;
use crate::json::{to_string as json_to_string, Map, Value as JsonValue};
use crate::runtime::ScanPage;
use crate::storage::{CrossRef, EntityData, EntityKind, UnifiedEntity};
use reddb_types::Value;

pub(crate) fn created_entity_output_json(output: &CreateEntityOutput) -> JsonValue {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(true));
    object.insert("rid".to_string(), exact_u64_to_json(output.id.raw()));
    object.insert("id".to_string(), exact_u64_to_json(output.id.raw()));
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

pub(crate) use crate::entity_render::{
    base_entity_object, compact_entity_json, compact_entity_json_string, named_fields_json,
};

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

#[cfg(test)]
use crate::query_payload_render::single_key_object;
pub(crate) use crate::query_payload_render::{
    exact_u64_to_json, storage_json_bytes_to_json, storage_value_to_json,
};

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
                    Some(named) => JsonValue::Object(named_fields_json(named)),
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
                        .map(|(k, v)| {
                            (
                                k.clone(),
                                crate::runtime::query_exec::decode_stored_tag_value(v),
                            )
                        })
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

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn storage_value_to_json_preserves_exact_numbers() {
        assert_eq!(
            storage_value_to_json(&Value::Integer(9_007_199_254_740_993)),
            single_key_object("$int", JsonValue::String("9007199254740993".to_string()))
        );
        assert_eq!(
            storage_value_to_json(&Value::UnsignedInteger(i64::MAX as u64 + 1)),
            single_key_object(
                "$uint",
                JsonValue::String("9223372036854775808".to_string())
            )
        );
        assert_eq!(
            storage_value_to_json(&Value::Decimal(1_234_567)),
            single_key_object("$decimal", JsonValue::String("123.4567".to_string()))
        );
        assert_eq!(
            storage_value_to_json(&Value::DecimalText(
                "3.141592653589793238462643383279".to_string()
            )),
            single_key_object(
                "$decimal",
                JsonValue::String("3.141592653589793238462643383279".to_string())
            )
        );
    }

    #[test]
    fn keystone_json_is_byte_identical_to_presentation_json() {
        let document_fields = [
            Value::Integer(7),
            Value::DecimalText("18446744073709551616.0001".to_string()),
        ];
        let mut document_body = Vec::new();
        reddb_types::document_body_codec::encode(
            &[
                ("integer", &document_fields[0]),
                ("decimal", &document_fields[1]),
            ],
            &mut document_body,
        )
        .expect("representative document body should encode");

        let corpus = [
            Value::Null,
            Value::Integer(9_007_199_254_740_992),
            Value::UnsignedInteger(u64::MAX),
            Value::Float(1.25),
            Value::text("hello"),
            Value::Blob(vec![0x00, 0xff]),
            Value::Boolean(true),
            Value::Timestamp(1_700_000_000),
            Value::TimestampMs(1_700_000_000_123),
            Value::Duration(-500),
            Value::IpAddr(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            Value::MacAddr([0, 1, 2, 3, 4, 5]),
            Value::Vector(vec![1.5, -2.0]),
            Value::Json(br#"{"nested":[1,2.5]}"#.to_vec()),
            Value::Json(vec![0xff]),
            Value::Json(document_body),
            Value::Uuid([0; 16]),
            Value::VectorRef("embeddings".to_string(), 7),
            Value::RowRef("users".to_string(), 8),
            Value::Cidr(0xc0a8_0100, 24),
            Value::Decimal(1_234_567),
            Value::DecimalText("3.141592653589793238462643383279".to_string()),
            Value::Array(vec![
                Value::Boolean(false),
                Value::Array(vec![Value::Integer(2)]),
            ]),
            Value::GeoPoint(12_345_678, -87_654_321),
            Value::Money {
                asset_code: "BRL".to_string(),
                minor_units: 12_345,
                scale: 2,
            },
            Value::KeyRef("settings".to_string(), "theme".to_string()),
            Value::DocRef("profiles".to_string(), 9),
            Value::Secret(vec![1, 2, 3]),
            Value::Password("hash".to_string()),
        ];

        for value in corpus {
            let legacy = storage_value_to_json(&value).to_string_compact();
            let keystone = value.to_json().to_string_compact();
            assert_eq!(keystone.as_bytes(), legacy.as_bytes(), "{value:?}");
        }
    }
}
