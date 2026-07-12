use super::*;

const RED_CONFIG_COLLECTION: &str = "red_config";
const DEFAULT_MAX_INPUTS: usize = 256;
const DEFAULT_MAX_PROMPTS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiQuerySourceMode {
    Row,
    Result,
}

use crate::ai::AiProvider;

#[derive(Debug, Clone)]
struct AiEmbeddingSaveOptions {
    collection: String,
    include_content: bool,
    metadata: Vec<(String, MetadataValue)>,
}

#[derive(Debug, Clone)]
struct AiEmbeddingInputItem {
    text: String,
    source_row: Option<TableRef>,
}

#[derive(Debug, Clone)]
struct AiPromptSaveOptions {
    collection: String,
    prompt_field: String,
    response_field: String,
    metadata: Vec<(String, MetadataValue)>,
}

impl RedDBServer {
    /// GET /config — export all red_config KV pairs as a nested JSON tree.
    pub(crate) fn handle_config_export(&self) -> HttpResponse {
        let store = self.runtime.db().store();
        let Some(manager) = store.get_collection(RED_CONFIG_COLLECTION) else {
            return json_response(200, JsonValue::Object(Map::new()));
        };

        let entities = manager.query_all(|_| true);
        let mut root = Map::new();

        for entity in entities {
            if let EntityData::Row(ref row) = entity.data {
                let (key, value) = match row.named.as_ref() {
                    Some(named) => {
                        let k = named
                            .get("key")
                            .and_then(|v| match v {
                                Value::Text(s) => Some(s.as_ref()),
                                _ => None,
                            })
                            .unwrap_or("");
                        let v = named.get("value").cloned().unwrap_or(Value::Null);
                        (k.to_string(), v)
                    }
                    None => continue,
                };
                if key.is_empty() {
                    continue;
                }

                let json_val = match &value {
                    Value::Text(s) => JsonValue::String(s.to_string()),
                    Value::Integer(n) => JsonValue::Number(*n as f64),
                    Value::Float(n) => JsonValue::Number(*n),
                    Value::Boolean(b) => JsonValue::Bool(*b),
                    _ => JsonValue::String(value.to_string()),
                };

                // Build nested tree from dot-notation key
                let parts: Vec<&str> = key.split('.').collect();
                insert_nested(&mut root, &parts, json_val);
            }
        }

        let mut wrapper = Map::new();
        wrapper.insert("ok".to_string(), JsonValue::Bool(true));
        wrapper.insert("config".to_string(), JsonValue::Object(root));
        json_response(200, JsonValue::Object(wrapper))
    }

    /// GET /config/{key} — get a single config value or subtree.
    pub(crate) fn handle_config_get_key(&self, key_path: &str) -> HttpResponse {
        let store = self.runtime.db().store();
        let Some(manager) = store.get_collection(RED_CONFIG_COLLECTION) else {
            return json_error(404, format!("config key not found: {key_path}"));
        };

        let entities = manager.query_all(|_| true);
        let prefix = key_path.trim_matches('.');
        let mut exact_match = None;
        let mut subtree = Map::new();

        for entity in entities {
            if let EntityData::Row(ref row) = entity.data {
                let Some(named) = row.named.as_ref() else {
                    continue;
                };
                let key = named
                    .get("key")
                    .and_then(|v| match v {
                        Value::Text(s) => Some(s.as_ref()),
                        _ => None,
                    })
                    .unwrap_or("");
                if key.is_empty() {
                    continue;
                }
                let value = named.get("value").cloned().unwrap_or(Value::Null);
                let json_val = match &value {
                    Value::Text(s) => JsonValue::String(s.to_string()),
                    Value::Integer(n) => JsonValue::Number(*n as f64),
                    Value::Float(n) => JsonValue::Number(*n),
                    Value::Boolean(b) => JsonValue::Bool(*b),
                    Value::UnsignedInteger(n) => JsonValue::Number(*n as f64),
                    _ => JsonValue::String(value.to_string()),
                };

                if key == prefix {
                    exact_match = Some(json_val.clone());
                }
                if key.starts_with(prefix) {
                    let suffix = key.strip_prefix(prefix).unwrap_or(key);
                    let suffix = suffix.trim_start_matches('.');
                    if suffix.is_empty() {
                        subtree.insert("_value".to_string(), json_val);
                    } else {
                        let parts: Vec<&str> = suffix.split('.').collect();
                        insert_nested(&mut subtree, &parts, json_val);
                    }
                }
            }
        }

        if subtree.is_empty() {
            if let Some(val) = exact_match {
                let mut obj = Map::new();
                obj.insert("ok".to_string(), JsonValue::Bool(true));
                obj.insert("key".to_string(), JsonValue::String(prefix.to_string()));
                obj.insert("value".to_string(), val);
                return json_response(200, JsonValue::Object(obj));
            }
            return json_error(404, format!("config key not found: {prefix}"));
        }

        let mut obj = Map::new();
        obj.insert("ok".to_string(), JsonValue::Bool(true));
        obj.insert("key".to_string(), JsonValue::String(prefix.to_string()));
        if subtree.len() == 1 && subtree.contains_key("_value") {
            if let Some(value) = subtree.remove("_value") {
                obj.insert("value".to_string(), value);
            } else {
                obj.insert("value".to_string(), JsonValue::Object(subtree));
            }
        } else {
            obj.insert("value".to_string(), JsonValue::Object(subtree));
        }
        json_response(200, JsonValue::Object(obj))
    }

