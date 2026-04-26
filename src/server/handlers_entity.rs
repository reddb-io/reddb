use super::*;
use std::sync::Arc;

use crate::application::{RuntimeEntityPortCtx, RuntimeQueryPortCtx};
use crate::runtime::write_gate::WriteKind;

impl RedDBServer {
    pub(crate) fn handle_scan(
        &self,
        collection: &str,
        query: &BTreeMap<String, String>,
    ) -> HttpResponse {
        let offset = query
            .get("offset")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        let limit = query
            .get("limit")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(100)
            .max(1)
            .min(self.options.max_scan_limit);

        let ctx = self.build_read_context(None, None);
        let collection = collection.to_string();
        crate::server::transport::run_use_case(
            move || {
                self.runtime
                    .scan_collection_ctx(&ctx, &collection, None, limit + offset)
                    .map(|page| {
                        // The legacy scan use-case applied offset client-side
                        // before this migration; preserve that semantic by
                        // slicing the cursor-based page.
                        let mut page = page;
                        if offset > 0 && offset < page.items.len() {
                            page.items = page.items.split_off(offset);
                        }
                        page
                    })
            },
            |page| crate::presentation::entity_json::scan_page_json(page),
        )
    }

    pub(crate) fn handle_create_row(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::entity_payload::parse_create_row_input(
            collection.to_string(),
            &payload,
        ) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };

        let ctx = match self.build_write_context(WriteKind::Dml, None, None) {
            Ok(ctx) => ctx,
            Err(err) => return json_error(403, err.to_string()),
        };
        crate::server::transport::run_use_case(
            move || self.runtime.create_row_ctx(&ctx, input),
            |output| crate::presentation::entity_json::created_entity_output_json(output),
        )
    }

    pub(crate) fn handle_create_node(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::entity_payload::parse_create_node_input(
            collection.to_string(),
            &payload,
        ) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };

        let ctx = match self.build_write_context(WriteKind::Dml, None, None) {
            Ok(ctx) => ctx,
            Err(err) => return json_error(403, err.to_string()),
        };
        crate::server::transport::run_use_case(
            move || self.runtime.create_node_ctx(&ctx, input),
            |output| crate::presentation::entity_json::created_entity_output_json(output),
        )
    }

    pub(crate) fn handle_create_edge(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::entity_payload::parse_create_edge_input(
            collection.to_string(),
            &payload,
        ) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };

        match self.entity_use_cases().create_edge(input) {
            Ok(output) => json_response(
                200,
                crate::presentation::entity_json::created_entity_output_json(&output),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_bulk_create(
        &self,
        collection: &str,
        body: Vec<u8>,
        handler: fn(&Self, &str, Vec<u8>) -> HttpResponse,
    ) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(items) = payload.get("items").and_then(JsonValue::as_array) else {
            return json_error(400, "JSON body must contain an array field named 'items'");
        };
        if items.is_empty() {
            return json_error(400, "field 'items' cannot be empty");
        }

        let mut results = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let item_body = match json_to_vec(item) {
                Ok(body) => body,
                Err(err) => {
                    return json_error(
                        400,
                        format!("failed to encode bulk item at index {index}: {err}"),
                    )
                }
            };
            let response = handler(self, collection, item_body);
            if response.status >= 400 {
                let message = String::from_utf8_lossy(&response.body);
                return json_error(
                    response.status,
                    format!("bulk item {index} failed: {message}"),
                );
            }

            let parsed = match std::str::from_utf8(&response.body) {
                Ok(text) => parse_json(text)
                    .ok()
                    .map(JsonValue::from)
                    .unwrap_or(JsonValue::Null),
                Err(_) => JsonValue::Null,
            };
            results.push(parsed);
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("count".to_string(), JsonValue::Number(results.len() as f64));
        object.insert("items".to_string(), JsonValue::Array(results));
        json_response(200, JsonValue::Object(object))
    }

    /// Fast bulk insert for rows through the canonical runtime batch path.
    pub(crate) fn handle_bulk_create_rows_fast(
        &self,
        collection: &str,
        body: Vec<u8>,
    ) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(items) = payload.get("items").and_then(JsonValue::as_array) else {
            return json_error(400, "JSON body must contain an array field named 'items'");
        };
        if items.is_empty() {
            return json_error(400, "field 'items' cannot be empty");
        }

        let mut rows = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            match crate::application::entity_payload::parse_create_row_input(
                collection.to_string(),
                item,
            ) {
                Ok(input) => rows.push(input),
                Err(err) => {
                    return json_error(400, format!("bulk item {index} failed: {err}"));
                }
            }
        }

        let count = rows.len();
        if let Err(err) =
            self.entity_use_cases()
                .create_rows_batch(crate::application::CreateRowsBatchInput {
                    collection: collection.to_string(),
                    rows,
                })
        {
            return json_error(400, err.to_string());
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("count".to_string(), JsonValue::Number(count as f64));
        json_response(200, JsonValue::Object(object))
    }

    pub(crate) fn handle_create_vector(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let input = match crate::application::entity_payload::parse_create_vector_input(
            collection.to_string(),
            &payload,
        ) {
            Ok(input) => input,
            Err(err) => return json_error(400, err.to_string()),
        };

        match self.entity_use_cases().create_vector(input) {
            Ok(output) => json_response(
                200,
                crate::presentation::entity_json::created_entity_output_json(&output),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_patch_entity(
        &self,
        collection: &str,
        id: u64,
        body: Vec<u8>,
    ) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let patch_operations = match parse_patch_operations(&payload) {
            Ok(operations) => operations,
            Err(response) => return response,
        };

        let operations = patch_operations
            .into_iter()
            .map(|operation| PatchEntityOperation {
                op: match operation.op {
                    PatchOperationType::Set => PatchEntityOperationType::Set,
                    PatchOperationType::Replace => PatchEntityOperationType::Replace,
                    PatchOperationType::Unset => PatchEntityOperationType::Unset,
                },
                path: operation.path,
                value: operation.value,
            })
            .collect();

        match self.entity_use_cases().patch(PatchEntityInput {
            collection: collection.to_string(),
            id: EntityId::new(id),
            payload,
            operations,
        }) {
            Ok(output) => json_response(
                200,
                crate::presentation::entity_json::created_entity_output_json(&output),
            ),
            Err(err @ RedDBError::NotFound(_)) => json_error(404, err.to_string()),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    // ── KV endpoints ─────────────────────────────────────────────────

    pub(crate) fn handle_get_kv(&self, collection: &str, key: &str) -> HttpResponse {
        match self.entity_use_cases().get_kv(collection, key) {
            Ok(Some((value, id))) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "collection".to_string(),
                    JsonValue::String(collection.to_string()),
                );
                object.insert("key".to_string(), JsonValue::String(key.to_string()));
                object.insert(
                    "value".to_string(),
                    crate::presentation::entity_json::storage_value_to_json(&value),
                );
                object.insert("id".to_string(), JsonValue::Number(id.raw() as f64));
                json_response(200, JsonValue::Object(object))
            }
            Ok(None) => json_error(404, format!("key not found: {key}")),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_put_kv(&self, collection: &str, key: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let value = match payload.get("value") {
            Some(JsonValue::String(s)) => Value::text(s.clone()),
            Some(JsonValue::Number(n)) => {
                if n.fract().abs() < f64::EPSILON {
                    Value::Integer(*n as i64)
                } else {
                    Value::Float(*n)
                }
            }
            Some(JsonValue::Bool(b)) => Value::Boolean(*b),
            Some(JsonValue::Null) | None => Value::Null,
            Some(other) => Value::Json(crate::json::to_vec(other).unwrap_or_default()),
        };

        // Try to find existing KV to update, otherwise create
        match self.entity_use_cases().get_kv(collection, key) {
            Ok(Some((_, existing_id))) => {
                // Update existing
                match self.entity_use_cases().patch(PatchEntityInput {
                    collection: collection.to_string(),
                    id: existing_id,
                    payload: payload.clone(),
                    operations: vec![PatchEntityOperation {
                        op: PatchEntityOperationType::Set,
                        path: vec!["value".to_string()],
                        value: payload.get("value").cloned(),
                    }],
                }) {
                    Ok(output) => json_response(
                        200,
                        crate::presentation::entity_json::created_entity_output_json(&output),
                    ),
                    Err(err) => json_error(400, err.to_string()),
                }
            }
            Ok(None) => {
                // Create new
                match self.entity_use_cases().create_kv(CreateKvInput {
                    collection: collection.to_string(),
                    key: key.to_string(),
                    value,
                    metadata: Vec::new(),
                }) {
                    Ok(output) => json_response(
                        201,
                        crate::presentation::entity_json::created_entity_output_json(&output),
                    ),
                    Err(err) => json_error(400, err.to_string()),
                }
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_delete_kv(&self, collection: &str, key: &str) -> HttpResponse {
        match self.entity_use_cases().delete_kv(collection, key) {
            Ok(true) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("deleted".to_string(), JsonValue::Bool(true));
                object.insert("key".to_string(), JsonValue::String(key.to_string()));
                json_response(200, JsonValue::Object(object))
            }
            Ok(false) => json_error(404, format!("key not found: {key}")),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    // ── Document endpoint ───────────────────────────────────────────

    pub(crate) fn handle_create_document(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        // If payload has "body" field, use it; otherwise treat entire payload as the document body
        let body_value = payload
            .get("body")
            .cloned()
            .unwrap_or_else(|| payload.clone());

        match self
            .entity_use_cases()
            .create_document(CreateDocumentInput {
                collection: collection.to_string(),
                body: body_value,
                metadata: Vec::new(),
                node_links: Vec::new(),
                vector_links: Vec::new(),
            }) {
            Ok(output) => json_response(
                200,
                crate::presentation::entity_json::created_entity_output_json(&output),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_delete_entity(&self, collection: &str, id: u64) -> HttpResponse {
        match self.entity_use_cases().delete(DeleteEntityInput {
            collection: collection.to_string(),
            id: EntityId::new(id),
        }) {
            Ok(output) if output.deleted => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("deleted".to_string(), JsonValue::Bool(true));
                object.insert("id".to_string(), JsonValue::Number(id as f64));
                json_response(200, JsonValue::Object(object))
            }
            Ok(_) => json_error(404, format!("entity not found: {id}")),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_create_tree(&self, collection: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(name) = json_string_field(&payload, "name") else {
            return json_error(400, "field 'name' is required");
        };
        let Some(root_payload) = payload.get("root") else {
            return json_error(400, "field 'root' is required");
        };
        let root = match parse_tree_node_input(root_payload, false) {
            Ok(node) => node,
            Err(err) => return json_error(400, err),
        };
        let default_max_children = payload
            .get("default_max_children")
            .or_else(|| payload.get("max_children"))
            .and_then(JsonValue::as_i64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|value| *value > 0);
        let Some(default_max_children) = default_max_children else {
            return json_error(
                400,
                "field 'default_max_children' must be a positive integer",
            );
        };
        let input = crate::application::CreateTreeInput {
            collection: collection.to_string(),
            name,
            root,
            default_max_children,
            if_not_exists: json_bool_field(&payload, "if_not_exists").unwrap_or(false),
        };

        match self.tree_use_cases().create_tree(input) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_result_json::runtime_query_json(&result, &None, &None),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_drop_tree(&self, collection: &str, tree_name: &str) -> HttpResponse {
        match self
            .tree_use_cases()
            .drop_tree(crate::application::DropTreeInput {
                collection: collection.to_string(),
                name: tree_name.to_string(),
                if_exists: false,
            }) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_result_json::runtime_query_json(&result, &None, &None),
            ),
            Err(err @ RedDBError::NotFound(_)) => json_error(404, err.to_string()),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_tree_insert_node(
        &self,
        collection: &str,
        tree_name: &str,
        body: Vec<u8>,
    ) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let parent_id = payload
            .get("parent_id")
            .and_then(JsonValue::as_i64)
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0);
        let Some(parent_id) = parent_id else {
            return json_error(400, "field 'parent_id' must be a positive integer");
        };
        let node_payload = payload.get("node").unwrap_or(&payload);
        let node = match parse_tree_node_input(node_payload, true) {
            Ok(node) => node,
            Err(err) => return json_error(400, err),
        };
        let position = match parse_tree_position_input(&payload) {
            Ok(position) => position,
            Err(err) => return json_error(400, err),
        };

        match self
            .tree_use_cases()
            .insert_node(crate::application::InsertTreeNodeInput {
                collection: collection.to_string(),
                tree_name: tree_name.to_string(),
                parent_id,
                node,
                position,
            }) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_result_json::runtime_query_json(&result, &None, &None),
            ),
            Err(err @ RedDBError::NotFound(_)) => json_error(404, err.to_string()),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_tree_move(
        &self,
        collection: &str,
        tree_name: &str,
        body: Vec<u8>,
    ) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let node_id = payload
            .get("node_id")
            .and_then(JsonValue::as_i64)
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0);
        let parent_id = payload
            .get("parent_id")
            .and_then(JsonValue::as_i64)
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0);
        let Some(node_id) = node_id else {
            return json_error(400, "field 'node_id' must be a positive integer");
        };
        let Some(parent_id) = parent_id else {
            return json_error(400, "field 'parent_id' must be a positive integer");
        };
        let position = match parse_tree_position_input(&payload) {
            Ok(position) => position,
            Err(err) => return json_error(400, err),
        };

        match self
            .tree_use_cases()
            .move_node(crate::application::MoveTreeNodeInput {
                collection: collection.to_string(),
                tree_name: tree_name.to_string(),
                node_id,
                parent_id,
                position,
            }) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_result_json::runtime_query_json(&result, &None, &None),
            ),
            Err(err @ RedDBError::NotFound(_)) => json_error(404, err.to_string()),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_tree_delete_node(
        &self,
        collection: &str,
        tree_name: &str,
        node_id: u64,
    ) -> HttpResponse {
        match self
            .tree_use_cases()
            .delete_node(crate::application::DeleteTreeNodeInput {
                collection: collection.to_string(),
                tree_name: tree_name.to_string(),
                node_id,
            }) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_result_json::runtime_query_json(&result, &None, &None),
            ),
            Err(err @ RedDBError::NotFound(_)) => json_error(404, err.to_string()),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_tree_validate(&self, collection: &str, tree_name: &str) -> HttpResponse {
        match self
            .tree_use_cases()
            .validate(crate::application::ValidateTreeInput {
                collection: collection.to_string(),
                tree_name: tree_name.to_string(),
            }) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_result_json::runtime_query_json(&result, &None, &None),
            ),
            Err(err @ RedDBError::NotFound(_)) => json_error(404, err.to_string()),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_tree_rebalance(
        &self,
        collection: &str,
        tree_name: &str,
        body: Vec<u8>,
    ) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        match self
            .tree_use_cases()
            .rebalance(crate::application::RebalanceTreeInput {
                collection: collection.to_string(),
                tree_name: tree_name.to_string(),
                dry_run: json_bool_field(&payload, "dry_run").unwrap_or(false),
            }) {
            Ok(result) => json_response(
                200,
                crate::presentation::query_result_json::runtime_query_json(&result, &None, &None),
            ),
            Err(err @ RedDBError::NotFound(_)) => json_error(404, err.to_string()),
            Err(err) => json_error(400, err.to_string()),
        }
    }
}

fn parse_tree_node_input(
    payload: &JsonValue,
    allow_max_children: bool,
) -> Result<crate::application::TreeNodeInput, String> {
    let JsonValue::Object(object) = payload else {
        return Err("tree node payload must be a JSON object".to_string());
    };
    let label = object
        .get("label")
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .ok_or_else(|| "field 'label' is required".to_string())?;
    let node_type = object
        .get("node_type")
        .or_else(|| object.get("type"))
        .and_then(JsonValue::as_str)
        .map(str::to_string);

    let properties = object
        .get("properties")
        .map(parse_tree_properties)
        .transpose()?
        .unwrap_or_default();
    let metadata = object
        .get("metadata")
        .map(parse_tree_metadata)
        .transpose()?
        .unwrap_or_default();
    let max_children = if allow_max_children {
        object
            .get("max_children")
            .and_then(JsonValue::as_i64)
            .and_then(|value| usize::try_from(value).ok())
    } else {
        None
    };

    Ok(crate::application::TreeNodeInput {
        label,
        node_type,
        properties,
        metadata,
        max_children,
    })
}

fn parse_tree_properties(payload: &JsonValue) -> Result<Vec<(String, Value)>, String> {
    let JsonValue::Object(object) = payload else {
        return Err("field 'properties' must be an object".to_string());
    };
    object
        .iter()
        .map(|(key, value)| {
            crate::application::entity::json_to_storage_value(value)
                .map(|value| (key.clone(), value))
                .map_err(|err| format!("invalid property '{}': {}", key, err))
        })
        .collect()
}

fn parse_tree_metadata(payload: &JsonValue) -> Result<Vec<(String, MetadataValue)>, String> {
    let JsonValue::Object(object) = payload else {
        return Err("field 'metadata' must be an object".to_string());
    };
    object
        .iter()
        .map(|(key, value)| {
            crate::application::entity::json_to_metadata_value(value)
                .map(|value| (key.clone(), value))
                .map_err(|err| format!("invalid metadata '{}': {}", key, err))
        })
        .collect()
}

fn parse_tree_position_input(
    payload: &JsonValue,
) -> Result<crate::application::TreePositionInput, String> {
    match payload.get("position") {
        None | Some(JsonValue::Null) => Ok(crate::application::TreePositionInput::Last),
        Some(JsonValue::String(value)) if value.eq_ignore_ascii_case("first") => {
            Ok(crate::application::TreePositionInput::First)
        }
        Some(JsonValue::String(value)) if value.eq_ignore_ascii_case("last") => {
            Ok(crate::application::TreePositionInput::Last)
        }
        Some(JsonValue::Number(value)) if *value >= 0.0 && value.fract().abs() < f64::EPSILON => {
            Ok(crate::application::TreePositionInput::Index(
                *value as usize,
            ))
        }
        Some(_) => Err("field 'position' must be 'first', 'last', or an integer".to_string()),
    }
}
