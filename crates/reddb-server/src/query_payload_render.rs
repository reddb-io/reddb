//! Canonical query record/scalar JSON shared by execution budgets and transports.
//! Request selection, descriptors and response envelopes stay in presentation.
use crate::json::{from_slice as json_from_slice, Map, Value as JsonValue};
use crate::storage::query::unified::{
    GraphPath, MatchedEdge, MatchedNode, UnifiedRecord, VectorSearchResult,
};
use reddb_types::Value;
const MAX_JSON_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

pub(crate) fn storage_value_to_json(value: &Value) -> JsonValue {
    match value {
        Value::Null => JsonValue::Null,
        Value::Integer(value) => exact_i64_to_json(*value),
        Value::UnsignedInteger(value) => exact_u64_to_json(*value),
        Value::Float(value) if value.is_finite() => JsonValue::Number(*value),
        // Canonical query envelopes use the same lossless non-finite tags
        // understood by RedDB drivers. Bare NaN/Infinity are not RFC 8259
        // JSON and cannot be read back by `crate::json::from_slice`.
        Value::Float(value) => {
            let token = if value.is_nan() {
                "NaN"
            } else if value.is_sign_positive() {
                "Infinity"
            } else {
                "-Infinity"
            };
            single_key_object("$float", JsonValue::String(token.to_string()))
        }
        Value::Text(value) => JsonValue::String(value.to_string()),
        Value::Blob(value) => JsonValue::String(hex::encode(value)),
        Value::Boolean(value) => JsonValue::Bool(*value),
        Value::Timestamp(value) => exact_i64_to_json(*value),
        Value::Duration(value) => exact_i64_to_json(*value),
        Value::IpAddr(value) => JsonValue::String(value.to_string()),
        Value::MacAddr(value) => JsonValue::String(format_mac(value)),
        Value::Vector(value) => JsonValue::Array(
            value
                .iter()
                .map(|entry| JsonValue::Number(*entry as f64))
                .collect(),
        ),
        Value::Json(value) => storage_json_bytes_to_json(value),
        Value::Uuid(value) => JsonValue::String(hex::encode(value)),
        Value::NodeRef(value) => JsonValue::String(value.clone()),
        Value::EdgeRef(value) => JsonValue::String(value.clone()),
        Value::VectorRef(collection, id) => {
            let mut object = Map::new();
            object.insert(
                "collection".to_string(),
                JsonValue::String(collection.clone()),
            );
            object.insert("id".to_string(), exact_u64_to_json(*id));
            JsonValue::Object(object)
        }
        Value::RowRef(table, row_id) => {
            let mut object = Map::new();
            object.insert("table".to_string(), JsonValue::String(table.to_string()));
            object.insert("row_id".to_string(), exact_u64_to_json(*row_id));
            JsonValue::Object(object)
        }
        Value::Color([r, g, b]) => JsonValue::String(format!("#{:02X}{:02X}{:02X}", r, g, b)),
        Value::Email(s) => JsonValue::String(s.clone()),
        Value::Url(s) => JsonValue::String(s.clone()),
        Value::Phone(n) => exact_u64_to_json(*n),
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
        Value::Date(days) => JsonValue::Integer(i64::from(*days)),
        Value::Time(ms) => JsonValue::Integer(i64::from(*ms)),
        Value::Decimal(v) => {
            exact_decimal_to_json(reddb_types::Value::Decimal(*v).display_string())
        }
        Value::DecimalText(v) => exact_decimal_to_json(v.clone()),
        Value::EnumValue(i) => JsonValue::Integer(i64::from(*i)),
        Value::Array(elems) => JsonValue::Array(elems.iter().map(storage_value_to_json).collect()),
        Value::TimestampMs(ms) => exact_i64_to_json(*ms),
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
        Value::Port(p) => JsonValue::Integer(i64::from(*p)),
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
        Value::AssetCode(code) => JsonValue::String(code.clone()),
        Value::Money {
            asset_code,
            minor_units,
            scale,
        } => {
            let mut object = Map::new();
            object.insert(
                "asset_code".to_string(),
                JsonValue::String(asset_code.clone()),
            );
            object.insert("minor_units".to_string(), exact_i64_to_json(*minor_units));
            object.insert("scale".to_string(), exact_i64_to_json(i64::from(*scale)));
            JsonValue::Object(object)
        }
        Value::ColorAlpha([r, g, b, a]) => {
            JsonValue::String(format!("#{:02X}{:02X}{:02X}{:02X}", r, g, b, a))
        }
        Value::BigInt(v) => exact_i64_to_json(*v),
        Value::KeyRef(col, key) => {
            let mut object = Map::new();
            object.insert("collection".to_string(), JsonValue::String(col.clone()));
            object.insert("key".to_string(), JsonValue::String(key.clone()));
            JsonValue::Object(object)
        }
        Value::DocRef(col, id) => {
            let mut object = Map::new();
            object.insert("collection".to_string(), JsonValue::String(col.clone()));
            object.insert("id".to_string(), exact_u64_to_json(*id));
            JsonValue::Object(object)
        }
        Value::TableRef(name) => JsonValue::String(name.clone()),
        Value::PageRef(page_id) => exact_u64_to_json(u64::from(*page_id)),
        // Secrets and passwords are always masked in JSON output.
        // SELECT decryption (if the vault is unsealed) happens earlier
        // in the query pipeline and replaces Value::Secret with
        // Value::Text before serialization.
        Value::Secret(_) => JsonValue::String("***".to_string()),
        Value::Password(_) => JsonValue::String("***".to_string()),
    }
}

