use crate::application::entity::{
    json_to_metadata_value, json_to_storage_value, CreateEdgeInput, CreateNodeEmbeddingInput,
    CreateNodeGraphLinkInput, CreateNodeInput, CreateNodeTableLinkInput, CreateRowInput,
    CreateVectorInput,
};
use crate::json::{Map, Value as JsonValue};
use crate::storage::schema::Value;
use crate::storage::unified::devx::refs::{NodeRef, TableRef, VectorRef};
use crate::storage::unified::MetadataValue;
use crate::storage::EntityId;
use crate::{RedDBError, RedDBResult};

pub(crate) fn parse_create_row_input(
    collection: String,
    payload: &JsonValue,
) -> RedDBResult<CreateRowInput> {
    let fields = parse_required_value_map(payload, "fields", "row create payload")?;
    let metadata = parse_metadata_entries(payload)?;
    let mut node_links = Vec::new();
    let mut vector_links = Vec::new();

    if let Some(links) = payload.get("links").and_then(JsonValue::as_object) {
        if let Some(nodes) = links.get("nodes").and_then(JsonValue::as_array) {
            for node in nodes {
                let (target_collection, id) = parse_collection_entity_ref(node, "row node link")?;
                node_links.push(NodeRef::new(target_collection, EntityId::new(id)));
            }
        }
        if let Some(vectors) = links.get("vectors").and_then(JsonValue::as_array) {
            for vector in vectors {
                let (target_collection, id) =
                    parse_collection_entity_ref(vector, "row vector link")?;
                vector_links.push(VectorRef::new(target_collection, EntityId::new(id)));
            }
        }
    }

    Ok(CreateRowInput {
        collection,
        fields,
        metadata,
        node_links,
        vector_links,
    })
}

pub(crate) fn parse_create_node_input(
    collection: String,
    payload: &JsonValue,
) -> RedDBResult<CreateNodeInput> {
    let label = payload
        .get("label")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            RedDBError::Query("payload must contain a string field named 'label'".to_string())
        })?;
    let properties = parse_optional_value_map(payload, &["properties", "fields"])?;
    let metadata = parse_metadata_entries(payload)?;
    let embeddings = parse_node_embeddings(payload)?;
    let mut table_links = Vec::new();
    let mut node_links = Vec::new();

    if let Some(links) = payload.get("links").and_then(JsonValue::as_object) {
        if let Some(tables) = links.get("tables").and_then(JsonValue::as_array) {
            for table in tables {
                let object = table
                    .as_object()
                    .ok_or_else(|| RedDBError::Query("table links must be objects".to_string()))?;
                let key = object
                    .get("key")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| RedDBError::Query("table links require 'key'".to_string()))?;
                let table_name = object
                    .get("table")
                    .and_then(JsonValue::as_str)
                    .ok_or_else(|| RedDBError::Query("table links require 'table'".to_string()))?;
                let row_id = parse_required_u64_field(object, "row_id", "table links")?;
                table_links.push(CreateNodeTableLinkInput {
                    key: key.to_string(),
                    table: TableRef::new(table_name, row_id),
                });
            }
        }
        if let Some(nodes) = links.get("nodes").and_then(JsonValue::as_array) {
            for node in nodes {
                let object = node
                    .as_object()
                    .ok_or_else(|| RedDBError::Query("node links must be objects".to_string()))?;
                let target = parse_required_u64_field(object, "id", "node links")?;
                let edge_label = object
                    .get("edge_label")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("RELATED_TO");
                let weight = object
                    .get("weight")
                    .and_then(JsonValue::as_f64)
                    .unwrap_or(1.0);
                node_links.push(CreateNodeGraphLinkInput {
                    target: EntityId::new(target),
                    edge_label: edge_label.to_string(),
                    weight: weight as f32,
                });
            }
        }
    }

    Ok(CreateNodeInput {
        collection,
        label: label.to_string(),
        node_type: payload
            .get("node_type")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        properties,
        metadata,
        embeddings,
        table_links,
        node_links,
    })
}

pub(crate) fn parse_create_edge_input(
    collection: String,
    payload: &JsonValue,
) -> RedDBResult<CreateEdgeInput> {
    let label = payload
        .get("label")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| {
            RedDBError::Query("payload must contain a string field named 'label'".to_string())
        })?;
    let from = parse_required_u64_json(payload, "from", "edge create payload")?;
    let to = parse_required_u64_json(payload, "to", "edge create payload")?;

    Ok(CreateEdgeInput {
        collection,
        label: label.to_string(),
        from: EntityId::new(from),
        to: EntityId::new(to),
        weight: payload
            .get("weight")
            .and_then(JsonValue::as_f64)
            .map(|value| value as f32),
        properties: parse_optional_value_map(payload, &["properties", "fields"])?,
        metadata: parse_metadata_entries(payload)?,
    })
}

