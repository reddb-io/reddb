use super::*;

pub(crate) fn analytics_job_json(job: &crate::PhysicalAnalyticsJob) -> JsonValue {
    crate::presentation::admin_json::analytics_job_json(job)
}

pub(crate) fn create_row_reply(
    runtime: &GrpcRuntime,
    request: JsonCreateRequest,
) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let input =
        crate::application::entity_payload::parse_create_row_input(request.collection, &payload)
            .map_err(entity_error_to_status)?;

    runtime
        .entity_use_cases()
        .create_row(input)
        .map(entity_reply_from_output)
        .map_err(entity_error_to_status)
}

pub(crate) fn create_node_reply(
    runtime: &GrpcRuntime,
    request: JsonCreateRequest,
) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let input =
        crate::application::entity_payload::parse_create_node_input(request.collection, &payload)
            .map_err(entity_error_to_status)?;

    runtime
        .entity_use_cases()
        .create_node(input)
        .map(entity_reply_from_output)
        .map_err(entity_error_to_status)
}

pub(crate) fn create_edge_reply(
    runtime: &GrpcRuntime,
    request: JsonCreateRequest,
) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let input =
        crate::application::entity_payload::parse_create_edge_input(request.collection, &payload)
            .map_err(entity_error_to_status)?;

    runtime
        .entity_use_cases()
        .create_edge(input)
        .map(entity_reply_from_output)
        .map_err(entity_error_to_status)
}

pub(crate) fn create_vector_reply(
    runtime: &GrpcRuntime,
    request: JsonCreateRequest,
) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let input =
        crate::application::entity_payload::parse_create_vector_input(request.collection, &payload)
            .map_err(entity_error_to_status)?;

    runtime
        .entity_use_cases()
        .create_vector(input)
        .map(entity_reply_from_output)
        .map_err(entity_error_to_status)
}

pub(crate) fn create_document_reply(
    runtime: &GrpcRuntime,
    request: JsonCreateRequest,
) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let body = payload
        .get("body")
        .cloned()
        .unwrap_or_else(|| payload.clone());

    runtime
        .entity_use_cases()
        .create_document(crate::application::CreateDocumentInput {
            collection: request.collection,
            body,
            metadata: Vec::new(),
            node_links: Vec::new(),
            vector_links: Vec::new(),
        })
        .map(entity_reply_from_output)
        .map_err(entity_error_to_status)
}

pub(crate) fn create_kv_reply(
    runtime: &GrpcRuntime,
    request: JsonCreateRequest,
) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let key = payload
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Status::invalid_argument("field 'key' must be a string"))?
        .to_string();
    let value = match payload.get("value") {
        Some(crate::serde_json::Value::String(s)) => crate::storage::schema::Value::Text(s.clone()),
        Some(crate::serde_json::Value::Number(n)) => {
            if n.fract().abs() < f64::EPSILON {
                crate::storage::schema::Value::Integer(*n as i64)
            } else {
                crate::storage::schema::Value::Float(*n)
            }
        }
        Some(crate::serde_json::Value::Bool(b)) => crate::storage::schema::Value::Boolean(*b),
        _ => crate::storage::schema::Value::Null,
    };

    runtime
        .entity_use_cases()
        .create_kv(crate::application::CreateKvInput {
            collection: request.collection,
            key,
            value,
            metadata: Vec::new(),
        })
        .map(entity_reply_from_output)
        .map_err(entity_error_to_status)
}

