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
    fn resolve_api_key_prefers_new_vault_path_over_removed_paths() {
        let provider = AiProvider::OpenAi;
        let alias = "vault_unit_alias";
        let secret_path = ai_api_secret_path(&provider, alias);
        let removed_legacy = removed_plaintext_config_key(&provider, alias);
        let removed_vault = removed_vault_api_key_path(&provider, alias);

        let resolved = resolve_api_key(&provider, Some(alias), |key| {
            if key == secret_path {
                Ok(Some("vault-key".to_string()))
            } else if key == removed_legacy || key == removed_vault {
                Ok(Some("stale-key".to_string()))
            } else {
                Ok(None)
            }
        })
        .expect("resolve");

        assert_eq!(resolved, "vault-key");
    }

    #[test]
    fn resolve_api_key_rejects_removed_plaintext_config_path() {
        let provider = AiProvider::Custom("cred1745legacy".to_string());
        let alias = "prod";
        let removed_legacy = removed_plaintext_config_key(&provider, alias);
        let new_path = ai_api_secret_path(&provider, alias);

        // Only the removed plaintext config path holds a value: resolution
        // must reject didactically, naming the new vault path.
        let err = resolve_api_key(&provider, Some(alias), |key| {
            if key == removed_legacy {
                Ok(Some("stale-plaintext-key".to_string()))
            } else {
                Ok(None)
            }
        })
        .expect_err("must reject removed path");
        let msg = err.to_string();
        assert!(msg.contains(&removed_legacy), "names removed path: {msg}");
        assert!(msg.contains(&new_path), "names new vault path: {msg}");
    }

    #[test]
    fn resolve_api_key_rejects_removed_vault_api_key_path() {
        let provider = AiProvider::Custom("cred1745oldvault".to_string());
        let removed_vault = removed_vault_api_key_path(&provider, "default");
        let new_path = ai_api_secret_path(&provider, "default");

        let err = resolve_api_key(&provider, None, |key| {
            if key == removed_vault {
                Ok(Some("stale-vault-key".to_string()))
            } else {
                Ok(None)
            }
        })
        .expect_err("must reject removed vault path");
        let msg = err.to_string();
        assert!(msg.contains(&removed_vault), "names removed path: {msg}");
        assert!(msg.contains(&new_path), "names new vault path: {msg}");
    }

    #[test]
    fn resolve_api_key_alias_token_overrides_default_per_request() {
        let provider = AiProvider::OpenAi;
        let default_path = ai_api_secret_path(&provider, "default");
        let prod_path = ai_api_secret_path(&provider, "prod");
        let getter = |key: &str| {
            if key == default_path {
                Ok(Some("default-token".to_string()))
            } else if key == prod_path {
                Ok(Some("prod-token".to_string()))
            } else {
                Ok(None)
            }
        };
        // A request naming the `prod` alias resolves the tenant token; a
        // request naming no credential resolves the implicit `default`.
        assert_eq!(
            resolve_api_key(&provider, Some("prod"), getter).expect("prod"),
            "prod-token"
        );
        assert_eq!(
            resolve_api_key(&provider, None, getter).expect("default"),
            "default-token"
        );
    }

    #[test]
    fn ai_api_secret_path_uses_providers_tokens_shape() {
        let path = ai_api_secret_path(&AiProvider::OpenAi, "default");
        assert_eq!(path, "red.secret.ai.providers.openai.tokens.default");
        let aliased = ai_api_secret_path(&AiProvider::OpenAi, "Prod");
        assert_eq!(aliased, "red.secret.ai.providers.openai.tokens.prod");
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

    // Vault-first credential resolution with env fallback (issue #1270).
    // Each test uses a unique `Custom` provider token so the derived env
    // var name (`REDDB_<TOKEN>_API_KEY`) is process-unique and the tests
    // can set/unset env without racing other tests.

    #[test]
    fn resolve_api_key_uses_env_when_no_vault_entry() {
        let provider = AiProvider::Custom("cred1270envonly".to_string());
        let env_name = provider.default_key_env_name();
        std::env::set_var(&env_name, "env-fallback-key");

        // kv_getter returns nothing → no vault/legacy entry exists.
        let resolved = resolve_api_key(&provider, None, |_| Ok(None));

        std::env::remove_var(&env_name);
        assert_eq!(resolved.expect("resolve"), "env-fallback-key");
    }

    #[test]
    fn resolve_api_key_prefers_vault_over_env() {
        let provider = AiProvider::Custom("cred1270both".to_string());
        let env_name = provider.default_key_env_name();
        let secret_path = ai_api_secret_path(&provider, "default");
        std::env::set_var(&env_name, "env-fallback-key");

        // Both the vault secret and the env var are set; vault wins.
        let resolved = resolve_api_key(&provider, None, |key| {
            if key == secret_path {
                Ok(Some("vault-managed-key".to_string()))
            } else {
                Ok(None)
            }
        });

        std::env::remove_var(&env_name);
        assert_eq!(resolved.expect("resolve"), "vault-managed-key");
    }

    #[test]
    fn resolve_api_key_alias_prefers_vault_over_env() {
        let provider = AiProvider::Custom("cred1270alias".to_string());
        let alias = "prod";
        let env_name = provider.alias_key_env_name(alias);
        let secret_path = ai_api_secret_path(&provider, alias);
        std::env::set_var(&env_name, "env-alias-key");

        let resolved = resolve_api_key(&provider, Some(alias), |key| {
            if key == secret_path {
                Ok(Some("vault-alias-key".to_string()))
            } else {
                Ok(None)
            }
        });

        std::env::remove_var(&env_name);
        assert_eq!(resolved.expect("resolve"), "vault-alias-key");
    }

    #[test]
    fn resolve_api_key_alias_falls_back_to_env_without_vault() {
        let provider = AiProvider::Custom("cred1270aliasenv".to_string());
        let alias = "prod";
        let env_name = provider.alias_key_env_name(alias);
        std::env::set_var(&env_name, "env-alias-key");

        let resolved = resolve_api_key(&provider, Some(alias), |_| Ok(None));

        std::env::remove_var(&env_name);
        assert_eq!(resolved.expect("resolve"), "env-alias-key");
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
                let body = String::from_utf8_lossy(&data[header_len..header_len + content_length])
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
        // The wire-protocol mode selector still wins over the task pointer.
        let kv = |key: &str| -> crate::RedDBResult<Option<String>> {
            match key {
                "red.config.ai.provider" => Ok(Some("anthropic-native".to_string())),
                "red.config.ai.inference.provider" => Ok(Some("groq".to_string())),
                _ => Ok(None),
            }
        };
        assert_eq!(resolve_default_provider(&kv), AiProvider::Anthropic);
    }

    // ---- ADR-0068 §5 config schema (issue #1746) --------------------------

    #[test]
    fn inference_provider_ask_specific_beats_task_pointer() {
        let kv = |key: &str| -> crate::RedDBResult<Option<String>> {
            match key {
                "red.config.ai.ask.provider" => Ok(Some("groq".to_string())),
                "red.config.ai.inference.provider" => Ok(Some("deepseek".to_string())),
                _ => Ok(None),
            }
        };
        assert_eq!(resolve_default_provider(&kv), AiProvider::Groq);
    }

    #[test]
    fn inference_provider_falls_through_to_task_pointer_then_default() {
        let pointer = |key: &str| -> crate::RedDBResult<Option<String>> {
            match key {
                "red.config.ai.inference.provider" => Ok(Some("deepseek".to_string())),
                _ => Ok(None),
            }
        };
        assert_eq!(resolve_default_provider(&pointer), AiProvider::DeepSeek);

        let empty = |_: &str| -> crate::RedDBResult<Option<String>> { Ok(None) };
        assert_eq!(resolve_default_provider(&empty), AiProvider::OpenAi);
    }

    #[test]
    fn inference_model_ask_specific_beats_models_block_beats_builtin() {
        let ask = |key: &str| -> crate::RedDBResult<Option<String>> {
            match key {
                "red.config.ai.ask.model" => Ok(Some("gpt-ask".to_string())),
                "red.config.ai.providers.openai.models.inference" => {
                    Ok(Some("gpt-block".to_string()))
                }
                _ => Ok(None),
            }
        };
        assert_eq!(resolve_default_model(&AiProvider::OpenAi, &ask), "gpt-ask");

        let block = |key: &str| -> crate::RedDBResult<Option<String>> {
            match key {
                "red.config.ai.providers.openai.models.inference" => {
                    Ok(Some("gpt-block".to_string()))
                }
                _ => Ok(None),
            }
        };
        assert_eq!(
            resolve_default_model(&AiProvider::OpenAi, &block),
            "gpt-block"
        );

        let empty = |_: &str| -> crate::RedDBResult<Option<String>> { Ok(None) };
        assert_eq!(
            resolve_default_model(&AiProvider::OpenAi, &empty),
            AiProvider::OpenAi.default_prompt_model()
        );
    }

    #[test]
    fn embeddings_provider_follows_task_pointer() {
        let kv = |key: &str| -> crate::RedDBResult<Option<String>> {
            match key {
                "red.config.ai.embeddings.provider" => Ok(Some("ollama".to_string())),
                _ => Ok(None),
            }
        };
        assert_eq!(
            resolve_embeddings_provider(&kv).unwrap(),
            AiProvider::Ollama
        );

        let empty = |_: &str| -> crate::RedDBResult<Option<String>> { Ok(None) };
        assert_eq!(
            resolve_embeddings_provider(&empty).unwrap(),
            AiProvider::OpenAi
        );
    }

    #[test]
    fn embeddings_provider_rejects_modality_incapable_pointer() {
        let kv = |key: &str| -> crate::RedDBResult<Option<String>> {
            match key {
                "red.config.ai.embeddings.provider" => Ok(Some("anthropic".to_string())),
                _ => Ok(None),
            }
        };
        let err = resolve_embeddings_provider(&kv).unwrap_err().to_string();
        assert!(
            err.contains("red.config.ai.embeddings.provider"),
            "error must name the pointer to fix: {err}"
        );
        assert!(
            err.contains("openai") && err.contains("no embeddings API"),
            "error must name capable alternatives: {err}"
        );
    }

    #[test]
    fn embeddings_model_block_beats_builtin() {
        let block = |key: &str| -> crate::RedDBResult<Option<String>> {
            match key {
                "red.config.ai.providers.openai.models.embeddings" => {
                    Ok(Some("text-embedding-custom".to_string()))
                }
                _ => Ok(None),
            }
        };
        assert_eq!(
            resolve_embeddings_model(&AiProvider::OpenAi, &block),
            "text-embedding-custom"
        );

        let empty = |_: &str| -> crate::RedDBResult<Option<String>> { Ok(None) };
        assert_eq!(
            resolve_embeddings_model(&AiProvider::Ollama, &empty),
            AiProvider::Ollama.default_embedding_model()
        );
    }

    #[test]
    fn base_url_reads_provider_block_key() {
        let kv = |key: &str| -> crate::RedDBResult<Option<String>> {
            if key == "red.config.ai.providers.openai.base_url" {
                Ok(Some("https://proxy.example/v1".to_string()))
            } else {
                Ok(None)
            }
        };
        assert_eq!(
            AiProvider::OpenAi.resolve_api_base_with_kv("default", &kv),
            "https://proxy.example/v1"
        );
    }

    #[test]
    fn removed_config_keys_rejected_naming_replacements() {
        let err = validate_ai_config_key_on_write("red.config.ai.default.provider")
            .unwrap_err()
            .to_string();
        assert!(err.contains("red.config.ai.inference.provider"), "{err}");

        let err = validate_ai_config_key_on_write("red.config.ai.default.model")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("red.config.ai.providers.<provider>.models.inference"),
            "{err}"
        );

        let err = validate_ai_config_key_on_write("red.config.ai.openai.default.base_url")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("red.config.ai.providers.<provider>.base_url"),
            "{err}"
        );

        // New-schema keys and unrelated keys are accepted.
        assert!(validate_ai_config_key_on_write("red.config.ai.inference.provider").is_ok());
        assert!(validate_ai_config_key_on_write("red.config.ai.providers.openai.base_url").is_ok());
        assert!(validate_ai_config_key_on_write("acme.some.other.key").is_ok());
    }

    #[test]
    fn ask_planner_model_and_effort_resolve() {
        let kv = |key: &str| -> crate::RedDBResult<Option<String>> {
            match key {
                "red.config.ai.ask.planner_model" => Ok(Some("planner-x".to_string())),
                "red.config.ai.ask.effort" => Ok(Some("high".to_string())),
                _ => Ok(None),
            }
        };
        assert_eq!(resolve_ask_planner_model(&kv, "fallback"), "planner-x");
        assert_eq!(resolve_ask_effort(&kv), Some("high".to_string()));

        let empty = |_: &str| -> crate::RedDBResult<Option<String>> { Ok(None) };
        assert_eq!(resolve_ask_planner_model(&empty, "fallback"), "fallback");
        assert_eq!(resolve_ask_effort(&empty), None);
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
    MiniMax,
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
            Self::MiniMax => "minimax",
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
            Self::MiniMax => "abab6.5s-chat",
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
            Self::MiniMax => "embo-01",
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
            Self::MiniMax => "https://api.minimax.chat/v1",
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

    /// Resolve API base URL checking KV store too (for custom base_url
    /// config). ADR-0068 §5 clean break: the base URL now lives at
    /// `red.config.ai.providers.<provider>.base_url` (per provider, no
    /// credential alias). The old `red.config.ai.<provider>.<alias>.base_url`
    /// shape is rejected on write; see [`validate_ai_config_key_on_write`].
    pub fn resolve_api_base_with_kv<F>(&self, _alias: &str, kv_getter: &F) -> String
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
        // 2. Provider block: red.config.ai.providers.<provider>.base_url
        if let Ok(Some(value)) = kv_getter(&provider_base_url_key(self)) {
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
                | Self::MiniMax
                | Self::Custom(_)
        )
    }

    /// Whether this provider requires an API key (Ollama/Local don't).
    pub fn requires_api_key(&self) -> bool {
        !matches!(self, Self::Ollama | Self::Local)
    }

    /// Whether this provider offers an embeddings API. Anthropic famously
    /// does not; every other provider RedDB speaks does (Local embeds
    /// in-process). Used to fail an embeddings task pointer loudly rather
    /// than silently re-routing to a different provider (ADR-0068 §5).
    pub fn supports_embeddings(&self) -> bool {
        !matches!(self, Self::Anthropic)
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
        "minimax" | "mini_max" => Ok(AiProvider::MiniMax),
        "huggingface" | "hf" => Ok(AiProvider::HuggingFace),
        "local" => Ok(AiProvider::Local),
        other => {
            // Treat as custom provider if it looks like a URL
            if other.starts_with("http://") || other.starts_with("https://") {
                Ok(AiProvider::Custom(other.to_string()))
            } else {
                Err(crate::RedDBError::Query(format!(
                    "unsupported AI provider '{other}'; expected: openai, anthropic, groq, \
                     openrouter, together, venice, ollama, deepseek, minimax, huggingface, local"
                )))
            }
        }
    }
}