pub(crate) fn parse_create_vector_input(
    collection: String,
    payload: &JsonValue,
) -> RedDBResult<CreateVectorInput> {
    let dense = parse_required_f32_array(payload, "dense", "vector create payload")?;
    let metadata = parse_metadata_entries(payload)?;
    let mut link_row = None;
    let mut link_node = None;

    if let Some(link) = payload.get("link").and_then(JsonValue::as_object) {
        if let Some(row) = link.get("row") {
            let object = row.as_object().ok_or_else(|| {
                RedDBError::Query("vector row link must be an object".to_string())
            })?;
            let table = object
                .get("table")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| RedDBError::Query("vector row link requires 'table'".to_string()))?;
            let row_id = parse_required_u64_field(object, "row_id", "vector row link")?;
            link_row = Some(TableRef::new(table, row_id));
        }
        if let Some(node) = link.get("node") {
            let (target_collection, id) = parse_collection_entity_ref(node, "vector node link")?;
            link_node = Some(NodeRef::new(target_collection, EntityId::new(id)));
        }
    }

    Ok(CreateVectorInput {
        collection,
        dense,
        content: payload
            .get("content")
            .and_then(JsonValue::as_str)
            .map(str::to_string),
        metadata,
        link_row,
        link_node,
    })
}

fn parse_optional_value_map(
    payload: &JsonValue,
    fields: &[&str],
) -> RedDBResult<Vec<(String, Value)>> {
    for field in fields {
        if let Some(object) = payload.get(field).and_then(JsonValue::as_object) {
            let mut out = Vec::with_capacity(object.len());
            for (key, value) in object {
                out.push((key.clone(), json_to_storage_value(value)?));
            }
            return Ok(out);
        }
    }
    Ok(Vec::new())
}

fn parse_required_value_map(
    payload: &JsonValue,
    field: &str,
    context: &str,
) -> RedDBResult<Vec<(String, Value)>> {
    let object = payload
        .get(field)
        .and_then(JsonValue::as_object)
        .ok_or_else(|| {
            RedDBError::Query(format!(
                "{context} must contain an object field named '{field}'"
            ))
        })?;
    let mut out = Vec::with_capacity(object.len());
    for (key, value) in object {
        out.push((key.clone(), json_to_storage_value(value)?));
    }
    Ok(out)
}

fn parse_metadata_entries(payload: &JsonValue) -> RedDBResult<Vec<(String, MetadataValue)>> {
    let mut out = Vec::new();
    if let Some(metadata) = payload.get("metadata").and_then(JsonValue::as_object) {
        out.reserve(metadata.len());
        for (key, value) in metadata {
            out.push((key.clone(), json_to_metadata_value(value)?));
        }
    }

    for field in ["_ttl", "_ttl_ms", "_expires_at"] {
        let Some(value) = payload.get(field) else {
            continue;
        };

        if out.iter().any(|(key, _)| key == field) {
            return Err(RedDBError::Query(format!(
                "ttl field '{field}' cannot be defined both at the top level and inside metadata"
            )));
        }

        out.push((field.to_string(), parse_ttl_metadata_value(field, value)?));
    }

    Ok(out)
}

fn parse_ttl_metadata_value(field: &str, value: &JsonValue) -> RedDBResult<MetadataValue> {
    match value {
        JsonValue::Null => Ok(MetadataValue::Null),
        JsonValue::Number(value) => parse_ttl_u64(field, *value).map(ttl_u64_to_metadata),
        JsonValue::String(value) => parse_ttl_text(field, value),
        _ => Err(RedDBError::Query(format!(
            "field '{field}' expects a numeric value for TTL metadata"
        ))),
    }
}

fn parse_ttl_text(field: &str, value: &str) -> RedDBResult<MetadataValue> {
    let value = value.trim();

    if let Ok(value) = value.parse::<u64>() {
        return Ok(ttl_u64_to_metadata(value));
    }

    if let Ok(value) = value.parse::<i64>() {
        if value < 0 {
            return Err(RedDBError::Query(format!(
                "field '{field}' must be non-negative for TTL metadata"
            )));
        }
        return Ok(ttl_u64_to_metadata(value as u64));
    }

    if let Ok(value) = value.parse::<f64>() {
        return parse_ttl_u64(field, value).map(ttl_u64_to_metadata);
    }

    Err(RedDBError::Query(format!(
        "field '{field}' expects a numeric value for TTL metadata"
    )))
}

fn parse_ttl_u64(field: &str, value: f64) -> RedDBResult<u64> {
    if !value.is_finite() {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be a finite number"
        )));
    }
    if value.fract().abs() >= f64::EPSILON {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be an integer (TTL metadata must be an integer)"
        )));
    }
    if value < 0.0 {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be non-negative for TTL metadata"
        )));
    }
    if value > u64::MAX as f64 {
        return Err(RedDBError::Query(format!(
            "field '{field}' value is too large"
        )));
    }
    Ok(value as u64)
}

