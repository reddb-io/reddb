use super::*;

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

        match self
            .query_use_cases()
            .scan(crate::application::ScanCollectionInput {
                collection: collection.to_string(),
                offset,
                limit,
            }) {
            Ok(page) => json_response(200, crate::presentation::entity_json::scan_page_json(&page)),
            Err(err) => json_error(404, err.to_string()),
        }
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

        match self.entity_use_cases().create_row(input) {
            Ok(output) => json_response(
                200,
                crate::presentation::entity_json::created_entity_output_json(&output),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
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

        match self.entity_use_cases().create_node(input) {
            Ok(output) => json_response(
                200,
                crate::presentation::entity_json::created_entity_output_json(&output),
            ),
            Err(err) => json_error(400, err.to_string()),
        }
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
            Some(JsonValue::String(s)) => Value::Text(s.clone()),
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
}