// ============================================================================
// AI config schema (ADR-0068 §5) — clean break
//
//   red.config.ai.ask.{provider,model,planner_model,effort,max_plan_steps}
//   red.config.ai.providers.<provider>.base_url
//   red.config.ai.providers.<provider>.models.{inference,embeddings}
//   red.config.ai.inference.provider   # task pointer: who generates
//   red.config.ai.embeddings.provider  # task pointer: who embeds
//
// Resolution order for any AI call:
//   ASK-specific config -> task pointer -> pointed provider's models block
//   -> provider built-in default.
//
// The old flat keys (`red.config.ai.default.provider|model` and the old
// per-alias `red.config.ai.<provider>.<alias>.base_url` base-URL shape)
// are removed and rejected on write; there is no deprecation window.
// ============================================================================

/// Providers that can serve embeddings, listed for didactic errors.
pub const EMBEDDING_CAPABLE_PROVIDERS: &str =
    "openai, groq, ollama, openrouter, together, venice, deepseek, minimax, huggingface, local";

/// KV key for a provider's base URL under the new schema.
pub fn provider_base_url_key(provider: &AiProvider) -> String {
    format!("red.config.ai.providers.{}.base_url", provider.token())
}

/// KV key for a provider's per-modality model under the new schema.
/// `modality` is `"inference"` or `"embeddings"`.
pub fn provider_models_key(provider: &AiProvider, modality: &str) -> String {
    format!(
        "red.config.ai.providers.{}.models.{modality}",
        provider.token()
    )
}