fn ttl_u64_to_metadata(value: u64) -> MetadataValue {
    if value <= i64::MAX as u64 {
        MetadataValue::Int(value as i64)
    } else {
        MetadataValue::Timestamp(value)
    }
}

fn parse_node_embeddings(payload: &JsonValue) -> RedDBResult<Vec<CreateNodeEmbeddingInput>> {
    let Some(values) = payload.get("embeddings").and_then(JsonValue::as_array) else {
        return Ok(Vec::new());
    };

    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let object = value
            .as_object()
            .ok_or_else(|| RedDBError::Query("embeddings must be objects".to_string()))?;
        let name = object
            .get("name")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| RedDBError::Query("embeddings require 'name'".to_string()))?;
        let vector = object
            .get("vector")
            .ok_or_else(|| RedDBError::Query("embeddings require 'vector'".to_string()))?;
        out.push(CreateNodeEmbeddingInput {
            name: name.to_string(),
            vector: parse_f32_array_value(vector, "embeddings.vector")?,
            model: object
                .get("model")
                .and_then(JsonValue::as_str)
                .map(str::to_string),
        });
    }
    Ok(out)
}

fn parse_collection_entity_ref(value: &JsonValue, context: &str) -> RedDBResult<(String, u64)> {
    let object = value
        .as_object()
        .ok_or_else(|| RedDBError::Query(format!("{context} must be an object")))?;
    let collection = object
        .get("collection")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| RedDBError::Query(format!("{context} requires 'collection'")))?;
    let id = parse_required_u64_field(object, "id", context)?;
    Ok((collection.to_string(), id))
}

fn parse_required_u64_json(payload: &JsonValue, field: &str, context: &str) -> RedDBResult<u64> {
    let value = payload
        .get(field)
        .ok_or_else(|| RedDBError::Query(format!("{context} requires '{field}'")))?;
    parse_u64_value(value, field)
}

fn parse_required_u64_field(
    object: &Map<String, JsonValue>,
    field: &str,
    context: &str,
) -> RedDBResult<u64> {
    let value = object
        .get(field)
        .ok_or_else(|| RedDBError::Query(format!("{context} requires '{field}'")))?;
    parse_u64_value(value, field)
}

fn parse_required_f32_array(
    payload: &JsonValue,
    field: &str,
    context: &str,
) -> RedDBResult<Vec<f32>> {
    let value = payload
        .get(field)
        .ok_or_else(|| RedDBError::Query(format!("{context} requires '{field}'")))?;
    parse_f32_array_value(value, field)
}

fn parse_f32_array_value(value: &JsonValue, field: &str) -> RedDBResult<Vec<f32>> {
    let values = value
        .as_array()
        .ok_or_else(|| RedDBError::Query(format!("field '{field}' must be an array")))?;
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let number = value.as_f64().ok_or_else(|| {
            RedDBError::Query(format!("field '{field}' must contain only numbers"))
        })?;
        out.push(number as f32);
    }
    Ok(out)
}

fn parse_u64_value(value: &JsonValue, field: &str) -> RedDBResult<u64> {
    let Some(value) = value.as_f64() else {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be a number"
        )));
    };
    if value.is_sign_negative() {
        return Err(RedDBError::Query(format!(
            "field '{field}' cannot be negative"
        )));
    }
    if value.fract().abs() > f64::EPSILON {
        return Err(RedDBError::Query(format!(
            "field '{field}' must be an integer"
        )));
    }
    if value > u64::MAX as f64 {
        return Err(RedDBError::Query(format!("field '{field}' is too large")));
    }
    Ok(value as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(entries: Vec<(&str, JsonValue)>) -> JsonValue {
        JsonValue::Object(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    #[test]
    fn parse_create_row_input_promotes_top_level_ttl_to_metadata() {
        let payload = object(vec![
            (
                "fields",
                object(vec![("name", JsonValue::String("alice".to_string()))]),
            ),
            ("_ttl_ms", JsonValue::Number(1500.0)),
        ]);

        let input = parse_create_row_input("users".to_string(), &payload)
            .expect("row payload with _ttl_ms should parse");

        assert!(input
            .metadata
            .iter()
            .any(|(key, value)| { key == "_ttl_ms" && matches!(value, MetadataValue::Int(1500)) }));
    }

    #[test]
    fn parse_create_node_input_rejects_duplicate_ttl_definition() {
        let payload = object(vec![
            ("label", JsonValue::String("host-a".to_string())),
            ("_ttl", JsonValue::Number(60.0)),
            ("metadata", object(vec![("_ttl", JsonValue::Number(30.0))])),
        ]);

        let err = parse_create_node_input("hosts".to_string(), &payload)
            .expect_err("duplicate ttl definitions must fail");

        assert!(
            err.to_string().contains("cannot be defined both"),
            "unexpected error: {err}"
        );
    }
}
