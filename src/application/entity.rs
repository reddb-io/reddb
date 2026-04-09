use std::collections::HashMap;

use crate::application::ports::RuntimeEntityPort;
use crate::json::{parse_json, to_vec as json_to_vec, Map, Value as JsonValue};
use crate::storage::schema::Value;
use crate::storage::unified::devx::refs::{NodeRef, TableRef, VectorRef};
use crate::storage::unified::{Metadata, MetadataValue, RefTarget, SparseVector, VectorData};
use crate::storage::{EntityId, UnifiedEntity};
use crate::{RedDBError, RedDBResult};

#[derive(Debug, Clone)]
pub struct CreateEntityOutput {
    pub id: EntityId,
    pub entity: Option<UnifiedEntity>,
}

#[derive(Debug, Clone)]
pub struct CreateRowInput {
    pub collection: String,
    pub fields: Vec<(String, Value)>,
    pub metadata: Vec<(String, MetadataValue)>,
    pub node_links: Vec<NodeRef>,
    pub vector_links: Vec<VectorRef>,
}

#[derive(Debug, Clone)]
pub struct CreateNodeEmbeddingInput {
    pub name: String,
    pub vector: Vec<f32>,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CreateNodeTableLinkInput {
    pub key: String,
    pub table: TableRef,
}

#[derive(Debug, Clone)]
pub struct CreateNodeGraphLinkInput {
    pub target: EntityId,
    pub edge_label: String,
    pub weight: f32,
}

#[derive(Debug, Clone)]
pub struct CreateNodeInput {
    pub collection: String,
    pub label: String,
    pub node_type: Option<String>,
    pub properties: Vec<(String, Value)>,
    pub metadata: Vec<(String, MetadataValue)>,
    pub embeddings: Vec<CreateNodeEmbeddingInput>,
    pub table_links: Vec<CreateNodeTableLinkInput>,
    pub node_links: Vec<CreateNodeGraphLinkInput>,
}

#[derive(Debug, Clone)]
pub struct CreateEdgeInput {
    pub collection: String,
    pub label: String,
    pub from: EntityId,
    pub to: EntityId,
    pub weight: Option<f32>,
    pub properties: Vec<(String, Value)>,
    pub metadata: Vec<(String, MetadataValue)>,
}

#[derive(Debug, Clone)]
pub struct CreateVectorInput {
    pub collection: String,
    pub dense: Vec<f32>,
    pub content: Option<String>,
    pub metadata: Vec<(String, MetadataValue)>,
    pub link_row: Option<TableRef>,
    pub link_node: Option<NodeRef>,
}

#[derive(Debug, Clone)]
pub struct CreateDocumentInput {
    pub collection: String,
    pub body: JsonValue,
    pub metadata: Vec<(String, MetadataValue)>,
    pub node_links: Vec<NodeRef>,
    pub vector_links: Vec<VectorRef>,
}

#[derive(Debug, Clone)]
pub struct CreateKvInput {
    pub collection: String,
    pub key: String,
    pub value: Value,
    pub metadata: Vec<(String, MetadataValue)>,
}

#[derive(Debug, Clone)]
pub struct DeleteEntityInput {
    pub collection: String,
    pub id: EntityId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchEntityOperationType {
    Set,
    Replace,
    Unset,
}

#[derive(Debug, Clone)]
pub struct PatchEntityOperation {
    pub op: PatchEntityOperationType,
    pub path: Vec<String>,
    pub value: Option<JsonValue>,
}

#[derive(Debug, Clone)]
pub struct PatchEntityInput {
    pub collection: String,
    pub id: EntityId,
    pub payload: JsonValue,
    pub operations: Vec<PatchEntityOperation>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteEntityOutput {
    pub deleted: bool,
    pub id: EntityId,
}

pub struct EntityUseCases<'a, P: ?Sized> {
    runtime: &'a P,
}

impl<'a, P: RuntimeEntityPort + ?Sized> EntityUseCases<'a, P> {
    pub fn new(runtime: &'a P) -> Self {
        Self { runtime }
    }

    pub fn create_row(&self, input: CreateRowInput) -> RedDBResult<CreateEntityOutput> {
        self.runtime.create_row(input)
    }