/// Reject an AI config key that was removed in the ADR-0068 clean break,
/// naming the replacement key. Called from the `SET CONFIG` write path so
/// operators cannot silently persist a key nothing reads. Returns `Ok(())`
/// for every key that is still valid (including non-AI keys).
pub fn validate_ai_config_key_on_write(key: &str) -> crate::RedDBResult<()> {
    let key = key.trim();
    if key == "red.config.ai.default.provider" {
        return Err(crate::RedDBError::Query(
            "AI config key 'red.config.ai.default.provider' was removed in the ADR-0068 \
             clean break; set the task pointer 'red.config.ai.inference.provider' (or \
             'red.config.ai.ask.provider' for the ASK planner) instead"
                .to_string(),
        ));
    }
    if key == "red.config.ai.default.model" {
        return Err(crate::RedDBError::Query(
            "AI config key 'red.config.ai.default.model' was removed in the ADR-0068 clean \
             break; set 'red.config.ai.ask.model' or \
             'red.config.ai.providers.<provider>.models.inference' instead"
                .to_string(),
        ));
    }
    // Old per-alias base-URL shape: red.config.ai.<provider>.<alias>.base_url
    // (anything under red.config.ai.* ending in .base_url that is NOT the
    // new red.config.ai.providers.<provider>.base_url form).
    if key.starts_with("red.config.ai.")
        && key.ends_with(".base_url")
        && !key.starts_with("red.config.ai.providers.")
    {
        return Err(crate::RedDBError::Query(format!(
            "AI config key '{key}' uses the removed per-credential base-URL shape; set \
             'red.config.ai.providers.<provider>.base_url' instead (ADR-0068 clean break)"
        )));
    }
    Ok(())
}

