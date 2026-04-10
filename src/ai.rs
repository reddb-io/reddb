//! External AI provider integration primitives.
//!
//! This module currently supports OpenAI embeddings and is intended to be
//! consumed by server handlers and future query/runtime integrations.

use std::time::Duration;

use crate::json::{parse_json, Map, Value as JsonValue};
use crate::{RedDBError, RedDBResult};

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
    pub max_output_tokens: Option<usize>,
    pub api_base: String,
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
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub stop_reason: Option<String>,
}

pub fn openai_embeddings(request: OpenAiEmbeddingRequest) -> RedDBResult<OpenAiEmbeddingResponse> {
    if request.api_key.trim().is_empty() {
        return Err(RedDBError::Query(
            "OpenAI API key cannot be empty".to_string(),
        ));
    }
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

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(90))
        .timeout_write(Duration::from_secs(30))
        .build();

    let response = agent
        .post(&url)
        .set("Authorization", &format!("Bearer {}", request.api_key))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_string(&payload);

    let body = match response {
        Ok(ok) => ok
            .into_string()
            .map_err(|err| RedDBError::Query(format!("failed to read OpenAI response: {err}")))?,
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_else(|_| "".to_string());
            let message = openai_error_message(&body)
                .unwrap_or_else(|| "OpenAI embeddings request failed".to_string());
            return Err(RedDBError::Query(format!(
                "OpenAI embeddings request failed (status {status}): {message}"
            )));
        }
        Err(ureq::Error::Transport(err)) => {
            return Err(RedDBError::Query(format!("OpenAI transport error: {err}")));
        }
    };

    parse_openai_embedding_response(&body)
}

pub fn openai_prompt(request: OpenAiPromptRequest) -> RedDBResult<AiPromptResponse> {
    if request.api_key.trim().is_empty() {
        return Err(RedDBError::Query(
            "OpenAI API key cannot be empty".to_string(),
        ));
    }
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
        request.max_output_tokens,
    );

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(30))
        .build();

    let response = agent
        .post(&url)
        .set("Authorization", &format!("Bearer {}", request.api_key))
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_string(&payload);

    let body = match response {
        Ok(ok) => ok
            .into_string()
            .map_err(|err| RedDBError::Query(format!("failed to read OpenAI response: {err}")))?,
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_else(|_| "".to_string());
            let message = openai_error_message(&body)
                .unwrap_or_else(|| "OpenAI prompt request failed".to_string());
            return Err(RedDBError::Query(format!(
                "OpenAI prompt request failed (status {status}): {message}"
            )));
        }
        Err(ureq::Error::Transport(err)) => {
            return Err(RedDBError::Query(format!("OpenAI transport error: {err}")));
        }
    };

    parse_openai_prompt_response(&body, &request.model)
}

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

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(30))
        .build();

    let response = agent
        .post(&url)
        .set("x-api-key", &request.api_key)
        .set("anthropic-version", &request.anthropic_version)
        .set("Content-Type", "application/json")
        .set("Accept", "application/json")
        .send_string(&payload);

    let body = match response {
        Ok(ok) => ok.into_string().map_err(|err| {
            RedDBError::Query(format!("failed to read Anthropic response: {err}"))
        })?,
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_else(|_| "".to_string());
            let message = anthropic_error_message(&body)
                .unwrap_or_else(|| "Anthropic prompt request failed".to_string());
            return Err(RedDBError::Query(format!(
                "Anthropic prompt request failed (status {status}): {message}"
            )));
        }
        Err(ureq::Error::Transport(err)) => {
            return Err(RedDBError::Query(format!(
                "Anthropic transport error: {err}"
            )));
        }
    };

    parse_anthropic_prompt_response(&body, &request.model)
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
    max_output_tokens: Option<usize>,
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

    if let Some(max_output_tokens) = max_output_tokens {
        object.insert(
            "max_tokens".to_string(),
            JsonValue::Number(max_output_tokens as f64),
        );
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
}