    pub fn create_node(&self, input: CreateNodeInput) -> RedDBResult<CreateEntityOutput> {
        self.runtime.create_node(input)
    }

    pub fn create_edge(&self, input: CreateEdgeInput) -> RedDBResult<CreateEntityOutput> {
        self.runtime.create_edge(input)
    }

    pub fn create_vector(&self, input: CreateVectorInput) -> RedDBResult<CreateEntityOutput> {
        self.runtime.create_vector(input)
    }

    pub fn create_document(&self, input: CreateDocumentInput) -> RedDBResult<CreateEntityOutput> {
        self.runtime.create_document(input)
    }

    pub fn create_kv(&self, input: CreateKvInput) -> RedDBResult<CreateEntityOutput> {
        self.runtime.create_kv(input)
    }

    pub fn get_kv(&self, collection: &str, key: &str) -> RedDBResult<Option<(Value, EntityId)>> {
        self.runtime.get_kv(collection, key)
    }

    pub fn delete_kv(&self, collection: &str, key: &str) -> RedDBResult<bool> {
        self.runtime.delete_kv(collection, key)
    }

    pub fn patch(&self, input: PatchEntityInput) -> RedDBResult<CreateEntityOutput> {
        self.runtime.patch_entity(input)
    }

    pub fn delete(&self, input: DeleteEntityInput) -> RedDBResult<DeleteEntityOutput> {
        self.runtime.delete_entity(input)
    }
}

pub(crate) fn json_to_storage_value(value: &JsonValue) -> RedDBResult<Value> {
    match value {
        JsonValue::Null => Ok(Value::Null),
        JsonValue::Bool(value) => Ok(Value::Boolean(*value)),
        JsonValue::Number(value) => {
            if value.fract().abs() < f64::EPSILON {
                Ok(Value::Integer(*value as i64))
            } else {
                Ok(Value::Float(*value))
            }
        }
        JsonValue::String(value) => Ok(Value::Text(value.clone())),
        JsonValue::Array(_) | JsonValue::Object(_) => json_to_vec(value)
            .map(Value::Json)
            .map_err(|err| RedDBError::Query(format!("failed to serialize JSON value: {err}"))),
    }
}

pub(crate) fn json_to_metadata_value(value: &JsonValue) -> RedDBResult<MetadataValue> {
    match value {
        JsonValue::Null => Ok(MetadataValue::Null),
        JsonValue::Bool(value) => Ok(MetadataValue::Bool(*value)),
        JsonValue::Number(value) => {
            if value.fract().abs() < f64::EPSILON {
                Ok(MetadataValue::Int(*value as i64))
            } else {
                Ok(MetadataValue::Float(*value))
            }
        }
        JsonValue::String(value) => Ok(MetadataValue::String(value.clone())),
        JsonValue::Array(values) => {
            let mut items = Vec::with_capacity(values.len());
            for value in values {
                items.push(json_to_metadata_value(value)?);
            }
            Ok(MetadataValue::Array(items))
        }
        JsonValue::Object(map) => {
            let mut object = HashMap::with_capacity(map.len());
            for (key, value) in map {
                object.insert(key.clone(), json_to_metadata_value(value)?);
            }
            Ok(MetadataValue::Object(object))
        }
    }
}

pub(crate) fn apply_patch_operations_to_storage_map(
    fields: &mut HashMap<String, Value>,
    operations: &[PatchEntityOperation],
) -> RedDBResult<()> {
    if operations.is_empty() {
        return Ok(());
    }

    let mut patch_target = JsonValue::Object(
        fields
            .iter()
            .map(|(key, value)| (key.clone(), storage_value_to_json(value)))
            .collect(),
    );
    apply_patch_operations_to_json(&mut patch_target, operations)
        .map_err(|error| RedDBError::Query(format!("patch fields failed: {error}")))?;

    let JsonValue::Object(object) = patch_target else {
        return Err(RedDBError::Query(
            "patch operations require object roots".to_string(),
        ));
    };

    let mut merged = HashMap::with_capacity(object.len());
    for (key, value) in object {
        merged.insert(key, json_to_storage_value(&value)?);
    }
    *fields = merged;
    Ok(())
}

pub(crate) fn apply_patch_operations_to_json(
    value: &mut JsonValue,
    operations: &[PatchEntityOperation],
) -> Result<(), String> {
    for operation in operations {
        if operation.path.is_empty() {
            return Err("patch path cannot be empty".to_string());
        }

        match operation.op {
            PatchEntityOperationType::Set | PatchEntityOperationType::Replace => {
                let Some(patch_value) = &operation.value else {
                    return Err("set/replace operations require a value".to_string());
                };
                apply_patch_json_set(value, &operation.path, patch_value.clone())?;
            }
            PatchEntityOperationType::Unset => {
                apply_patch_json_unset(value, &operation.path)?;
            }
        }
    }
    Ok(())
}

pub(crate) fn apply_patch_operations_to_vector_fields(
    vector: &mut VectorData,
    operations: &[PatchEntityOperation],
) -> RedDBResult<()> {
    if operations.is_empty() {
        return Ok(());
    }

    let mut vector_target = JsonValue::Object({
        let mut object = Map::new();
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
            vector.sparse.as_ref().map_or(JsonValue::Null, |sparse| {
                let mut object = Map::new();
                object.insert(
                    "indices".to_string(),
                    JsonValue::Array(
                        sparse
                            .indices
                            .iter()
                            .map(|value| JsonValue::Number(*value as f64))
                            .collect(),
                    ),
                );
                object.insert(
                    "values".to_string(),
                    JsonValue::Array(
                        sparse
                            .values
                            .iter()
                            .map(|value| JsonValue::Number(*value as f64))
                            .collect(),
                    ),
                );
                object.insert(
                    "dimension".to_string(),
                    JsonValue::Number(sparse.dimension as f64),
                );
                JsonValue::Object(object)
            }),
        );
        object.insert(
            "content".to_string(),
            match vector.content.as_ref() {
                Some(value) => JsonValue::String(value.clone()),
                None => JsonValue::Null,
            },
        );
        object
    });

