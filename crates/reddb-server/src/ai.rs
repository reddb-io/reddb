//! External AI provider integration primitives.
//!
//! This module currently supports OpenAI embeddings and is intended to be
//! consumed by server handlers and future query/runtime integrations.

use std::io::BufRead;
use std::time::Duration;

use crate::json::{parse_json, Map, Value as JsonValue};
use crate::{RedDBError, RedDBResult};

/// Shared HTTP helper for every outbound AI provider call. Centralises
/// the ureq 3.x builder and error conversion. Returns
/// `(status, body)` even on 4xx/5xx (via
/// `http_status_as_error(false)`) so callers can format a
/// provider-specific error without re-plumbing the 3.x error enum.
fn http_post_json(
    url: &str,
    api_key: &str,
    extra_headers: &[(&str, &str)],
    payload: String,
    read_timeout_secs: u64,
) -> Result<(u16, String), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_send_request(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(read_timeout_secs)))
        .timeout_recv_body(Some(Duration::from_secs(read_timeout_secs)))
        .http_status_as_error(false)
        .build()
        .into();

    let mut req = agent
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json");
    for (k, v) in extra_headers {
        req = req.header(*k, *v);
    }
    let trimmed_key = api_key.trim();
    if !trimmed_key.is_empty() {
        req = req.header("Authorization", &format!("Bearer {}", trimmed_key));
    }

    match req.send(payload) {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let body = resp
                .body_mut()
                .read_to_string()
                .map_err(|err| format!("failed to read response body: {err}"))?;
            Ok((status, body))
        }
        Err(err) => Err(format!("{err}")),
    }
}

pub const DEFAULT_OPENAI_EMBEDDING_MODEL: &str = "text-embedding-3-small";
pub const DEFAULT_OPENAI_API_BASE: &str = "https://api.openai.com/v1";
pub const DEFAULT_OPENAI_PROMPT_MODEL: &str = "gpt-4.1-mini";
pub const DEFAULT_ANTHROPIC_PROMPT_MODEL: &str = "claude-3-5-haiku-latest";
pub const DEFAULT_ANTHROPIC_API_BASE: &str = "https://api.anthropic.com/v1";
pub const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug, Clone)]
pub struct OpenAiEmbeddingRequest {
    pub api_key: String,
    pub model: String,
    pub inputs: Vec<String>,
    pub dimensions: Option<usize>,
    pub api_base: String,
}

#[derive(Debug, Clone)]
pub struct OpenAiEmbeddingResponse {
    pub provider: &'static str,
    pub model: String,
    pub embeddings: Vec<Vec<f32>>,
    pub prompt_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct OpenAiPromptRequest {
    pub api_key: String,
    pub model: String,
    pub prompt: String,
    pub temperature: Option<f32>,
    pub seed: Option<u64>,
    pub max_output_tokens: Option<usize>,
    pub api_base: String,
    pub stream: bool,
}

#[derive(Debug, Clone)]
pub struct AnthropicPromptRequest {
    pub api_key: String,
    pub model: String,
    pub prompt: String,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<usize>,
    pub api_base: String,
    pub anthropic_version: String,
}

#[derive(Debug, Clone)]
pub struct AiPromptResponse {
    pub provider: &'static str,
    pub model: String,
    pub output_text: String,
    pub output_chunks: Option<Vec<String>>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub stop_reason: Option<String>,
}

#[deprecated(
    since = "1.0.0",
    note = "use AiBatchClient::embed_batch for embeddings or openai_embeddings_async with AiTransport when token usage metadata is required"
)]
pub fn openai_embeddings(request: OpenAiEmbeddingRequest) -> RedDBResult<OpenAiEmbeddingResponse> {
    if request.model.trim().is_empty() {
        return Err(RedDBError::Query(
            "OpenAI embedding model cannot be empty".to_string(),
        ));
    }
    if request.inputs.is_empty() {
        return Err(RedDBError::Query(
            "at least one input is required for embeddings".to_string(),
        ));
    }

    let url = format!("{}/embeddings", request.api_base.trim_end_matches('/'));
    let payload =
        build_openai_embedding_payload(&request.model, &request.inputs, request.dimensions);

    let (status, body) = http_post_json(&url, &request.api_key, &[], payload, 90)
        .map_err(|err| RedDBError::Query(format!("OpenAI transport error: {err}")))?;

    if !(200..300).contains(&status) {
        let message = openai_error_message(&body)
            .unwrap_or_else(|| "OpenAI embeddings request failed".to_string());
        return Err(RedDBError::Query(format!(
            "OpenAI embeddings request failed (status {status}): {message}"
        )));
    }

    parse_openai_embedding_response(&body)
}

#[deprecated(since = "1.0.0", note = "use openai_prompt_async with AiTransport")]
pub fn openai_prompt(request: OpenAiPromptRequest) -> RedDBResult<AiPromptResponse> {
    if request.model.trim().is_empty() {
        return Err(RedDBError::Query(
            "OpenAI prompt model cannot be empty".to_string(),
        ));
    }
    if request.prompt.trim().is_empty() {
        return Err(RedDBError::Query("prompt cannot be empty".to_string()));
    }

    let url = format!(
        "{}/chat/completions",
        request.api_base.trim_end_matches('/')
    );
    let payload = build_openai_prompt_payload(
        &request.model,
        &request.prompt,
        request.temperature,
        request.seed,
        request.max_output_tokens,
        false,
    );

    let (status, body) = http_post_json(&url, &request.api_key, &[], payload, 120)
        .map_err(|err| RedDBError::Query(format!("OpenAI transport error: {err}")))?;

    if !(200..300).contains(&status) {
        let message = openai_error_message(&body)
            .unwrap_or_else(|| "OpenAI prompt request failed".to_string());
        return Err(RedDBError::Query(format!(
            "OpenAI prompt request failed (status {status}): {message}"
        )));
    }

    parse_openai_prompt_response(&body, &request.model)
}

#[deprecated(since = "1.0.0", note = "use anthropic_prompt_async with AiTransport")]
pub fn anthropic_prompt(request: AnthropicPromptRequest) -> RedDBResult<AiPromptResponse> {
    if request.api_key.trim().is_empty() {
        return Err(RedDBError::Query(
            "Anthropic API key cannot be empty".to_string(),
        ));
    }
    if request.model.trim().is_empty() {
        return Err(RedDBError::Query(
            "Anthropic model cannot be empty".to_string(),
        ));
    }
    if request.prompt.trim().is_empty() {
        return Err(RedDBError::Query("prompt cannot be empty".to_string()));
    }

    let url = format!("{}/messages", request.api_base.trim_end_matches('/'));
    let payload = build_anthropic_prompt_payload(
        &request.model,
        &request.prompt,
        request.temperature,
        request.max_output_tokens,
    );

    // Anthropic uses its own `x-api-key` header instead of
    // `Authorization: Bearer`, so skip the shared helper's default
    // auth header path — we pass an empty API key and set
    // `x-api-key` via extra_headers instead.
    let extra = [
        ("x-api-key", request.api_key.as_str()),
        ("anthropic-version", request.anthropic_version.as_str()),
    ];
    let (status, body) = http_post_json(&url, "", &extra, payload, 120)
        .map_err(|err| RedDBError::Query(format!("Anthropic transport error: {err}")))?;

    if !(200..300).contains(&status) {
        let message = anthropic_error_message(&body)
            .unwrap_or_else(|| "Anthropic prompt request failed".to_string());
        return Err(RedDBError::Query(format!(
            "Anthropic prompt request failed (status {status}): {message}"
        )));
    }

    parse_anthropic_prompt_response(&body, &request.model)
}

/// Async OpenAI-compatible embeddings via [`AiTransport`].
///
/// Uses the transport's connection pool and retry policy (429/5xx backoff)
/// instead of the deprecated one-shot blocking path.
pub async fn openai_embeddings_async(
    transport: &crate::runtime::ai::transport::AiTransport,
    request: OpenAiEmbeddingRequest,
) -> RedDBResult<OpenAiEmbeddingResponse> {
    if request.model.trim().is_empty() {
        return Err(RedDBError::Query(
            "OpenAI embedding model cannot be empty".to_string(),
        ));
    }
    if request.inputs.is_empty() {
        return Err(RedDBError::Query(
            "at least one input is required for embeddings".to_string(),
        ));
    }

    let url = format!("{}/embeddings", request.api_base.trim_end_matches('/'));
    let payload =
        build_openai_embedding_payload(&request.model, &request.inputs, request.dimensions);
    let mut http_req =
        crate::runtime::ai::transport::AiHttpRequest::post_json("openai-compatible", url, payload);
    let trimmed_key = request.api_key.trim();
    if !trimmed_key.is_empty() {
        http_req = http_req.header("authorization", format!("Bearer {}", trimmed_key));
    }

    let response = transport
        .request(http_req)
        .await
        .map_err(|e| RedDBError::Query(e.to_string()))?;

    parse_openai_embedding_response(&response.body)
}

/// Async OpenAI chat-completion prompt via [`AiTransport`].
///
/// Uses the transport's connection pool and retry policy (429/5xx backoff)
/// instead of the deprecated one-shot blocking path.
pub async fn openai_prompt_async(
    transport: &crate::runtime::ai::transport::AiTransport,
    request: OpenAiPromptRequest,
) -> RedDBResult<AiPromptResponse> {
    if request.model.trim().is_empty() {
        return Err(RedDBError::Query(
            "OpenAI prompt model cannot be empty".to_string(),
        ));
    }
    if request.prompt.trim().is_empty() {
        return Err(RedDBError::Query("prompt cannot be empty".to_string()));
    }

    let url = format!(
        "{}/chat/completions",
        request.api_base.trim_end_matches('/')
    );
    let payload = build_openai_prompt_payload(
        &request.model,
        &request.prompt,
        request.temperature,
        request.seed,
        request.max_output_tokens,
        request.stream,
    );
    let http_req = crate::runtime::ai::transport::AiHttpRequest::post_json("openai", url, payload)
        .model(request.model.clone())
        .header("authorization", format!("Bearer {}", request.api_key));

    let response = transport
        .request(http_req)
        .await
        .map_err(|e| RedDBError::Query(e.to_string()))?;

    if request.stream {
        parse_openai_streaming_prompt_response(&response.body, &request.model)
    } else {
        parse_openai_prompt_response(&response.body, &request.model)
    }
}