    /// PUT /config/{key} — set a single config value.
    pub(crate) fn handle_config_set_key(&self, key_path: &str, body: Vec<u8>) -> HttpResponse {
        let key = key_path.trim_matches('.').to_string();
        if key.is_empty() {
            return json_error(400, "config key cannot be empty");
        }

        let payload = match parse_json_body_allow_empty(&body) {
            Ok(p) => p,
            Err(resp) => return resp,
        };

        // Accept {"value": X} or just X directly
        let value = match payload.get("value") {
            Some(v) => v.clone(),
            None => payload.clone(),
        };

        // If value is an object, set as subtree
        if let JsonValue::Object(_) = &value {
            let store = self.runtime.db().store();
            let count = store.set_config_tree(&key, &value);
            let mut obj = Map::new();
            obj.insert("ok".to_string(), JsonValue::Bool(true));
            obj.insert("key".to_string(), JsonValue::String(key));
            obj.insert("set".to_string(), JsonValue::Number(count as f64));
            return json_response(200, JsonValue::Object(obj));
        }

        let store_value = match &value {
            JsonValue::String(s) => Value::text(s.clone()),
            JsonValue::Integer(n) => Value::Integer(*n),
            JsonValue::Number(n) => {
                if n.fract().abs() < f64::EPSILON {
                    Value::Integer(*n as i64)
                } else {
                    Value::Float(*n)
                }
            }
            JsonValue::Bool(b) => Value::Boolean(*b),
            JsonValue::Null => Value::Null,
            other => Value::text(crate::json::to_string(other).unwrap_or_default()),
        };

        let _ = self
            .entity_use_cases()
            .delete_kv(RED_CONFIG_COLLECTION, &key);
        match self.entity_use_cases().create_kv(CreateKvInput {
            collection: RED_CONFIG_COLLECTION.to_string(),
            key: key.clone(),
            value: store_value,
            metadata: Vec::new(),
        }) {
            Ok(_) => {
                let mut obj = Map::new();
                obj.insert("ok".to_string(), JsonValue::Bool(true));
                obj.insert("key".to_string(), JsonValue::String(key));
                obj.insert("value".to_string(), value);
                json_response(200, JsonValue::Object(obj))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    /// DELETE /config/{key} — delete a config key.
    pub(crate) fn handle_config_delete_key(&self, key_path: &str) -> HttpResponse {
        let key = key_path.trim_matches('.').to_string();
        match self
            .entity_use_cases()
            .delete_kv(RED_CONFIG_COLLECTION, &key)
        {
            Ok(true) => {
                let mut obj = Map::new();
                obj.insert("ok".to_string(), JsonValue::Bool(true));
                obj.insert("deleted".to_string(), JsonValue::String(key));
                json_response(200, JsonValue::Object(obj))
            }
            Ok(false) => json_error(404, format!("config key not found: {key}")),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    /// POST /config — import a JSON tree as flat dot-notation KV pairs into red_config.
    pub(crate) fn handle_config_import(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let config = match payload.get("config") {
            Some(c) => c.clone(),
            None => payload.clone(),
        };

        let mut pairs = Vec::new();
        flatten_json("", &config, &mut pairs);

        let mut saved = 0usize;
        for (key, value) in &pairs {
            let _ = self
                .entity_use_cases()
                .delete_kv(RED_CONFIG_COLLECTION, key);
            let store_value = match value {
                JsonValue::String(s) => Value::text(s.clone()),
                JsonValue::Integer(n) => Value::Integer(*n),
                JsonValue::Number(n) => {
                    if n.fract().abs() < f64::EPSILON {
                        Value::Integer(*n as i64)
                    } else {
                        Value::Float(*n)
                    }
                }
                JsonValue::Bool(b) => Value::Boolean(*b),
                JsonValue::Null => Value::Null,
                other => Value::text(crate::json::to_string(other).unwrap_or_default()),
            };
            if self
                .entity_use_cases()
                .create_kv(CreateKvInput {
                    collection: RED_CONFIG_COLLECTION.to_string(),
                    key: key.clone(),
                    value: store_value,
                    metadata: Vec::new(),
                })
                .is_ok()
            {
                saved += 1;
            }
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("imported".to_string(), JsonValue::Number(saved as f64));
        object.insert(
            "keys".to_string(),
            JsonValue::Array(
                pairs
                    .iter()
                    .map(|(k, _)| JsonValue::String(k.clone()))
                    .collect(),
            ),
        );
        json_response(200, JsonValue::Object(object))
    }

    pub(crate) fn handle_ai_ask(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let question = match payload.get("question").and_then(JsonValue::as_str) {
            Some(q) if !q.trim().is_empty() => q.trim().to_string(),
            _ => return json_error(400, "field 'question' must be a non-empty string"),
        };

        let provider_str =
            json_string_field(&payload, "provider").unwrap_or_else(|| "openai".to_string());
        let provider = match crate::ai::parse_provider(&provider_str) {
            Ok(p) => p,
            Err(err) => return json_error(400, err.to_string()),
        };
        if matches!(provider, AiProvider::Local) {
            let err = crate::ai::local_prompt_unavailable_error();
            let (status, msg) = crate::server::transport::map_runtime_error(&err);
            return json_error(status, msg);
        }
        let credential = json_string_field(&payload, "credential");
        let api_key = match self.resolve_provider_api_key(&provider, credential.as_deref()) {
            Ok(key) => key,
            Err(err) => return json_error(400, err),
        };

        // Search context
        let context_input = crate::application::SearchContextInput {
            query: question.clone(),
            field: json_string_field(&payload, "field"),
            vector: None,
            collections: crate::application::json_input::json_string_list_field(
                &payload,
                "collections",
            ),
            graph_depth: json_usize_field(&payload, "depth"),
            graph_max_edges: None,
            max_cross_refs: None,
            follow_cross_refs: None,
            expand_graph: None,
            global_scan: None,
            reindex: None,
            limit: json_usize_field(&payload, "limit"),
            min_score: None,
        };

        let context_result = match self.query_use_cases().search_context(context_input) {
            Ok(r) => r,
            Err(err) => return json_error(400, err.to_string()),
        };

        // Build context for LLM
        let context_json =
            crate::presentation::query_json::context_search_result_json(&context_result);
        let context_str =
            crate::json::to_string(&context_json).unwrap_or_else(|_| "{}".to_string());

        let model = json_string_field(&payload, "model").unwrap_or_else(|| {
            std::env::var(provider.prompt_model_env_name())
                .ok()
                .unwrap_or_else(|| provider.default_prompt_model().to_string())
        });
        let api_base = provider.resolve_api_base();

        let system_prompt = format!(
            "You are an AI assistant answering questions based on data from a multi-modal database. \
             Use the following context to answer the user's question. \
             If the context does not contain enough information, say so. \
             Always cite which collections and entity types your answer is based on.\n\n\
             Database context:\n{context_str}"
        );
        let full_prompt = format!("{system_prompt}\n\nQuestion: {question}");

        let transport = crate::runtime::ai::transport::AiTransport::from_runtime(&self.runtime);
        let prompt_result = match provider {
            AiProvider::Anthropic => {
                let request = crate::ai::AnthropicPromptRequest {
                    api_key,
                    model: model.clone(),
                    prompt: full_prompt,
                    temperature: Some(0.3),
                    max_output_tokens: Some(2048),
                    api_base,
                    anthropic_version: crate::ai::DEFAULT_ANTHROPIC_VERSION.to_string(),
                };
                crate::runtime::ai::block_on_ai(async move {
                    crate::ai::anthropic_prompt_async(&transport, request).await
                })
                .and_then(|result| result)
            }
            _ => {
                let request = crate::ai::OpenAiPromptRequest {
                    api_key,
                    model: model.clone(),
                    prompt: full_prompt,
                    temperature: Some(0.3),
                    seed: None,
                    max_output_tokens: Some(2048),
                    api_base,
                    stream: false,
                };
                crate::runtime::ai::block_on_ai(async move {
                    crate::ai::openai_prompt_async(&transport, request).await
                })
                .and_then(|result| result)
            }
        };
        let (answer, prompt_tokens, completion_tokens) = match prompt_result {
            Ok(resp) => (
                resp.output_text,
                resp.prompt_tokens.unwrap_or(0),
                resp.completion_tokens.unwrap_or(0),
            ),
            Err(err) => return json_error(502, err.to_string()),
        };

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("answer".to_string(), JsonValue::String(answer));
        object.insert(
            "provider".to_string(),
            JsonValue::String(provider.token().to_string()),
        );
        object.insert("model".to_string(), JsonValue::String(model));
        object.insert(
            "prompt_tokens".to_string(),
            JsonValue::Number(prompt_tokens as f64),
        );
        object.insert(
            "completion_tokens".to_string(),
            JsonValue::Number(completion_tokens as f64),
        );
        object.insert("sources".to_string(), context_json);
        json_response(200, JsonValue::Object(object))
    }

    pub(crate) fn handle_ai_embeddings(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        // An explicit `provider` is honoured verbatim; when absent, the
        // embeddings task pointer drives selection (ADR-0068 §5).
        let provider = if json_string_field(&payload, "provider").is_some() {
            match parse_ai_provider(&payload) {
                Ok(provider) => provider,
                Err(err) => return json_error(400, err),
            }
        } else {
            match crate::ai::resolve_embeddings_provider_from_runtime(&self.runtime, "") {
                Ok(provider) => provider,
                Err(err) => return json_error(400, err.to_string()),
            }
        };
        // Provider routing for embeddings:
        //
        // * OpenAI-compatible providers (Groq, Ollama, OpenRouter,
        //   Together, Venice, DeepSeek, Custom, OpenAI itself) all
        //   speak the same `POST /embeddings` shape, so they go
        //   through the shared transport below.
        // * HuggingFace has its own wire shape — feature-extraction
        //   pipeline endpoint per model, payload `{"inputs": "..."}`.
        //   Routed via `huggingface_embeddings()` below.
        // * Anthropic has no embeddings product. We fail fast with a
        //   clear, operator-actionable message rather than silently
        //   re-routing — silent fallback would mask config bugs and
        //   produce surprising bills against the wrong provider.
        // * Local needs the `local-models` feature flag.
        match &provider {
            crate::ai::AiProvider::Anthropic => {
                return json_error(
                    400,
                    "Anthropic does not offer an embeddings API. \
                     Re-issue the request against an OpenAI-compatible \
                     provider (openai, groq, ollama, openrouter, \
                     together, venice, deepseek), HuggingFace, or a \
                     custom base URL — RedDB does not silently route \
                     embeddings to a different provider than the one \
                     you named."
                        .to_string(),
                );
            }
            crate::ai::AiProvider::Local => {
                // Local provider has its own end-to-end path: it
                // resolves a registered+installed local model through
                // the runtime's swappable backend and short-circuits
                // before the OpenAI-compatible transport code below.
                return self.handle_ai_embeddings_local(&payload);
            }
            _ => {}
        }

        // Explicit `model` wins; otherwise resolve via env → provider models
        // block → built-in default (ADR-0068 §5).
        let model = crate::ai::resolve_embeddings_model_from_runtime(
            &self.runtime,
            &provider,
            json_string_field(&payload, "model").as_deref(),
        );
        let dimensions = match payload
            .get("dimensions")
            .and_then(JsonValue::as_i64)
            .map(|value| usize::try_from(value).ok())
        {
            Some(Some(value)) if value > 0 => Some(value),
            Some(_) => return json_error(400, "field 'dimensions' must be a positive integer"),
            None => None,
        };

        let max_inputs = match parse_optional_positive_usize(&payload, "max_inputs")
            .map(|v| v.unwrap_or(DEFAULT_MAX_INPUTS))
        {
            Ok(value) => value,
            Err(err) => return json_error(400, err),
        };

        let inputs = match self.collect_ai_embedding_inputs(&payload, max_inputs) {
            Ok(inputs) => inputs,
            Err(err) => return json_error(400, err),
        };

        let save_options = match parse_ai_embedding_save_options(&payload) {
            Ok(options) => options,
            Err(err) => return json_error(400, err),
        };

        let credential = json_string_field(&payload, "credential");
        let api_key = match self.resolve_provider_api_key(&provider, credential.as_deref()) {
            Ok(api_key) => api_key,
            Err(err) => return json_error(400, err),
        };

        let api_base = std::env::var(provider.api_base_env_name())
            .unwrap_or_else(|_| provider.default_api_base().to_string());

        let response = match &provider {
            crate::ai::AiProvider::HuggingFace => {
                let texts: Vec<String> = inputs.iter().map(|item| item.text.clone()).collect();
                match crate::ai::huggingface_embeddings(&api_key, &model, &texts, &api_base) {
                    Ok(r) => r,
                    Err(err) => return json_error(400, err.to_string()),
                }
            }
            _ => {
                let transport =
                    crate::runtime::ai::transport::AiTransport::from_runtime(&self.runtime);
                let request = crate::ai::OpenAiEmbeddingRequest {
                    api_key,
                    model: model.clone(),
                    inputs: inputs.iter().map(|item| item.text.clone()).collect(),
                    dimensions,
                    api_base,
                };
                match crate::runtime::ai::block_on_ai(async move {
                    crate::ai::openai_embeddings_async(&transport, request).await
                })
                .and_then(|result| result)
                {
                    Ok(response) => response,
                    Err(err) => return json_error(400, err.to_string()),
                }
            }
        };

        if response.embeddings.len() != inputs.len() {
            return json_error(
                400,
                "provider returned a different number of embeddings than requested inputs",
            );
        }

        let mut saved = Vec::new();
        if let Some(save) = save_options {
            // The provider request is already batched. Persist sequentially so
            // collection/index maintenance stays deterministic and safe.
            for (index, embedding) in response.embeddings.iter().cloned().enumerate() {
                let mut metadata = save.metadata.clone();
                metadata.push((
                    "_ai_provider".to_string(),
                    MetadataValue::String(response.provider.to_string()),
                ));
                metadata.push((
                    "_ai_model".to_string(),
                    MetadataValue::String(response.model.clone()),
                ));
                if let Some(ref credential) = credential {
                    metadata.push((
                        "_ai_credential".to_string(),
                        MetadataValue::String(credential.clone()),
                    ));
                }
                if let Some(source_row) = &inputs[index].source_row {
                    metadata.push((
                        "_source_collection".to_string(),
                        MetadataValue::String(source_row.table.clone()),
                    ));
                    metadata.push((
                        "_source_row_id".to_string(),
                        MetadataValue::Int(source_row.row_id as i64),
                    ));
                }
                let create_result = self.entity_use_cases().create_vector(CreateVectorInput {
                    collection: save.collection.clone(),
                    dense: embedding,
                    content: if save.include_content {
                        Some(inputs[index].text.clone())
                    } else {
                        None
                    },
                    metadata,
                    link_row: None,
                    link_node: None,
                });
                let output = match create_result {
                    Ok(output) => output,
                    Err(err) => {
                        return json_error(
                            400,
                            format!("failed to persist embedding at index {index}: {err}"),
                        );
                    }
                };
                let mut item = Map::new();
                item.insert("index".to_string(), JsonValue::Number(index as f64));
                item.insert("id".to_string(), JsonValue::Number(output.id.raw() as f64));
                if let Some(source_row) = &inputs[index].source_row {
                    item.insert(
                        "source_row_id".to_string(),
                        JsonValue::Number(source_row.row_id as f64),
                    );
                    item.insert(
                        "source_collection".to_string(),
                        JsonValue::String(source_row.table.clone()),
                    );
                }
                saved.push(JsonValue::Object(item));
            }
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "provider".to_string(),
            JsonValue::String(response.provider.to_string()),
        );
        object.insert("model".to_string(), JsonValue::String(response.model));
        object.insert(
            "count".to_string(),
            JsonValue::Number(response.embeddings.len() as f64),
        );
        object.insert(
            "embeddings".to_string(),
            JsonValue::Array(
                response
                    .embeddings
                    .iter()
                    .map(|embedding| {
                        JsonValue::Array(
                            embedding
                                .iter()
                                .map(|value| JsonValue::Number(*value as f64))
                                .collect(),
                        )
                    })
                    .collect(),
            ),
        );
        object.insert("saved".to_string(), JsonValue::Array(saved));
        if let Some(prompt_tokens) = response.prompt_tokens {
            object.insert(
                "prompt_tokens".to_string(),
                JsonValue::Number(prompt_tokens as f64),
            );
        }
        if let Some(total_tokens) = response.total_tokens {
            object.insert(
                "total_tokens".to_string(),
                JsonValue::Number(total_tokens as f64),
            );
        }

        json_response(200, JsonValue::Object(object))
    }

    /// Local-provider embedding path (#680).
    ///
    /// Routes through the runtime's swappable backend after resolving a
    /// registered+installed local model. Mirrors the OpenAI-compatible
    /// response shape (`provider`, `model`, `count`, `embeddings`,
    /// `saved`) so existing clients stay source-compatible, and adds
    /// `model_source`, `model_revision`, `model_engine`, `dimensions`
    /// fields that the local catalog publishes.
    fn handle_ai_embeddings_local(&self, payload: &JsonValue) -> HttpResponse {
        if let Err(err) = crate::runtime::ai::local_embedding::ensure_local_embedding_available() {
            let (status, msg) = crate::server::transport::map_runtime_error(&err);
            return json_error(status, msg);
        }

        let model_name = match json_string_field(payload, "model") {
            Some(name) if !name.trim().is_empty() => name.trim().to_string(),
            _ => {
                return json_error(
                    400,
                    "field 'model' is required for the local provider and must be the \
                     registered local model name (see POST /ai/models)",
                );
            }
        };

        let max_inputs = match parse_optional_positive_usize(payload, "max_inputs")
            .map(|v| v.unwrap_or(DEFAULT_MAX_INPUTS))
        {
            Ok(value) => value,
            Err(err) => return json_error(400, err),
        };

        let inputs = match self.collect_ai_embedding_inputs(payload, max_inputs) {
            Ok(inputs) => inputs,
            Err(err) => return json_error(400, err),
        };

        let save_options = match parse_ai_embedding_save_options(payload) {
            Ok(options) => options,
            Err(err) => return json_error(400, err),
        };

        let texts: Vec<String> = inputs.iter().map(|i| i.text.clone()).collect();
        let response = match crate::runtime::ai::local_embedding::embed_local(
            &self.runtime,
            &model_name,
            texts,
        ) {
            Ok(response) => response,
            Err(err) => {
                let (status, msg) = crate::server::transport::map_runtime_error(&err);
                return json_error(status, msg);
            }
        };

        if response.embeddings.len() != inputs.len() {
            return json_error(
                500,
                "local backend returned a different number of embeddings than requested inputs",
            );
        }

        let mut saved = Vec::new();
        if let Some(save) = save_options {
            for (index, embedding) in response.embeddings.iter().cloned().enumerate() {
                let mut metadata = save.metadata.clone();
                metadata.push((
                    "_ai_provider".to_string(),
                    MetadataValue::String(response.provider.to_string()),
                ));
                metadata.push((
                    "_ai_model".to_string(),
                    MetadataValue::String(response.name.clone()),
                ));
                metadata.push((
                    "_ai_model_source".to_string(),
                    MetadataValue::String(response.source.clone()),
                ));
                metadata.push((
                    "_ai_model_revision".to_string(),
                    MetadataValue::String(response.revision.clone()),
                ));
                if let Some(source_row) = &inputs[index].source_row {
                    metadata.push((
                        "_source_collection".to_string(),
                        MetadataValue::String(source_row.table.clone()),
                    ));
                    metadata.push((
                        "_source_row_id".to_string(),
                        MetadataValue::Int(source_row.row_id as i64),
                    ));
                }
                let create_result = self.entity_use_cases().create_vector(CreateVectorInput {
                    collection: save.collection.clone(),
                    dense: embedding,
                    content: if save.include_content {
                        Some(inputs[index].text.clone())
                    } else {
                        None
                    },
                    metadata,
                    link_row: None,
                    link_node: None,
                });
                let output = match create_result {
                    Ok(output) => output,
                    Err(err) => {
                        return json_error(
                            400,
                            format!("failed to persist embedding at index {index}: {err}"),
                        );
                    }
                };
                let mut item = Map::new();
                item.insert("index".to_string(), JsonValue::Number(index as f64));
                item.insert("id".to_string(), JsonValue::Number(output.id.raw() as f64));
                if let Some(source_row) = &inputs[index].source_row {
                    item.insert(
                        "source_row_id".to_string(),
                        JsonValue::Number(source_row.row_id as f64),
                    );
                    item.insert(
                        "source_collection".to_string(),
                        JsonValue::String(source_row.table.clone()),
                    );
                }
                saved.push(JsonValue::Object(item));
            }
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "provider".to_string(),
            JsonValue::String(response.provider.to_string()),
        );
        object.insert("model".to_string(), JsonValue::String(response.name));
        object.insert(
            "model_source".to_string(),
            JsonValue::String(response.source),
        );
        object.insert(
            "model_revision".to_string(),
            JsonValue::String(response.revision),
        );
        object.insert(
            "model_engine".to_string(),
            JsonValue::String(response.engine),
        );
        object.insert(
            "dimensions".to_string(),
            JsonValue::Number(response.dimensions as f64),
        );
        object.insert(
            "count".to_string(),
            JsonValue::Number(response.embeddings.len() as f64),
        );
        object.insert(
            "embeddings".to_string(),
            JsonValue::Array(
                response
                    .embeddings
                    .iter()
                    .map(|embedding| {
                        JsonValue::Array(
                            embedding
                                .iter()
                                .map(|value| JsonValue::Number(*value as f64))
                                .collect(),
                        )
                    })
                    .collect(),
            ),
        );
        object.insert("saved".to_string(), JsonValue::Array(saved));
        json_response(200, JsonValue::Object(object))
    }

    pub(crate) fn handle_ai_prompt(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let provider = match parse_ai_provider(&payload) {
            Ok(provider) => provider,
            Err(err) => return json_error(400, err),
        };
        if matches!(provider, AiProvider::Local) {
            let err = crate::ai::local_prompt_unavailable_error();
            let (status, msg) = crate::server::transport::map_runtime_error(&err);
            return json_error(status, msg);
        }

        let model = json_string_field(&payload, "model").unwrap_or_else(|| {
            std::env::var(provider.prompt_model_env_name())
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| provider.default_prompt_model().to_string())
        });
        if model.trim().is_empty() {
            return json_error(400, "field 'model' cannot be empty");
        }

        let temperature = match parse_optional_temperature(&payload) {
            Ok(value) => value,
            Err(err) => return json_error(400, err),
        };
        let max_output_tokens = match parse_optional_positive_usize(&payload, "max_output_tokens") {
            Ok(value) => value,
            Err(err) => return json_error(400, err),
        };
        let max_prompts = match parse_optional_positive_usize(&payload, "max_prompts") {
            Ok(Some(value)) => value,
            Ok(None) => DEFAULT_MAX_PROMPTS,
            Err(err) => return json_error(400, err),
        };

        let prompts = match self.collect_ai_prompt_inputs(&payload, max_prompts) {
            Ok(prompts) => prompts,
            Err(err) => return json_error(400, err),
        };

        let save_options = match parse_ai_prompt_save_options(&payload) {
            Ok(value) => value,
            Err(err) => return json_error(400, err),
        };

        let credential = json_string_field(&payload, "credential");
        let api_key = match self.resolve_provider_api_key(&provider, credential.as_deref()) {
            Ok(key) => key,
            Err(err) => return json_error(400, err),
        };
        let api_base = std::env::var(provider.api_base_env_name())
            .unwrap_or_else(|_| provider.default_api_base().to_string());
        let anthropic_version = std::env::var("REDDB_ANTHROPIC_VERSION")
            .unwrap_or_else(|_| crate::ai::DEFAULT_ANTHROPIC_VERSION.to_string());

        let transport = crate::runtime::ai::transport::AiTransport::from_runtime(&self.runtime);

        let mut outputs = Vec::with_capacity(prompts.len());
        let mut saved = Vec::new();
        let mut prompt_tokens_total = 0u64;
        let mut completion_tokens_total = 0u64;
        let mut total_tokens_total = 0u64;
        let mut has_prompt_tokens = false;
        let mut has_completion_tokens = false;
        let mut has_total_tokens = false;

        for (index, prompt) in prompts.iter().enumerate() {
            let response = match provider {
                AiProvider::Anthropic => {
                    let transport = transport.clone();
                    let request = crate::ai::AnthropicPromptRequest {
                        api_key: api_key.clone(),
                        model: model.clone(),
                        prompt: prompt.clone(),
                        temperature,
                        max_output_tokens,
                        api_base: api_base.clone(),
                        anthropic_version: anthropic_version.clone(),
                    };
                    crate::runtime::ai::block_on_ai(async move {
                        crate::ai::anthropic_prompt_async(&transport, request).await
                    })
                    .and_then(|result| result)
                }
                _ => {
                    let transport = transport.clone();
                    let request = crate::ai::OpenAiPromptRequest {
                        api_key: api_key.clone(),
                        model: model.clone(),
                        prompt: prompt.clone(),
                        temperature,
                        seed: None,
                        max_output_tokens,
                        api_base: api_base.clone(),
                        stream: false,
                    };
                    crate::runtime::ai::block_on_ai(async move {
                        crate::ai::openai_prompt_async(&transport, request).await
                    })
                    .and_then(|result| result)
                }
            };
            let response = match response {
                Ok(value) => value,
                Err(err) => {
                    return json_error(
                        400,
                        format!("prompt execution failed at index {index}: {err}"),
                    )
                }
            };

            if let Some(tokens) = response.prompt_tokens {
                has_prompt_tokens = true;
                prompt_tokens_total = prompt_tokens_total.saturating_add(tokens);
            }
            if let Some(tokens) = response.completion_tokens {
                has_completion_tokens = true;
                completion_tokens_total = completion_tokens_total.saturating_add(tokens);
            }
            if let Some(tokens) = response.total_tokens {
                has_total_tokens = true;
                total_tokens_total = total_tokens_total.saturating_add(tokens);
            }

            let mut output_item = Map::new();
            output_item.insert("index".to_string(), JsonValue::Number(index as f64));
            output_item.insert(
                "text".to_string(),
                JsonValue::String(response.output_text.clone()),
            );
            output_item.insert(
                "model".to_string(),
                JsonValue::String(response.model.clone()),
            );
            if let Some(ref stop_reason) = response.stop_reason {
                output_item.insert(
                    "stop_reason".to_string(),
                    JsonValue::String(stop_reason.clone()),
                );
            }
            outputs.push(JsonValue::Object(output_item));

            if let Some(ref save) = save_options {
                let mut metadata = save.metadata.clone();
                metadata.push((
                    "_ai_provider".to_string(),
                    MetadataValue::String(provider.token().to_string()),
                ));
                metadata.push((
                    "_ai_model".to_string(),
                    MetadataValue::String(response.model.clone()),
                ));
                if let Some(ref credential) = credential {
                    metadata.push((
                        "_ai_credential".to_string(),
                        MetadataValue::String(credential.clone()),
                    ));
                }

                let create_result = self.entity_use_cases().create_row(CreateRowInput {
                    collection: save.collection.clone(),
                    fields: vec![
                        (save.prompt_field.clone(), Value::text(prompt.clone())),
                        (
                            save.response_field.clone(),
                            Value::text(response.output_text.clone()),
                        ),
                        (
                            "provider".to_string(),
                            Value::text(provider.token().to_string()),
                        ),
                        ("model".to_string(), Value::text(response.model.clone())),
                        ("index".to_string(), Value::Integer(index as i64)),
                    ],
                    metadata,
                    node_links: Vec::new(),
                    vector_links: Vec::new(),
                });
                let output = match create_result {
                    Ok(output) => output,
                    Err(err) => {
                        return json_error(
                            400,
                            format!("failed to persist prompt output at index {index}: {err}"),
                        )
                    }
                };

                let mut saved_item = Map::new();
                saved_item.insert("index".to_string(), JsonValue::Number(index as f64));
                saved_item.insert("id".to_string(), JsonValue::Number(output.id.raw() as f64));
                saved.push(JsonValue::Object(saved_item));
            }
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "provider".to_string(),
            JsonValue::String(provider.token().to_string()),
        );
        object.insert("model".to_string(), JsonValue::String(model));
        object.insert("count".to_string(), JsonValue::Number(outputs.len() as f64));
        object.insert("outputs".to_string(), JsonValue::Array(outputs));
        object.insert("saved".to_string(), JsonValue::Array(saved));
        if has_prompt_tokens {
            object.insert(
                "prompt_tokens".to_string(),
                JsonValue::Number(prompt_tokens_total as f64),
            );
        }
        if has_completion_tokens {
            object.insert(
                "completion_tokens".to_string(),
                JsonValue::Number(completion_tokens_total as f64),
            );
        }
        if has_total_tokens {
            object.insert(
                "total_tokens".to_string(),
                JsonValue::Number(total_tokens_total as f64),
            );
        }

        json_response(200, JsonValue::Object(object))
    }

    pub(crate) fn handle_ai_credentials(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let provider = match parse_ai_provider(&payload) {
            Ok(provider) => provider,
            Err(err) => return json_error(400, err),
        };
        let alias = json_string_field(&payload, "alias").unwrap_or_else(|| "default".to_string());
        let alias = alias.trim();
        if alias.is_empty() {
            return json_error(400, "field 'alias' cannot be empty");
        }

        let api_key = json_string_field(&payload, "api_key")
            .or_else(|| json_string_field(&payload, "key"))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let api_base = json_string_field(&payload, "api_base")
            .or_else(|| json_string_field(&payload, "base_url"))
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        if api_key.is_none() && api_base.is_none() {
            return json_error(400, "at least 'api_key' or 'api_base' must be provided");
        }

        let metadata = match parse_metadata_entries(payload.get("metadata")) {
            Ok(value) => value,
            Err(err) => return json_error(400, err),
        };

        let mut saved_keys = Vec::new();

        // Save API key
        if let Some(api_key) = &api_key {
            let secret_path = crate::ai::ai_api_secret_path(&provider, alias);
            if let Err(err) = self
                .runtime()
                .vault_kv_try_set(secret_path.clone(), api_key.clone())
            {
                return json_error(400, format!("failed to store credential in vault: {err}"));
            }

            let key_name = crate::ai::ai_api_secret_ref_config_key(&provider, alias);
            let _ = self
                .entity_use_cases()
                .delete_kv(RED_CONFIG_COLLECTION, &key_name);
            match self.entity_use_cases().create_kv(CreateKvInput {
                collection: RED_CONFIG_COLLECTION.to_string(),
                key: key_name.clone(),
                value: Value::text(secret_path.clone()),
                metadata: metadata.clone(),
            }) {
                Ok(output) => saved_keys.push((key_name, output.id.raw())),
                Err(err) => return json_error(400, format!("failed to store credential: {err}")),
            }
        }

        // Save API base URL (ADR-0068 §5: per-provider, no credential alias).
        if let Some(api_base) = &api_base {
            let base_key = crate::ai::provider_base_url_key(&provider);
            let _ = self
                .entity_use_cases()
                .delete_kv(RED_CONFIG_COLLECTION, &base_key);
            match self.entity_use_cases().create_kv(CreateKvInput {
                collection: RED_CONFIG_COLLECTION.to_string(),
                key: base_key.clone(),
                value: Value::text(api_base.clone()),
                metadata: Vec::new(),
            }) {
                Ok(output) => saved_keys.push((base_key, output.id.raw())),
                Err(err) => return json_error(400, format!("failed to store base URL: {err}")),
            }
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "provider".to_string(),
            JsonValue::String(provider.token().to_string()),
        );
        object.insert("alias".to_string(), JsonValue::String(alias.to_string()));
        if api_key.is_some() {
            object.insert(
                "secret_ref".to_string(),
                JsonValue::String(crate::ai::ai_api_secret_path(&provider, alias)),
            );
        }
        if let Some(ref base) = api_base {
            object.insert("api_base".to_string(), JsonValue::String(base.clone()));
        }
        object.insert(
            "saved".to_string(),
            JsonValue::Array(
                saved_keys
                    .iter()
                    .map(|(k, id)| {
                        let mut o = Map::new();
                        o.insert("key".to_string(), JsonValue::String(k.clone()));
                        o.insert("id".to_string(), JsonValue::Number(*id as f64));
                        JsonValue::Object(o)
                    })
                    .collect(),
            ),
        );

        // If "default": true, save this provider+model as the global default
        let is_default = payload
            .get("default")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        if is_default {
            // ADR-0068 §5 clean break: point the inference task pointer at
            // this provider and pin its inference model in the provider
            // models block. When the provider can embed, aim the embeddings
            // task pointer at it too so it becomes the default for both
            // modalities; otherwise leave the embeddings pointer untouched.
            let set_pointer = |key: String, value: String| {
                let _ = self
                    .entity_use_cases()
                    .delete_kv(RED_CONFIG_COLLECTION, &key);
                let _ = self.entity_use_cases().create_kv(CreateKvInput {
                    collection: RED_CONFIG_COLLECTION.to_string(),
                    key,
                    value: Value::text(value),
                    metadata: Vec::new(),
                });
            };

            set_pointer(
                "red.config.ai.inference.provider".to_string(),
                provider.token().to_string(),
            );

            let model = json_string_field(&payload, "model")
                .unwrap_or_else(|| provider.default_prompt_model().to_string());
            set_pointer(
                crate::ai::provider_models_key(&provider, "inference"),
                model.clone(),
            );

            if provider.supports_embeddings() {
                set_pointer(
                    "red.config.ai.embeddings.provider".to_string(),
                    provider.token().to_string(),
                );
            }

            object.insert("is_default".to_string(), JsonValue::Bool(true));
            object.insert("default_model".to_string(), JsonValue::String(model));
        }

        json_response(200, JsonValue::Object(object))
    }

    fn collect_ai_embedding_inputs(
        &self,
        payload: &JsonValue,
        max_inputs: usize,
    ) -> Result<Vec<AiEmbeddingInputItem>, String> {
        if let Some(source_query) = json_string_field(payload, "source_query") {
            if source_query.trim().is_empty() {
                return Err("field 'source_query' cannot be empty".to_string());
            }
            let source_mode = parse_source_mode(payload)?;
            return self.collect_embedding_inputs_from_query_source(
                &source_query,
                source_mode,
                payload,
                max_inputs,
            );
        }

        if let Some(values) = payload.get("inputs").and_then(JsonValue::as_array) {
            let mut out = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let Some(text) = value.as_str() else {
                    return Err(format!("field 'inputs[{index}]' must be a string"));
                };
                if text.trim().is_empty() {
                    return Err(format!("field 'inputs[{index}]' cannot be empty"));
                }
                out.push(AiEmbeddingInputItem {
                    text: text.to_string(),
                    source_row: None,
                });
            }
            if out.is_empty() {
                return Err("field 'inputs' cannot be empty".to_string());
            }
            if out.len() > max_inputs {
                return Err(format!(
                    "too many inputs: {} (max_inputs = {max_inputs})",
                    out.len()
                ));
            }
            return Ok(out);
        }

        if let Some(input) = json_string_field(payload, "input") {
            if input.trim().is_empty() {
                return Err("field 'input' cannot be empty".to_string());
            }
            return Ok(vec![AiEmbeddingInputItem {
                text: input,
                source_row: None,
            }]);
        }

        Err("provide either 'input', 'inputs', or 'source_query'".to_string())
    }

    fn collect_embedding_inputs_from_query_source(
        &self,
        query: &str,
        source_mode: AiQuerySourceMode,
        payload: &JsonValue,
        max_inputs: usize,
    ) -> Result<Vec<AiEmbeddingInputItem>, String> {
        let result = self
            .query_use_cases()
            .execute(ExecuteQueryInput {
                query: query.to_string(),
            })
            .map_err(|err| format!("source_query failed: {err}"))?;

        match source_mode {
            AiQuerySourceMode::Row => {
                let source_field = json_string_field(payload, "source_field").ok_or_else(|| {
                    "field 'source_field' is required when source_mode='row'".to_string()
                })?;
                if source_field.trim().is_empty() {
                    return Err("field 'source_field' cannot be empty".to_string());
                }
                let source_collection = json_string_field(payload, "source_collection")
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty());

                let mut out = Vec::new();
                for (index, record) in result.result.records.iter().enumerate() {
                    let Some(value) = record.get(&source_field) else {
                        return Err(format!(
                            "source_field '{source_field}' not found in row {index}"
                        ));
                    };
                    if matches!(value, Value::Null) {
                        continue;
                    }
                    let text = value.display_string();
                    if text.trim().is_empty() {
                        continue;
                    }
                    out.push(AiEmbeddingInputItem {
                        text,
                        source_row: embedding_source_row_ref(record, source_collection.as_deref()),
                    });
                    if out.len() > max_inputs {
                        return Err(format!(
                            "source_query produced more than max_inputs ({max_inputs}); add LIMIT or increase max_inputs"
                        ));
                    }
                }
                Ok(out)
            }
            AiQuerySourceMode::Result => {
                let result_json = crate::presentation::query_result_json::runtime_query_json(
                    &result, &None, &None,
                );
                Ok(vec![AiEmbeddingInputItem {
                    text: result_json.to_string_compact(),
                    source_row: None,
                }])
            }
        }
    }

    fn collect_ai_prompt_inputs(
        &self,
        payload: &JsonValue,
        max_prompts: usize,
    ) -> Result<Vec<String>, String> {
        if let Some(source_query) = json_string_field(payload, "source_query") {
            if source_query.trim().is_empty() {
                return Err("field 'source_query' cannot be empty".to_string());
            }
            let source_mode = parse_source_mode(payload)?;
            return self.collect_prompts_from_query_source(
                &source_query,
                source_mode,
                payload,
                max_prompts,
            );
        }

        if let Some(values) = payload.get("prompts").and_then(JsonValue::as_array) {
            let mut out = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let Some(text) = value.as_str() else {
                    return Err(format!("field 'prompts[{index}]' must be a string"));
                };
                if text.trim().is_empty() {
                    return Err(format!("field 'prompts[{index}]' cannot be empty"));
                }
                out.push(text.to_string());
            }
            if out.is_empty() {
                return Err("field 'prompts' cannot be empty".to_string());
            }
            if out.len() > max_prompts {
                return Err(format!(
                    "too many prompts: {} (max_prompts = {max_prompts})",
                    out.len()
                ));
            }
            return Ok(out);
        }

        if let Some(prompt) = json_string_field(payload, "prompt") {
            if prompt.trim().is_empty() {
                return Err("field 'prompt' cannot be empty".to_string());
            }
            return Ok(vec![prompt]);
        }

        Err("provide either 'prompt', 'prompts', or 'source_query'".to_string())
    }

    fn collect_prompts_from_query_source(
        &self,
        query: &str,
        source_mode: AiQuerySourceMode,
        payload: &JsonValue,
        max_prompts: usize,
    ) -> Result<Vec<String>, String> {
        let result = self
            .query_use_cases()
            .execute(ExecuteQueryInput {
                query: query.to_string(),
            })
            .map_err(|err| format!("source_query failed: {err}"))?;

        match source_mode {
            AiQuerySourceMode::Row => {
                let prompt_template = json_string_field(payload, "prompt_template");
                let source_field = json_string_field(payload, "source_field")
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty());

                if prompt_template.is_none() && source_field.is_none() {
                    return Err(
                        "for source_mode='row', provide either 'prompt_template' or 'source_field'"
                            .to_string(),
                    );
                }

                let mut out = Vec::new();
                for (index, record) in result.result.records.iter().enumerate() {
                    let prompt = if let Some(template) = prompt_template.as_deref() {
                        render_prompt_template(template, |token| {
                            if token.eq_ignore_ascii_case("row_index") {
                                return Some(index.to_string());
                            }
                            record.get(token).map(|value| value.display_string())
                        })
                    } else if let Some(ref source_field) = source_field {
                        let Some(value) = record.get(source_field) else {
                            return Err(format!(
                                "source_field '{source_field}' not found in row {index}"
                            ));
                        };
                        if matches!(value, Value::Null) {
                            continue;
                        }
                        value.display_string()
                    } else {
                        continue;
                    };

                    if prompt.trim().is_empty() {
                        continue;
                    }

                    out.push(prompt);
                    if out.len() > max_prompts {
                        return Err(format!(
                            "source_query produced more than max_prompts ({max_prompts}); add LIMIT or increase max_prompts"
                        ));
                    }
                }

                if out.is_empty() {
                    return Err("source_query produced no promptable rows".to_string());
                }

                Ok(out)
            }
            AiQuerySourceMode::Result => {
                let result_json = crate::presentation::query_result_json::runtime_query_json(
                    &result, &None, &None,
                )
                .to_string_compact();

                let prompt = if let Some(template) = json_string_field(payload, "prompt_template") {
                    render_prompt_template(&template, |token| match token {
                        "result" | "result_json" | "query_result" => Some(result_json.clone()),
                        _ => None,
                    })
                } else {
                    result_json
                };

                if prompt.trim().is_empty() {
                    return Err("generated prompt is empty".to_string());
                }

                Ok(vec![prompt])
            }
        }
    }

    fn resolve_provider_api_key(
        &self,
        provider: &AiProvider,
        credential_alias: Option<&str>,
    ) -> Result<String, String> {
        crate::ai::resolve_api_key(provider, credential_alias, |kv_key| {
            if kv_key.starts_with("red.secret.") {
                return Ok(self.runtime().vault_kv_get(kv_key));
            }
            match self
                .entity_use_cases()
                .get_kv(RED_CONFIG_COLLECTION, kv_key)
            {
                Ok(Some((Value::Text(secret), _))) => Ok(Some(secret.to_string())),
                Ok(_) => Ok(None),
                Err(err) => Err(crate::RedDBError::Query(format!(
                    "failed to read AI credential store: {err}"
                ))),
            }
        })
        .map_err(|e| e.to_string())
    }

    /// POST /ai/models — register a local AI embedding model.
    pub(crate) fn handle_ai_model_register(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let spec = match LocalAiModelSpec::from_payload(&payload) {
            Ok(spec) => spec,
            Err(err) => return json_error(400, err),
        };
        let key = ai_model_config_key(&spec.name);
        match self.entity_use_cases().get_kv(RED_CONFIG_COLLECTION, &key) {
            Ok(Some(_)) => {
                return json_error(
                    409,
                    format!("local AI model '{}' is already registered", spec.name),
                )
            }
            Ok(None) => {}
            Err(err) => return json_error(500, format!("failed to read model registry: {err}")),
        }

        let now = now_unix_ms();
        let stored = spec.to_stored_json(AI_MODEL_STATUS_REGISTERED, now, now);
        let stored_text = match crate::json::to_string(&stored) {
            Ok(s) => s,
            Err(err) => return json_error(500, format!("failed to encode model entry: {err}")),
        };
        if let Err(err) = self.entity_use_cases().create_kv(CreateKvInput {
            collection: RED_CONFIG_COLLECTION.to_string(),
            key,
            value: Value::text(stored_text),
            metadata: Vec::new(),
        }) {
            return json_error(400, format!("failed to register model: {err}"));
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("model".to_string(), stored);
        json_response(201, JsonValue::Object(object))
    }

    /// PUT /ai/models/{name} — update an already-registered local AI model.
    pub(crate) fn handle_ai_model_update(&self, name: &str, body: Vec<u8>) -> HttpResponse {
        if name.trim().is_empty() {
            return json_error(400, "model name path segment cannot be empty");
        }
        let payload = match parse_json_body(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };
        let mut spec = match LocalAiModelSpec::from_payload(&payload) {
            Ok(spec) => spec,
            Err(err) => return json_error(400, err),
        };
        // The path name is authoritative; reject a body whose `name` field
        // disagrees rather than silently picking one.
        if let Some(body_name) = json_string_field(&payload, "name") {
            if body_name.trim() != name {
                return json_error(
                    400,
                    format!(
                        "model name in path '{name}' does not match body field '{}'",
                        body_name.trim()
                    ),
                );
            }
        }
        spec.name = name.to_string();
        let key = ai_model_config_key(name);
        let existing: Option<JsonValue> = match self
            .entity_use_cases()
            .get_kv(RED_CONFIG_COLLECTION, &key)
        {
            Ok(Some((Value::Text(text), _))) => {
                crate::json::parse_json(&text).ok().map(JsonValue::from)
            }
            Ok(_) => None,
            Err(err) => return json_error(500, format!("failed to read model registry: {err}")),
        };
        let created_at = existing
            .as_ref()
            .and_then(|v| v.get("created_at_unix_ms"))
            .and_then(JsonValue::as_u64)
            .unwrap_or_else(now_unix_ms);
        if existing.is_none() {
            return json_error(404, format!("local AI model '{name}' is not registered"));
        }
        let now = now_unix_ms();
        let stored = spec.to_stored_json(AI_MODEL_STATUS_REGISTERED, created_at, now);
        let stored_text = match crate::json::to_string(&stored) {
            Ok(s) => s,
            Err(err) => return json_error(500, format!("failed to encode model entry: {err}")),
        };
        let _ = self
            .entity_use_cases()
            .delete_kv(RED_CONFIG_COLLECTION, &key);
        if let Err(err) = self.entity_use_cases().create_kv(CreateKvInput {
            collection: RED_CONFIG_COLLECTION.to_string(),
            key,
            value: Value::text(stored_text),
            metadata: Vec::new(),
        }) {
            return json_error(400, format!("failed to update model: {err}"));
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("model".to_string(), stored);
        json_response(200, JsonValue::Object(object))
    }

    /// GET /ai/models — list all registered local AI models.
    pub(crate) fn handle_ai_model_list(&self) -> HttpResponse {
        let entries = self.collect_ai_model_entries();
        let mut models: Vec<JsonValue> = entries.into_iter().map(|(_, v)| v).collect();
        models.sort_by(|a, b| {
            let lhs = a.get("name").and_then(JsonValue::as_str).unwrap_or("");
            let rhs = b.get("name").and_then(JsonValue::as_str).unwrap_or("");
            lhs.cmp(rhs)
        });
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("count".to_string(), JsonValue::Number(models.len() as f64));
        object.insert("models".to_string(), JsonValue::Array(models));
        json_response(200, JsonValue::Object(object))
    }

    /// GET /ai/models/{name} — inspect one registered local AI model.
    pub(crate) fn handle_ai_model_get(&self, name: &str) -> HttpResponse {
        if name.trim().is_empty() {
            return json_error(400, "model name path segment cannot be empty");
        }
        let key = ai_model_config_key(name);
        match self.entity_use_cases().get_kv(RED_CONFIG_COLLECTION, &key) {
            Ok(Some((Value::Text(text), _))) => match crate::json::parse_json(&text) {
                Ok(model) => {
                    let model: JsonValue = model.into();
                    let mut object = Map::new();
                    object.insert("ok".to_string(), JsonValue::Bool(true));
                    object.insert("model".to_string(), model);
                    json_response(200, JsonValue::Object(object))
                }
                Err(err) => {
                    json_error(500, format!("model entry for '{name}' is corrupted: {err}"))
                }
            },
            Ok(_) => json_error(404, format!("local AI model '{name}' is not registered")),
            Err(err) => json_error(500, format!("failed to read model registry: {err}")),
        }
    }

    pub(crate) fn collect_ai_model_entries(&self) -> Vec<(String, JsonValue)> {
        let store = self.runtime.db().store();
        let Some(manager) = store.get_collection(RED_CONFIG_COLLECTION) else {
            return Vec::new();
        };
        let entities = manager.query_all(|_| true);
        let mut out = Vec::new();
        for entity in entities {
            let EntityData::Row(row) = &entity.data else {
                continue;
            };
            let Some(named) = row.named.as_ref() else {
                continue;
            };
            let key = match named.get("key") {
                Some(Value::Text(s)) => s.to_string(),
                _ => continue,
            };
            let Some(model_name) = key.strip_prefix(AI_MODEL_KEY_PREFIX) else {
                continue;
            };
            if model_name.is_empty() || model_name.contains('.') {
                continue;
            }
            let value = match named.get("value") {
                Some(Value::Text(s)) => s.to_string(),
                _ => continue,
            };
            if let Ok(parsed) = crate::json::parse_json(&value) {
                out.push((model_name.to_string(), JsonValue::from(parsed)));
            }
        }
        out
    }
}

const AI_MODEL_KEY_PREFIX: &str = "red.config.ai.models.";
const AI_MODEL_TASK_EMBEDDING: &str = "embedding";
const AI_MODEL_ENGINE_CANDLE: &str = "candle";
const AI_MODEL_PROVIDER_LOCAL: &str = "local";
const AI_MODEL_STATUS_REGISTERED: &str = "registered";
/// Canonical pull policy names. The operator-facing contract uses
/// `never` / `if_missing` / `always`; the legacy alternative names
/// (`manual` / `on_demand` / `eager`) are accepted by `from_payload`
/// and normalised through [`normalize_pull_policy`] before storage.
pub(crate) const AI_MODEL_PULL_POLICY_NEVER: &str = "never";
pub(crate) const AI_MODEL_PULL_POLICY_IF_MISSING: &str = "if_missing";
pub(crate) const AI_MODEL_PULL_POLICY_ALWAYS: &str = "always";
const AI_MODEL_PULL_POLICIES: &[&str] = &[
    AI_MODEL_PULL_POLICY_NEVER,
    AI_MODEL_PULL_POLICY_IF_MISSING,
    AI_MODEL_PULL_POLICY_ALWAYS,
];
/// Fields that must never be accepted at the model-registration boundary
/// because they would lead to plaintext secrets being persisted into the
/// model registry. Callers must use `credential_alias` plus the vault
/// path conventions instead.
const AI_MODEL_REJECTED_PLAINTEXT_FIELDS: &[&str] = &[
    "api_key",
    "apikey",
    "api_token",
    "token",
    "auth_token",
    "bearer_token",
    "password",
    "secret",
    "hf_token",
    "huggingface_token",
    "huggingface_api_key",
];

/// Map a raw `pull_policy` string (canonical name or legacy alias) to
/// its canonical name. Returns `None` if the input is not a recognised
/// policy. Matching is case-insensitive.
pub(crate) fn normalize_pull_policy(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "never" | "manual" => Some(AI_MODEL_PULL_POLICY_NEVER),
        "if_missing" | "ifmissing" | "on_demand" | "ondemand" => {
            Some(AI_MODEL_PULL_POLICY_IF_MISSING)
        }
        "always" | "eager" => Some(AI_MODEL_PULL_POLICY_ALWAYS),
        _ => None,
    }
}
const AI_MODEL_TRUST_DISABLED: &str = "disabled";
const AI_MODEL_TRUST_ALLOW_REMOTE_CODE: &str = "allow_remote_code";
const AI_MODEL_TRUST_POLICIES: &[&str] = &["disabled", "allow_remote_code"];

fn ai_model_config_key(name: &str) -> String {
    format!("{AI_MODEL_KEY_PREFIX}{name}")
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone)]
struct LocalAiModelSpec {
    name: String,
    provider: String,
    source: String,
    task: String,
    revision: String,
    engine: String,
    dimensions: u32,
    pull_policy: String,
    trust_policy: String,
    /// Vault alias used to resolve provider credentials for the pull
    /// (currently HuggingFace). Resolves via
    /// `red.secret.ai.providers.{provider}.tokens.{alias}` per the shared
    /// `resolve_api_key` contract. Plaintext secrets are never accepted at
    /// this boundary.
    credential_alias: Option<String>,
}

impl LocalAiModelSpec {
    fn from_payload(payload: &JsonValue) -> Result<Self, String> {
        // Reject any plaintext-credential field at this boundary. The
        // registry is read by query-time code and must not carry secrets.
        for field in AI_MODEL_REJECTED_PLAINTEXT_FIELDS {
            if payload.get(field).is_some() {
                return Err(format!(
                    "field '{field}' is rejected: registered models must not store plaintext \
                     provider credentials. Use 'credential_alias' and store the secret in the \
                     vault at 'red.secret.ai.providers.{{provider}}.tokens.{{alias}}' instead."
                ));
            }
        }

        let name = json_string_field(payload, "name")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "field 'name' is required and cannot be empty".to_string())?;
        validate_model_name(&name)?;

