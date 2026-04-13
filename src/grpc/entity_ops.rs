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
                table: Arc::from(collection.as_str()),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
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

/// Binary bulk insert — ZERO JSON. Converts protobuf typed values
/// directly into `UnifiedEntity`s and drops them into
/// `store.bulk_insert`, which owns the full write path (segment
/// manager + persistent B-tree + index maintenance).
///
/// History: this used to run through a `PageBulkWriter` fast path as
/// well, writing every row's cells directly to a disconnected chain
/// of B-tree leaf pages AND to the segment manager. That double
/// write did twice the serialization work, twice the flush work, and
/// produced a stretch of orphaned pages on disk that no index
/// pointed at — so queries never saw them. BASELINE.md Finding #2
/// called it out as "the current design pays for both and gets the
/// benefit of neither". We now commit to the single
/// `store.bulk_insert` path: O(M) slotted leaf inserts keep it fast,
/// and every row is immediately visible to the query engine via
/// the segment manager.
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
    let store = runtime.runtime.db().store();
    let field_names: Vec<String> = request.field_names;

    // Single pass: proto values → UnifiedEntity. No parallel
    // `PageBulkWriter` chain. `store.bulk_insert` handles segment
    // + B-tree together. We also remember a flattened `Vec<(name,
    // value)>` per row so we can push each inserted entity through
    // the secondary-index maintenance pipeline after its ID is known.
    let mut entities = Vec::with_capacity(n);
    let mut indexed_fields: Vec<Vec<(String, Value)>> = Vec::with_capacity(n);
    for row in request.rows {
        let mut named = std::collections::HashMap::with_capacity(num_fields);
        let mut field_snapshot: Vec<(String, Value)> = Vec::with_capacity(num_fields);
        for (i, bval) in row.values.into_iter().enumerate() {
            if i >= num_fields {
                break;
            }
            let value = match bval.kind {
                Some(super::proto::binary_value::Kind::TextValue(s)) => Value::Text(s),
                Some(super::proto::binary_value::Kind::IntValue(n)) => Value::Integer(n),
                Some(super::proto::binary_value::Kind::FloatValue(f)) => Value::Float(f),
                Some(super::proto::binary_value::Kind::BoolValue(b)) => Value::Boolean(b),
                Some(super::proto::binary_value::Kind::BlobValue(b)) => Value::Blob(b),
                None => Value::Null,
            };
            field_snapshot.push((field_names[i].clone(), value.clone()));
            named.insert(field_names[i].clone(), value);
        }
        indexed_fields.push(field_snapshot);

        entities.push(crate::storage::unified::UnifiedEntity::new(
            crate::storage::unified::EntityId::new(0),
            crate::storage::unified::EntityKind::TableRow {
                table: Arc::from(collection.as_str()),
                row_id: 0,
            },
            crate::storage::unified::EntityData::Row(crate::storage::unified::RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        ));
    }

    let ids = store
        .bulk_insert(&collection, entities)
        .map_err(|e| Status::internal(e.to_string()))?;
    let first_id = ids.first().map(|id| id.raw()).unwrap_or(0);

    // Feed every inserted row to the secondary-index maintenance
    // hook so that indexes registered BEFORE this bulk insert
    // observe the new rows. Indexes created AFTER this call will
    // pick them up via `CREATE INDEX`'s full scan.
    for (id, fields) in ids.iter().zip(indexed_fields.iter()) {
        runtime
            .runtime
            .index_store_ref()
            .index_entity_insert(&collection, *id, fields);
    }

    Ok(super::proto::BulkInsertReply {
        ok: true,
        count: ids.len() as u64,
        first_id,
    })
}