/// Resolve the inference (generation) provider. Precedence:
/// 0. Wire-protocol mode selector (`red.config.ai.provider`) when set.
/// 1. `REDDB_AI_PROVIDER` env var.
/// 2. ASK-specific config `red.config.ai.ask.provider`.
/// 3. Inference task pointer `red.config.ai.inference.provider`.
/// 4. Falls back to OpenAI.
pub fn resolve_default_provider<F>(kv_getter: &F) -> AiProvider
where
    F: Fn(&str) -> crate::RedDBResult<Option<String>>,
{
    // 0. Wire-protocol mode selector takes precedence when explicitly set.
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
    // 2. ASK-specific config, then 3. inference task pointer.
    for key in [
        "red.config.ai.ask.provider",
        "red.config.ai.inference.provider",
    ] {
        if let Ok(Some(value)) = kv_getter(key) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                if let Ok(provider) = parse_provider(&value) {
                    return provider;
                }
            }
        }
    }
    AiProvider::OpenAi
}

/// Resolve the inference (generation) model for `provider`. Precedence:
/// 1. `REDDB_AI_MODEL` env var.
/// 2. Provider-specific prompt-model env var.
/// 3. ASK-specific config `red.config.ai.ask.model`.
/// 4. Provider models block `…providers.<p>.models.inference`.
/// 5. Provider built-in default prompt model.
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
    // 3. ASK-specific config, then 4. provider models block.
    for key in [
        "red.config.ai.ask.model".to_string(),
        provider_models_key(provider, "inference"),
    ] {
        if let Ok(Some(value)) = kv_getter(&key) {
            let value = value.trim().to_string();
            if !value.is_empty() {
                return value;
            }
        }
    }
    provider.default_prompt_model().to_string()
}