/// Blocking OpenAI-compatible streaming prompt.
///
/// This is used by the socket-level `ASK ... STREAM` path so each provider
/// `delta.content` can be forwarded to the HTTP client before the provider
/// body has completed.
pub fn openai_prompt_streaming(
    request: OpenAiPromptRequest,
    mut on_chunk: impl FnMut(&str) -> RedDBResult<()>,
) -> RedDBResult<AiPromptResponse> {
    if request.model.trim().is_empty() {
        return Err(RedDBError::Query(
            "OpenAI prompt model cannot be empty".to_string(),
        ));
    }
    if request.prompt.trim().is_empty() {
        return Err(RedDBError::Query("prompt cannot be empty".to_string()));
    }

    let url = format!(
        "{}/chat/completions",
        request.api_base.trim_end_matches('/')
    );
    let payload = build_openai_prompt_payload(
        &request.model,
        &request.prompt,
        request.temperature,
        request.seed,
        request.max_output_tokens,
        true,
    );

    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_send_request(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(120)))
        .timeout_recv_body(Some(Duration::from_secs(120)))
        .http_status_as_error(false)
        .build()
        .into();

    let mut req = agent
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream");
    let trimmed_key = request.api_key.trim();
    if !trimmed_key.is_empty() {
        req = req.header("authorization", &format!("Bearer {}", trimmed_key));
    }

    let mut response = req
        .send(payload)
        .map_err(|err| RedDBError::Query(format!("OpenAI transport error: {err}")))?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response
            .body_mut()
            .read_to_string()
            .unwrap_or_else(|err| format!("failed to read response body: {err}"));
        let message = openai_error_message(&body)
            .unwrap_or_else(|| "OpenAI prompt request failed".to_string());
        return Err(RedDBError::Query(format!(
            "OpenAI prompt request failed (status {status}): {message}"
        )));
    }

    let mut model = request.model;
    let mut chunks = Vec::new();
    let mut prompt_tokens = None;
    let mut completion_tokens = None;
    let mut total_tokens = None;
    let mut stop_reason = None;

    let mut reader = std::io::BufReader::new(response.body_mut().as_reader());
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).map_err(|err| {
            RedDBError::Query(format!("failed to read OpenAI streaming response: {err}"))
        })?;
        if read == 0 {
            break;
        }

        let trimmed = line.trim();
        let Some(data) = trimmed.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }

        let parsed = parse_json(data).map_err(|err| {
            RedDBError::Query(format!(
                "invalid OpenAI streaming prompt JSON response: {err}"
            ))
        })?;
        let json = JsonValue::from(parsed);
        if let Some(value) = json.get("model").and_then(JsonValue::as_str) {
            model = value.to_string();
        }
        if let Some(usage) = json.get("usage") {
            prompt_tokens = usage
                .get("prompt_tokens")
                .and_then(JsonValue::as_i64)
                .and_then(|value| u64::try_from(value).ok())
                .or(prompt_tokens);
            completion_tokens = usage
                .get("completion_tokens")
                .and_then(JsonValue::as_i64)
                .and_then(|value| u64::try_from(value).ok())
                .or(completion_tokens);
            total_tokens = usage
                .get("total_tokens")
                .and_then(JsonValue::as_i64)
                .and_then(|value| u64::try_from(value).ok())
                .or(total_tokens);
        }

        let Some(choices) = json.get("choices").and_then(JsonValue::as_array) else {
            continue;
        };
        let Some(first_choice) = choices.first() else {
            continue;
        };
        if let Some(reason) = first_choice
            .get("finish_reason")
            .and_then(JsonValue::as_str)
        {
            stop_reason = Some(reason.to_string());
        }
        if let Some(text) = first_choice
            .get("delta")
            .and_then(|delta| delta.get("content"))
            .and_then(JsonValue::as_str)
        {
            if !text.is_empty() {
                on_chunk(text)?;
                chunks.push(text.to_string());
            }
        }
    }

    if chunks.is_empty() {
        return Err(RedDBError::Query(
            "OpenAI streaming prompt response missing text content".to_string(),
        ));
    }

    let output_text = chunks.concat();
    let total_tokens = total_tokens.or_else(|| match (prompt_tokens, completion_tokens) {
        (Some(prompt), Some(completion)) => Some(prompt.saturating_add(completion)),
        _ => None,
    });

    Ok(AiPromptResponse {
        provider: "openai",
        model,
        output_text,
        output_chunks: Some(chunks),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        stop_reason,
    })
}

/// Async Anthropic messages-API prompt via [`AiTransport`].
///
/// Uses the transport's connection pool and retry policy (429/5xx backoff)
/// instead of the deprecated one-shot blocking path.
pub async fn anthropic_prompt_async(
    transport: &crate::runtime::ai::transport::AiTransport,
    request: AnthropicPromptRequest,
) -> RedDBResult<AiPromptResponse> {
    if request.api_key.trim().is_empty() {
        return Err(RedDBError::Query(
            "Anthropic API key cannot be empty".to_string(),
        ));
    }
    if request.model.trim().is_empty() {
        return Err(RedDBError::Query(
            "Anthropic model cannot be empty".to_string(),
        ));
    }
    if request.prompt.trim().is_empty() {
        return Err(RedDBError::Query("prompt cannot be empty".to_string()));
    }

    let url = format!("{}/messages", request.api_base.trim_end_matches('/'));
    let payload = build_anthropic_prompt_payload(
        &request.model,
        &request.prompt,
        request.temperature,
        request.max_output_tokens,
    );
    let http_req =
        crate::runtime::ai::transport::AiHttpRequest::post_json("anthropic", url, payload)
            .model(request.model.clone())
            .header("x-api-key", request.api_key)
            .header("anthropic-version", request.anthropic_version);

    let response = transport
        .request(http_req)
        .await
        .map_err(|e| RedDBError::Query(e.to_string()))?;

    parse_anthropic_prompt_response(&response.body, &request.model)
}

/// Build an OpenAI-compatible embedding request payload.
pub(crate) fn build_embedding_payload(model: &str, inputs: &[String]) -> String {
    build_openai_embedding_payload(model, inputs, None)
}

/// Parse an OpenAI-compatible embedding response, returning only the vectors.
pub(crate) fn parse_embedding_vectors(body: &str) -> Result<Vec<Vec<f32>>, String> {
    parse_openai_embedding_response(body)
        .map(|r| r.embeddings)
        .map_err(|e| e.to_string())
}

pub(crate) fn parse_embedding_response(body: &str) -> Result<OpenAiEmbeddingResponse, String> {
    parse_openai_embedding_response(body).map_err(|e| e.to_string())
}

fn build_openai_embedding_payload(
    model: &str,
    inputs: &[String],
    dimensions: Option<usize>,
) -> String {
    let mut object = Map::new();
    object.insert("model".to_string(), JsonValue::String(model.to_string()));
    if inputs.len() == 1 {
        object.insert("input".to_string(), JsonValue::String(inputs[0].clone()));
    } else {
        object.insert(
            "input".to_string(),
            JsonValue::Array(inputs.iter().cloned().map(JsonValue::String).collect()),
        );
    }
    if let Some(dimensions) = dimensions {
        object.insert(
            "dimensions".to_string(),
            JsonValue::Number(dimensions as f64),
        );
    }
    object.insert(
        "encoding_format".to_string(),
        JsonValue::String("float".to_string()),
    );
    JsonValue::Object(object).to_string_compact()
}

fn openai_error_message(body: &str) -> Option<String> {
    provider_error_message(body)
}

fn anthropic_error_message(body: &str) -> Option<String> {
    provider_error_message(body)
}

fn provider_error_message(body: &str) -> Option<String> {
    let parsed = parse_json(body).ok().map(JsonValue::from)?;
    let error = parsed.get("error")?;
    if let Some(message) = error.get("message").and_then(JsonValue::as_str) {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn build_openai_prompt_payload(
    model: &str,
    prompt: &str,
    temperature: Option<f32>,
    seed: Option<u64>,
    max_output_tokens: Option<usize>,
    stream: bool,
) -> String {
    let mut object = Map::new();
    object.insert("model".to_string(), JsonValue::String(model.to_string()));

    let mut message = Map::new();
    message.insert("role".to_string(), JsonValue::String("user".to_string()));
    message.insert("content".to_string(), JsonValue::String(prompt.to_string()));
    object.insert(
        "messages".to_string(),
        JsonValue::Array(vec![JsonValue::Object(message)]),
    );

    if let Some(temperature) = temperature {
        object.insert(
            "temperature".to_string(),
            JsonValue::Number(temperature as f64),
        );
    }

    if let Some(seed) = seed {
        object.insert("seed".to_string(), JsonValue::Number(seed as f64));
    }

    if let Some(max_output_tokens) = max_output_tokens {
        object.insert(
            "max_tokens".to_string(),
            JsonValue::Number(max_output_tokens as f64),
        );
    }

    if stream {
        object.insert("stream".to_string(), JsonValue::Bool(true));
        let mut options = Map::new();
        options.insert("include_usage".to_string(), JsonValue::Bool(true));
        object.insert("stream_options".to_string(), JsonValue::Object(options));
    }

    JsonValue::Object(object).to_string_compact()
}

fn build_anthropic_prompt_payload(
    model: &str,
    prompt: &str,
    temperature: Option<f32>,
    max_output_tokens: Option<usize>,
) -> String {
    let mut object = Map::new();
    object.insert("model".to_string(), JsonValue::String(model.to_string()));
    object.insert(
        "max_tokens".to_string(),
        JsonValue::Number(max_output_tokens.unwrap_or(512) as f64),
    );

    let mut message = Map::new();
    message.insert("role".to_string(), JsonValue::String("user".to_string()));
    message.insert("content".to_string(), JsonValue::String(prompt.to_string()));
    object.insert(
        "messages".to_string(),
        JsonValue::Array(vec![JsonValue::Object(message)]),
    );

    if let Some(temperature) = temperature {
        object.insert(
            "temperature".to_string(),
            JsonValue::Number(temperature as f64),
        );
    }

    JsonValue::Object(object).to_string_compact()
}

fn extract_text_from_parts(parts: &[JsonValue]) -> Option<String> {
    let mut chunks = Vec::new();
    for part in parts {
        if let Some(text) = part.as_str() {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                chunks.push(trimmed.to_string());
            }
            continue;
        }

        let Some(object) = part.as_object() else {
            continue;
        };
        let Some(text) = object.get("text").and_then(JsonValue::as_str) else {
            continue;
        };
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            chunks.push(trimmed.to_string());
        }
    }

    if chunks.is_empty() {
        None
    } else {
        Some(chunks.join("\n\n"))
    }
}

