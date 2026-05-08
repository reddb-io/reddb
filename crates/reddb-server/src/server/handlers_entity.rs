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
            crate::presentation::entity_json::scan_page_json,
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
            crate::presentation::entity_json::created_entity_output_json,
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
            crate::presentation::entity_json::created_entity_output_json,
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
        match HttpKvOps::new(self).get(collection, key) {
            Ok(Some((value, id))) => json_response(
                200,
                kv_envelope(collection, key)
                    .with_found(true)
                    .with_value(&value)
                    .with_entity_id(id)
                    .into_json(),
            ),
            Ok(None) => json_error(404, format!("key not found: {key}")),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_put_kv(&self, collection: &str, key: &str, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let value_arg = payload.get("value").unwrap_or(&JsonValue::Null);
        let value = match crate::application::entity::json_to_storage_value(value_arg) {
            Ok(value) => value,
            Err(err) => return json_error(400, err.to_string()),
        };
        let tags = match parse_kv_tags_arg(&payload) {
            Ok(tags) => tags,
            Err(err) => return json_error(400, err),
        };

        if !tags.is_empty() {
            let _ = self
                .runtime
                .db()
                .store()
                .get_or_create_collection(collection);
            let result = crate::runtime::kv_atomic::KvAtomicOps::new(&self.runtime).set(
                "HTTP PUT KV",
                &format!("{collection}.{key}"),
                value.clone(),
                None,
                &tags,
                false,
            );
            if let Err(err) = result {
                return json_error(400, err.to_string());
            }
            return match HttpKvOps::new(self).get(collection, key) {
                Ok(Some((stored, id))) => json_response(
                    200,
                    kv_envelope(collection, key)
                        .with_found(true)
                        .with_value(&stored)
                        .with_entity_id(id)
                        .with_created(false)
                        .with_tags(&tags)
                        .into_json(),
                ),
                Ok(None) => json_error(500, "tagged KV put did not create a readable entry"),
                Err(err) => json_error(400, err.to_string()),
            };
        }

        match HttpKvOps::new(self).put(collection, key, value, Some(value_arg.clone())) {
            Ok(KvPutOutcome { value, id, created }) => {
                let status = if created { 201 } else { 200 };
                json_response(
                    status,
                    kv_envelope(collection, key)
                        .with_found(true)
                        .with_value(&value)
                        .with_entity_id(id)
                        .with_created(created)
                        .into_json(),
                )
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_invalidate_kv_tags(
        &self,
        collection: &str,
        body: Vec<u8>,
    ) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let tags = match parse_kv_tags_arg(&payload) {
            Ok(tags) if !tags.is_empty() => tags,
            Ok(_) => return json_error(400, "field 'tags' must contain at least one tag"),
            Err(err) => return json_error(400, err),
        };

        match crate::runtime::kv_atomic::KvAtomicOps::new(&self.runtime).invalidate_tags(
            "HTTP INVALIDATE TAGS",
            collection,
            &tags,
        ) {
            Ok(result) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "collection".to_string(),
                    JsonValue::String(collection.to_string()),
                );
                object.insert(
                    "tags".to_string(),
                    JsonValue::Array(tags.into_iter().map(JsonValue::String).collect()),
                );
                object.insert(
                    "invalidated".to_string(),
                    JsonValue::Number(result.affected_rows as f64),
                );
                object.insert(
                    "affected".to_string(),
                    JsonValue::Number(result.affected_rows as f64),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_delete_kv(&self, collection: &str, key: &str) -> HttpResponse {
        match HttpKvOps::new(self).delete(collection, key) {
            Ok(true) => json_response(
                200,
                kv_envelope(collection, key).with_deleted(true).into_json(),
            ),
            Ok(false) => json_error(404, format!("key not found: {key}")),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_incr_kv(
        &self,
        collection: &str,
        key: &str,
        query: &BTreeMap<String, String>,
        decr: bool,
    ) -> HttpResponse {
        let by = match query.get("by") {
            Some(value) => match value.parse::<i64>() {
                Ok(value) => value,
                Err(_) => return json_error(400, "query parameter 'by' must be an integer"),
            },
            None => 1,
        };
        let ttl_ms = query
            .get("ttl_ms")
            .or_else(|| query.get("ttlMs"))
            .or_else(|| query.get("expire_ms"))
            .map(|value| value.parse::<u64>())
            .transpose();
        let ttl_ms = match ttl_ms {
            Ok(value) => value,
            Err(_) => return json_error(400, "TTL query parameter must be an unsigned integer"),
        };
        let delta = if decr {
            match by.checked_neg() {
                Some(value) => value,
                None => return json_error(400, "DECR BY value overflows i64"),
            }
        } else {
            by
        };

        match HttpKvOps::new(self).incr(collection, key, delta, ttl_ms) {
            Ok(value) => json_response(
                200,
                kv_envelope(collection, key)
                    .with_value(&Value::Integer(value))
                    .into_json(),
            ),
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

struct HttpKvOps<'a> {
    server: &'a RedDBServer,
}

struct KvPutOutcome {
    value: Value,
    id: EntityId,
    created: bool,
}

impl<'a> HttpKvOps<'a> {
    fn new(server: &'a RedDBServer) -> Self {
        Self { server }
    }

    fn get(
        &self,
        collection: &str,
        key: &str,
    ) -> RedDBResult<Option<(crate::storage::schema::Value, EntityId)>> {
        self.server.entity_use_cases().get_kv(collection, key)
    }

    fn put(
        &self,
        collection: &str,
        key: &str,
        value: Value,
        json_value: Option<JsonValue>,
    ) -> RedDBResult<KvPutOutcome> {
        let uc = self.server.entity_use_cases();
        match uc.get_kv(collection, key)? {
            Some((_, existing_id)) => {
                uc.patch(PatchEntityInput {
                    collection: collection.to_string(),
                    id: existing_id,
                    payload: kv_patch_payload(json_value.clone().unwrap_or(JsonValue::Null)),
                    operations: vec![PatchEntityOperation {
                        op: PatchEntityOperationType::Set,
                        path: vec!["value".to_string()],
                        value: json_value,
                    }],
                })?;
                Ok(KvPutOutcome {
                    value,
                    id: existing_id,
                    created: false,
                })
            }
            None => {
                let output = uc.create_kv(CreateKvInput {
                    collection: collection.to_string(),
                    key: key.to_string(),
                    value: value.clone(),
                    metadata: Vec::new(),
                })?;
                Ok(KvPutOutcome {
                    value,
                    id: output.id,
                    created: true,
                })
            }
        }
    }

    fn delete(&self, collection: &str, key: &str) -> RedDBResult<bool> {
        self.server.entity_use_cases().delete_kv(collection, key)
    }

    fn incr(&self, collection: &str, key: &str, by: i64, ttl_ms: Option<u64>) -> RedDBResult<i64> {
        crate::runtime::kv_atomic::KvAtomicOps::new(&self.server.runtime)
            .incr(collection, key, by, ttl_ms)
    }
}

fn kv_patch_payload(value: JsonValue) -> JsonValue {
    let mut payload = Map::new();
    payload.insert("value".to_string(), value);
    JsonValue::Object(payload)
}

fn kv_envelope(collection: &str, key: &str) -> KvEnvelope {
    KvEnvelope {
        object: {
            let mut object = Map::new();
            object.insert("ok".to_string(), JsonValue::Bool(true));
            object.insert(
                "collection".to_string(),
                JsonValue::String(collection.to_string()),
            );
            object.insert("key".to_string(), JsonValue::String(key.to_string()));
            object
        },
    }
}

struct KvEnvelope {
    object: Map<String, JsonValue>,
}

impl KvEnvelope {
    fn with_found(mut self, found: bool) -> Self {
        self.object
            .insert("found".to_string(), JsonValue::Bool(found));
        self
    }

    fn with_value(mut self, value: &Value) -> Self {
        self.object.insert(
            "value".to_string(),
            crate::presentation::entity_json::storage_value_to_json(value),
        );
        self
    }

    fn with_entity_id(mut self, id: EntityId) -> Self {
        let id_value = JsonValue::Number(id.raw() as f64);
        self.object.insert("id".to_string(), id_value.clone());
        self.object.insert("entity_id".to_string(), id_value);
        self
    }

    fn with_created(mut self, created: bool) -> Self {
        self.object
            .insert("created".to_string(), JsonValue::Bool(created));
        self.object
            .insert("updated".to_string(), JsonValue::Bool(!created));
        self
    }

    fn with_deleted(mut self, deleted: bool) -> Self {
        self.object
            .insert("deleted".to_string(), JsonValue::Bool(deleted));
        self
    }

    fn with_tags(mut self, tags: &[String]) -> Self {
        self.object.insert(
            "tags".to_string(),
            JsonValue::Array(tags.iter().cloned().map(JsonValue::String).collect()),
        );
        self
    }

    fn into_json(self) -> JsonValue {
        JsonValue::Object(self.object)
    }
}

fn parse_kv_tags_arg(payload: &JsonValue) -> Result<Vec<String>, String> {
    match payload.get("tags") {
        None | Some(JsonValue::Null) => Ok(Vec::new()),
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| match value {
                JsonValue::String(tag) if !tag.is_empty() => Ok(tag.clone()),
                JsonValue::String(_) => Err("KV tags must be non-empty strings".to_string()),
                _ => Err("field 'tags' must be an array of strings".to_string()),
            })
            .collect(),
        Some(_) => Err("field 'tags' must be an array of strings".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, path: &str, body: &str) -> HttpRequest {
        HttpRequest {
            method: method.to_string(),
            path: path.to_string(),
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn response_json(response: &HttpResponse) -> JsonValue {
        let text = std::str::from_utf8(&response.body).expect("response body is utf8");
        parse_json(text).expect("response body is json").into()
    }

    #[test]
    fn http_kv_canonical_route_put_get_delete_uses_common_envelope() {
        let runtime = RedDBRuntime::in_memory().expect("runtime starts");
        let server = RedDBServer::new(runtime);

        let created = server.route(request(
            "PUT",
            "/collections/kv_default/kv/theme",
            r#"{"value":"dark"}"#,
        ));
        assert_eq!(created.status, 201);
        let created_json = response_json(&created);
        assert_eq!(
            created_json.get("ok").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(
            created_json.get("collection").and_then(JsonValue::as_str),
            Some("kv_default")
        );
        assert_eq!(
            created_json.get("key").and_then(JsonValue::as_str),
            Some("theme")
        );
        assert_eq!(
            created_json.get("value").and_then(JsonValue::as_str),
            Some("dark")
        );
        assert_eq!(
            created_json.get("created").and_then(JsonValue::as_bool),
            Some(true)
        );

        let updated = server.route(request(
            "PUT",
            "/collections/kv_default/kv/theme",
            r#"{"value":"light"}"#,
        ));
        assert_eq!(updated.status, 200);
        let updated_json = response_json(&updated);
        assert_eq!(
            updated_json.get("updated").and_then(JsonValue::as_bool),
            Some(true)
        );

        let found = server.route(request("GET", "/collections/kv_default/kv/theme", ""));
        assert_eq!(found.status, 200);
        let found_json = response_json(&found);
        assert_eq!(
            found_json.get("ok").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(
            found_json.get("found").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(
            found_json.get("value").and_then(JsonValue::as_str),
            Some("light")
        );

        let deleted = server.route(request("DELETE", "/collections/kv_default/kv/theme", ""));
        assert_eq!(deleted.status, 200);
        let deleted_json = response_json(&deleted);
        assert_eq!(
            deleted_json.get("deleted").and_then(JsonValue::as_bool),
            Some(true)
        );

        let missing = server.route(request("GET", "/collections/kv_default/kv/theme", ""));
        assert_eq!(missing.status, 404);
    }

    #[test]
    fn http_kv_incr_and_decr_return_post_update_value() {
        let runtime = RedDBRuntime::in_memory().expect("runtime starts");
        let server = RedDBServer::new(runtime);

        let mut incr = request("POST", "/collections/kv_default/kv/views/incr", "");
        incr.query.insert("by".to_string(), "5".to_string());
        let incr = server.route(incr);
        assert_eq!(incr.status, 200);
        let incr_json = response_json(&incr);
        assert_eq!(
            incr_json.get("value").and_then(JsonValue::as_f64),
            Some(5.0)
        );

        let mut decr = request("POST", "/collections/kv_default/kv/views/decr", "");
        decr.query.insert("by".to_string(), "2".to_string());
        let decr = server.route(decr);
        assert_eq!(decr.status, 200);
        let decr_json = response_json(&decr);
        assert_eq!(
            decr_json.get("value").and_then(JsonValue::as_f64),
            Some(3.0)
        );
    }

    #[test]
    fn http_kv_tags_can_be_invalidated_in_batch() {
        let runtime = RedDBRuntime::in_memory().expect("runtime starts");
        let server = RedDBServer::new(runtime);

        let a = server.route(request(
            "PUT",
            "/collections/sessions/kv/a",
            r#"{"value":"one","tags":["active","user"]}"#,
        ));
        assert_eq!(a.status, 200);
        let b = server.route(request(
            "PUT",
            "/collections/sessions/kv/b",
            r#"{"value":"two","tags":["admin"]}"#,
        ));
        assert_eq!(b.status, 200);

        let invalidated = server.route(request(
            "POST",
            "/collections/sessions/kv/invalidate-tags",
            r#"{"tags":["active"]}"#,
        ));
        assert_eq!(invalidated.status, 200);
        let invalidated_json = response_json(&invalidated);
        assert_eq!(
            invalidated_json
                .get("invalidated")
                .and_then(JsonValue::as_f64),
            Some(1.0)
        );

        assert_eq!(
            server
                .route(request("GET", "/collections/sessions/kv/a", ""))
                .status,
            404
        );
        assert_eq!(
            server
                .route(request("GET", "/collections/sessions/kv/b", ""))
                .status,
            200
        );
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