/// Resolve the embeddings provider from the embeddings task pointer
/// (`red.config.ai.embeddings.provider`), falling back to OpenAI. Fails
/// with a didactic error — naming the pointer and the capable providers —
/// when the pointer names a provider that has no embeddings API, rather
/// than silently re-routing (ADR-0068 §5; the Anthropic case generalized).
///
/// `REDDB_AI_EMBEDDINGS_PROVIDER` keeps env precedence over config.
pub fn resolve_embeddings_provider<F>(kv_getter: &F) -> crate::RedDBResult<AiProvider>
where
    F: Fn(&str) -> crate::RedDBResult<Option<String>>,
{
    let mut provider = AiProvider::OpenAi;
    if let Ok(value) = std::env::var("REDDB_AI_EMBEDDINGS_PROVIDER") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            provider = parse_provider(&value)?;
        }
    } else if let Ok(Some(value)) = kv_getter("red.config.ai.embeddings.provider") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            provider = parse_provider(&value)?;
        }
    }
    ensure_provider_supports_embeddings(&provider)?;
    Ok(provider)
}

/// Fail with a didactic error when `provider` cannot serve embeddings.
pub fn ensure_provider_supports_embeddings(provider: &AiProvider) -> crate::RedDBResult<()> {
    if provider.supports_embeddings() {
        return Ok(());
    }
    Err(crate::RedDBError::Query(format!(
        "the embeddings task pointer 'red.config.ai.embeddings.provider' names '{}', which \
         has no embeddings API. Point it at a capable provider ({}) — RedDB never silently \
         re-routes embeddings to a different provider than the one you named.",
        provider.token(),
        EMBEDDING_CAPABLE_PROVIDERS
    )))
}

/// Resolve the embeddings model for `provider`. Precedence:
/// 1. Provider-specific `REDDB_<P>_EMBEDDING_MODEL` env var.
/// 2. `REDDB_OPENAI_EMBEDDING_MODEL` env var (legacy shared override).
/// 3. Provider models block `…providers.<p>.models.embeddings`.
/// 4. Provider built-in default embedding model.
pub fn resolve_embeddings_model<F>(provider: &AiProvider, kv_getter: &F) -> String
where
    F: Fn(&str) -> crate::RedDBResult<Option<String>>,
{
    if let Ok(value) = std::env::var(format!(
        "REDDB_{}_EMBEDDING_MODEL",
        provider.token().to_ascii_uppercase()
    )) {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return value;
        }
    }
    if let Ok(value) = std::env::var("REDDB_OPENAI_EMBEDDING_MODEL") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return value;
        }
    }
    if let Ok(Some(value)) = kv_getter(&provider_models_key(provider, "embeddings")) {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return value;
        }
    }
    provider.default_embedding_model().to_string()
}

/// Resolve the ASK planner model (`red.config.ai.ask.planner_model`),
/// falling back to `fallback_model` (typically the resolved ASK model).
/// Inert until the ASK planner slice consumes it.
pub fn resolve_ask_planner_model<F>(kv_getter: &F, fallback_model: &str) -> String
where
    F: Fn(&str) -> crate::RedDBResult<Option<String>>,
{
    if let Ok(Some(value)) = kv_getter("red.config.ai.ask.planner_model") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return value;
        }
    }
    fallback_model.to_string()
}

