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
    /// When the body contains `auto_embed`, embeddings are generated in a single
    /// provider round-trip before any rows are inserted (so a provider failure
    /// leaves the collection untouched).
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

        if let Some(auto_embed) = payload.get("auto_embed") {
            return self.handle_bulk_rows_with_embed(collection, rows, auto_embed);
        }

        let count = rows.len();
        if let Err(err) =
            self.entity_use_cases()
                .create_rows_batch(crate::application::CreateRowsBatchInput {
                    collection: collection.to_string(),
                    rows,
                    suppress_events: false,
                })
        {
            return json_error(400, err.to_string());
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("count".to_string(), JsonValue::Number(count as f64));
        json_response(200, JsonValue::Object(object))
    }

    /// Bulk insert with AUTO EMBED: embed all rows in one provider batch call,
    /// then insert rows. Provider failure aborts before any row is written.
    fn handle_bulk_rows_with_embed(
        &self,
        collection: &str,
        rows: Vec<crate::application::CreateRowInput>,
        auto_embed_json: &JsonValue,
    ) -> HttpResponse {
        let provider_str = match auto_embed_json.get("provider").and_then(JsonValue::as_str) {
            Some(p) => p,
            None => return json_error(400, "auto_embed.provider is required"),
        };
        let fields: Vec<String> = match auto_embed_json.get("fields").and_then(JsonValue::as_array)
        {
            Some(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            None => return json_error(400, "auto_embed.fields is required"),
        };
        if fields.is_empty() {
            return json_error(400, "auto_embed.fields cannot be empty");
        }
        let model = auto_embed_json
            .get("model")
            .and_then(JsonValue::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| {
                std::env::var("REDDB_OPENAI_EMBEDDING_MODEL")
                    .ok()
                    .unwrap_or_else(|| crate::ai::DEFAULT_OPENAI_EMBEDDING_MODEL.to_string())
            });

        let provider = match crate::ai::parse_provider(provider_str) {
            Ok(p) => p,
            Err(e) => return json_error(400, e.to_string()),
        };
        let api_key = match crate::ai::resolve_api_key_from_runtime(&provider, None, &self.runtime)
        {
            Ok(k) => k,
            Err(e) => return json_error(400, e.to_string()),
        };

        // Collect one text per row by joining the requested fields.
        let texts: Vec<String> = rows
            .iter()
            .map(|row| {
                let parts: Vec<String> = fields
                    .iter()
                    .filter_map(|field| {
                        row.fields
                            .iter()
                            .find(|(k, _)| k == field)
                            .and_then(|(_, v)| match v {
                                Value::Text(t) if !t.is_empty() => Some(t.to_string()),
                                _ => None,
                            })
                    })
                    .collect();
                parts.join(" ")
            })
            .collect();

        // Embed BEFORE insert so a provider failure leaves the collection untouched.
        let batch_client =
            crate::runtime::ai::batch_client::AiBatchClient::from_runtime(&self.runtime);
        let embeddings = match tokio::runtime::Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(|| {
                handle.block_on(batch_client.embed_batch(
                    &provider,
                    &model,
                    &api_key,
                    texts.clone(),
                ))
            }),
            Err(_) => return json_error(500, "AUTO EMBED requires a Tokio runtime context"),
        };
        let embeddings = match embeddings {
            Ok(e) => e,
            Err(e) => return json_error(502, format!("embedding provider error: {e}")),
        };

        let count = rows.len();
        if let Err(err) =
            self.entity_use_cases()
                .create_rows_batch(crate::application::CreateRowsBatchInput {
                    collection: collection.to_string(),
                    rows,
                    suppress_events: false,
                })
        {
            return json_error(400, err.to_string());
        }

        // Store vectors for rows with non-empty embeddings.
        let mut embedded_count = 0usize;
        for (combined, dense) in texts.iter().zip(embeddings) {
            if dense.is_empty() || combined.trim().is_empty() {
                continue;
            }
            if self
                .entity_use_cases()
                .create_vector(crate::application::CreateVectorInput {
                    collection: collection.to_string(),
                    dense,
                    content: Some(combined.clone()),
                    metadata: Vec::new(),
                    link_row: None,
                    link_node: None,
                })
                .is_ok()
            {
                embedded_count += 1;
            }
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("created_count".to_string(), JsonValue::Number(count as f64));
        object.insert(
            "embedded_count".to_string(),
            JsonValue::Number(embedded_count as f64),
        );
        // One batch call regardless of row count — the whole point of this slice.
        object.insert("provider_requests".to_string(), JsonValue::Number(1.0));
        json_response(200, JsonValue::Object(object))
    }

    /// Issue #582 — Analytics slice 4. `POST /collections/:name/batch`.
    /// All-or-nothing commit of a JSON array of rows under a single
    /// Statement frame, with `AnalyticsSchemaRegistry` validation up
    /// front and `Idempotency-Key` replay served from an in-memory
    /// process-wide cache (see [`crate::runtime::batch_insert`]).
    pub(crate) fn handle_batch_insert(
        &self,
        collection: &str,
        body: Vec<u8>,
        idempotency_key: Option<&str>,
    ) -> HttpResponse {
        use crate::runtime::batch_insert::{
            global_cache, BatchInsertConfig, BatchInsertError,
        };
        use std::time::Instant;

        let cache = global_cache();
        let config = BatchInsertConfig::from_env();
        let now = Instant::now();

        // Idempotency replay short-circuits before any parse work so a
        // misshapen retry of an earlier success still returns the
        // cached success.
        if let Some(key) = idempotency_key {
            if !key.is_empty() {
                if let Some(cached) = cache.lookup(collection, key, now) {
                    return HttpResponse {
                        status: cached.status,
                        body: cached.body,
                        content_type: "application/json",
                        extra_headers: Vec::new(),
                    };
                }
            }
        }

        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let Some(items) = payload.as_array() else {
            let err = BatchInsertError::BodyNotJsonArray;
            return batch_error_response(err);
        };

        if items.len() > config.max_rows {
            let err = BatchInsertError::BatchTooLarge {
                limit: config.max_rows,
                got: items.len(),
            };
            return batch_error_response(err);
        }

        // Two-phase: parse + schema-validate every row BEFORE any
        // storage write. The brief's all-or-nothing contract requires
        // that row K's failure leave the collection untouched, so we
        // refuse to start the commit until every row clears the gate.
        let mut row_inputs = Vec::with_capacity(items.len());
        let store = self.runtime.db().store();
        for (index, item) in items.iter().enumerate() {
            let input = match crate::application::entity_payload::parse_create_row_input(
                collection.to_string(),
                item,
            ) {
                Ok(input) => input,
                Err(err) => {
                    return batch_error_response(BatchInsertError::RowParseFailure {
                        index,
                        reason: err.to_string(),
                    });
                }
            };

            if let Some(reason) = schema_validate_row(store.as_ref(), &input) {
                return batch_error_response(BatchInsertError::RowSchemaRejected {
                    index,
                    reason,
                });
            }

            row_inputs.push(input);
        }

        let count = row_inputs.len();
        let result =
            self.entity_use_cases()
                .create_rows_batch(crate::application::CreateRowsBatchInput {
                    collection: collection.to_string(),
                    rows: row_inputs,
                    suppress_events: false,
                });

        let response = match result {
            Ok(_) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("count".to_string(), JsonValue::Number(count as f64));
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(400, err.to_string()),
        };

        // Cache successful results AND deterministic 4xx outcomes; the
        // brief calls for "the cached prior result" without
        // distinguishing success vs. failure. A retry with the same key
        // must see the same outcome, otherwise the dedup window leaks.
        if let Some(key) = idempotency_key {
            if !key.is_empty() {
                cache.store(
                    collection,
                    key,
                    response.status,
                    response.body.clone(),
                    config.idempotency_window,
                    now,
                );
            }
        }

        response
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
        let tags = match json_string_array_field(&payload, "tags") {
            Ok(tags) => tags,
            Err(message) => return json_error(400, message),
        };

        let ops = crate::runtime::impl_kv::KvAtomicOps::new(&self.runtime);
        match ops.set_with_tags(collection, key, value, None, &tags, false) {
            Ok((created, id)) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("id".to_string(), JsonValue::Number(id.raw() as f64));
                object.insert("created".to_string(), JsonValue::Bool(created));
                object.insert(
                    "tags".to_string(),
                    JsonValue::Array(tags.into_iter().map(JsonValue::String).collect()),
                );
                json_response(if created { 201 } else { 200 }, JsonValue::Object(object))
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
        let tags = match json_string_array_field(&payload, "tags") {
            Ok(tags) if !tags.is_empty() => tags,
            Ok(_) => return json_error(400, "field 'tags' must contain at least one string"),
            Err(message) => return json_error(400, message),
        };
        let ops = crate::runtime::impl_kv::KvAtomicOps::new(&self.runtime);
        match ops.invalidate_tags(collection, &tags) {
            Ok(count) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("invalidated".to_string(), JsonValue::Number(count as f64));
                object.insert(
                    "tags".to_string(),
                    JsonValue::Array(tags.into_iter().map(JsonValue::String).collect()),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    pub(crate) fn handle_delete_kv(&self, collection: &str, key: &str) -> HttpResponse {
        let ops = crate::runtime::impl_kv::KvAtomicOps::new(&self.runtime);
        match ops.delete(crate::catalog::CollectionModel::Kv, collection, key) {
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

    pub(crate) fn handle_watch_kv(
        &self,
        collection: &str,
        key: &str,
        query: &BTreeMap<String, String>,
    ) -> HttpResponse {
        let since_lsn = query
            .get("since_lsn")
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_else(|| self.runtime.cdc_current_lsn());
        let limit = query
            .get("limit")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(100)
            .clamp(1, 1000);

        let (watch_key, prefix) = key
            .strip_suffix(".*")
            .or_else(|| key.strip_suffix(".%2A"))
            .or_else(|| key.strip_suffix(".%2a"))
            .map(|prefix| (prefix, true))
            .unwrap_or((key, false));
        let events = if prefix {
            self.runtime
                .kv_watch_events_since_prefix(collection, watch_key, since_lsn, limit)
        } else {
            self.runtime
                .kv_watch_events_since(collection, watch_key, since_lsn, limit)
        };

        let mut body = Vec::new();
        for event in events {
            body.extend_from_slice(b"event: kv\n");
            body.extend_from_slice(b"data: ");
            body.extend_from_slice(
                crate::json::to_string(&event.to_json_value())
                    .unwrap_or_else(|_| "{}".to_string())
                    .as_bytes(),
            );
            body.extend_from_slice(b"\n\n");
        }

        HttpResponse {
            status: 200,
            body,
            content_type: "text/event-stream",
            extra_headers: Vec::new(),
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
        let metadata = match payload.get("metadata").map(parse_tree_metadata).transpose() {
            Ok(metadata) => metadata.unwrap_or_default(),
            Err(err) => return json_error(400, err),
        };

        match self
            .entity_use_cases()
            .create_document(CreateDocumentInput {
                collection: collection.to_string(),
                body: body_value,
                metadata,
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

    pub(crate) fn handle_get_entity(&self, collection: &str, id: u64) -> HttpResponse {
        match self.runtime.db().store().get(collection, EntityId::new(id)) {
            Some(entity) => {
                json_response(200, crate::presentation::entity_json::entity_json(&entity))
            }
            None => json_error(404, format!("entity not found: {id}")),
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

/// Build the HTTP error body for a `BatchInsertError`. Uses
/// `json_error_code` so the response carries both `code` (machine-
/// readable for the brief's typed-error contract) and `error`/`message`
/// (human-readable) on the same envelope every other 4xx uses.
fn batch_error_response(err: crate::runtime::batch_insert::BatchInsertError) -> HttpResponse {
    let mut object = Map::new();
    object.insert("ok".to_string(), JsonValue::Bool(false));
    object.insert("code".to_string(), JsonValue::String(err.code().to_string()));
    let message = err.message();
    object.insert(
        "error".to_string(),
        crate::json_field::SerializedJsonField::tainted(&message),
    );
    object.insert(
        "message".to_string(),
        crate::json_field::SerializedJsonField::tainted(&message),
    );
    if let Some(index) = err.row_index() {
        object.insert("row_index".to_string(), JsonValue::Number(index as f64));
    }
    json_response(err.http_status(), JsonValue::Object(object))
}

/// Run `AnalyticsSchemaRegistry::validate` against a single row's
/// `payload` when its `event_name` field names a registered schema.
/// Returns `None` when the row passes (or no schema is registered)
/// and `Some(reason)` when the registry rejects.
fn schema_validate_row(
    store: &crate::storage::unified::UnifiedStore,
    input: &crate::application::CreateRowInput,
) -> Option<String> {
    let event_name = input.fields.iter().find_map(|(k, v)| {
        if k.eq_ignore_ascii_case("event_name") {
            match v {
                Value::Text(t) => Some(t.to_string()),
                _ => None,
            }
        } else {
            None
        }
    })?;
    crate::runtime::analytics_schema_registry::latest(store, &event_name)?;
    let payload_json = input
        .fields
        .iter()
        .find_map(|(k, v)| {
            if k.eq_ignore_ascii_case("payload") {
                match v {
                    Value::Text(t) => Some(t.to_string()),
                    _ => None,
                }
            } else {
                None
            }
        })
        .unwrap_or_else(|| "{}".to_string());
    match crate::runtime::analytics_schema_registry::validate(store, &event_name, &payload_json) {
        Ok(()) => None,
        Err(err) => {
            let mapped = crate::runtime::analytics_schema_registry::validation_error_to_reddb(err);
            Some(mapped.to_string())
        }
    }
}

fn json_string_array_field(payload: &JsonValue, field: &str) -> Result<Vec<String>, String> {
    match payload.get(field) {
        None | Some(JsonValue::Null) => Ok(Vec::new()),
        Some(JsonValue::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| format!("field '{field}' must be an array of strings"))
            })
            .collect(),
        _ => Err(format!("field '{field}' must be an array of strings")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedDBOptions, RedDBRuntime};

    fn make_server() -> RedDBServer {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        RedDBServer::new(rt)
    }

    fn post_bulk_rows(server: &RedDBServer, collection: &str, body: &str) -> HttpResponse {
        server.handle_bulk_create_rows_fast(collection, body.as_bytes().to_vec())
    }

    #[test]
    fn bulk_create_rows_legacy_path_unchanged() {
        let server = make_server();
        // CREATE the collection first
        let ddl = r#"{"query": "CREATE TABLE articles (id INTEGER, title TEXT)"}"#;
        let r = server.handle_query(ddl.as_bytes().to_vec());
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));

        let body = r#"{"items": [{"fields": {"id": 1, "title": "hello"}}, {"fields": {"id": 2, "title": "world"}}]}"#;
        let r = post_bulk_rows(&server, "articles", body);
        assert_eq!(r.status, 200);
        let parsed = crate::json::parse_json(std::str::from_utf8(&r.body).unwrap()).unwrap();
        assert_eq!(parsed.get("count").and_then(|v| v.as_f64()), Some(2.0));
        // legacy response has no embedded_count field
        assert!(parsed.get("embedded_count").is_none());
    }

    #[test]
    fn bulk_rows_with_embed_missing_provider_returns_400() {
        let server = make_server();
        let ddl = r#"{"query": "CREATE TABLE docs (body TEXT)"}"#;
        server.handle_query(ddl.as_bytes().to_vec());

        let body =
            r#"{"items": [{"fields": {"body": "hello"}}], "auto_embed": {"fields": ["body"]}}"#;
        let r = post_bulk_rows(&server, "docs", body);
        assert_eq!(r.status, 400);
        let text = String::from_utf8_lossy(&r.body);
        assert!(
            text.contains("provider"),
            "expected provider error, got: {text}"
        );
    }

    #[test]
    fn bulk_rows_with_embed_missing_fields_returns_400() {
        let server = make_server();
        let ddl = r#"{"query": "CREATE TABLE docs (body TEXT)"}"#;
        server.handle_query(ddl.as_bytes().to_vec());

        let body =
            r#"{"items": [{"fields": {"body": "hello"}}], "auto_embed": {"provider": "openai"}}"#;
        let r = post_bulk_rows(&server, "docs", body);
        assert_eq!(r.status, 400);
        let text = String::from_utf8_lossy(&r.body);
        assert!(
            text.contains("fields"),
            "expected fields error, got: {text}"
        );
    }

    #[test]
    fn bulk_rows_with_embed_empty_fields_returns_400() {
        let server = make_server();
        let ddl = r#"{"query": "CREATE TABLE docs (body TEXT)"}"#;
        server.handle_query(ddl.as_bytes().to_vec());

        let body = r#"{"items": [{"fields": {"body": "hello"}}], "auto_embed": {"provider": "openai", "fields": []}}"#;
        let r = post_bulk_rows(&server, "docs", body);
        assert_eq!(r.status, 400);
        let text = String::from_utf8_lossy(&r.body);
        assert!(
            text.contains("fields"),
            "expected fields error, got: {text}"
        );
    }

    #[test]
    fn bulk_rows_with_embed_invalid_provider_returns_400() {
        let server = make_server();
        let ddl = r#"{"query": "CREATE TABLE docs (body TEXT)"}"#;
        server.handle_query(ddl.as_bytes().to_vec());

        let body = r#"{"items": [{"fields": {"body": "hello"}}], "auto_embed": {"provider": "not-a-real-provider", "fields": ["body"]}}"#;
        let r = post_bulk_rows(&server, "docs", body);
        assert_eq!(r.status, 400);
    }

    #[test]
    fn bulk_rows_empty_items_returns_400() {
        let server = make_server();
        let r = post_bulk_rows(&server, "any", r#"{"items": []}"#);
        assert_eq!(r.status, 400);
    }

    #[test]
    fn bulk_rows_missing_items_returns_400() {
        let server = make_server();
        let r = post_bulk_rows(&server, "any", r#"{"rows": []}"#);
        assert_eq!(r.status, 400);
    }

    // ── Issue #582 — BatchInsertEndpoint ──────────────────────────────

    fn post_batch(
        server: &RedDBServer,
        collection: &str,
        body: &str,
        idempotency_key: Option<&str>,
    ) -> HttpResponse {
        server.handle_batch_insert(collection, body.as_bytes().to_vec(), idempotency_key)
    }

    fn create_table(server: &RedDBServer, ddl: &str) {
        let body = format!(r#"{{"query": "{ddl}"}}"#);
        let r = server.handle_query(body.into_bytes());
        assert_eq!(r.status, 200, "ddl failed: {}", String::from_utf8_lossy(&r.body));
    }

    #[test]
    fn batch_insert_happy_path_returns_200_with_count() {
        let server = make_server();
        create_table(&server, "CREATE TABLE events (id INTEGER, name TEXT)");

        let body = r#"[
            {"fields": {"id": 1, "name": "a"}},
            {"fields": {"id": 2, "name": "b"}},
            {"fields": {"id": 3, "name": "c"}}
        ]"#;
        let r = post_batch(&server, "events", body, None);
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        let parsed = crate::json::parse_json(std::str::from_utf8(&r.body).unwrap()).unwrap();
        assert_eq!(parsed.get("count").and_then(|v| v.as_f64()), Some(3.0));
    }

    #[test]
    fn batch_insert_oversize_returns_413_before_storage() {
        let server = make_server();
        create_table(&server, "CREATE TABLE events (id INTEGER, name TEXT)");

        // Build a body one row over the documented default ceiling so
        // we don't touch the process-wide env, which other tests in
        // this module read.
        let max = 10_000;
        let mut body = String::from("[");
        for i in 0..(max + 1) {
            if i > 0 {
                body.push(',');
            }
            body.push_str(&format!(
                "{{\"fields\":{{\"id\":{i},\"name\":\"x\"}}}}"
            ));
        }
        body.push(']');
        let r = post_batch(&server, "events", &body, None);
        assert_eq!(r.status, 413, "{}", String::from_utf8_lossy(&r.body));
        let text = String::from_utf8_lossy(&r.body);
        assert!(text.contains("BatchTooLarge"), "body={text}");
        assert!(text.contains("\"code\":\"BatchTooLarge\""), "body={text}");

        // Storage should be untouched — a scan returns zero rows.
        let scan = server.handle_scan("events", &Default::default());
        let scan_text = String::from_utf8_lossy(&scan.body);
        assert!(
            !scan_text.contains("\"id\":1"),
            "oversize batch leaked rows: {scan_text}"
        );
    }

    #[test]
    fn batch_insert_row_failure_rolls_back_whole_batch() {
        let server = make_server();
        create_table(&server, "CREATE TABLE events (id INTEGER, name TEXT)");

        // Row 1 omits the required `fields` object — the row-parse
        // step rejects before any commit fires.
        let body = r#"[
            {"fields": {"id": 1, "name": "a"}},
            {"not_fields": {"id": 2}},
            {"fields": {"id": 3, "name": "c"}}
        ]"#;
        let r = post_batch(&server, "events", body, None);
        assert_eq!(r.status, 400, "{}", String::from_utf8_lossy(&r.body));
        let text = String::from_utf8_lossy(&r.body);
        assert!(text.contains("\"row_index\":1"), "body={text}");
        assert!(text.contains("RowParseFailure"), "body={text}");

        // Storage is untouched (row 0 was never committed even
        // though it would have parsed cleanly on its own).
        let scan = server.handle_scan("events", &Default::default());
        let scan_text = String::from_utf8_lossy(&scan.body);
        assert!(
            !scan_text.contains("\"id\":1"),
            "row 0 leaked despite row 1 rejection: {scan_text}"
        );
    }

    #[test]
    fn batch_insert_idempotency_key_replays_cached_result() {
        let server = make_server();
        create_table(&server, "CREATE TABLE events (id INTEGER, name TEXT)");

        let body = r#"[{"fields": {"id": 1, "name": "first"}}]"#;
        let r1 = post_batch(&server, "events", body, Some("replay-token-1"));
        assert_eq!(r1.status, 200);
        let body1 = String::from_utf8_lossy(&r1.body).to_string();

        // Replay with the same key + DIFFERENT body should still
        // return the cached prior result and NOT execute again.
        let other_body = r#"[{"fields": {"id": 2, "name": "second"}}]"#;
        let r2 = post_batch(&server, "events", other_body, Some("replay-token-1"));
        assert_eq!(r2.status, 200);
        assert_eq!(
            String::from_utf8_lossy(&r2.body).to_string(),
            body1,
            "replay must return the cached body byte-for-byte"
        );

        // Storage holds only the first row, proving the replay did
        // not re-execute.
        let scan = server.handle_scan("events", &Default::default());
        let scan_text = String::from_utf8_lossy(&scan.body);
        assert!(scan_text.contains("\"name\":\"first\""), "{scan_text}");
        assert!(
            !scan_text.contains("\"name\":\"second\""),
            "replay re-executed: {scan_text}"
        );
    }

    #[test]
    fn batch_insert_schema_validation_rejects_unknown_field() {
        use crate::runtime::analytics_schema_registry as reg;

        let server = make_server();
        create_table(
            &server,
            "CREATE TABLE events (event_name TEXT, payload TEXT)",
        );

        // Register a schema that only allows {"url"}.
        let schema =
            r#"{"type":"object","properties":{"url":{"type":"string"}},"required":["url"]}"#;
        reg::register(server.runtime().db().store().as_ref(), "click", schema)
            .expect("register schema");

        // Row 1 carries an unknown `extra` field — registry must
        // reject before any commit.
        let body = r#"[
            {"fields": {"event_name": "click", "payload": "{\"url\":\"/a\"}"}},
            {"fields": {"event_name": "click", "payload": "{\"url\":\"/b\",\"extra\":1}"}}
        ]"#;
        let r = post_batch(&server, "events", body, None);
        assert_eq!(r.status, 400, "{}", String::from_utf8_lossy(&r.body));
        let text = String::from_utf8_lossy(&r.body);
        assert!(text.contains("RowSchemaRejected"), "body={text}");
        assert!(text.contains("\"row_index\":1"), "body={text}");

        // Storage is untouched.
        let scan = server.handle_scan("events", &Default::default());
        let scan_text = String::from_utf8_lossy(&scan.body);
        assert!(
            !scan_text.contains("\"url\":\"/a\""),
            "row 0 leaked despite row 1 schema rejection: {scan_text}"
        );
    }
}