fn exact_i64_to_json(value: i64) -> JsonValue {
    if (-MAX_JSON_SAFE_INTEGER..=MAX_JSON_SAFE_INTEGER).contains(&value) {
        JsonValue::Integer(value)
    } else {
        single_key_object("$int", JsonValue::String(value.to_string()))
    }
}

pub(crate) fn exact_u64_to_json(value: u64) -> JsonValue {
    if value <= MAX_JSON_SAFE_INTEGER as u64 {
        JsonValue::Integer(value as i64)
    } else {
        single_key_object("$uint", JsonValue::String(value.to_string()))
    }
}

fn exact_decimal_to_json(value: String) -> JsonValue {
    single_key_object("$decimal", JsonValue::String(value))
}

pub(crate) fn single_key_object(key: &str, value: JsonValue) -> JsonValue {
    JsonValue::Object([(key.to_string(), value)].into_iter().collect())
}

pub(crate) fn storage_json_bytes_to_json(bytes: &[u8]) -> JsonValue {
    // A document body may be stored as the native binary container (PRD-1398);
    // decode it back to JSON for the wire. Plain-JSON bytes fall through.
    if let Some(body) = crate::document_body::decode_container_to_json(bytes) {
        return body;
    }
    json_from_slice::<JsonValue>(bytes).unwrap_or_else(|_| {
        JsonValue::Object(
            [
                (
                    "code".to_string(),
                    JsonValue::String("INVALID_JSON".to_string()),
                ),
                ("hex".to_string(), JsonValue::String(hex::encode(bytes))),
            ]
            .into_iter()
            .collect(),
        )
    })
}