/// Resolve the ASK planner effort (`red.config.ai.ask.effort`) if set.
/// Inert until the ASK planner slice consumes it.
pub fn resolve_ask_effort<F>(kv_getter: &F) -> Option<String>
where
    F: Fn(&str) -> crate::RedDBResult<Option<String>>,
{
    if let Ok(Some(value)) = kv_getter("red.config.ai.ask.effort") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
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

/// Resolve the embeddings provider for an AUTO EMBED / embeddings call from
/// a runtime port. `explicit` is the `USING <provider>` override (empty when
/// none was given); when empty the embeddings task pointer drives selection.
/// A modality-incapable provider fails didactically (ADR-0068 §5).
pub fn resolve_embeddings_provider_from_runtime<
    P: crate::application::ports::RuntimeEntityPort + ?Sized,
>(
    runtime: &P,
    explicit: &str,
) -> crate::RedDBResult<AiProvider> {
    let explicit = explicit.trim();
    if !explicit.is_empty() {
        let provider = parse_provider(explicit)?;
        ensure_provider_supports_embeddings(&provider)?;
        return Ok(provider);
    }
    let kv_getter = |key: &str| -> crate::RedDBResult<Option<String>> {
        match runtime.get_kv("red_config", key)? {
            Some((crate::storage::schema::Value::Text(s), _)) => Ok(Some(s.to_string())),
            _ => Ok(None),
        }
    };
    resolve_embeddings_provider(&kv_getter)
}

/// Resolve the embeddings model for `provider` from a runtime port,
/// honouring an explicit `MODEL '<name>'` override before the config
/// resolution order (env → provider models block → built-in default).
pub fn resolve_embeddings_model_from_runtime<
    P: crate::application::ports::RuntimeEntityPort + ?Sized,
>(
    runtime: &P,
    provider: &AiProvider,
    explicit: Option<&str>,
) -> String {
    if let Some(model) = explicit.map(str::trim).filter(|m| !m.is_empty()) {
        return model.to_string();
    }
    let kv_getter = |key: &str| -> crate::RedDBResult<Option<String>> {
        match runtime.get_kv("red_config", key)? {
            Some((crate::storage::schema::Value::Text(s), _)) => Ok(Some(s.to_string())),
            _ => Ok(None),
        }
    };
    resolve_embeddings_model(provider, &kv_getter)
}

/// Resolve an API key for a provider, **preferring the encrypted vault
/// over environment variables** (issue #1270). The env vars are a
/// bootstrap fallback so a fresh deployment can talk to a provider
/// before any key has been written to the vault.
///
/// Resolution order (issue #1745 — clean break, no deprecation window):
/// 1. Vault token path: `red.secret.ai.providers.<provider>.tokens.<alias>`
/// 2. Vault token indirected via
///    `red.config.ai.providers.<provider>.tokens.<alias>.secret_ref`
/// 3. Env fallback: `REDDB_<PROVIDER>_API_KEY[_<ALIAS>]`
///
/// The alias `default` is implicit when `credential_alias = None`. First
/// non-empty source wins per request.
///
/// The old vault path shape (`red.secret.ai.<provider>.<alias>.api_key`)
/// and the legacy plaintext config path (`red.config.ai.<provider>.<alias>.key`)
/// are **removed**: a credential still sitting at either is rejected with a
/// didactic error naming the new vault path to populate — never silently
/// read.
///
/// `kv_getter` receives either a `red.secret.*` path (routed to the
/// encrypted vault by [`resolve_api_key_from_runtime`]) or a
/// `red.config.*` key and returns the value if found. Vault-stored keys
/// are therefore encrypted at rest and rotatable via the existing vault
/// KV path; the env vars carry no such guarantees, which is why they are
/// the fallback rather than the primary source.
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
        // 1. Vault token path (managed, encrypted at rest).
        if let Some(key) = kv_getter(&ai_api_secret_path(provider, alias))? {
            if !key.trim().is_empty() {
                return Ok(key);
            }
        }
        // 2. Vault token reachable through a configured indirection ref.
        if let Some(secret_ref) = kv_getter(&ai_api_secret_ref_config_key(provider, alias))? {
            if let Some(key) = kv_getter(secret_ref.trim())? {
                if !key.trim().is_empty() {
                    return Ok(key);
                }
            }
        }
        // 3. Env fallback with alias (bootstrap before vault is populated).
        if let Some(key) = resolve_key_from_env_alias(provider, alias) {
            return Ok(key);
        }
        // Clean break: a credential still sitting at a removed path is
        // rejected didactically, never silently read (issue #1745).
        reject_removed_credential_paths(provider, alias, &kv_getter)?;
        return Err(crate::RedDBError::Query(format!(
            "credential '{alias}' not found for {}. Set env {} or store it in the vault at '{}'",
            provider.token(),
            provider.alias_key_env_name(alias),
            ai_api_secret_path(provider, alias),
        )));
    }

    // 1. Vault token path (managed, encrypted at rest).
    if let Some(key) = kv_getter(&ai_api_secret_path(provider, "default"))? {
        if !key.trim().is_empty() {
            return Ok(key);
        }
    }
    // 2. Vault token reachable through a configured indirection ref.
    if let Some(secret_ref) = kv_getter(&ai_api_secret_ref_config_key(provider, "default"))? {
        if let Some(key) = kv_getter(secret_ref.trim())? {
            if !key.trim().is_empty() {
                return Ok(key);
            }
        }
    }

    // 3. Env fallback (bootstrap before the vault is populated).
    if let Ok(value) = std::env::var(provider.default_key_env_name()) {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Ok(value);
        }
    }

    // Clean break: reject a credential still sitting at a removed path
    // instead of silently reading it (issue #1745).
    reject_removed_credential_paths(provider, "default", &kv_getter)?;

    Err(crate::RedDBError::Query(format!(
        "missing {} API key. Set {} or store it in the vault at '{}'",
        provider.token(),
        provider.default_key_env_name(),
        ai_api_secret_path(provider, "default"),
    )))
}