        let provider = json_string_field(payload, "provider")
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| AI_MODEL_PROVIDER_LOCAL.to_string());
        if provider != AI_MODEL_PROVIDER_LOCAL {
            return Err(format!(
                "field 'provider' must be '{AI_MODEL_PROVIDER_LOCAL}' for the local model catalog (got '{provider}'); other providers are not registered through this endpoint"
            ));
        }

        let source = json_string_field(payload, "source")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                "field 'source' is required and cannot be empty (e.g. HuggingFace repo id 'sentence-transformers/all-MiniLM-L6-v2')".to_string()
            })?;
        if source.chars().any(|c| c.is_whitespace()) {
            return Err("field 'source' must not contain whitespace".to_string());
        }

        let task = json_string_field(payload, "task")
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                format!(
                    "field 'task' is required; only '{AI_MODEL_TASK_EMBEDDING}' is supported in this slice"
                )
            })?;
        if matches!(
            task.as_str(),
            "prompt" | "generation" | "chat" | "completion"
        ) {
            return Err(format!(
                "task '{task}' is out of scope: local prompt and generation are not supported; only '{AI_MODEL_TASK_EMBEDDING}' is supported"
            ));
        }
        if task != AI_MODEL_TASK_EMBEDDING {
            return Err(format!(
                "unsupported task '{task}'; only '{AI_MODEL_TASK_EMBEDDING}' is supported"
            ));
        }

        let revision = json_string_field(payload, "revision")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                "field 'revision' is required and must be a pinned git revision or tag (no floating refs)".to_string()
            })?;
        if revision.chars().any(|c| c.is_whitespace()) {
            return Err("field 'revision' must not contain whitespace".to_string());
        }

        let engine = json_string_field(payload, "engine")
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| AI_MODEL_ENGINE_CANDLE.to_string());
        if engine != AI_MODEL_ENGINE_CANDLE {
            return Err(format!(
                "field 'engine' '{engine}' is not supported; only '{AI_MODEL_ENGINE_CANDLE}' is supported in this slice"
            ));
        }

        let dimensions_value = payload
            .get("dimensions")
            .ok_or_else(|| "field 'dimensions' is required".to_string())?;
        let dimensions = match dimensions_value {
            JsonValue::Integer(n) if *n >= 1 => u32::try_from(*n)
                .map_err(|_| format!("field 'dimensions' must be between 1 and 65536 (got {n})"))?,
            JsonValue::Number(n)
                if n.is_finite() && *n >= 1.0 && n.fract().abs() < f64::EPSILON =>
            {
                let as_u = *n as u32;
                if (as_u as f64 - *n).abs() >= f64::EPSILON {
                    return Err(format!(
                        "field 'dimensions' must be a positive integer (got {n})"
                    ));
                }
                as_u
            }
            _ => {
                return Err(format!(
                    "field 'dimensions' must be a positive integer (got {dimensions_value:?})"
                ))
            }
        };
        if !(1..=65_536).contains(&dimensions) {
            return Err(format!(
                "field 'dimensions' must be between 1 and 65536 (got {dimensions})"
            ));
        }

        let raw_pull_policy = json_string_field(payload, "pull_policy")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| AI_MODEL_PULL_POLICY_IF_MISSING.to_string());
        let pull_policy = normalize_pull_policy(&raw_pull_policy)
            .ok_or_else(|| {
                format!(
                    "field 'pull_policy' '{raw_pull_policy}' is invalid; expected one of {AI_MODEL_PULL_POLICIES:?}"
                )
            })?
            .to_string();

        let trust_policy = json_string_field(payload, "trust_policy")
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| AI_MODEL_TRUST_DISABLED.to_string());
        if !AI_MODEL_TRUST_POLICIES.contains(&trust_policy.as_str()) {
            return Err(format!(
                "field 'trust_policy' '{trust_policy}' is invalid; expected one of {AI_MODEL_TRUST_POLICIES:?}"
            ));
        }
        if trust_policy == AI_MODEL_TRUST_ALLOW_REMOTE_CODE {
            // The contract requires an explicit acknowledgement; reject
            // unless the caller passes `acknowledge_remote_code_risk: true`.
            let acked = payload
                .get("acknowledge_remote_code_risk")
                .and_then(JsonValue::as_bool)
                .unwrap_or(false);
            if !acked {
                return Err(format!(
                    "trust_policy '{AI_MODEL_TRUST_ALLOW_REMOTE_CODE}' requires 'acknowledge_remote_code_risk': true; defaulting to '{AI_MODEL_TRUST_DISABLED}' otherwise"
                ));
            }
        }

        let credential_alias = json_string_field(payload, "credential_alias")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(alias) = credential_alias.as_deref() {
            if alias.len() > 128
                || !alias
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            {
                return Err(format!(
                    "field 'credential_alias' '{alias}' is invalid; only ASCII alphanumerics, \
                     '_' and '-' are allowed (max 128 chars)"
                ));
            }
        }

        Ok(Self {
            name,
            provider,
            source,
            task,
            revision,
            engine,
            dimensions,
            pull_policy,
            trust_policy,
            credential_alias,
        })
    }

    fn to_stored_json(&self, status: &str, created_at: u64, updated_at: u64) -> JsonValue {
        let mut obj = Map::new();
        obj.insert("name".to_string(), JsonValue::String(self.name.clone()));
        obj.insert(
            "provider".to_string(),
            JsonValue::String(self.provider.clone()),
        );
        obj.insert("source".to_string(), JsonValue::String(self.source.clone()));
        obj.insert("task".to_string(), JsonValue::String(self.task.clone()));
        obj.insert(
            "revision".to_string(),
            JsonValue::String(self.revision.clone()),
        );
        obj.insert("engine".to_string(), JsonValue::String(self.engine.clone()));
        obj.insert(
            "dimensions".to_string(),
            JsonValue::Number(self.dimensions as f64),
        );
        obj.insert(
            "pull_policy".to_string(),
            JsonValue::String(self.pull_policy.clone()),
        );
        obj.insert(
            "trust_policy".to_string(),
            JsonValue::String(self.trust_policy.clone()),
        );
        if let Some(alias) = &self.credential_alias {
            obj.insert(
                "credential_alias".to_string(),
                JsonValue::String(alias.clone()),
            );
        }
        obj.insert("status".to_string(), JsonValue::String(status.to_string()));
        obj.insert(
            "created_at_unix_ms".to_string(),
            JsonValue::Number(created_at as f64),
        );
        obj.insert(
            "updated_at_unix_ms".to_string(),
            JsonValue::Number(updated_at as f64),
        );
        JsonValue::Object(obj)
    }
}