    let touched_dense = operations
        .iter()
        .any(|operation| operation.path.first().is_some_and(|key| key == "dense"));
    let touched_sparse = operations
        .iter()
        .any(|operation| operation.path.first().is_some_and(|key| key == "sparse"));
    let touched_content = operations
        .iter()
        .any(|operation| operation.path.first().is_some_and(|key| key == "content"));

    apply_patch_operations_to_json(&mut vector_target, operations)
        .map_err(|error| RedDBError::Query(format!("patch fields failed: {error}")))?;

    let JsonValue::Object(object) = vector_target else {
        return Err(RedDBError::Query(
            "patch operations require object roots".to_string(),
        ));
    };

    if touched_dense {
        let Some(value) = object.get("dense") else {
            return Err(RedDBError::Query(
                "field 'dense' cannot be unset".to_string(),
            ));
        };
        vector.dense = parse_patch_f32_vector(value, "dense")?;
    }

    if touched_content {
        vector.content = match object.get("content") {
            None | Some(JsonValue::Null) => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| {
                        RedDBError::Query("field 'content' must be a string".to_string())
                    })?
                    .to_string(),
            ),
        };
    }

    if touched_sparse {
        vector.sparse = match object.get("sparse") {
            Some(value) => parse_sparse_vector_value(value)?,
            None => None,
        };
    }

    Ok(())
}

pub(crate) fn metadata_to_json(metadata: &Metadata) -> JsonValue {
    JsonValue::Object(
        metadata
            .iter()
            .map(|(key, value)| (key.clone(), metadata_value_to_json(value)))
            .collect(),
    )
}

pub(crate) fn metadata_from_json(payload: &JsonValue) -> RedDBResult<Metadata> {
    let JsonValue::Object(object) = payload else {
        return Err(RedDBError::Query(
            "metadata patch requires an object".to_string(),
        ));
    };

    let mut metadata = Metadata::new();
    for (key, value) in object {
        metadata.set(key.clone(), metadata_value_from_json(value)?);
    }
    Ok(metadata)
}

fn storage_value_to_json(value: &Value) -> JsonValue {
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
        Value::Json(value) => {
            let text = String::from_utf8_lossy(value);
            match parse_json(&text) {
                Ok(parsed) => JsonValue::from(parsed),
                Err(_) => JsonValue::String(hex::encode(value)),
            }
        }
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
            object.insert("table".to_string(), JsonValue::String(table.clone()));
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
    }
}