fn parse_openai_prompt_response(
    body: &str,
    requested_model: &str,
) -> RedDBResult<AiPromptResponse> {
    let parsed = parse_json(body)
        .map_err(|err| RedDBError::Query(format!("invalid OpenAI prompt JSON response: {err}")))?;
    let json = JsonValue::from(parsed);

    let model = json
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or(requested_model)
        .to_string();

    let Some(choices) = json.get("choices").and_then(JsonValue::as_array) else {
        return Err(RedDBError::Query(
            "OpenAI prompt response missing 'choices' array".to_string(),
        ));
    };
    let Some(first_choice) = choices.first() else {
        return Err(RedDBError::Query(
            "OpenAI prompt response contains no choices".to_string(),
        ));
    };

    let output_text = first_choice
        .get("message")
        .and_then(|message| {
            if let Some(text) = message.get("content").and_then(JsonValue::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            message
                .get("content")
                .and_then(JsonValue::as_array)
                .and_then(extract_text_from_parts)
        })
        .ok_or_else(|| {
            RedDBError::Query("OpenAI prompt response missing text content".to_string())
        })?;

    let prompt_tokens = json
        .get("usage")
        .and_then(|usage| usage.get("prompt_tokens"))
        .and_then(JsonValue::as_i64)
        .and_then(|value| u64::try_from(value).ok());
    let completion_tokens = json
        .get("usage")
        .and_then(|usage| usage.get("completion_tokens"))
        .and_then(JsonValue::as_i64)
        .and_then(|value| u64::try_from(value).ok());
    let total_tokens = json
        .get("usage")
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(JsonValue::as_i64)
        .and_then(|value| u64::try_from(value).ok())
        .or_else(|| match (prompt_tokens, completion_tokens) {
            (Some(prompt), Some(completion)) => Some(prompt.saturating_add(completion)),
            _ => None,
        });

    let stop_reason = first_choice
        .get("finish_reason")
        .and_then(JsonValue::as_str)
        .map(str::to_string);

    Ok(AiPromptResponse {
        provider: "openai",
        model,
        output_text,
        output_chunks: None,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        stop_reason,
    })
}

fn parse_openai_streaming_prompt_response(
    body: &str,
    requested_model: &str,
) -> RedDBResult<AiPromptResponse> {
    let mut model = requested_model.to_string();
    let mut chunks = Vec::new();
    let mut prompt_tokens = None;
    let mut completion_tokens = None;
    let mut total_tokens = None;
    let mut stop_reason = None;

    for line in body.lines() {
        let line = line.trim();
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }

        let parsed = parse_json(data).map_err(|err| {
            RedDBError::Query(format!(
                "invalid OpenAI streaming prompt JSON response: {err}"
            ))
        })?;
        let json = JsonValue::from(parsed);
        if let Some(value) = json.get("model").and_then(JsonValue::as_str) {
            model = value.to_string();
        }
        if let Some(usage) = json.get("usage") {
            prompt_tokens = usage
                .get("prompt_tokens")
                .and_then(JsonValue::as_i64)
                .and_then(|value| u64::try_from(value).ok())
                .or(prompt_tokens);
            completion_tokens = usage
                .get("completion_tokens")
                .and_then(JsonValue::as_i64)
                .and_then(|value| u64::try_from(value).ok())
                .or(completion_tokens);
            total_tokens = usage
                .get("total_tokens")
                .and_then(JsonValue::as_i64)
                .and_then(|value| u64::try_from(value).ok())
                .or(total_tokens);
        }

        let Some(choices) = json.get("choices").and_then(JsonValue::as_array) else {
            continue;
        };
        let Some(first_choice) = choices.first() else {
            continue;
        };
        if let Some(reason) = first_choice
            .get("finish_reason")
            .and_then(JsonValue::as_str)
        {
            stop_reason = Some(reason.to_string());
        }
        if let Some(text) = first_choice
            .get("delta")
            .and_then(|delta| delta.get("content"))
            .and_then(JsonValue::as_str)
        {
            if !text.is_empty() {
                chunks.push(text.to_string());
            }
        }
    }

    if chunks.is_empty() {
        return Err(RedDBError::Query(
            "OpenAI streaming prompt response missing text content".to_string(),
        ));
    }

    let output_text = chunks.concat();
    let total_tokens = total_tokens.or_else(|| match (prompt_tokens, completion_tokens) {
        (Some(prompt), Some(completion)) => Some(prompt.saturating_add(completion)),
        _ => None,
    });

    Ok(AiPromptResponse {
        provider: "openai",
        model,
        output_text,
        output_chunks: Some(chunks),
        prompt_tokens,
        completion_tokens,
        total_tokens,
        stop_reason,
    })
}

fn parse_anthropic_prompt_response(
    body: &str,
    requested_model: &str,
) -> RedDBResult<AiPromptResponse> {
    let parsed = parse_json(body).map_err(|err| {
        RedDBError::Query(format!("invalid Anthropic prompt JSON response: {err}"))
    })?;
    let json = JsonValue::from(parsed);

    let model = json
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or(requested_model)
        .to_string();

    let Some(content_parts) = json.get("content").and_then(JsonValue::as_array) else {
        return Err(RedDBError::Query(
            "Anthropic prompt response missing 'content' array".to_string(),
        ));
    };

    let output_text = extract_text_from_parts(content_parts).ok_or_else(|| {
        RedDBError::Query("Anthropic prompt response missing text content".to_string())
    })?;

    let prompt_tokens = json
        .get("usage")
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(JsonValue::as_i64)
        .and_then(|value| u64::try_from(value).ok());
    let completion_tokens = json
        .get("usage")
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(JsonValue::as_i64)
        .and_then(|value| u64::try_from(value).ok());
    let total_tokens = match (prompt_tokens, completion_tokens) {
        (Some(prompt), Some(completion)) => Some(prompt.saturating_add(completion)),
        _ => None,
    };

    let stop_reason = json
        .get("stop_reason")
        .and_then(JsonValue::as_str)
        .map(str::to_string);

    Ok(AiPromptResponse {
        provider: "anthropic",
        model,
        output_text,
        output_chunks: None,
        prompt_tokens,
        completion_tokens,
        total_tokens,
        stop_reason,
    })
}