fn validate_model_name(name: &str) -> Result<(), String> {
    if name.len() > 128 {
        return Err(format!(
            "field 'name' must be at most 128 characters (got {})",
            name.len()
        ));
    }
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !valid {
        return Err(format!(
            "field 'name' '{name}' is invalid; only ASCII alphanumerics, '_' and '-' are allowed"
        ));
    }
    if name
        .chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        != Some(true)
    {
        return Err(format!(
            "field 'name' '{name}' must start with an ASCII letter or '_'"
        ));
    }
    Ok(())
}

fn parse_ai_provider(payload: &JsonValue) -> Result<AiProvider, String> {
    let provider = json_string_field(payload, "provider")
        .or_else(|| std::env::var("REDDB_AI_PROVIDER").ok())
        .unwrap_or_else(|| "openai".to_string());

    crate::ai::parse_provider(&provider).map_err(|e| e.to_string())
}

fn parse_source_mode(payload: &JsonValue) -> Result<AiQuerySourceMode, String> {
    let mode = json_string_field(payload, "source_mode")
        .unwrap_or_else(|| "row".to_string())
        .to_ascii_lowercase();
    match mode.as_str() {
        "row" => Ok(AiQuerySourceMode::Row),
        "result" => Ok(AiQuerySourceMode::Result),
        _ => Err(format!(
            "invalid source_mode '{mode}'; expected 'row' or 'result'"
        )),
    }
}