fn metadata_value_to_json(value: &MetadataValue) -> JsonValue {
    match value {
        MetadataValue::Null => JsonValue::Null,
        MetadataValue::Bool(value) => JsonValue::Bool(*value),
        MetadataValue::Int(value) => JsonValue::Number(*value as f64),
        MetadataValue::Float(value) => JsonValue::Number(*value),
        MetadataValue::String(value) => JsonValue::String(value.clone()),
        MetadataValue::Bytes(value) => {
            let mut object = Map::new();
            object.insert(
                "__redb_type".to_string(),
                JsonValue::String("bytes".to_string()),
            );
            object.insert(
                "value".to_string(),
                JsonValue::Array(
                    value
                        .iter()
                        .map(|value| JsonValue::Number(*value as f64))
                        .collect(),
                ),
            );
            JsonValue::Object(object)
        }
        MetadataValue::Array(values) => {
            JsonValue::Array(values.iter().map(metadata_value_to_json).collect())
        }
        MetadataValue::Object(object) => JsonValue::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), metadata_value_to_json(value)))
                .collect(),
        ),
        MetadataValue::Timestamp(value) => JsonValue::Number(*value as f64),
        MetadataValue::Geo { lat, lon } => {
            let mut object = Map::new();
            object.insert(
                "__redb_type".to_string(),
                JsonValue::String("geo".to_string()),
            );
            object.insert("lat".to_string(), JsonValue::Number(*lat));
            object.insert("lon".to_string(), JsonValue::Number(*lon));
            JsonValue::Object(object)
        }
        MetadataValue::Reference(value) => {
            let mut object = Map::new();
            object.insert(
                "__redb_type".to_string(),
                JsonValue::String("reference".to_string()),
            );
            let (kind, collection, id) = match value {
                RefTarget::TableRow { table, row_id } => ("table_row", table.as_str(), *row_id),
                RefTarget::Node {
                    collection,
                    node_id,
                } => ("node", collection.as_str(), node_id.raw()),
                RefTarget::Edge {
                    collection,
                    edge_id,
                } => ("edge", collection.as_str(), edge_id.raw()),
                RefTarget::Vector {
                    collection,
                    vector_id,
                } => ("vector", collection.as_str(), vector_id.raw()),
                RefTarget::Entity {
                    collection,
                    entity_id,
                } => ("entity", collection.as_str(), entity_id.raw()),
            };
            object.insert("kind".to_string(), JsonValue::String(kind.to_string()));
            object.insert(
                "collection".to_string(),
                JsonValue::String(collection.to_string()),
            );
            object.insert("id".to_string(), JsonValue::Number(id as f64));
            JsonValue::Object(object)
        }
        MetadataValue::References(values) => {
            let mut object = Map::new();
            object.insert(
                "__redb_type".to_string(),
                JsonValue::String("references".to_string()),
            );
            object.insert(
                "values".to_string(),
                JsonValue::Array(
                    values
                        .iter()
                        .map(|r| metadata_value_to_json(&MetadataValue::Reference(r.clone())))
                        .collect(),
                ),
            );
            JsonValue::Object(object)
        }
    }
}