fn parse_openai_embedding_response(body: &str) -> RedDBResult<OpenAiEmbeddingResponse> {
    let parsed = parse_json(body).map_err(|err| {
        RedDBError::Query(format!("invalid OpenAI embeddings JSON response: {err}"))
    })?;
    let json = JsonValue::from(parsed);

    let model = json
        .get("model")
        .and_then(JsonValue::as_str)
        .unwrap_or(DEFAULT_OPENAI_EMBEDDING_MODEL)
        .to_string();

    let Some(data) = json.get("data").and_then(JsonValue::as_array) else {
        return Err(RedDBError::Query(
            "OpenAI response missing 'data' array".to_string(),
        ));
    };

    let mut rows: Vec<(usize, Vec<f32>)> = Vec::with_capacity(data.len());
    for (position, item) in data.iter().enumerate() {
        let index = item
            .get("index")
            .and_then(JsonValue::as_i64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(position);

        let Some(embedding_values) = item.get("embedding").and_then(JsonValue::as_array) else {
            return Err(RedDBError::Query(
                "OpenAI response contains item without 'embedding' array".to_string(),
            ));
        };
        if embedding_values.is_empty() {
            return Err(RedDBError::Query(
                "OpenAI response contains empty embedding vector".to_string(),
            ));
        }

        let mut embedding = Vec::with_capacity(embedding_values.len());
        for value in embedding_values {
            let Some(number) = value.as_f64() else {
                return Err(RedDBError::Query(
                    "OpenAI response contains non-numeric embedding value".to_string(),
                ));
            };
            embedding.push(number as f32);
        }
        rows.push((index, embedding));
    }
    rows.sort_by_key(|(index, _)| *index);
    let embeddings = rows.into_iter().map(|(_, embedding)| embedding).collect();

    let prompt_tokens = json
        .get("usage")
        .and_then(|usage| usage.get("prompt_tokens"))
        .and_then(JsonValue::as_i64)
        .and_then(|value| u64::try_from(value).ok());
    let total_tokens = json
        .get("usage")
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(JsonValue::as_i64)
        .and_then(|value| u64::try_from(value).ok());

    Ok(OpenAiEmbeddingResponse {
        provider: "openai",
        model,
        embeddings,
        prompt_tokens,
        total_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_openai_embedding_response_extracts_vectors() {
        let body = r#"{
          "object":"list",
          "data":[
            {"object":"embedding","index":1,"embedding":[0.3,0.4]},
            {"object":"embedding","index":0,"embedding":[0.1,0.2]}
          ],
          "model":"text-embedding-3-small",
          "usage":{"prompt_tokens":12,"total_tokens":12}
        }"#;

        let result = parse_openai_embedding_response(body).expect("response should parse");
        assert_eq!(result.provider, "openai");
        assert_eq!(result.model, "text-embedding-3-small");
        assert_eq!(result.embeddings.len(), 2);
        assert_eq!(result.embeddings[0], vec![0.1, 0.2]);
        assert_eq!(result.embeddings[1], vec![0.3, 0.4]);
        assert_eq!(result.prompt_tokens, Some(12));
        assert_eq!(result.total_tokens, Some(12));
    }

    #[test]
    fn openai_error_message_extracts_nested_message() {
        let body = r#"{"error":{"message":"bad api key","type":"invalid_request_error"}}"#;
        assert_eq!(openai_error_message(body).as_deref(), Some("bad api key"));
    }

    #[test]
    fn parse_openai_prompt_response_extracts_text_and_usage() {
        let body = r#"{
          "id":"chatcmpl_1",
          "object":"chat.completion",
          "model":"gpt-4.1-mini",
          "choices":[
            {
              "index":0,
              "finish_reason":"stop",
              "message":{"role":"assistant","content":"Resumo pronto."}
            }
          ],
          "usage":{"prompt_tokens":10,"completion_tokens":4,"total_tokens":14}
        }"#;

        let parsed =
            parse_openai_prompt_response(body, DEFAULT_OPENAI_PROMPT_MODEL).expect("parse");
        assert_eq!(parsed.provider, "openai");
        assert_eq!(parsed.model, "gpt-4.1-mini");
        assert_eq!(parsed.output_text, "Resumo pronto.");
        assert_eq!(parsed.prompt_tokens, Some(10));
        assert_eq!(parsed.completion_tokens, Some(4));
        assert_eq!(parsed.total_tokens, Some(14));
        assert_eq!(parsed.stop_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn parse_anthropic_prompt_response_extracts_text_and_usage() {
        let body = r#"{
          "id":"msg_1",
          "model":"claude-3-5-haiku-latest",
          "type":"message",
          "content":[{"type":"text","text":"Action complete."}],
          "usage":{"input_tokens":11,"output_tokens":5},
          "stop_reason":"end_turn"
        }"#;

        let parsed =
            parse_anthropic_prompt_response(body, DEFAULT_ANTHROPIC_PROMPT_MODEL).expect("parse");
        assert_eq!(parsed.provider, "anthropic");
        assert_eq!(parsed.model, "claude-3-5-haiku-latest");
        assert_eq!(parsed.output_text, "Action complete.");
        assert_eq!(parsed.prompt_tokens, Some(11));
        assert_eq!(parsed.completion_tokens, Some(5));
        assert_eq!(parsed.total_tokens, Some(16));
        assert_eq!(parsed.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn resolve_api_key_prefers_vault_secret_over_legacy_config() {
        let provider = AiProvider::OpenAi;
        let alias = "vault_unit_alias";
        let secret_path = ai_api_secret_path(&provider, alias);
        let legacy_key = ai_api_legacy_config_key(&provider, alias);

        let resolved = resolve_api_key(&provider, Some(alias), |key| {
            if key == secret_path {
                Ok(Some("vault-key".to_string()))
            } else if key == legacy_key {
                Ok(Some("legacy-key".to_string()))
            } else {
                Ok(None)
            }
        })
        .expect("resolve");

        assert_eq!(resolved, "vault-key");
    }

    #[test]
    fn resolve_api_key_uses_default_vault_secret_path() {
        let provider = AiProvider::OpenAi;
        let secret_path = ai_api_secret_path(&provider, "default");

        let resolved = resolve_api_key(&provider, None, |key| {
            if key == secret_path {
                Ok(Some("default-vault-key".to_string()))
            } else {
                Ok(None)
            }
        })
        .expect("resolve");

        assert_eq!(resolved, "default-vault-key");
    }

    #[test]
    fn openai_prompt_payload_includes_temperature_and_seed_when_present() {
        let payload = build_openai_prompt_payload(
            "gpt-4.1-mini",
            "hello",
            Some(0.0),
            Some(42),
            Some(128),
            false,
        );
        let parsed = JsonValue::from(parse_json(&payload).expect("valid json"));

        assert_eq!(
            parsed.get("temperature").and_then(JsonValue::as_f64),
            Some(0.0)
        );
        assert_eq!(parsed.get("seed").and_then(JsonValue::as_u64), Some(42));
        assert_eq!(
            parsed.get("max_tokens").and_then(JsonValue::as_u64),
            Some(128)
        );
    }

    #[test]
    fn openai_prompt_payload_omits_seed_when_none() {
        let payload =
            build_openai_prompt_payload("gpt-4.1-mini", "hello", Some(0.0), None, None, false);
        let parsed = JsonValue::from(parse_json(&payload).expect("valid json"));

        assert!(parsed.get("seed").is_none());
        assert!(parsed.get("stream").is_none());
        assert_eq!(
            parsed.get("temperature").and_then(JsonValue::as_f64),
            Some(0.0)
        );
    }

    #[test]
    fn openai_prompt_payload_enables_stream_options() {
        let payload =
            build_openai_prompt_payload("gpt-4.1-mini", "hello", Some(0.0), None, None, true);
        let parsed = JsonValue::from(parse_json(&payload).expect("valid json"));

        assert_eq!(
            parsed.get("stream").and_then(JsonValue::as_bool),
            Some(true)
        );
        assert_eq!(
            parsed
                .get("stream_options")
                .and_then(|value| value.get("include_usage"))
                .and_then(JsonValue::as_bool),
            Some(true)
        );
    }

    #[test]
    fn openai_streaming_prompt_response_collects_delta_chunks() {
        let body = concat!(
            "data: {\"model\":\"gpt-test\",\"choices\":[{\"delta\":{\"content\":\"login \"},\"finish_reason\":null}]}\n\n",
            "data: {\"model\":\"gpt-test\",\"choices\":[{\"delta\":{\"content\":\"failed\"},\"finish_reason\":null}]}\n\n",
            "data: {\"model\":\"gpt-test\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":2,\"total_tokens\":14}}\n\n",
            "data: [DONE]\n\n",
        );
        let parsed = parse_openai_streaming_prompt_response(body, "fallback").unwrap();

        assert_eq!(parsed.model, "gpt-test");
        assert_eq!(parsed.output_text, "login failed");
        assert_eq!(
            parsed.output_chunks.as_deref(),
            Some(["login ".to_string(), "failed".to_string()].as_slice())
        );
        assert_eq!(parsed.prompt_tokens, Some(12));
        assert_eq!(parsed.completion_tokens, Some(2));
        assert_eq!(parsed.total_tokens, Some(14));
        assert_eq!(parsed.stop_reason.as_deref(), Some("stop"));
    }

    #[tokio::test]
    async fn openai_prompt_async_rejects_empty_model() {
        let transport = crate::runtime::ai::transport::AiTransport::new(Default::default());
        let request = OpenAiPromptRequest {
            api_key: "key".to_string(),
            model: "  ".to_string(),
            prompt: "hello".to_string(),
            temperature: None,
            seed: None,
            max_output_tokens: None,
            api_base: "https://api.openai.com/v1".to_string(),
            stream: false,
        };
        let err = openai_prompt_async(&transport, request).await.unwrap_err();
        assert!(err.to_string().contains("model cannot be empty"));
    }

    #[tokio::test]
    async fn openai_prompt_async_rejects_empty_prompt() {
        let transport = crate::runtime::ai::transport::AiTransport::new(Default::default());
        let request = OpenAiPromptRequest {
            api_key: "key".to_string(),
            model: "gpt-4.1-mini".to_string(),
            prompt: "".to_string(),
            temperature: None,
            seed: None,
            max_output_tokens: None,
            api_base: "https://api.openai.com/v1".to_string(),
            stream: false,
        };
        let err = openai_prompt_async(&transport, request).await.unwrap_err();
        assert!(err.to_string().contains("prompt cannot be empty"));
    }

    // ========================================================================
    // openai-compat client tests (issue gh-516)
    //
    // Each test spins up a tiny TCP server, hands its base URL to the
    // new generic client, and asserts on the captured request +
    // synthesised response. Tests run in parallel-safe fashion (each
    // server binds to port 0).
    // ========================================================================

    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    struct CapturedRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    fn parse_http_request(stream: &mut std::net::TcpStream) -> CapturedRequest {
        let mut buf = [0u8; 8192];
        let mut data = Vec::new();
        loop {
            let read = stream.read(&mut buf).unwrap_or(0);
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buf[..read]);
            if let Some(idx) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                let header_len = idx + 4;
                let header_str = String::from_utf8_lossy(&data[..idx]).to_string();
                let mut lines = header_str.split("\r\n");
                let request_line = lines.next().unwrap_or("");
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut headers = Vec::new();
                let mut content_length: usize = 0;
                for line in lines {
                    if let Some((k, v)) = line.split_once(':') {
                        let k = k.trim().to_string();
                        let v = v.trim().to_string();
                        if k.eq_ignore_ascii_case("content-length") {
                            content_length = v.parse().unwrap_or(0);
                        }
                        headers.push((k, v));
                    }
                }
                while data.len() < header_len + content_length {
                    let read = stream.read(&mut buf).unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..read]);
                }
                let body =
                    String::from_utf8_lossy(&data[header_len..header_len + content_length])
                        .to_string();
                return CapturedRequest {
                    method,
                    path,
                    headers,
                    body,
                };
            }
        }
        CapturedRequest {
            method: String::new(),
            path: String::new(),
            headers: Vec::new(),
            body: String::new(),
        }
    }

    /// Spawn a one-shot HTTP server that replies with `(status, body)`
    /// to a single request, captures it, and returns `(base_url, captured)`.
    fn spawn_mock(
        status: u16,
        response_body: &'static str,
    ) -> (String, Arc<Mutex<Option<CapturedRequest>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let captured: Arc<Mutex<Option<CapturedRequest>>> = Arc::new(Mutex::new(None));
        let captured_clone = Arc::clone(&captured);
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let req = parse_http_request(&mut stream);
                *captured_clone.lock().unwrap() = Some(req);
                let status_line = match status {
                    200 => "200 OK",
                    400 => "400 Bad Request",
                    401 => "401 Unauthorized",
                    500 => "500 Internal Server Error",
                    _ => "200 OK",
                };
                let resp = format!(
                    "HTTP/1.1 {status_line}\r\n\
                     Content-Type: application/json\r\n\
                     Content-Length: {}\r\n\
                     Connection: close\r\n\r\n{}",
                    response_body.len(),
                    response_body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        (format!("http://{}", addr), captured)
    }

    #[test]
    fn openai_compat_chat_roundtrip_honors_arbitrary_api_base_and_headers() {
        let body = r#"{
            "id":"chatcmpl_x",
            "model":"custom-model",
            "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"hi"}}],
            "usage":{"prompt_tokens":7,"completion_tokens":2,"total_tokens":9}
        }"#;
        let (base, captured) = spawn_mock(200, body);

        let req = OpenAiCompatChatRequest {
            api_base: base.clone(),
            api_key: "sk-test".to_string(),
            model: "custom-model".to_string(),
            prompt: "say hi".to_string(),
            temperature: None,
            seed: None,
            max_output_tokens: None,
            extra_headers: vec![("X-Custom-Tag".to_string(), "abc".to_string())],
        };
        let resp = openai_compat_chat(req).expect("ok");

        assert_eq!(resp.output_text, "hi");
        assert_eq!(resp.model, "custom-model");
        assert_eq!(resp.usage.input_tokens, Some(7));
        assert_eq!(resp.usage.output_tokens, Some(2));
        assert_eq!(resp.usage.total_tokens, Some(9));
        assert_eq!(resp.stop_reason.as_deref(), Some("stop"));

        let cap = captured.lock().unwrap().take().expect("captured");
        assert_eq!(cap.method, "POST");
        assert_eq!(cap.path, "/chat/completions");
        let has_auth = cap
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("authorization") && v == "Bearer sk-test");
        assert!(has_auth, "Authorization header missing");
        let has_custom = cap
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("x-custom-tag") && v == "abc");
        assert!(has_custom, "extra header missing");
        assert!(cap.body.contains("\"model\":\"custom-model\""));
    }

    #[test]
    fn openai_compat_embeddings_roundtrip_with_dimensions() {
        let body = r#"{
            "object":"list",
            "model":"embed-model",
            "data":[{"object":"embedding","index":0,"embedding":[0.5,0.25]}],
            "usage":{"prompt_tokens":4,"total_tokens":4}
        }"#;
        let (base, captured) = spawn_mock(200, body);

        let req = OpenAiCompatEmbeddingsRequest {
            api_base: base,
            api_key: "sk-emb".to_string(),
            model: "embed-model".to_string(),
            inputs: vec!["hello".to_string()],
            dimensions: Some(2),
            extra_headers: vec![],
        };
        let resp = openai_compat_embeddings(req).expect("ok");

        assert_eq!(resp.embeddings.len(), 1);
        assert_eq!(resp.embeddings[0], vec![0.5_f32, 0.25_f32]);
        assert_eq!(resp.usage.total_tokens, Some(4));
        assert_eq!(resp.usage.input_tokens, Some(4));

        let cap = captured.lock().unwrap().take().expect("captured");
        assert_eq!(cap.path, "/embeddings");
        assert!(cap.body.contains("\"dimensions\":2"));
    }

    #[test]
    fn openai_compat_chat_non_2xx_returns_structured_error() {
        let body = r#"{"error":{"message":"bad api key","type":"invalid_request_error"}}"#;
        let (base, _captured) = spawn_mock(401, body);

        let req = OpenAiCompatChatRequest {
            api_base: base,
            api_key: "bad".to_string(),
            model: "m".to_string(),
            prompt: "hi".to_string(),
            temperature: None,
            seed: None,
            max_output_tokens: None,
            extra_headers: vec![],
        };
        let err = openai_compat_chat(req).unwrap_err().to_string();
        assert!(err.contains("status 401"), "got: {err}");
        assert!(err.contains("bad api key"), "got: {err}");
    }

    #[test]
    fn openai_compat_chat_rejects_empty_model_and_prompt() {
        let req = OpenAiCompatChatRequest {
            api_base: "http://localhost:1".to_string(),
            api_key: "k".to_string(),
            model: "  ".to_string(),
            prompt: "hi".to_string(),
            temperature: None,
            seed: None,
            max_output_tokens: None,
            extra_headers: vec![],
        };
        let err = openai_compat_chat(req).unwrap_err().to_string();
        assert!(err.contains("model cannot be empty"), "got: {err}");

        let req = OpenAiCompatChatRequest {
            api_base: "http://localhost:1".to_string(),
            api_key: "k".to_string(),
            model: "m".to_string(),
            prompt: "  ".to_string(),
            temperature: None,
            seed: None,
            max_output_tokens: None,
            extra_headers: vec![],
        };
        let err = openai_compat_chat(req).unwrap_err().to_string();
        assert!(err.contains("prompt cannot be empty"), "got: {err}");
    }

    #[test]
    fn parse_provider_mode_recognizes_all_three_tokens() {
        assert_eq!(
            parse_provider_mode("openai-compat"),
            Some(AiProviderMode::OpenAiCompat)
        );
        assert_eq!(
            parse_provider_mode("OPENAI_NATIVE"),
            Some(AiProviderMode::OpenAiNative)
        );
        assert_eq!(
            parse_provider_mode("anthropic-native"),
            Some(AiProviderMode::AnthropicNative)
        );
        assert_eq!(parse_provider_mode("groq"), None);
    }

    #[test]
    fn resolve_provider_mode_reads_kv_key() {
        let kv = |key: &str| -> crate::RedDBResult<Option<String>> {
            if key == "red.config.ai.provider" {
                Ok(Some("anthropic-native".to_string()))
            } else {
                Ok(None)
            }
        };
        assert_eq!(
            resolve_provider_mode(&kv),
            Some(AiProviderMode::AnthropicNative)
        );
    }

    #[test]
    fn resolve_default_provider_honors_mode_key() {
        let kv = |key: &str| -> crate::RedDBResult<Option<String>> {
            match key {
                "red.config.ai.provider" => Ok(Some("anthropic-native".to_string())),
                "red.config.ai.default.provider" => Ok(Some("groq".to_string())),
                _ => Ok(None),
            }
        };
        assert_eq!(resolve_default_provider(&kv), AiProvider::Anthropic);
    }

    #[tokio::test]
    async fn anthropic_prompt_async_rejects_empty_api_key() {
        let transport = crate::runtime::ai::transport::AiTransport::new(Default::default());
        let request = AnthropicPromptRequest {
            api_key: "  ".to_string(),
            model: "claude-3-5-haiku-latest".to_string(),
            prompt: "hello".to_string(),
            temperature: None,
            max_output_tokens: None,
            api_base: "https://api.anthropic.com/v1".to_string(),
            anthropic_version: DEFAULT_ANTHROPIC_VERSION.to_string(),
        };
        let err = anthropic_prompt_async(&transport, request)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("API key cannot be empty"));
    }
}