fn embedding_source_row_ref(
    record: &crate::storage::query::UnifiedRecord,
    source_collection: Option<&str>,
) -> Option<TableRef> {
    let row_id = record
        .get("entity_id")
        .or_else(|| record.get("_entity_id"))
        // The row rid envelope exposes the logical row identity under the
        // canonical `rid` key, so fall back to `rid` for query-sourced rows.
        .or_else(|| record.get("rid"))
        .and_then(|value| match value {
            Value::UnsignedInteger(id) => Some(*id),
            Value::Integer(id) if *id >= 0 => Some(*id as u64),
            _ => None,
        })?;

    let collection = source_collection
        .or_else(|| {
            record.get("red_collection").and_then(|value| match value {
                Value::Text(value) if !value.trim().is_empty() => Some(value.as_ref()),
                _ => None,
            })
        })
        .or_else(|| {
            record.get("_collection").and_then(|value| match value {
                Value::Text(value) if !value.trim().is_empty() => Some(value.as_ref()),
                _ => None,
            })
        })?;

    Some(TableRef::new(collection, row_id))
}

fn parse_optional_positive_usize(
    payload: &JsonValue,
    field: &str,
) -> Result<Option<usize>, String> {
    let Some(value) = payload.get(field) else {
        return Ok(None);
    };
    let Some(number) = value.as_i64() else {
        return Err(format!("field '{field}' must be a positive integer"));
    };
    let Ok(number) = usize::try_from(number) else {
        return Err(format!("field '{field}' must be a positive integer"));
    };
    if number == 0 {
        return Err(format!("field '{field}' must be a positive integer"));
    }
    Ok(Some(number))
}

