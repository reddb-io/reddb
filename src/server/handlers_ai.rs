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

        let (answer, prompt_tokens, completion_tokens) = match provider {
            AiProvider::Anthropic => {
                match crate::ai::anthropic_prompt(crate::ai::AnthropicPromptRequest {
                    api_key,
                    model: model.clone(),
                    prompt: full_prompt,
                    temperature: Some(0.3),
                    max_output_tokens: Some(2048),
                    api_base,
                    anthropic_version: crate::ai::DEFAULT_ANTHROPIC_VERSION.to_string(),
                }) {
                    Ok(resp) => (
                        resp.output_text,
                        resp.prompt_tokens.unwrap_or(0),
                        resp.completion_tokens.unwrap_or(0),
                    ),
                    Err(err) => return json_error(502, err.to_string()),
                }
            }
            _ => {
                match crate::ai::openai_prompt(crate::ai::OpenAiPromptRequest {
                    api_key,
                    model: model.clone(),
                    prompt: full_prompt,
                    temperature: Some(0.3),
                    max_output_tokens: Some(2048),
                    api_base,
                }) {
                    Ok(resp) => (
                        resp.output_text,
                        resp.prompt_tokens.unwrap_or(0),
                        resp.completion_tokens.unwrap_or(0),
                    ),
                    Err(err) => return json_error(502, err.to_string()),
                }
            }
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

        let provider = match parse_ai_provider(&payload) {
            Ok(provider) => provider,
            Err(err) => return json_error(400, err),
        };
        // OpenAI-compatible providers (Groq, Ollama, OpenRouter, Together,
        // Venice, DeepSeek, Custom, OpenAI itself) all speak the same
        // `POST /embeddings` shape, so we route them through the shared
        // transport. Non-compatible providers (Anthropic has no
        // embeddings endpoint today, HuggingFace uses a different
        // shape, Local needs the `local-models` feature flag) are
        // rejected with a clear, provider-specific message.
        if !provider.is_openai_compatible() {
            return json_error(
                400,
                format!(
                    "embeddings are not yet available for provider '{}'. \
                     Use an OpenAI-compatible provider (openai, groq, ollama, \
                     openrouter, together, venice, deepseek, or a custom base URL).",
                    provider.token()
                ),
            );
        }

        let model = json_string_field(&payload, "model").unwrap_or_else(|| {
            // Provider-specific embedding model env var first, then generic
            // fallback, then the provider's compiled-in default.
            std::env::var(format!(
                "REDDB_{}_EMBEDDING_MODEL",
                provider.token().to_ascii_uppercase()
            ))
            .ok()
            .or_else(|| std::env::var("REDDB_OPENAI_EMBEDDING_MODEL").ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| provider.default_embedding_model().to_string())
        });
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

        let response = match crate::ai::openai_embeddings(crate::ai::OpenAiEmbeddingRequest {
            api_key,
            model: model.clone(),
            inputs: inputs.iter().map(|item| item.text.clone()).collect(),
            dimensions,
            api_base,
        }) {
            Ok(response) => response,
            Err(err) => return json_error(400, err.to_string()),
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

    pub(crate) fn handle_ai_prompt(&self, body: Vec<u8>) -> HttpResponse {
        let payload = match parse_json_body_allow_empty(&body) {
            Ok(payload) => payload,
            Err(response) => return response,
        };

        let provider = match parse_ai_provider(&payload) {
            Ok(provider) => provider,
            Err(err) => return json_error(400, err),
        };

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
                    crate::ai::anthropic_prompt(crate::ai::AnthropicPromptRequest {
                        api_key: api_key.clone(),
                        model: model.clone(),
                        prompt: prompt.clone(),
                        temperature,
                        max_output_tokens,
                        api_base: api_base.clone(),
                        anthropic_version: anthropic_version.clone(),
                    })
                }
                _ => crate::ai::openai_prompt(crate::ai::OpenAiPromptRequest {
                    api_key: api_key.clone(),
                    model: model.clone(),
                    prompt: prompt.clone(),
                    temperature,
                    max_output_tokens,
                    api_base: api_base.clone(),
                }),
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
            let key_name = format!("red.config.ai.{}.{}.key", provider.token(), alias);
            let _ = self
                .entity_use_cases()
                .delete_kv(RED_CONFIG_COLLECTION, &key_name);
            match self.entity_use_cases().create_kv(CreateKvInput {
                collection: RED_CONFIG_COLLECTION.to_string(),
                key: key_name.clone(),
                value: Value::text(api_key.clone()),
                metadata: metadata.clone(),
            }) {
                Ok(output) => saved_keys.push((key_name, output.id.raw())),
                Err(err) => return json_error(400, format!("failed to store credential: {err}")),
            }
        }

        // Save API base URL
        if let Some(api_base) = &api_base {
            let base_key = format!("red.config.ai.{}.{}.base_url", provider.token(), alias);
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
            let _ = self
                .entity_use_cases()
                .delete_kv(RED_CONFIG_COLLECTION, "red.config.ai.default.provider");
            let _ = self.entity_use_cases().create_kv(CreateKvInput {
                collection: RED_CONFIG_COLLECTION.to_string(),
                key: "red.config.ai.default.provider".to_string(),
                value: Value::text(provider.token().to_string()),
                metadata: Vec::new(),
            });

            let model = json_string_field(&payload, "model")
                .unwrap_or_else(|| provider.default_prompt_model().to_string());
            let _ = self
                .entity_use_cases()
                .delete_kv(RED_CONFIG_COLLECTION, "red.config.ai.default.model");
            let _ = self.entity_use_cases().create_kv(CreateKvInput {
                collection: RED_CONFIG_COLLECTION.to_string(),
                key: "red.config.ai.default.model".to_string(),
                value: Value::text(model.clone()),
                metadata: Vec::new(),
            });

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
        .get("red_entity_id")
        .or_else(|| record.get("entity_id"))
        .or_else(|| record.get("_entity_id"))
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
}
