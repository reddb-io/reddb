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