fn parse_optional_temperature(payload: &JsonValue) -> Result<Option<f32>, String> {
    let Some(value) = payload.get("temperature") else {
        return Ok(None);
    };
    let Some(number) = value.as_f64() else {
        return Err("field 'temperature' must be a number".to_string());
    };
    if !number.is_finite() {
        return Err("field 'temperature' must be finite".to_string());
    }
    Ok(Some(number as f32))
}

fn parse_ai_embedding_save_options(
    payload: &JsonValue,
) -> Result<Option<AiEmbeddingSaveOptions>, String> {
    let save_object = payload.get("save").and_then(JsonValue::as_object);

    let collection = save_object
        .and_then(|object| object.get("collection").and_then(JsonValue::as_str))
        .map(str::to_string)
        .or_else(|| json_string_field(payload, "save_collection"));

    let Some(collection) = collection else {
        return Ok(None);
    };
    if collection.trim().is_empty() {
        return Err("save collection cannot be empty".to_string());
    }

    let include_content = save_object
        .and_then(|object| object.get("include_content").and_then(JsonValue::as_bool))
        .or_else(|| json_bool_field(payload, "save_include_content"))
        .unwrap_or(true);

    let metadata_source = save_object
        .and_then(|object| object.get("metadata"))
        .or_else(|| payload.get("save_metadata"));
    let metadata = parse_metadata_entries(metadata_source)?;

    Ok(Some(AiEmbeddingSaveOptions {
        collection,
        include_content,
        metadata,
    }))
}

fn parse_ai_prompt_save_options(
    payload: &JsonValue,
) -> Result<Option<AiPromptSaveOptions>, String> {
    let save_object = payload.get("save").and_then(JsonValue::as_object);

    let collection = save_object
        .and_then(|object| object.get("collection").and_then(JsonValue::as_str))
        .map(str::to_string)
        .or_else(|| json_string_field(payload, "save_collection"));

    let Some(collection) = collection else {
        return Ok(None);
    };
    if collection.trim().is_empty() {
        return Err("save collection cannot be empty".to_string());
    }

    let prompt_field = save_object
        .and_then(|object| object.get("prompt_field").and_then(JsonValue::as_str))
        .map(str::to_string)
        .or_else(|| json_string_field(payload, "save_prompt_field"))
        .unwrap_or_else(|| "prompt".to_string());
    if prompt_field.trim().is_empty() {
        return Err("save prompt_field cannot be empty".to_string());
    }

    let response_field = save_object
        .and_then(|object| object.get("response_field").and_then(JsonValue::as_str))
        .map(str::to_string)
        .or_else(|| json_string_field(payload, "save_response_field"))
        .unwrap_or_else(|| "response".to_string());
    if response_field.trim().is_empty() {
        return Err("save response_field cannot be empty".to_string());
    }

    if prompt_field == response_field {
        return Err("save prompt_field and response_field must be different".to_string());
    }

    let metadata_source = save_object
        .and_then(|object| object.get("metadata"))
        .or_else(|| payload.get("save_metadata"));
    let metadata = parse_metadata_entries(metadata_source)?;

    Ok(Some(AiPromptSaveOptions {
        collection,
        prompt_field,
        response_field,
        metadata,
    }))
}

fn parse_metadata_entries(
    value: Option<&JsonValue>,
) -> Result<Vec<(String, MetadataValue)>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let Some(object) = value.as_object() else {
        return Err("save metadata must be a JSON object".to_string());
    };

    let mut entries = Vec::with_capacity(object.len());
    for (key, value) in object {
        let metadata_value = crate::application::entity::json_to_metadata_value(value)
            .map_err(|err| format!("invalid save metadata field '{key}': {err}"))?;
        entries.push((key.clone(), metadata_value));
    }
    Ok(entries)
}

fn render_prompt_template<F>(template: &str, mut resolver: F) -> String
where
    F: FnMut(&str) -> Option<String>,
{
    let mut output = String::with_capacity(template.len() + 32);
    let mut cursor = 0usize;

    while let Some(start_rel) = template[cursor..].find("{{") {
        let start = cursor + start_rel;
        output.push_str(&template[cursor..start]);

        let token_start = start + 2;
        let Some(end_rel) = template[token_start..].find("}}") else {
            output.push_str(&template[start..]);
            return output;
        };
        let token_end = token_start + end_rel;
        let token = template[token_start..token_end].trim();
        if !token.is_empty() {
            if let Some(value) = resolver(token) {
                output.push_str(&value);
            }
        }
        cursor = token_end + 2;
    }

    output.push_str(&template[cursor..]);
    output
}

/// Insert a value into a nested Map following dot-separated path segments.
fn insert_nested(root: &mut Map<String, JsonValue>, parts: &[&str], value: JsonValue) {
    if parts.is_empty() {
        return;
    }
    if parts.len() == 1 {
        root.insert(parts[0].to_string(), value);
        return;
    }
    let child = root
        .entry(parts[0].to_string())
        .or_insert_with(|| JsonValue::Object(Map::new()));
    if let JsonValue::Object(ref mut map) = child {
        insert_nested(map, &parts[1..], value);
    }
}