// ============================================================================
// Provider & Credential Resolution (shared between HTTP, gRPC, and runtime)
// ============================================================================

/// AI provider identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AiProvider {
    OpenAi,
    Anthropic,
    Groq,
    OpenRouter,
    Together,
    Venice,
    Ollama,
    DeepSeek,
    HuggingFace,
    Local,
    Custom(String),
}

impl AiProvider {
    pub fn token(&self) -> &str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Groq => "groq",
            Self::OpenRouter => "openrouter",
            Self::Together => "together",
            Self::Venice => "venice",
            Self::Ollama => "ollama",
            Self::DeepSeek => "deepseek",
            Self::HuggingFace => "huggingface",
            Self::Local => "local",
            Self::Custom(name) => name.as_str(),
        }
    }

    pub fn default_prompt_model(&self) -> &str {
        match self {
            Self::OpenAi => DEFAULT_OPENAI_PROMPT_MODEL,
            Self::Anthropic => DEFAULT_ANTHROPIC_PROMPT_MODEL,
            Self::Groq => "llama-3.3-70b-versatile",
            Self::OpenRouter => "auto",
            Self::Together => "meta-llama/Meta-Llama-3-8B-Instruct",
            Self::Venice => "llama-3.3-70b",
            Self::Ollama => "llama3",
            Self::DeepSeek => "deepseek-chat",
            Self::HuggingFace => "mistralai/Mistral-7B-Instruct-v0.3",
            Self::Local => "sentence-transformers/all-MiniLM-L6-v2",
            Self::Custom(_) => DEFAULT_OPENAI_PROMPT_MODEL,
        }
    }

    pub fn prompt_model_env_name(&self) -> String {
        format!("REDDB_{}_PROMPT_MODEL", self.token().to_ascii_uppercase())
    }

    pub fn default_embedding_model(&self) -> &str {
        match self {
            Self::Ollama => "nomic-embed-text",
            Self::HuggingFace | Self::Local => "sentence-transformers/all-MiniLM-L6-v2",
            _ => DEFAULT_OPENAI_EMBEDDING_MODEL,
        }
    }

    pub fn default_api_base(&self) -> &str {
        match self {
            Self::OpenAi => DEFAULT_OPENAI_API_BASE,
            Self::Anthropic => DEFAULT_ANTHROPIC_API_BASE,
            Self::Groq => "https://api.groq.com/openai/v1",
            Self::OpenRouter => "https://openrouter.ai/api/v1",
            Self::Together => "https://api.together.xyz/v1",
            Self::Venice => "https://api.venice.ai/api/v1",
            Self::Ollama => "http://localhost:11434/v1",
            Self::DeepSeek => "https://api.deepseek.com/v1",
            Self::HuggingFace => "https://api-inference.huggingface.co",
            Self::Local => "local",
            Self::Custom(base) => base.as_str(),
        }
    }

    pub fn api_base_env_name(&self) -> String {
        format!("REDDB_{}_API_BASE", self.token().to_ascii_uppercase())
    }

    pub fn default_key_env_name(&self) -> String {
        format!("REDDB_{}_API_KEY", self.token().to_ascii_uppercase())
    }

    pub fn alias_key_env_name(&self, alias: &str) -> String {
        let normalized = normalize_alias_token(alias);
        format!(
            "REDDB_{}_API_KEY_{normalized}",
            self.token().to_ascii_uppercase()
        )
    }

    pub fn resolve_api_base(&self) -> String {
        if let Ok(value) = std::env::var(self.api_base_env_name()) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return value;
            }
        }
        self.default_api_base().to_string()
    }

    /// Resolve API base URL checking KV store too (for custom base_url config).
    pub fn resolve_api_base_with_kv<F>(&self, alias: &str, kv_getter: &F) -> String
    where
        F: Fn(&str) -> crate::RedDBResult<Option<String>>,
    {
        // 1. Env var
        if let Ok(value) = std::env::var(self.api_base_env_name()) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return value;
            }
        }
        // 2. KV store: {provider}/{alias}/base_url
        let kv_key = format!("red.config.ai.{}.{alias}.base_url", self.token());
        if let Ok(Some(value)) = kv_getter(&kv_key) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return value;
            }
        }
        self.default_api_base().to_string()
    }

    /// Whether this provider uses the OpenAI-compatible API format.
    pub fn is_openai_compatible(&self) -> bool {
        matches!(
            self,
            Self::OpenAi
                | Self::Groq
                | Self::OpenRouter
                | Self::Together
                | Self::Venice
                | Self::Ollama
                | Self::DeepSeek
                | Self::Custom(_)
        )
    }

    /// Whether this provider requires an API key (Ollama/Local don't).
    pub fn requires_api_key(&self) -> bool {
        !matches!(self, Self::Ollama | Self::Local)
    }
}

