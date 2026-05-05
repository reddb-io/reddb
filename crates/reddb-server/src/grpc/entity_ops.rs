use super::*;
use std::sync::Arc;

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
        Some(crate::serde_json::Value::String(s)) => crate::storage::schema::Value::text(s.clone()),
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
/// the canonical runtime row-batch path so every transport shares the same
/// validation, contract, indexing and persistence semantics.
pub(crate) fn bulk_create_rows_fast(
    runtime: &GrpcRuntime,
    request: JsonBulkCreateRequest,
) -> Result<BulkEntityReply, Status> {
    if request.payload_json.is_empty() {
        return Err(Status::invalid_argument("payload_json cannot be empty"));
    }

    let collection = request.collection;
    let mut rows = Vec::with_capacity(request.payload_json.len());
    for payload_json in request.payload_json {
        let payload = parse_json_payload(&payload_json)?;
        rows.push(
            crate::application::entity_payload::parse_create_row_input(
                collection.clone(),
                &payload,
            )
            .map_err(entity_error_to_status)?,
        );
    }

    let outputs = runtime
        .entity_use_cases()
        .create_rows_batch(crate::application::CreateRowsBatchInput { collection, rows })
        .map_err(entity_error_to_status)?;

    let items: Vec<_> = outputs
        .iter()
        .map(|output| EntityReply {
            ok: true,
            id: output.id.raw(),
            entity_json: String::new(),
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
        // PLAN.md Phase 4.1 — operator-pinned cap exceeded. gRPC
        // doesn't have a 1:1 status for "storage full" or "rate
        // limited"; `ResourceExhausted` is the canonical match and
        // gives clients a clear retry-or-back-off signal.
        crate::api::RedDBError::QuotaExceeded(msg) => Status::resource_exhausted(msg),
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

/// Binary bulk insert — ZERO JSON. Converts protobuf typed values into the
/// canonical row batch input and hands execution to the shared runtime path.
///
/// This keeps binary as just an encoding optimization; validation,
/// uniqueness, index maintenance and persistence stay centralized.
pub(crate) fn bulk_insert_binary(
    runtime: &GrpcRuntime,
    request: super::proto::BinaryBulkInsertRequest,
) -> Result<super::proto::BulkInsertReply, Status> {
    use crate::storage::schema::Value;

    let collection = request.collection;
    let n = request.rows.len();

    if n == 0 {
        return Ok(super::proto::BulkInsertReply {
            ok: true,
            count: 0,
            first_id: 0,
        });
    }

    let num_fields = request.field_names.len();
    let field_names: Vec<String> = request.field_names;

    let mut rows = Vec::with_capacity(n);
    for row in request.rows {
        let mut fields: Vec<(String, Value)> = Vec::with_capacity(num_fields);
        for (i, bval) in row.values.into_iter().enumerate() {
            if i >= num_fields {
                break;
            }
            let value = match bval.kind {
                Some(super::proto::binary_value::Kind::TextValue(s)) => Value::text(s),
                Some(super::proto::binary_value::Kind::IntValue(n)) => Value::Integer(n),
                Some(super::proto::binary_value::Kind::FloatValue(f)) => Value::Float(f),
                Some(super::proto::binary_value::Kind::BoolValue(b)) => Value::Boolean(b),
                Some(super::proto::binary_value::Kind::BlobValue(b)) => Value::Blob(b),
                None => Value::Null,
            };
            fields.push((field_names[i].clone(), value));
        }
        rows.push(crate::application::CreateRowInput {
            collection: collection.clone(),
            fields,
            metadata: Vec::new(),
            node_links: Vec::new(),
            vector_links: Vec::new(),
        });
    }

    let outputs = runtime
        .entity_use_cases()
        .create_rows_batch(crate::application::CreateRowsBatchInput { collection, rows })
        .map_err(entity_error_to_status)?;
    let first_id = outputs.first().map(|output| output.id.raw()).unwrap_or(0);

    Ok(super::proto::BulkInsertReply {
        ok: true,
        count: outputs.len() as u64,
        first_id,
    })
}