/// Fast-path bulk insert for rows. Parses all JSONs up front, then calls
/// store.bulk_insert() which acquires locks ONCE for the entire batch.
pub(crate) fn bulk_create_rows_fast(
    runtime: &GrpcRuntime,
    request: JsonBulkCreateRequest,
) -> Result<BulkEntityReply, Status> {
    use crate::storage::schema::Value;
    use crate::storage::unified::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};

    if request.payload_json.is_empty() {
        return Err(Status::invalid_argument("payload_json cannot be empty"));
    }

    let collection = request.collection;
    let n = request.payload_json.len();

    // Phase 1: Parse all JSONs in bulk (no locks needed)
    let mut entities = Vec::with_capacity(n);
    for payload_json in request.payload_json {
        // Fast JSON parse — avoid double-allocation from parse_json_payload
        let payload: crate::json::Value = crate::json::from_str(&payload_json)
            .map_err(|e| Status::invalid_argument(format!("invalid JSON: {e}")))?;
        let fields = match payload.get("fields").and_then(|f| f.as_object()) {
            Some(f) => f,
            None => return Err(Status::invalid_argument("missing 'fields' object")),
        };

        let mut named = std::collections::HashMap::with_capacity(fields.len());
        for (key, val) in fields {
            let value = match val {
                crate::json::Value::String(s) => Value::Text(s.clone()),
                crate::json::Value::Number(n) => {
                    if n.fract().abs() < f64::EPSILON {
                        Value::Integer(*n as i64)
                    } else {
                        Value::Float(*n)
                    }
                }
                crate::json::Value::Bool(b) => Value::Boolean(*b),
                crate::json::Value::Null => Value::Null,
                _ => continue, // skip complex types for speed
            };
            named.insert(key.clone(), value);
        }

        entities.push(UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: collection.clone(),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
            }),
        ));
    }

    // Phase 2: Batch insert (single lock acquisition)
    let store = runtime.runtime.db().store();
    let ids = store
        .bulk_insert(&collection, entities)
        .map_err(|e| Status::internal(e.to_string()))?;

    // Phase 3: Build response
    let items: Vec<_> = ids
        .iter()
        .map(|id| EntityReply {
            ok: true,
            id: id.raw(),
            entity_json: String::new(), // skip serialization for speed
        })
        .collect();

    Ok(BulkEntityReply {
        ok: true,
        count: items.len() as u64,
        items,
    })
}

pub(crate) fn bulk_create_reply(
    runtime: &GrpcRuntime,
    request: JsonBulkCreateRequest,
    handler: fn(&GrpcRuntime, JsonCreateRequest) -> Result<EntityReply, Status>,
) -> Result<BulkEntityReply, Status> {
    if request.payload_json.is_empty() {
        return Err(Status::invalid_argument("payload_json cannot be empty"));
    }

    let mut items = Vec::with_capacity(request.payload_json.len());
    for payload_json in request.payload_json {
        items.push(handler(
            runtime,
            JsonCreateRequest {
                collection: request.collection.clone(),
                payload_json,
            },
        )?);
    }

    Ok(BulkEntityReply {
        ok: true,
        count: items.len() as u64,
        items,
    })
}

pub(crate) fn patch_entity_reply(
    runtime: &GrpcRuntime,
    request: UpdateEntityRequest,
) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    runtime
        .entity_use_cases()
        .patch(PatchEntityInput {
            collection: request.collection,
            id: EntityId::new(request.id),
            payload,
            operations: Vec::new(),
        })
        .map(entity_reply_from_output)
        .map_err(entity_error_to_status)
}

pub(crate) fn entity_json_string(entity: &crate::storage::UnifiedEntity) -> String {
    crate::presentation::entity_json::compact_entity_json_string(entity)
}

pub(crate) fn entity_reply_from_output(output: CreateEntityOutput) -> EntityReply {
    EntityReply {
        ok: true,
        id: output.id.raw(),
        entity_json: output
            .entity
            .as_ref()
            .map(entity_json_string)
            .unwrap_or_else(|| "{}".to_string()),
    }
}

pub(crate) fn entity_error_to_status(err: crate::api::RedDBError) -> Status {
    match err {
        crate::api::RedDBError::NotFound(msg) => Status::not_found(msg),
        crate::api::RedDBError::InvalidConfig(msg)
        | crate::api::RedDBError::FeatureNotEnabled(msg)
        | crate::api::RedDBError::ReadOnly(msg)
        | crate::api::RedDBError::Catalog(msg)
        | crate::api::RedDBError::Query(msg) => Status::invalid_argument(msg),
        other => Status::internal(other.to_string()),
    }
}

pub(crate) fn parse_json_payload(payload_json: &str) -> Result<JsonValue, Status> {
    json_from_str::<JsonValue>(payload_json)
        .map_err(|err| Status::invalid_argument(format!("invalid payload_json: {err}")))
}