fn format_mac(bytes: &[u8; 6]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

pub(crate) fn unified_record_json(record: &UnifiedRecord, columns: &[String]) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "values".to_string(),
        JsonValue::Object(projected_values_json(record, columns)),
    );
    let meta = record_metadata_json(record, columns);
    if !meta.is_empty() {
        object.insert("meta".to_string(), JsonValue::Object(meta));
    }
    object.insert(
        "nodes".to_string(),
        JsonValue::Object(
            record
                .nodes
                .iter()
                .map(|(key, value)| (key.clone(), matched_node_json(value)))
                .collect(),
        ),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Object(
            record
                .edges
                .iter()
                .map(|(key, value)| (key.clone(), matched_edge_json(value)))
                .collect(),
        ),
    );
    object.insert(
        "paths".to_string(),
        JsonValue::Array(record.paths.iter().map(graph_path_json).collect()),
    );
    object.insert(
        "vector_results".to_string(),
        JsonValue::Array(
            record
                .vector_results
                .iter()
                .map(vector_search_result_json)
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn projected_values_json(record: &UnifiedRecord, columns: &[String]) -> Map<String, JsonValue> {
    if columns.is_empty() {
        return record
            .iter_fields()
            .map(|(key, value)| (key.to_string(), storage_value_to_json(value)))
            .collect();
    }

    columns
        .iter()
        .filter_map(|column| {
            record
                .get(column)
                .map(|value| (column.clone(), storage_value_to_json(value)))
        })
        .collect()
}

fn record_metadata_json(record: &UnifiedRecord, columns: &[String]) -> Map<String, JsonValue> {
    record
        .iter_fields()
        .filter_map(|(key, value)| {
            let key = key.as_ref();
            if columns.iter().any(|column| column == key) || !is_record_metadata_key(key) {
                return None;
            }
            Some((key.to_string(), storage_value_to_json(value)))
        })
        .collect()
}

fn is_record_metadata_key(key: &str) -> bool {
    matches!(
        key,
        "rid"
            | "collection"
            | "red_collection"
            | "kind"
            | "red_kind"
            | "tenant"
            | "created_at"
            | "updated_at"
            | "row_id"
            | "red_sequence_id"
            | "red_entity_type"
            | "red_capabilities"
    )
}

fn matched_node_json(node: &MatchedNode) -> JsonValue {
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::String(node.id.clone()));
    object.insert("label".to_string(), JsonValue::String(node.label.clone()));
    object.insert(
        "node_type".to_string(),
        JsonValue::String(node.node_label.clone()),
    );
    JsonValue::Object(object)
}

fn matched_edge_json(edge: &MatchedEdge) -> JsonValue {
    let mut object = Map::new();
    object.insert("from".to_string(), JsonValue::String(edge.from.clone()));
    object.insert("to".to_string(), JsonValue::String(edge.to.clone()));
    object.insert(
        "edge_type".to_string(),
        JsonValue::String(edge.edge_label.clone()),
    );
    object.insert("weight".to_string(), JsonValue::Number(edge.weight as f64));
    JsonValue::Object(object)
}

fn graph_path_json(path: &GraphPath) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "nodes".to_string(),
        JsonValue::Array(path.nodes.iter().cloned().map(JsonValue::String).collect()),
    );
    object.insert(
        "edges".to_string(),
        JsonValue::Array(path.edges.iter().map(matched_edge_json).collect()),
    );
    object.insert(
        "total_weight".to_string(),
        JsonValue::Number(path.total_weight as f64),
    );
    JsonValue::Object(object)
}

fn vector_search_result_json(result: &VectorSearchResult) -> JsonValue {
    let score = 1.0 / (1.0 + result.distance);
    let mut object = Map::new();
    object.insert("id".to_string(), JsonValue::Number(result.id as f64));
    object.insert("entity_id".to_string(), JsonValue::Number(result.id as f64));
    object.insert("rid".to_string(), JsonValue::Number(result.id as f64));
    object.insert(
        "collection".to_string(),
        JsonValue::String(result.collection.clone()),
    );
    object.insert(
        "red_collection".to_string(),
        JsonValue::String(result.collection.clone()),
    );
    object.insert(
        "red_kind".to_string(),
        JsonValue::String("vector".to_string()),
    );
    object.insert(
        "red_entity_type".to_string(),
        JsonValue::String("vector".to_string()),
    );
    object.insert(
        "red_capabilities".to_string(),
        JsonValue::String("vector,similarity,embedding".to_string()),
    );
    object.insert("_score".to_string(), JsonValue::Number(score as f64));
    object.insert("final_score".to_string(), JsonValue::Number(score as f64));
    object.insert(
        "distance".to_string(),
        JsonValue::Number(result.distance as f64),
    );
    object.insert(
        "_distance".to_string(),
        JsonValue::Number(result.distance as f64),
    );
    object.insert(
        "vector_distance".to_string(),
        JsonValue::Number(result.distance as f64),
    );
    object.insert(
        "vector".to_string(),
        match &result.vector {
            Some(vector) => JsonValue::Array(
                vector
                    .iter()
                    .map(|value| JsonValue::Number(*value as f64))
                    .collect(),
            ),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "metadata".to_string(),
        match &result.metadata {
            Some(metadata) => JsonValue::Object(
                metadata
                    .iter()
                    .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
                    .collect(),
            ),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "linked_node".to_string(),
        match &result.linked_node {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "linked_row".to_string(),
        match &result.linked_row {
            Some((table, row_id)) => {
                let mut linked = Map::new();
                linked.insert("table".to_string(), JsonValue::String(table.clone()));
                linked.insert("row_id".to_string(), JsonValue::Number(*row_id as f64));
                JsonValue::Object(linked)
            }
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}