/// Vault token path (issue #1745): the sole credential source in the vault.
pub fn ai_api_secret_path(provider: &AiProvider, alias: &str) -> String {
    format!(
        "red.secret.ai.providers.{}.tokens.{}",
        provider.token(),
        normalize_credential_alias_path(alias)
    )
}

/// Config key holding a vault indirection ref for the token (issue #1745).
pub fn ai_api_secret_ref_config_key(provider: &AiProvider, alias: &str) -> String {
    format!(
        "red.config.ai.providers.{}.tokens.{}.secret_ref",
        provider.token(),
        normalize_credential_alias_path(alias)
    )
}

/// Removed vault path shape (`red.secret.ai.<provider>.<alias>.api_key`,
/// issue #1745). Probed ONLY to reject with a migration error — never read
/// as a credential source.
fn removed_vault_api_key_path(provider: &AiProvider, alias: &str) -> String {
    format!(
        "red.secret.ai.{}.{}.api_key",
        provider.token(),
        normalize_credential_alias_path(alias)
    )
}

/// Removed plaintext config path (`red.config.ai.<provider>.<alias>.key`,
/// issue #1745). Probed ONLY to reject with a migration error.
fn removed_plaintext_config_key(provider: &AiProvider, alias: &str) -> String {
    format!(
        "red.config.ai.{}.{}.key",
        provider.token(),
        normalize_credential_alias_path(alias)
    )
}