/// Parse a provider string into AiProvider.
pub fn parse_provider(name: &str) -> crate::RedDBResult<AiProvider> {
    match name.trim().to_ascii_lowercase().as_str() {
        "openai" => Ok(AiProvider::OpenAi),
        "anthropic" => Ok(AiProvider::Anthropic),
        "groq" => Ok(AiProvider::Groq),
        "openrouter" | "open_router" => Ok(AiProvider::OpenRouter),
        "together" => Ok(AiProvider::Together),
        "venice" => Ok(AiProvider::Venice),
        "ollama" => Ok(AiProvider::Ollama),
        "deepseek" | "deep_seek" => Ok(AiProvider::DeepSeek),
        "huggingface" | "hf" => Ok(AiProvider::HuggingFace),
        "local" => Ok(AiProvider::Local),
        other => {
            // Treat as custom provider if it looks like a URL
            if other.starts_with("http://") || other.starts_with("https://") {
                Ok(AiProvider::Custom(other.to_string()))
            } else {
                Err(crate::RedDBError::Query(format!(
                    "unsupported AI provider '{other}'; expected: openai, anthropic, groq, \
                     openrouter, together, venice, ollama, deepseek, huggingface, local"
                )))
            }
        }
    }
}

/// Resolve the default AI provider. Checks:
/// 1. `REDDB_AI_PROVIDER` env var
/// 2. `red_config` KV key `red.config.ai.default.provider`
/// 3. Falls back to OpenAI
pub fn resolve_default_provider<F>(kv_getter: &F) -> AiProvider
where
    F: Fn(&str) -> crate::RedDBResult<Option<String>>,
{
    // 0. New mode selector (red.config.ai.provider) takes precedence
    //    when explicitly set — it picks the wire-protocol family.
    if let Some(mode) = resolve_provider_mode(kv_getter) {
        return provider_mode_to_provider(mode);
    }
    // 1. Env var
    if let Ok(value) = std::env::var("REDDB_AI_PROVIDER") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            if let Ok(provider) = parse_provider(&value) {
                return provider;
            }
        }
    }
    // 2. KV store
    if let Ok(Some(value)) = kv_getter("red.config.ai.default.provider") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            if let Ok(provider) = parse_provider(&value) {
                return provider;
            }
        }
    }
    AiProvider::OpenAi
}

/// Resolve the default AI model. Checks:
/// 1. `REDDB_AI_MODEL` env var
/// 2. `red_config` KV key `red.config.ai.default.model`
/// 3. Falls back to provider's default
pub fn resolve_default_model<F>(provider: &AiProvider, kv_getter: &F) -> String
where
    F: Fn(&str) -> crate::RedDBResult<Option<String>>,
{
    // 1. Env var
    if let Ok(value) = std::env::var("REDDB_AI_MODEL") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return value;
        }
    }
    // 2. Provider-specific env var
    if let Ok(value) = std::env::var(provider.prompt_model_env_name()) {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return value;
        }
    }
    // 3. KV store
    if let Ok(Some(value)) = kv_getter("red.config.ai.default.model") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return value;
        }
    }
    provider.default_prompt_model().to_string()
}

/// Resolve default provider + model from runtime KV store.
pub fn resolve_defaults_from_runtime(
    runtime: &crate::runtime::RedDBRuntime,
) -> (AiProvider, String) {
    use crate::application::ports::RuntimeEntityPort;
    let kv_getter = |key: &str| -> crate::RedDBResult<Option<String>> {
        match runtime.get_kv("red_config", key)? {
            Some((crate::storage::schema::Value::Text(s), _)) => Ok(Some(s.to_string())),
            _ => Ok(None),
        }
    };
    let provider = resolve_default_provider(&kv_getter);
    let model = resolve_default_model(&provider, &kv_getter);
    (provider, model)
}

/// Resolve default provider + model via RuntimeEntityPort trait (for use in QueryUseCases).
pub fn resolve_defaults_from_runtime_port<
    P: crate::application::ports::RuntimeEntityPort + ?Sized,
>(
    runtime: &P,
) -> (AiProvider, String) {
    let kv_getter = |key: &str| -> crate::RedDBResult<Option<String>> {
        match runtime.get_kv("red_config", key)? {
            Some((crate::storage::schema::Value::Text(s), _)) => Ok(Some(s.to_string())),
            _ => Ok(None),
        }
    };
    let provider = resolve_default_provider(&kv_getter);
    let model = resolve_default_model(&provider, &kv_getter);
    (provider, model)
}

/// Resolve an API key for a provider. Uses the chain:
/// 1. Environment variable with alias: `REDDB_OPENAI_API_KEY_{ALIAS}`
/// 2. Vault secret lookup via `kv_getter` closure
/// 3. Legacy KV store lookup via `kv_getter` closure
/// 4. Default environment variable: `REDDB_OPENAI_API_KEY`
///
/// `kv_getter` receives either a `red.secret.*` path or a legacy `red.config.*`
/// key and returns the value if found.
pub fn resolve_api_key<F>(
    provider: &AiProvider,
    credential_alias: Option<&str>,
    kv_getter: F,
) -> crate::RedDBResult<String>
where
    F: Fn(&str) -> crate::RedDBResult<Option<String>>,
{
    // Providers that don't require API keys
    if !provider.requires_api_key() {
        // Still try to find a key (user may have one for auth'd Ollama)
        if let Ok(value) = std::env::var(provider.default_key_env_name()) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return Ok(value);
            }
        }
        return Ok(String::new());
    }

    if let Some(alias) = credential_alias.map(str::trim).filter(|a| !a.is_empty()) {
        // Try env var with alias
        if let Some(key) = resolve_key_from_env_alias(provider, alias) {
            return Ok(key);
        }
        if let Some(key) = kv_getter(&ai_api_secret_path(provider, alias))? {
            if !key.trim().is_empty() {
                return Ok(key);
            }
        }
        if let Some(secret_ref) = kv_getter(&ai_api_secret_ref_config_key(provider, alias))? {
            if let Some(key) = kv_getter(secret_ref.trim())? {
                if !key.trim().is_empty() {
                    return Ok(key);
                }
            }
        }
        let legacy_key = ai_api_legacy_config_key(provider, alias);
        if let Some(key) = kv_getter(&legacy_key)? {
            if !key.trim().is_empty() {
                return Ok(key);
            }
        }
        return Err(crate::RedDBError::Query(format!(
            "credential '{alias}' not found for {}. Set env {} or store it in the vault",
            provider.token(),
            provider.alias_key_env_name(alias)
        )));
    }

    // Default env var
    if let Ok(value) = std::env::var(provider.default_key_env_name()) {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Ok(value);
        }
    }

    if let Some(key) = kv_getter(&ai_api_secret_path(provider, "default"))? {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }
    if let Some(secret_ref) = kv_getter(&ai_api_secret_ref_config_key(provider, "default"))? {
        if let Some(key) = kv_getter(secret_ref.trim())? {
            if !key.trim().is_empty() {
                return Ok(key);
            }
        }
    }
    if let Some(key) = kv_getter(&ai_api_legacy_config_key(provider, "default"))? {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }

    let legacy_short_key = format!("{}/default", provider.token());
    if let Some(key) = kv_getter(&legacy_short_key)? {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }

    Err(crate::RedDBError::Query(format!(
        "missing {} API key. Set {} or provide credential alias",
        provider.token(),
        provider.default_key_env_name()
    )))
}

pub fn ai_api_secret_path(provider: &AiProvider, alias: &str) -> String {
    format!(
        "red.secret.ai.{}.{}.api_key",
        provider.token(),
        normalize_credential_alias_path(alias)
    )
}

pub fn ai_api_secret_ref_config_key(provider: &AiProvider, alias: &str) -> String {
    format!(
        "red.config.ai.{}.{}.secret_ref",
        provider.token(),
        normalize_credential_alias_path(alias)
    )
}

pub fn ai_api_legacy_config_key(provider: &AiProvider, alias: &str) -> String {
    format!(
        "red.config.ai.{}.{}.key",
        provider.token(),
        normalize_credential_alias_path(alias)
    )
}

fn normalize_credential_alias_path(alias: &str) -> String {
    let alias = alias.trim();
    if alias.is_empty() {
        "default".to_string()
    } else {
        alias.to_ascii_lowercase()
    }
}