/// Flatten a nested JSON object into dot-notation key-value pairs.
fn flatten_json(prefix: &str, value: &JsonValue, out: &mut Vec<(String, JsonValue)>) {
    match value {
        JsonValue::Object(map) => {
            for (k, v) in map {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_json(&key, v, out);
            }
        }
        _ => {
            if !prefix.is_empty() {
                out.push((prefix.to_string(), value.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::auth::{AuthConfig, AuthStore};
    use crate::{RedDBOptions, RedDBRuntime};

    #[test]
    fn parse_source_mode_defaults_to_row() {
        let payload = JsonValue::Object(Map::new());
        assert_eq!(
            parse_source_mode(&payload).expect("mode"),
            AiQuerySourceMode::Row
        );
    }

    #[test]
    fn parse_ai_provider_accepts_openai_and_anthropic() {
        let openai = JsonValue::Object({
            let mut map = Map::new();
            map.insert(
                "provider".to_string(),
                JsonValue::String("openai".to_string()),
            );
            map
        });
        assert_eq!(
            parse_ai_provider(&openai).expect("provider"),
            AiProvider::OpenAi
        );

        let anthropic = JsonValue::Object({
            let mut map = Map::new();
            map.insert(
                "provider".to_string(),
                JsonValue::String("anthropic".to_string()),
            );
            map
        });
        assert_eq!(
            parse_ai_provider(&anthropic).expect("provider"),
            AiProvider::Anthropic
        );
    }

    #[test]
    fn render_prompt_template_replaces_tokens() {
        let rendered =
            render_prompt_template("Summarize host {{ip}} seen on port {{port}}", |token| {
                match token {
                    "ip" => Some("10.0.0.4".to_string()),
                    "port" => Some("443".to_string()),
                    _ => None,
                }
            });
        assert_eq!(rendered, "Summarize host 10.0.0.4 seen on port 443");
    }

    #[test]
    fn ai_credentials_store_api_key_in_vault_and_config_ref_only() {
        let mut path = std::env::temp_dir();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        path.push(format!(
            "reddb_ai_credentials_vault_{}_{}",
            std::process::id(),
            nanos
        ));

        let options = RedDBOptions::persistent(&path)
            .with_storage_profile(crate::StorageDeployPreset::Serverless.selection())
            .expect("serverless storage profile should expose pager");
        let rt = RedDBRuntime::with_options(options).expect("runtime opens");
        let db = rt.db();
        let store = db.store();
        let pager = store.pager().expect("persistent runtime has pager");
        const TEST_CERTIFICATE: &str =
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let auth = Arc::new(
            AuthStore::with_vault_certificate(
                AuthConfig::default(),
                Arc::clone(pager),
                TEST_CERTIFICATE,
            )
            .expect("vault opens"),
        );
        let server = RedDBServer::new(rt).with_auth(Arc::clone(&auth));

        let alias = format!("vault_unit_{nanos}");
        let body_json =
            format!(r#"{{"provider":"openai","alias":"{alias}","api_key":"sk_test_vault"}}"#);
        let response = server.handle_ai_credentials(body_json.into_bytes());
        assert_eq!(response.status, 200);
        let body = String::from_utf8(response.body).expect("json body");
        assert!(
            !body.contains("sk_test_vault"),
            "credential response must not echo plaintext: {body}"
        );

        let secret_path = crate::ai::ai_api_secret_path(&AiProvider::OpenAi, &alias);
        assert_eq!(
            auth.vault_kv_get(&secret_path).as_deref(),
            Some("sk_test_vault")
        );

        let ref_key = crate::ai::ai_api_secret_ref_config_key(&AiProvider::OpenAi, &alias);
        let ref_value = server
            .entity_use_cases()
            .get_kv(RED_CONFIG_COLLECTION, &ref_key)
            .expect("read ref")
            .expect("ref exists")
            .0;
        assert_eq!(ref_value, Value::Text(secret_path.clone().into()));

        let removed_legacy = format!("red.config.ai.openai.{alias}.key");
        assert!(
            server
                .entity_use_cases()
                .get_kv(RED_CONFIG_COLLECTION, &removed_legacy)
                .expect("read legacy")
                .is_none(),
            "legacy plaintext config key must not be written"
        );

        let resolved = crate::ai::resolve_api_key_from_runtime(
            &AiProvider::OpenAi,
            Some(&alias),
            server.runtime(),
        )
        .expect("resolve from vault");
        assert_eq!(resolved, "sk_test_vault");

        let _ = std::fs::remove_file(path);
    }

    // ====================================================================
    // Local AI embedding routing (#680).
    //
    // Drives both HTTP (`handle_ai_embeddings`) and gRPC
    // (`crate::ai::grpc_embeddings`) through the same scenarios so the
    // two surfaces stay aligned. The deterministic fake backend lives
    // in `runtime::ai::local_embedding` and is installed per-test;
    // every test that exercises the success path explicitly installs
    // it, and the `disabled_feature_*` tests clear it so the cfg gate
    // is the only signal left.
    // ====================================================================

    use crate::runtime::ai::local_embedding::{
        clear_local_embedding_backend_for_tests, install_local_embedding_backend,
        DeterministicFakeBackend, LocalEmbeddingBackend,
    };

    // The local embedding backend lives in a process-global OnceLock,
    // so any test that touches it must serialize against the rest of
    // the local-embedding suite. Without this lock the parallel test
    // runner interleaves `install` and `clear`, and the disabled-feature
    // path either disappears or leaks into the success tests.
    fn backend_test_lock() -> &'static std::sync::Mutex<()> {
        static L: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        L.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn lock_backend() -> std::sync::MutexGuard<'static, ()> {
        backend_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn fresh_runtime_path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "reddb_local_emb_{label}_{}_{}.rdb",
            std::process::id(),
            nanos
        ))
    }

    fn make_server(label: &str) -> (RedDBServer, std::path::PathBuf) {
        let path = fresh_runtime_path(label);
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime opens");
        (RedDBServer::new(rt), path)
    }

    fn register_local_model(server: &RedDBServer, name: &str, dimensions: u32) {
        let body = format!(
            r#"{{
                "name": "{name}",
                "provider": "local",
                "source": "sentence-transformers/all-MiniLM-L6-v2",
                "task": "embedding",
                "revision": "main",
                "engine": "candle",
                "dimensions": {dimensions}
            }}"#
        );
        let response = server.handle_ai_model_register(body.into_bytes());
        assert_eq!(
            response.status,
            201,
            "register failed: {}",
            String::from_utf8_lossy(&response.body)
        );
    }

    fn stamp_installed_in_registry(server: &RedDBServer, name: &str) {
        // Promote the registry entry to `installed` directly. The cache
        // pull path (#679) writes the on-disk artifacts; this test
        // doesn't exercise pull, only the embeddings surface, so we
        // edit the registry KV in place.
        let key = format!("red.config.ai.models.{name}");
        let entry = server
            .entity_use_cases()
            .get_kv(RED_CONFIG_COLLECTION, &key)
            .expect("read")
            .expect("registered");
        let raw = match entry.0 {
            Value::Text(s) => s.to_string(),
            other => panic!("unexpected value: {other:?}"),
        };
        let mut parsed: JsonValue = crate::json::parse_json(&raw).expect("parse").into();
        if let JsonValue::Object(ref mut object) = parsed {
            object.insert(
                "status".to_string(),
                JsonValue::String("installed".to_string()),
            );
        }
        let encoded = crate::json::to_string(&parsed).expect("re-encode");
        let _ = server
            .entity_use_cases()
            .delete_kv(RED_CONFIG_COLLECTION, &key);
        server
            .entity_use_cases()
            .create_kv(CreateKvInput {
                collection: RED_CONFIG_COLLECTION.to_string(),
                key,
                value: Value::text(encoded),
                metadata: Vec::new(),
            })
            .expect("stamp installed");
    }

    /// Take the backend lock and install the deterministic fake.
    /// Callers bind the returned guard to `_g` so the lock is held for
    /// the whole test body; dropping it lets the next backend test
    /// claim the slot.
    fn install_fake_backend() -> std::sync::MutexGuard<'static, ()> {
        let guard = lock_backend();
        let backend: std::sync::Arc<dyn LocalEmbeddingBackend> =
            std::sync::Arc::new(DeterministicFakeBackend);
        install_local_embedding_backend(backend);
        guard
    }

    /// Take the backend lock and clear the installed slot. The
    /// disabled-feature tests rely on this to make `cfg!(feature =
    /// "local-models")` the only signal left.
    fn clear_backend_for_test() -> std::sync::MutexGuard<'static, ()> {
        let guard = lock_backend();
        clear_local_embedding_backend_for_tests();
        guard
    }

    fn parse_json_body(body: &[u8]) -> JsonValue {
        let text = std::str::from_utf8(body).expect("utf8");
        crate::json::parse_json(text).expect("body json").into()
    }

    #[test]
    fn http_local_embeddings_returns_deterministic_vector_when_registered_and_installed() {
        let _g = install_fake_backend();
        let (server, path) = make_server("http_ok");
        register_local_model(&server, "mini", 8);
        stamp_installed_in_registry(&server, "mini");

        let body = br#"{"provider":"local","model":"mini","inputs":["hello","world"]}"#.to_vec();
        let response = server.handle_ai_embeddings(body);
        assert_eq!(
            response.status,
            200,
            "expected 200, got {} body={}",
            response.status,
            String::from_utf8_lossy(&response.body)
        );
        let payload = parse_json_body(&response.body);
        assert_eq!(
            payload.get("provider").and_then(JsonValue::as_str),
            Some("local")
        );
        assert_eq!(
            payload.get("model").and_then(JsonValue::as_str),
            Some("mini")
        );
        assert_eq!(
            payload.get("dimensions").and_then(JsonValue::as_u64),
            Some(8)
        );
        assert_eq!(payload.get("count").and_then(JsonValue::as_u64), Some(2));
        let embeddings = payload
            .get("embeddings")
            .and_then(JsonValue::as_array)
            .expect("embeddings array");
        assert_eq!(embeddings.len(), 2);
        for row in embeddings {
            let row = row.as_array().expect("row");
            assert_eq!(row.len(), 8);
        }
        // Determinism: replay returns the same bytes.
        let response2 = server.handle_ai_embeddings(
            br#"{"provider":"local","model":"mini","inputs":["hello"]}"#.to_vec(),
        );
        let payload2 = parse_json_body(&response2.body);
        let first = embeddings[0].as_array().unwrap();
        let replay = payload2
            .get("embeddings")
            .and_then(JsonValue::as_array)
            .unwrap()[0]
            .as_array()
            .unwrap();
        assert_eq!(first, replay);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn http_local_embeddings_404_when_model_not_registered() {
        let _g = install_fake_backend();
        let (server, path) = make_server("http_missing");
        let body = br#"{"provider":"local","model":"ghost","inputs":["x"]}"#.to_vec();
        let response = server.handle_ai_embeddings(body);
        assert_eq!(
            response.status,
            404,
            "body={}",
            String::from_utf8_lossy(&response.body)
        );
        let payload = parse_json_body(&response.body);
        let err = payload
            .get("error")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        assert!(
            err.contains("'ghost' is not registered"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn http_local_embeddings_404_when_registered_but_not_installed() {
        let _g = install_fake_backend();
        let (server, path) = make_server("http_not_installed");
        register_local_model(&server, "mini", 4);
        // Skip the install stamp on purpose.
        let body = br#"{"provider":"local","model":"mini","inputs":["x"]}"#.to_vec();
        let response = server.handle_ai_embeddings(body);
        assert_eq!(response.status, 404);
        let payload = parse_json_body(&response.body);
        let err = payload
            .get("error")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        assert!(
            err.contains("not installed") && err.contains("pull"),
            "unexpected error: {err}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn http_local_embeddings_400_when_model_field_missing() {
        let _g = install_fake_backend();
        let (server, path) = make_server("http_no_model");
        let body = br#"{"provider":"local","inputs":["x"]}"#.to_vec();
        let response = server.handle_ai_embeddings(body);
        assert_eq!(response.status, 400);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn http_local_embeddings_501_when_feature_disabled_and_no_backend() {
        // The cfg-feature gate only fires when no backend was installed.
        // Tests run with the default cargo feature set (no `local-models`),
        // so clearing the backend is enough to surface FeatureNotEnabled.
        if cfg!(feature = "local-models") {
            // When the feature flag is on the gate falls back to the
            // deterministic fake; the disabled-feature error is not
            // reachable. Skip the assertion (the dedicated coverage
            // lives in the no-feature CI matrix).
            return;
        }
        let _g = clear_backend_for_test();
        let (server, path) = make_server("http_disabled");
        register_local_model(&server, "mini", 4);
        stamp_installed_in_registry(&server, "mini");
        let body = br#"{"provider":"local","model":"mini","inputs":["x"]}"#.to_vec();
        let response = server.handle_ai_embeddings(body);
        assert_eq!(
            response.status,
            501,
            "expected 501 feature-not-enabled, got {} body={}",
            response.status,
            String::from_utf8_lossy(&response.body)
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn http_local_embeddings_response_does_not_leak_into_huggingface_path() {
        // Remote HuggingFace stays distinct: local routing only fires
        // when provider="local", so a provider="huggingface" request
        // hits the huggingface_embeddings transport (which 4xxs here
        // for lack of API key) and never touches the local backend.
        let _g = install_fake_backend();
        let (server, path) = make_server("http_hf_distinct");
        register_local_model(&server, "mini", 4);
        stamp_installed_in_registry(&server, "mini");
        // Drop the env API key so the HF path 400s on auth, not on
        // accidental local routing.
        std::env::remove_var("REDDB_HUGGINGFACE_API_KEY");
        let body = br#"{"provider":"huggingface","model":"mini","inputs":["x"]}"#.to_vec();
        let response = server.handle_ai_embeddings(body);
        assert_ne!(response.status, 200, "HF without key must not succeed");
        let payload = parse_json_body(&response.body);
        let err = payload
            .get("error")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        // Whatever the failure mode (missing key, network), the message
        // must not name the local provider or the local backend.
        assert!(
            !err.to_ascii_lowercase().contains("local model"),
            "HF path leaked local routing: {err}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn grpc_local_embeddings_returns_provider_local_with_model_metadata() {
        let _g = install_fake_backend();
        let (server, path) = make_server("grpc_ok");
        register_local_model(&server, "mini", 6);
        stamp_installed_in_registry(&server, "mini");

        let payload_text = r#"{"provider":"local","model":"mini","inputs":["a","b"]}"#;
        let payload: JsonValue = crate::json::parse_json(payload_text).expect("parse").into();
        let response = crate::ai::grpc_embeddings(server.runtime(), &payload)
            .expect("grpc embeddings succeeds");
        assert_eq!(
            response.get("provider").and_then(JsonValue::as_str),
            Some("local")
        );
        assert_eq!(
            response.get("model").and_then(JsonValue::as_str),
            Some("mini")
        );
        assert_eq!(
            response.get("model_source").and_then(JsonValue::as_str),
            Some("sentence-transformers/all-MiniLM-L6-v2")
        );
        assert_eq!(
            response.get("model_revision").and_then(JsonValue::as_str),
            Some("main")
        );
        assert_eq!(
            response.get("dimensions").and_then(JsonValue::as_u64),
            Some(6)
        );
        let embeddings = response
            .get("embeddings")
            .and_then(JsonValue::as_array)
            .expect("embeddings");
        assert_eq!(embeddings.len(), 2);
        for row in embeddings {
            assert_eq!(row.as_array().unwrap().len(), 6);
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn grpc_local_embeddings_errors_when_model_not_registered() {
        let _g = install_fake_backend();
        let (server, path) = make_server("grpc_missing");
        let payload: JsonValue =
            crate::json::parse_json(r#"{"provider":"local","model":"ghost","inputs":["a"]}"#)
                .expect("parse")
                .into();
        let err =
            crate::ai::grpc_embeddings(server.runtime(), &payload).expect_err("should not succeed");
        let msg = err.to_string();
        assert!(msg.contains("'ghost' is not registered"), "got: {msg}");
        assert!(matches!(err, crate::RedDBError::NotFound(_)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn grpc_local_embeddings_errors_when_registered_but_not_installed() {
        let _g = install_fake_backend();
        let (server, path) = make_server("grpc_uninstalled");
        register_local_model(&server, "mini", 4);
        let payload: JsonValue =
            crate::json::parse_json(r#"{"provider":"local","model":"mini","inputs":["a"]}"#)
                .expect("parse")
                .into();
        let err =
            crate::ai::grpc_embeddings(server.runtime(), &payload).expect_err("should not succeed");
        let msg = err.to_string();
        assert!(
            msg.contains("not installed") && msg.contains("pull"),
            "got: {msg}"
        );
        assert!(matches!(err, crate::RedDBError::NotFound(_)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn grpc_local_embeddings_errors_when_feature_disabled_and_no_backend() {
        if cfg!(feature = "local-models") {
            return;
        }
        let _g = clear_backend_for_test();
        let (server, path) = make_server("grpc_disabled");
        register_local_model(&server, "mini", 4);
        stamp_installed_in_registry(&server, "mini");
        let payload: JsonValue =
            crate::json::parse_json(r#"{"provider":"local","model":"mini","inputs":["a"]}"#)
                .expect("parse")
                .into();
        let err =
            crate::ai::grpc_embeddings(server.runtime(), &payload).expect_err("should not succeed");
        assert!(matches!(err, crate::RedDBError::FeatureNotEnabled(_)));
        let _ = std::fs::remove_file(path);
    }

    // ====================================================================
    // #683 — credential resolution + pull-policy enforcement
    // ====================================================================

    fn register_local_model_with(
        server: &RedDBServer,
        name: &str,
        dimensions: u32,
        extra_fields: &str,
    ) -> HttpResponse {
        let body = format!(
            r#"{{
                "name": "{name}",
                "provider": "local",
                "source": "sentence-transformers/all-MiniLM-L6-v2",
                "task": "embedding",
                "revision": "main",
                "engine": "candle",
                "dimensions": {dimensions}{extra_fields}
            }}"#
        );
        server.handle_ai_model_register(body.into_bytes())
    }

    #[test]
    fn registration_rejects_plaintext_api_key_field() {
        let (server, path) = make_server("reg_no_plain");
        let resp = register_local_model_with(&server, "mini", 4, r#", "api_key":"sk-leak""#);
        assert_eq!(resp.status, 400, "must reject api_key in body");
        let body = String::from_utf8_lossy(&resp.body);
        assert!(
            body.contains("api_key") && body.contains("plaintext"),
            "unhelpful error: {body}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn registration_rejects_huggingface_token_field() {
        let (server, path) = make_server("reg_no_hf_token");
        let resp = register_local_model_with(&server, "mini", 4, r#", "hf_token":"hf_leak""#);
        assert_eq!(resp.status, 400);
        let body = String::from_utf8_lossy(&resp.body);
        assert!(body.contains("hf_token"), "unhelpful error: {body}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn registration_persists_credential_alias_not_secret() {
        let (server, path) = make_server("reg_with_alias");
        let resp =
            register_local_model_with(&server, "mini", 4, r#", "credential_alias":"hf_prod""#);
        assert_eq!(
            resp.status,
            201,
            "register failed: {}",
            String::from_utf8_lossy(&resp.body)
        );
        let key = format!("red.config.ai.models.mini");
        let (value, _) = server
            .entity_use_cases()
            .get_kv(RED_CONFIG_COLLECTION, &key)
            .expect("read")
            .expect("registered");
        let Value::Text(raw) = value else {
            panic!("not text");
        };
        assert!(
            raw.contains("\"credential_alias\":\"hf_prod\""),
            "alias not stored: {raw}"
        );
        // The persisted entry must never contain any of the rejected
        // plaintext credential field names.
        for forbidden in &[
            "api_key",
            "hf_token",
            "huggingface_token",
            "secret",
            "password",
        ] {
            assert!(
                !raw.contains(&format!("\"{forbidden}\":")),
                "plaintext credential leaked into model entry as '{forbidden}': {raw}"
            );
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn registration_accepts_canonical_and_legacy_pull_policies() {
        let (server, path) = make_server("reg_policies");
        // Canonical names.
        for canonical in &["never", "if_missing", "always"] {
            let extra = format!(r#", "pull_policy":"{canonical}""#);
            let body = format!(
                r#"{{"name":"m_{canonical}","provider":"local","source":"s/r","task":"embedding","revision":"main","engine":"candle","dimensions":4{extra}}}"#
            );
            let resp = server.handle_ai_model_register(body.into_bytes());
            assert_eq!(
                resp.status,
                201,
                "canonical '{canonical}' rejected: {}",
                String::from_utf8_lossy(&resp.body)
            );
        }
        // Legacy aliases.
        for (legacy, canonical) in &[
            ("manual", "never"),
            ("on_demand", "if_missing"),
            ("eager", "always"),
        ] {
            let extra = format!(r#", "pull_policy":"{legacy}""#);
            let body = format!(
                r#"{{"name":"m_{legacy}","provider":"local","source":"s/r","task":"embedding","revision":"main","engine":"candle","dimensions":4{extra}}}"#
            );
            let resp = server.handle_ai_model_register(body.into_bytes());
            assert_eq!(resp.status, 201, "legacy '{legacy}' rejected");
            let parsed = parse_json_body(&resp.body);
            let stored = parsed
                .get("model")
                .and_then(|m| m.get("pull_policy"))
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            assert_eq!(
                stored, *canonical,
                "legacy '{legacy}' must normalise to '{canonical}', got '{stored}'"
            );
        }
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn registration_rejects_unknown_pull_policy() {
        let (server, path) = make_server("reg_bad_policy");
        let resp = register_local_model_with(&server, "mini", 4, r#", "pull_policy":"sometimes""#);
        assert_eq!(resp.status, 400);
        let body = String::from_utf8_lossy(&resp.body);
        assert!(body.contains("sometimes"), "expected echo: {body}");
        let _ = std::fs::remove_file(path);
    }

    fn stamp_policy_on_entry(server: &RedDBServer, name: &str, policy: &str) {
        let key = format!("red.config.ai.models.{name}");
        let (value, _) = server
            .entity_use_cases()
            .get_kv(RED_CONFIG_COLLECTION, &key)
            .expect("read")
            .expect("registered");
        let Value::Text(raw) = value else {
            panic!("not text")
        };
        let mut parsed: JsonValue = crate::json::parse_json(&raw).expect("parse").into();
        if let JsonValue::Object(ref mut obj) = parsed {
            obj.insert(
                "pull_policy".to_string(),
                JsonValue::String(policy.to_string()),
            );
        }
        let encoded = crate::json::to_string(&parsed).expect("re-encode");
        let _ = server
            .entity_use_cases()
            .delete_kv(RED_CONFIG_COLLECTION, &key);
        server
            .entity_use_cases()
            .create_kv(CreateKvInput {
                collection: RED_CONFIG_COLLECTION.to_string(),
                key,
                value: Value::text(encoded),
                metadata: Vec::new(),
            })
            .expect("stamp policy");
    }

    #[test]
    fn embed_local_policy_never_returns_clear_missing_artifact_error() {
        let _g = install_fake_backend();
        let (server, path) = make_server("policy_never");
        register_local_model(&server, "mini", 4);
        stamp_policy_on_entry(&server, "mini", "never");
        // Skip install stamp — model is registered but not installed.
        let body = br#"{"provider":"local","model":"mini","inputs":["x"]}"#.to_vec();
        let response = server.handle_ai_embeddings(body);
        assert_eq!(response.status, 404, "expected 404 missing-artifact");
        let payload = parse_json_body(&response.body);
        let err = payload
            .get("error")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        assert!(
            err.contains("pull_policy='never'") && err.contains("forbids"),
            "policy not surfaced: {err}"
        );
        // The error must point the operator at the explicit pull
        // endpoint and must NOT mention any remote-provider fallback.
        assert!(
            err.contains("POST /ai/models/mini/pull"),
            "no remediation: {err}"
        );
        assert!(
            !err.to_ascii_lowercase().contains("falling back")
                && !err.to_ascii_lowercase().contains("openai")
                && !err.to_ascii_lowercase().contains("huggingface"),
            "silent remote fallback hinted in error: {err}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn embed_local_policy_if_missing_returns_distinct_error() {
        let _g = install_fake_backend();
        let (server, path) = make_server("policy_if_missing");
        register_local_model(&server, "mini", 4);
        stamp_policy_on_entry(&server, "mini", "if_missing");
        let body = br#"{"provider":"local","model":"mini","inputs":["x"]}"#.to_vec();
        let response = server.handle_ai_embeddings(body);
        assert_eq!(response.status, 404);
        let payload = parse_json_body(&response.body);
        let err = payload
            .get("error")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        assert!(
            err.contains("pull_policy='if_missing'") && err.contains("POST /ai/models/mini/pull"),
            "policy not surfaced or remediation missing: {err}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn embed_local_policy_always_surfaces_refresh_intent_in_error() {
        let _g = install_fake_backend();
        let (server, path) = make_server("policy_always");
        register_local_model(&server, "mini", 4);
        stamp_policy_on_entry(&server, "mini", "always");
        let body = br#"{"provider":"local","model":"mini","inputs":["x"]}"#.to_vec();
        let response = server.handle_ai_embeddings(body);
        assert_eq!(response.status, 404);
        let payload = parse_json_body(&response.body);
        let err = payload
            .get("error")
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        assert!(
            err.contains("pull_policy='always'") && err.contains("refresh"),
            "policy not surfaced: {err}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn embed_local_no_silent_remote_fallback_when_local_not_installed() {
        // Confirm that even with valid HF env credentials set, a
        // provider=local request that fails locally is NOT routed to
        // the remote HuggingFace transport.
        let _g = install_fake_backend();
        let (server, path) = make_server("no_silent_fb");
        register_local_model(&server, "mini", 4);
        // Don't stamp installed.
        std::env::set_var("REDDB_HUGGINGFACE_API_KEY", "hf_test_key");
        let body = br#"{"provider":"local","model":"mini","inputs":["x"]}"#.to_vec();
        let response = server.handle_ai_embeddings(body);
        std::env::remove_var("REDDB_HUGGINGFACE_API_KEY");
        // 4xx, not 200; and the error must not name a remote provider.
        assert!(
            (400..500).contains(&response.status),
            "unexpected success-or-5xx status: {}",
            response.status
        );
        let payload = parse_json_body(&response.body);
        let err = payload
            .get("error")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        assert!(
            !err.contains("openai") && !err.contains("huggingface api"),
            "remote provider mentioned in local-failure error: {err}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pull_rejects_plaintext_api_key_in_body() {
        let (server, path) = make_server("pull_no_plain");
        register_local_model(&server, "mini", 4);
        let body = br#"{"api_key":"sk-leak"}"#.to_vec();
        let resp = server.handle_ai_model_pull("mini", body);
        assert_eq!(resp.status, 400, "pull must reject plaintext api_key");
        let body = String::from_utf8_lossy(&resp.body);
        assert!(
            body.contains("api_key") && body.contains("plaintext"),
            "unhelpful error: {body}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pull_with_unset_credential_alias_errors_with_vault_remediation() {
        // The model entry has credential_alias=hf_prod but no secret has
        // been stored at red.secret.ai.providers.huggingface.tokens.hf_prod,
        // so the pull must fail before touching the cache and the error must
        // point the operator at the vault path.
        let (server, path) = make_server("pull_alias_unset");
        let resp =
            register_local_model_with(&server, "mini", 4, r#", "credential_alias":"hf_prod""#);
        assert_eq!(resp.status, 201);
        // Ensure no env override silently provides the key.
        std::env::remove_var("REDDB_HUGGINGFACE_API_KEY_HF_PROD");
        std::env::remove_var("REDDB_HUGGINGFACE_API_KEY");
        let resp = server.handle_ai_model_pull("mini", Vec::new());
        assert_eq!(resp.status, 400, "expected 400 missing-credential");
        let body = String::from_utf8_lossy(&resp.body);
        assert!(
            body.contains("hf_prod")
                && body.contains("red.secret.ai.providers.huggingface.tokens.hf_prod"),
            "vault remediation missing: {body}"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn pull_resolves_credential_from_vault_via_alias() {
        // Stage a vault secret at the canonical path and confirm the
        // pull resolves it (and proceeds to the fixture-dir check,
        // which fails next — but with a *different* error class).
        let (server, path) = make_server("pull_alias_ok");
        let resp =
            register_local_model_with(&server, "mini", 4, r#", "credential_alias":"hf_prod""#);
        assert_eq!(resp.status, 201);
        // Stage the credential via the env fallback (the resolution order is
        // vault → secret_ref → env). Env is the simplest stand-in that does
        // not need an AuthStore-backed vault set up in this unit test. The
        // removed legacy plaintext config path is no longer read (#1745).
        std::env::set_var("REDDB_HUGGINGFACE_API_KEY_HF_PROD", "hf_real_token");
        std::env::remove_var("REDDB_HUGGINGFACE_API_KEY");

        // No fixture_dir → the fixture stage fails. We assert the
        // error is the fixture-stage error, proving credential
        // resolution passed.
        let resp = server.handle_ai_model_pull("mini", Vec::new());
        std::env::remove_var("REDDB_HUGGINGFACE_API_KEY_HF_PROD");
        assert_eq!(resp.status, 400);
        let body = String::from_utf8_lossy(&resp.body);
        assert!(
            body.contains("fixture_dir") || body.contains("no artifact source"),
            "expected fixture-stage error after successful credential resolution: {body}"
        );
        // And critically: the resolved token must not appear in the
        // model entry KV.
        let key = format!("red.config.ai.models.mini");
        let (value, _) = server
            .entity_use_cases()
            .get_kv(RED_CONFIG_COLLECTION, &key)
            .expect("read")
            .expect("registered");
        let Value::Text(raw) = value else {
            panic!("not text")
        };
        assert!(
            !raw.contains("hf_real_token"),
            "resolved token leaked into registry entry: {raw}"
        );
        let _ = std::fs::remove_file(path);
    }
}