/// Fail with a didactic error if a credential is still parked at either of
/// the removed paths (old vault shape or legacy plaintext config). The
/// clean break forbids silently falling back to them (issue #1745).
fn reject_removed_credential_paths<F>(
    provider: &AiProvider,
    alias: &str,
    kv_getter: &F,
) -> crate::RedDBResult<()>
where
    F: Fn(&str) -> crate::RedDBResult<Option<String>>,
{
    let new_path = ai_api_secret_path(provider, alias);
    for removed in [
        removed_vault_api_key_path(provider, alias),
        removed_plaintext_config_key(provider, alias),
    ] {
        if let Some(value) = kv_getter(&removed)? {
            if !value.trim().is_empty() {
                return Err(crate::RedDBError::Query(format!(
                    "AI credential found at removed path '{removed}'. The AI credential vault \
                     path changed (issue #1745): store the token at '{new_path}' instead. The \
                     old vault path shape and the legacy plaintext config path are no longer read."
                )));
            }
        }
    }
    Ok(())
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
///
/// Emits an `ai.credential.resolve` audit event so operators can answer
/// "which principal caused us to read
/// `red.secret.ai.providers.<provider>.tokens.*`?"
/// even though the read itself is performed as system (the AI subsystem
/// must always be able to fetch the key the query needs — denying it
/// would be denying the query at the wrong layer). The audit record
/// never contains the secret value.
pub fn resolve_api_key_from_runtime(
    provider: &AiProvider,
    credential_alias: Option<&str>,
    runtime: &crate::runtime::RedDBRuntime,
) -> crate::RedDBResult<String> {
    use crate::application::ports::RuntimeEntityPort;
    let alias_for_audit = credential_alias.unwrap_or("default").to_string();
    let provider_token = provider.token().to_string();
    let audited_paths: std::cell::RefCell<Vec<(String, bool)>> =
        std::cell::RefCell::new(Vec::new());
    let result = resolve_api_key(provider, credential_alias, |kv_key| {
        if kv_key.starts_with("red.secret.") {
            let value = runtime.vault_kv_get(kv_key);
            audited_paths
                .borrow_mut()
                .push((kv_key.to_string(), value.is_some()));
            return Ok(value);
        }
        match runtime.get_kv("red_config", kv_key)? {
            Some((crate::storage::schema::Value::Text(secret), _)) => {
                audited_paths.borrow_mut().push((kv_key.to_string(), true));
                Ok(Some(secret.to_string()))
            }
            Some(_) => {
                audited_paths.borrow_mut().push((kv_key.to_string(), false));
                Ok(None)
            }
            None => {
                audited_paths.borrow_mut().push((kv_key.to_string(), false));
                Ok(None)
            }
        }
    });
    let audited_paths = audited_paths.into_inner();

    let principal = crate::runtime::impl_core::current_auth_identity_for_audit()
        .map(|(user, _role)| user)
        .unwrap_or_else(|| "system".to_string());
    let outcome = if result.is_ok() { "hit" } else { "miss" };
    let target = format!("ai.credential:{provider_token}/{alias_for_audit}");
    let paths_json: Vec<crate::serde_json::Value> = audited_paths
        .iter()
        .map(|(p, hit)| {
            crate::serde_json::json!({
                "path": p,
                "hit": hit,
            })
        })
        .collect();
    let details = crate::serde_json::json!({
        "provider": provider_token,
        "alias": alias_for_audit,
        "paths_checked": paths_json,
    });
    runtime.audit_log().record(
        "ai.credential.resolve",
        &principal,
        &target,
        outcome,
        details,
    );
    result
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

const LOCAL_MODELS_DISABLED_MESSAGE: &str = "local embeddings require the `local-models` feature \
flag at engine build time. Build with: cargo build --features local-models. Alternatively, use \
the 'ollama' provider with a local Ollama server.";

const LOCAL_EMBEDDINGS_NOT_IMPLEMENTED_MESSAGE: &str = "local embeddings are registered by the \
`local-models` feature, but local model artifact execution is not implemented in this slice. \
Alternatively, use the 'ollama' provider with a local Ollama server.";

const LOCAL_PROMPT_OUT_OF_SCOPE_MESSAGE: &str = "local prompt and generation are out of scope for \
the `local-models` feature; the local provider contract is embeddings-only for this slice.";

pub fn local_embeddings_unavailable_error() -> crate::RedDBError {
    if cfg!(feature = "local-models") {
        crate::RedDBError::Query(LOCAL_EMBEDDINGS_NOT_IMPLEMENTED_MESSAGE.to_string())
    } else {
        crate::RedDBError::FeatureNotEnabled(LOCAL_MODELS_DISABLED_MESSAGE.to_string())
    }
}

pub fn local_prompt_unavailable_error() -> crate::RedDBError {
    crate::RedDBError::Query(LOCAL_PROMPT_OUT_OF_SCOPE_MESSAGE.to_string())
}

/// Local embedding via candle — requires `local-models` feature.
pub fn local_embeddings(
    _model_id: &str,
    _texts: &[String],
) -> crate::RedDBResult<OpenAiEmbeddingResponse> {
    Err(local_embeddings_unavailable_error())
}

/// Local prompt via candle — requires `local-models` feature.
pub fn local_prompt(_model_id: &str, _prompt: &str) -> crate::RedDBResult<AiPromptResponse> {
    Err(local_prompt_unavailable_error())
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
    // An explicit `provider` is honoured verbatim; when absent, the
    // embeddings task pointer drives selection (ADR-0068 §5).
    let provider = match payload
        .get("provider")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(name) => parse_provider(name)?,
        None => resolve_embeddings_provider_from_runtime(runtime, "")?,
    };
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
            return grpc_embeddings_local(runtime, payload);
        }
        _ => {}
    }

    let inputs: Vec<String> = grpc_collect_embedding_inputs(runtime, payload)?;

    let explicit_model = payload.get("model").and_then(|v| v.as_str());
    let model = resolve_embeddings_model_from_runtime(runtime, &provider, explicit_model);

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

/// gRPC local-provider embedding path (#680).
///
/// Mirrors the HTTP local path: resolves a registered+installed local
/// model, runs the runtime backend, and returns the same JSON shape
/// the HTTP handler produces (`provider`, `model`, `model_source`,
/// `model_revision`, `model_engine`, `dimensions`, `embeddings`).
/// Save-side behaviour is HTTP-only; gRPC mirrors the OpenAI-compatible
/// gRPC path which also does not persist.
fn grpc_embeddings_local(
    runtime: &crate::runtime::RedDBRuntime,
    payload: &JsonValue,
) -> crate::RedDBResult<JsonValue> {
    crate::runtime::ai::local_embedding::ensure_local_embedding_available()?;

    let model_name = payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            crate::RedDBError::Query(
                "field 'model' is required for the local provider and must be the \
                 registered local model name (see POST /ai/models)"
                    .to_string(),
            )
        })?
        .to_string();

    let inputs = grpc_collect_embedding_inputs(runtime, payload)?;
    let response = crate::runtime::ai::local_embedding::embed_local(runtime, &model_name, inputs)?;

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
    obj.insert("model".to_string(), JsonValue::String(response.name));
    obj.insert(
        "model_source".to_string(),
        JsonValue::String(response.source),
    );
    obj.insert(
        "model_revision".to_string(),
        JsonValue::String(response.revision),
    );
    obj.insert(
        "model_engine".to_string(),
        JsonValue::String(response.engine),
    );
    obj.insert(
        "dimensions".to_string(),
        JsonValue::Number(response.dimensions as f64),
    );
    obj.insert("embeddings".to_string(), JsonValue::Array(embeddings_json));
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