fn resolve_key_from_env_alias(provider: &AiProvider, alias: &str) -> Option<String> {
    let env_name = provider.alias_key_env_name(alias);
    std::env::var(env_name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn normalize_alias_token(alias: &str) -> String {
    let mut out = String::with_capacity(alias.len());
    for character in alias.chars() {
        if character.is_ascii_alphanumeric() {
            out.push(character.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    out.trim_matches('_').to_string()
}

/// Convenience: resolve API key using a RedDBRuntime's KV store.
pub fn resolve_api_key_from_runtime(
    provider: &AiProvider,
    credential_alias: Option<&str>,
    runtime: &crate::runtime::RedDBRuntime,
) -> crate::RedDBResult<String> {
    use crate::application::ports::RuntimeEntityPort;
    resolve_api_key(provider, credential_alias, |kv_key| {
        if kv_key.starts_with("red.secret.") {
            return Ok(runtime.vault_kv_get(kv_key));
        }
        match runtime.get_kv("red_config", kv_key)? {
            Some((crate::storage::schema::Value::Text(secret), _)) => Ok(Some(secret.to_string())),
            Some(_) => Ok(None),
            None => Ok(None),
        }
    })
}

// ============================================================================
// HuggingFace Inference API
// ============================================================================

/// Generate embeddings via HuggingFace Inference API.
pub fn huggingface_embeddings(
    api_key: &str,
    model: &str,
    inputs: &[String],
    api_base: &str,
) -> crate::RedDBResult<OpenAiEmbeddingResponse> {
    let url = format!("{api_base}/pipeline/feature-extraction/{model}");
    let mut embeddings = Vec::with_capacity(inputs.len());

    for input in inputs {
        let payload = crate::serde_json::json!({ "inputs": input }).to_string_compact();
        let (status, body_str) = http_post_json(&url, api_key, &[], payload, 90)
            .map_err(|e| crate::RedDBError::Query(format!("HuggingFace API error: {e}")))?;
        if !(200..300).contains(&status) {
            return Err(crate::RedDBError::Query(format!(
                "HuggingFace API error (status {status}): {body_str}"
            )));
        }
        let body: JsonValue = crate::serde_json::from_str(&body_str).map_err(|e| {
            crate::RedDBError::Query(format!("HuggingFace response parse error: {e}"))
        })?;

        // HF returns [[f32, ...]] for single input
        let vector: Vec<f32> = match &body {
            JsonValue::Array(outer) => outer
                .iter()
                .filter_map(|v| v.as_f64().map(|n| n as f32))
                .collect(),
            _ => {
                return Err(crate::RedDBError::Query(
                    "unexpected HuggingFace embedding response format".to_string(),
                ))
            }
        };
        embeddings.push(vector);
    }

    Ok(OpenAiEmbeddingResponse {
        provider: "huggingface",
        model: model.to_string(),
        embeddings,
        prompt_tokens: None,
        total_tokens: None,
    })
}

/// Generate text via HuggingFace Inference API.
pub fn huggingface_prompt(
    api_key: &str,
    model: &str,
    prompt: &str,
    temperature: Option<f32>,
    max_tokens: Option<usize>,
    api_base: &str,
) -> crate::RedDBResult<AiPromptResponse> {
    let url = format!("{api_base}/models/{model}");
    let mut params = Map::new();
    if let Some(t) = temperature {
        params.insert("temperature".into(), JsonValue::Number(t as f64));
    }
    params.insert(
        "max_new_tokens".into(),
        JsonValue::Number(max_tokens.unwrap_or(512) as f64),
    );
    let payload = crate::serde_json::json!({
        "inputs": prompt,
        "parameters": JsonValue::Object(params)
    });

    let (status, body_str) =
        http_post_json(&url, api_key, &[], payload.to_string_compact(), 120)
            .map_err(|e| crate::RedDBError::Query(format!("HuggingFace API error: {e}")))?;
    if !(200..300).contains(&status) {
        return Err(crate::RedDBError::Query(format!(
            "HuggingFace API error (status {status}): {body_str}"
        )));
    }
    let body: JsonValue = crate::serde_json::from_str(&body_str)
        .map_err(|e| crate::RedDBError::Query(format!("HuggingFace response parse error: {e}")))?;

    let output_text = match &body {
        JsonValue::Array(arr) => arr
            .first()
            .and_then(|v| v.get("generated_text"))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string(),
        _ => body
            .get("generated_text")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string(),
    };

    Ok(AiPromptResponse {
        provider: "huggingface",
        model: model.to_string(),
        output_text,
        output_chunks: None,
        prompt_tokens: None,
        completion_tokens: None,
        total_tokens: None,
        stop_reason: None,
    })
}

// ============================================================================
// Local model stubs (requires 'local-models' feature flag)
// ============================================================================

/// Local embedding via candle — requires `local-models` feature.
pub fn local_embeddings(
    _model_id: &str,
    _texts: &[String],
) -> crate::RedDBResult<OpenAiEmbeddingResponse> {
    Err(crate::RedDBError::FeatureNotEnabled(
        "local model inference requires the 'local-models' feature flag. \
         Build with: cargo build --features local-models. \
         Alternatively, use 'ollama' provider with a local Ollama server."
            .to_string(),
    ))
}

/// Local prompt via candle — requires `local-models` feature.
pub fn local_prompt(_model_id: &str, _prompt: &str) -> crate::RedDBResult<AiPromptResponse> {
    Err(crate::RedDBError::FeatureNotEnabled(
        "local model inference requires the 'local-models' feature flag. \
         Build with: cargo build --features local-models. \
         Alternatively, use 'ollama' provider with a local Ollama server."
            .to_string(),
    ))
}

// ============================================================================
// gRPC input collection — parity with HTTP /ai/embeddings
// ============================================================================

/// Collect embedding inputs from any of the three supported shapes.
///
/// * `input: "..."` — single string.
/// * `inputs: ["...", ...]` — array of strings.
/// * `source_query: "SELECT ..."` — runs a SQL query and projects
///   either the named `source_field` from each row (source_mode =
///   "row", default) or every string cell of every result row
///   (source_mode = "result").
fn grpc_collect_embedding_inputs(
    runtime: &crate::runtime::RedDBRuntime,
    payload: &JsonValue,
) -> crate::RedDBResult<Vec<String>> {
    if let Some(source_query) = payload
        .get("source_query")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return grpc_collect_inputs_from_source_query(runtime, payload, source_query);
    }

    if let Some(arr) = payload.get("inputs").and_then(|v| v.as_array()) {
        let mut out = Vec::with_capacity(arr.len());
        for (idx, v) in arr.iter().enumerate() {
            let text = v.as_str().ok_or_else(|| {
                crate::RedDBError::Query(format!("field 'inputs[{idx}]' must be a string"))
            })?;
            if text.trim().is_empty() {
                return Err(crate::RedDBError::Query(format!(
                    "field 'inputs[{idx}]' cannot be empty"
                )));
            }
            out.push(text.to_string());
        }
        if out.is_empty() {
            return Err(crate::RedDBError::Query(
                "field 'inputs' must be a non-empty array of strings".to_string(),
            ));
        }
        return Ok(out);
    }

    if let Some(single) = payload
        .get("input")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(vec![single.to_string()]);
    }

    Err(crate::RedDBError::Query(
        "provide either 'input', 'inputs', or 'source_query'".to_string(),
    ))
}

fn grpc_collect_inputs_from_source_query(
    runtime: &crate::runtime::RedDBRuntime,
    payload: &JsonValue,
    source_query: &str,
) -> crate::RedDBResult<Vec<String>> {
    let result = runtime
        .execute_query(source_query)
        .map_err(|err| crate::RedDBError::Query(format!("source_query failed: {err}")))?;

    let source_mode = payload
        .get("source_mode")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("row")
        .to_ascii_lowercase();

    let mut out: Vec<String> = Vec::new();
    match source_mode.as_str() {
        "row" => {
            let field = payload
                .get("source_field")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    crate::RedDBError::Query(
                        "field 'source_field' is required when source_mode='row'".to_string(),
                    )
                })?;
            for rec in &result.result.records {
                for (key, value) in rec.iter_fields() {
                    if key.as_ref() == field {
                        if let crate::storage::schema::Value::Text(text) = value {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                out.push(trimmed.to_string());
                            }
                        }
                    }
                }
            }
        }
        "result" => {
            for rec in &result.result.records {
                for (_, value) in rec.iter_fields() {
                    if let crate::storage::schema::Value::Text(text) = value {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            out.push(trimmed.to_string());
                        }
                    }
                }
            }
        }
        other => {
            return Err(crate::RedDBError::Query(format!(
                "field 'source_mode' must be 'row' or 'result' (got '{other}')"
            )));
        }
    }

    if out.is_empty() {
        return Err(crate::RedDBError::Query(
            "source_query produced zero non-empty text inputs".to_string(),
        ));
    }
    Ok(out)
}

// ============================================================================
// gRPC stubs — delegate to the same logic as HTTP handlers
// ============================================================================

/// gRPC embeddings — shared entrypoint that mirrors the HTTP handler.
///
/// Accepts the same JSON payload shape as `POST /ai/embeddings`:
///
/// ```json
/// { "provider": "openai", "model": "text-embedding-3-small",
///   "inputs": ["hello", "world"], "credential": "optional-alias" }
/// ```
///
/// Input shapes at parity with HTTP: `input` (single string),
/// `inputs` (array of strings), and `source_query` (SQL that the
/// runtime executes to materialise the input texts; `source_mode`
/// = `row` needs `source_collection` + `source_field`, `result`
/// uses the projected columns). Returns a JSON object with
/// `provider`, `model`, `embeddings`, `prompt_tokens`,
/// `total_tokens`. Non-OpenAI-compatible providers are rejected
/// with a clear message, matching the HTTP handler.
pub fn grpc_embeddings(
    runtime: &crate::runtime::RedDBRuntime,
    payload: &JsonValue,
) -> crate::RedDBResult<JsonValue> {
    let provider_name = payload
        .get("provider")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("openai");
    let provider = parse_provider(provider_name)?;
    // Routing matrix mirrors `handle_ai_embeddings`. See that function
    // for the rationale; in short: HuggingFace gets its own wire
    // shape, Anthropic fails fast (no embeddings product), and Local
    // requires a build-time feature flag.
    match &provider {
        AiProvider::Anthropic => {
            return Err(crate::RedDBError::Query(
                "Anthropic does not offer an embeddings API. \
                 Re-issue the request against an OpenAI-compatible \
                 provider (openai, groq, ollama, openrouter, together, \
                 venice, deepseek), HuggingFace, or a custom base URL — \
                 RedDB does not silently route embeddings to a \
                 different provider than the one you named."
                    .to_string(),
            ));
        }
        AiProvider::Local => {
            return Err(crate::RedDBError::Query(
                "Local embeddings require the `local-models` feature \
                 flag at engine build time."
                    .to_string(),
            ));
        }
        _ => {}
    }

    let inputs: Vec<String> = grpc_collect_embedding_inputs(runtime, payload)?;

    let model = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            std::env::var(format!(
                "REDDB_{}_EMBEDDING_MODEL",
                provider.token().to_ascii_uppercase()
            ))
            .ok()
        })
        .or_else(|| std::env::var("REDDB_OPENAI_EMBEDDING_MODEL").ok())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| provider.default_embedding_model().to_string());

    let credential = payload
        .get("credential")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let api_key = resolve_api_key_from_runtime(&provider, credential.as_deref(), runtime)?;

    let dimensions = payload
        .get("dimensions")
        .and_then(|v| v.as_i64())
        .and_then(|v| usize::try_from(v).ok())
        .filter(|v| *v > 0);

    let response = match &provider {
        AiProvider::HuggingFace => {
            huggingface_embeddings(&api_key, &model, &inputs, &provider.resolve_api_base())?
        }
        _ => {
            let transport = crate::runtime::ai::transport::AiTransport::from_runtime(runtime);
            let request = OpenAiEmbeddingRequest {
                api_key,
                model,
                inputs,
                dimensions,
                api_base: provider.resolve_api_base(),
            };
            crate::runtime::ai::block_on_ai(async move {
                openai_embeddings_async(&transport, request).await
            })
            .and_then(|result| result)?
        }
    };

    let embeddings_json: Vec<JsonValue> = response
        .embeddings
        .into_iter()
        .map(|vec| {
            JsonValue::Array(
                vec.into_iter()
                    .map(|f| JsonValue::Number(f as f64))
                    .collect(),
            )
        })
        .collect();

    let mut obj = Map::new();
    obj.insert(
        "provider".to_string(),
        JsonValue::String(response.provider.to_string()),
    );
    obj.insert("model".to_string(), JsonValue::String(response.model));
    obj.insert("embeddings".to_string(), JsonValue::Array(embeddings_json));
    if let Some(pt) = response.prompt_tokens {
        obj.insert("prompt_tokens".to_string(), JsonValue::Number(pt as f64));
    }
    if let Some(tt) = response.total_tokens {
        obj.insert("total_tokens".to_string(), JsonValue::Number(tt as f64));
    }
    Ok(JsonValue::Object(obj))
}