fn metadata_value_from_json(value: &JsonValue) -> RedDBResult<MetadataValue> {
    match value {
        JsonValue::Null => Ok(MetadataValue::Null),
        JsonValue::Bool(value) => Ok(MetadataValue::Bool(*value)),
        JsonValue::Number(value) => {
            if value.fract().abs() < f64::EPSILON {
                Ok(MetadataValue::Int(*value as i64))
            } else {
                Ok(MetadataValue::Float(*value))
            }
        }
        JsonValue::String(value) => Ok(MetadataValue::String(value.clone())),
        JsonValue::Array(values) => {
            let mut out = Vec::with_capacity(values.len());
            for value in values {
                out.push(metadata_value_from_json(value)?);
            }
            Ok(MetadataValue::Array(out))
        }
        JsonValue::Object(object) => {
            if let Some(marker) = object.get("__redb_type").and_then(JsonValue::as_str) {
                match marker {
                    "bytes" => {
                        let values = object
                            .get("value")
                            .and_then(JsonValue::as_array)
                            .ok_or_else(|| {
                                RedDBError::Query(
                                    "metadata marker 'bytes' requires array value".to_string(),
                                )
                            })?;
                        let mut out = Vec::with_capacity(values.len());
                        for value in values {
                            let value = value.as_i64().ok_or_else(|| {
                                RedDBError::Query(
                                    "metadata bytes must contain integer values".to_string(),
                                )
                            })?;
                            if !(0..=255).contains(&value) {
                                return Err(RedDBError::Query(
                                    "metadata bytes must contain values between 0 and 255"
                                        .to_string(),
                                ));
                            }
                            out.push(value as u8);
                        }
                        return Ok(MetadataValue::Bytes(out));
                    }
                    "geo" => {
                        let lat =
                            object
                                .get("lat")
                                .and_then(JsonValue::as_f64)
                                .ok_or_else(|| {
                                    RedDBError::Query(
                                        "metadata marker 'geo' requires numeric 'lat'".to_string(),
                                    )
                                })?;
                        let lon =
                            object
                                .get("lon")
                                .and_then(JsonValue::as_f64)
                                .ok_or_else(|| {
                                    RedDBError::Query(
                                        "metadata marker 'geo' requires numeric 'lon'".to_string(),
                                    )
                                })?;
                        return Ok(MetadataValue::Geo { lat, lon });
                    }
                    "reference" => {
                        return parse_metadata_reference(object).map(MetadataValue::Reference)
                    }
                    "references" => {
                        let values = object
                            .get("values")
                            .and_then(JsonValue::as_array)
                            .ok_or_else(|| {
                                RedDBError::Query(
                                    "metadata marker 'references' requires array 'values'"
                                        .to_string(),
                                )
                            })?;
                        let mut references = Vec::with_capacity(values.len());
                        for value in values {
                            references.push(parse_metadata_reference_value(value)?);
                        }
                        return Ok(MetadataValue::References(references));
                    }
                    _ => {}
                }
            }

            let mut out = HashMap::with_capacity(object.len());
            for (key, value) in object {
                out.insert(key.clone(), metadata_value_from_json(value)?);
            }
            Ok(MetadataValue::Object(out))
        }
    }
}

fn parse_metadata_reference(object: &Map<String, JsonValue>) -> RedDBResult<RefTarget> {
    let kind = object
        .get("kind")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| RedDBError::Query("metadata reference requires 'kind'".to_string()))?;
    let collection = object
        .get("collection")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| RedDBError::Query("metadata reference requires 'collection'".to_string()))?;
    let id = object
        .get("id")
        .ok_or_else(|| RedDBError::Query("metadata reference requires 'id'".to_string()))?;
    let id = parse_patch_u64_value(id, "id")?;

    let target = match kind {
        "table_row" | "table" => RefTarget::table(collection.to_string(), id),
        "node" => RefTarget::node(collection.to_string(), EntityId::new(id)),
        "edge" => RefTarget::Edge {
            collection: collection.to_string(),
            edge_id: EntityId::new(id),
        },
        "vector" => RefTarget::vector(collection.to_string(), EntityId::new(id)),
        "entity" => RefTarget::Entity {
            collection: collection.to_string(),
            entity_id: EntityId::new(id),
        },
        _ => {
            return Err(RedDBError::Query(format!(
                "unsupported metadata reference kind '{kind}'"
            )));
        }
    };

    Ok(target)
}

fn parse_metadata_reference_value(value: &JsonValue) -> RedDBResult<RefTarget> {
    let JsonValue::Object(object) = value else {
        return Err(RedDBError::Query(
            "metadata reference entries must be objects".to_string(),
        ));
    };
    parse_metadata_reference(object)
}

fn parse_patch_u64_value(value: &JsonValue, field: &str) -> RedDBResult<u64> {
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

fn parse_patch_f32_vector(value: &JsonValue, field: &str) -> RedDBResult<Vec<f32>> {
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
    if out.is_empty() {
        return Err(RedDBError::Query(format!(
            "field '{field}' cannot be empty"
        )));
    }
    Ok(out)
}

fn parse_sparse_index_array(value: &JsonValue, field: &str) -> RedDBResult<Vec<u32>> {
    let values = value
        .as_array()
        .ok_or_else(|| RedDBError::Query(format!("field '{field}' must be an array")))?;
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let value = value.as_f64().ok_or_else(|| {
            RedDBError::Query(format!("field '{field}' must contain only integers"))
        })?;
        if value.is_sign_negative() || value.fract().abs() > f64::EPSILON {
            return Err(RedDBError::Query(format!(
                "field '{field}' must contain only u32 values"
            )));
        }
        if value > u32::MAX as f64 {
            return Err(RedDBError::Query(format!(
                "field '{field}' value is too large"
            )));
        }
        out.push(value as u32);
    }
    Ok(out)
}

