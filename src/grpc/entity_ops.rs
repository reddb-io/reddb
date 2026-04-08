use super::*;

fn analytics_job_json(job: &crate::PhysicalAnalyticsJob) -> JsonValue {
    crate::presentation::admin_json::analytics_job_json(job)
}

fn create_row_reply(
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

fn create_node_reply(
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

fn create_edge_reply(
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

fn create_vector_reply(
    runtime: &GrpcRuntime,
    request: JsonCreateRequest,
) -> Result<EntityReply, Status> {
    let payload = parse_json_payload(&request.payload_json)?;
    let input = crate::application::entity_payload::parse_create_vector_input(
        request.collection,
        &payload,
    )
    .map_err(entity_error_to_status)?;

    runtime
        .entity_use_cases()
        .create_vector(input)
        .map(entity_reply_from_output)
        .map_err(entity_error_to_status)
}

fn bulk_create_reply(
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

fn patch_entity_reply(
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

fn entity_reply_from_output(output: CreateEntityOutput) -> EntityReply {
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

fn entity_error_to_status(err: crate::api::RedDBError) -> Status {
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

fn parse_json_payload(payload_json: &str) -> Result<JsonValue, Status> {
    json_from_str::<JsonValue>(payload_json)
        .map_err(|err| Status::invalid_argument(format!("invalid payload_json: {err}")))
}