/// gRPC stub for AI prompt.
pub fn grpc_prompt(
    _runtime: &crate::runtime::RedDBRuntime,
    _payload: &JsonValue,
) -> crate::RedDBResult<JsonValue> {
    Err(crate::RedDBError::FeatureNotEnabled(
        "AI prompt via gRPC requires HTTP endpoint; use POST /ai/prompt".to_string(),
    ))
}

/// gRPC stub for AI credentials.
pub fn grpc_credentials(
    _runtime: &crate::runtime::RedDBRuntime,
    _payload: &JsonValue,
) -> crate::RedDBResult<JsonValue> {
    Err(crate::RedDBError::FeatureNotEnabled(
        "AI credentials via gRPC requires HTTP endpoint; use POST /ai/credentials".to_string(),
    ))
}

// ============================================================================
// Generic OpenAI-compatible client (issue gh-516)
//
// Thin blocking client that targets any `{api_base}/chat/completions`
// and `{api_base}/embeddings` endpoint with arbitrary auth headers.
// Existing vendor-native paths (`openai_prompt_async`,
// `anthropic_prompt_async`) remain unchanged; this exists so callers
// can talk to non-OpenAI providers that expose an OpenAI-compatible
// surface (Groq, OpenRouter, Together, Ollama, vLLM, LM Studio, ...)
// without having to register a new `AiProvider` variant.
// ============================================================================

/// Normalized usage block. Field names follow the Anthropic shape
/// (`input_tokens` / `output_tokens`) so downstream cost-accounting
/// has one canonical schema regardless of the upstream provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenAiCompatUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatChatRequest {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    pub prompt: String,
    pub temperature: Option<f32>,
    pub seed: Option<u64>,
    pub max_output_tokens: Option<usize>,
    pub extra_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatChatResponse {
    pub model: String,
    pub output_text: String,
    pub stop_reason: Option<String>,
    pub usage: OpenAiCompatUsage,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatEmbeddingsRequest {
    pub api_base: String,
    pub api_key: String,
    pub model: String,
    pub inputs: Vec<String>,
    pub dimensions: Option<usize>,
    pub extra_headers: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatEmbeddingsResponse {
    pub model: String,
    pub embeddings: Vec<Vec<f32>>,
    pub usage: OpenAiCompatUsage,
}

fn extra_header_refs(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

/// POST `{api_base}/chat/completions` and return a normalized response.
///
/// Errors:
/// * empty model / prompt → `RedDBError::Query`.
/// * transport / non-2xx → `RedDBError::Query` carrying the status code
///   and the provider's parsed `error.message` when available, raw body
///   otherwise.
pub fn openai_compat_chat(
    request: OpenAiCompatChatRequest,
) -> RedDBResult<OpenAiCompatChatResponse> {
    if request.model.trim().is_empty() {
        return Err(RedDBError::Query(
            "openai-compat: model cannot be empty".to_string(),
        ));
    }
    if request.prompt.trim().is_empty() {
        return Err(RedDBError::Query(
            "openai-compat: prompt cannot be empty".to_string(),
        ));
    }

    let url = format!(
        "{}/chat/completions",
        request.api_base.trim_end_matches('/')
    );
    let payload = build_openai_prompt_payload(
        &request.model,
        &request.prompt,
        request.temperature,
        request.seed,
        request.max_output_tokens,
        false,
    );

    let extra = extra_header_refs(&request.extra_headers);
    let (status, body) = http_post_json(&url, &request.api_key, &extra, payload, 120)
        .map_err(|err| RedDBError::Query(format!("openai-compat transport error: {err}")))?;

    if !(200..300).contains(&status) {
        let message = openai_error_message(&body).unwrap_or_else(|| {
            if body.trim().is_empty() {
                "openai-compat chat request failed".to_string()
            } else {
                body.clone()
            }
        });
        return Err(RedDBError::Query(format!(
            "openai-compat chat request failed (status {status}): {message}"
        )));
    }

    let parsed = parse_openai_prompt_response(&body, &request.model)?;
    Ok(OpenAiCompatChatResponse {
        model: parsed.model,
        output_text: parsed.output_text,
        stop_reason: parsed.stop_reason,
        usage: OpenAiCompatUsage {
            input_tokens: parsed.prompt_tokens,
            output_tokens: parsed.completion_tokens,
            total_tokens: parsed.total_tokens,
        },
    })
}

/// POST `{api_base}/embeddings` and return a normalized response.
pub fn openai_compat_embeddings(
    request: OpenAiCompatEmbeddingsRequest,
) -> RedDBResult<OpenAiCompatEmbeddingsResponse> {
    if request.model.trim().is_empty() {
        return Err(RedDBError::Query(
            "openai-compat: embedding model cannot be empty".to_string(),
        ));
    }
    if request.inputs.is_empty() {
        return Err(RedDBError::Query(
            "openai-compat: at least one input is required".to_string(),
        ));
    }

    let url = format!("{}/embeddings", request.api_base.trim_end_matches('/'));
    let payload =
        build_openai_embedding_payload(&request.model, &request.inputs, request.dimensions);

    let extra = extra_header_refs(&request.extra_headers);
    let (status, body) = http_post_json(&url, &request.api_key, &extra, payload, 90)
        .map_err(|err| RedDBError::Query(format!("openai-compat transport error: {err}")))?;

    if !(200..300).contains(&status) {
        let message = openai_error_message(&body).unwrap_or_else(|| {
            if body.trim().is_empty() {
                "openai-compat embeddings request failed".to_string()
            } else {
                body.clone()
            }
        });
        return Err(RedDBError::Query(format!(
            "openai-compat embeddings request failed (status {status}): {message}"
        )));
    }

    let parsed = parse_openai_embedding_response(&body)?;
    Ok(OpenAiCompatEmbeddingsResponse {
        model: parsed.model,
        embeddings: parsed.embeddings,
        usage: OpenAiCompatUsage {
            input_tokens: parsed.prompt_tokens,
            output_tokens: None,
            total_tokens: parsed.total_tokens,
        },
    })
}

// ============================================================================
// Provider mode selector (issue gh-516)
//
// `red.config.ai.provider` picks the wire-protocol family that engine
// consumers (currently AskPipeline) should use. This is intentionally
// distinct from `red.config.ai.default.provider`, which names a
// concrete vendor (openai, groq, ollama, ...). The mode selector
// answers the prior question of which HTTP shape to speak.
// ============================================================================

/// Wire-protocol family used by engine-side AI consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiProviderMode {
    /// Generic OpenAI-compatible client (`POST {api_base}/chat/completions`).
    OpenAiCompat,
    /// Vendor-native OpenAI client (api.openai.com, default headers).
    OpenAiNative,
    /// Vendor-native Anthropic client (api.anthropic.com, x-api-key).
    AnthropicNative,
}

impl AiProviderMode {
    pub fn token(&self) -> &'static str {
        match self {
            Self::OpenAiCompat => "openai-compat",
            Self::OpenAiNative => "openai-native",
            Self::AnthropicNative => "anthropic-native",
        }
    }
}

/// Parse a mode token. Accepts hyphen or underscore spellings.
pub fn parse_provider_mode(name: &str) -> Option<AiProviderMode> {
    match name.trim().to_ascii_lowercase().as_str() {
        "openai-compat" | "openai_compat" | "openaicompat" => Some(AiProviderMode::OpenAiCompat),
        "openai-native" | "openai_native" | "openainative" => Some(AiProviderMode::OpenAiNative),
        "anthropic-native" | "anthropic_native" | "anthropicnative" => {
            Some(AiProviderMode::AnthropicNative)
        }
        _ => None,
    }
}

/// Resolve the provider mode. Lookup chain:
/// 1. `REDDB_AI_PROVIDER_MODE` env var.
/// 2. `red_config` KV key `red.config.ai.provider`.
/// 3. Returns `None` so callers can fall back to their existing
///    vendor-based routing.
pub fn resolve_provider_mode<F>(kv_getter: &F) -> Option<AiProviderMode>
where
    F: Fn(&str) -> crate::RedDBResult<Option<String>>,
{
    if let Ok(value) = std::env::var("REDDB_AI_PROVIDER_MODE") {
        if let Some(mode) = parse_provider_mode(&value) {
            return Some(mode);
        }
    }
    if let Ok(Some(value)) = kv_getter("red.config.ai.provider") {
        if let Some(mode) = parse_provider_mode(&value) {
            return Some(mode);
        }
    }
    None
}

/// Map a mode to the matching [`AiProvider`] variant. `OpenAiCompat`
/// stays as a `Custom("")` marker — callers must resolve the actual
/// api_base separately (typically via `resolve_api_base_with_kv`).
pub fn provider_mode_to_provider(mode: AiProviderMode) -> AiProvider {
    match mode {
        AiProviderMode::OpenAiNative => AiProvider::OpenAi,
        AiProviderMode::AnthropicNative => AiProvider::Anthropic,
        AiProviderMode::OpenAiCompat => AiProvider::Custom(String::new()),
    }
}