fn parse_sparse_value_array(value: &JsonValue, field: &str) -> RedDBResult<Vec<f32>> {
    parse_patch_f32_vector(value, field)
}

fn parse_sparse_vector_value(value: &JsonValue) -> RedDBResult<Option<SparseVector>> {
    match value {
        JsonValue::Null => Ok(None),
        JsonValue::Object(object) => {
            let indices = parse_sparse_index_array(
                object.get("indices").ok_or_else(|| {
                    RedDBError::Query("sparse metadata requires 'indices'".to_string())
                })?,
                "sparse.indices",
            )?;
            let values = parse_sparse_value_array(
                object.get("values").ok_or_else(|| {
                    RedDBError::Query("sparse metadata requires 'values'".to_string())
                })?,
                "sparse.values",
            )?;
            if indices.len() != values.len() {
                return Err(RedDBError::Query(
                    "sparse indices and values lengths must match".to_string(),
                ));
            }
            let dimension = match object.get("dimension").and_then(JsonValue::as_f64) {
                Some(value) => {
                    if value.is_sign_negative() || value.fract().abs() > f64::EPSILON {
                        return Err(RedDBError::Query(
                            "sparse dimension must be a non-negative integer".to_string(),
                        ));
                    }
                    if value > usize::MAX as f64 {
                        return Err(RedDBError::Query(
                            "sparse dimension is too large".to_string(),
                        ));
                    }
                    value as usize
                }
                None => indices
                    .iter()
                    .max()
                    .map_or(0, |index| (*index as usize) + 1),
            };
            if indices.iter().any(|index| (*index as usize) >= dimension) {
                return Err(RedDBError::Query(
                    "sparse indices must be smaller than dimension".to_string(),
                ));
            }
            Ok(Some(SparseVector::new(indices, values, dimension)))
        }
        _ => Err(RedDBError::Query(
            "field 'sparse' must be an object or null".to_string(),
        )),
    }
}

fn apply_patch_json_set(
    target: &mut JsonValue,
    path: &[String],
    value: JsonValue,
) -> Result<(), String> {
    if path.is_empty() {
        return Err("patch path cannot be empty".to_string());
    }

    let mut current = target;
    for segment in &path[..path.len() - 1] {
        let JsonValue::Object(object) = current else {
            return Err("patch path target must be an object".to_string());
        };
        let value = object
            .entry(segment.clone())
            .or_insert_with(|| JsonValue::Object(Map::new()));
        if !matches!(value, JsonValue::Object(_)) {
            *value = JsonValue::Object(Map::new());
        }
        current = value;
    }

    let JsonValue::Object(object) = current else {
        return Err("patch path target must be an object".to_string());
    };
    object.insert(path[path.len() - 1].clone(), value);
    Ok(())
}

fn apply_patch_json_unset(target: &mut JsonValue, path: &[String]) -> Result<(), String> {
    if path.is_empty() {
        return Err("patch path cannot be empty".to_string());
    }

    if path.len() == 1 {
        let JsonValue::Object(object) = target else {
            return Err("patch path target must be an object".to_string());
        };
        object.remove(&path[0]);
        return Ok(());
    }

    let mut current = target;
    for segment in &path[..path.len() - 1] {
        let Some(value) = (match current {
            JsonValue::Object(object) => object.get_mut(segment),
            _ => {
                return Err("patch path target must be an object".to_string());
            }
        }) else {
            return Ok(());
        };

        if !matches!(value, JsonValue::Object(_)) {
            return Ok(());
        }
        current = value;
    }

    let JsonValue::Object(object) = current else {
        return Err("patch path target must be an object".to_string());
    };
    object.remove(&path[path.len() - 1]);
    Ok(())
}

fn format_mac(bytes: &[u8; 6]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}
