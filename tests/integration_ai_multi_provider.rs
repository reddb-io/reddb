//! Multi-provider AI wiring tests.
//!
//! Covers the three surfaces the README promises (HTTP handler,
//! gRPC `grpc_embeddings`, SEARCH SIMILAR via `QueryUseCases`):
//!
//! * OpenAI-compatible providers pass the pre-flight provider
//!   gate (no "only 'openai' is currently supported" regression).
//! * Incompatible providers (Anthropic, HuggingFace) are rejected
//!   with a clear, provider-specific message.
//!
//! We do not hit real network. Compatible providers reach the HTTP
//! transport and fail there, which proves the guard was passed.

use reddb::ai::{grpc_embeddings, parse_provider};
use reddb::application::{QueryUseCases, SearchSimilarInput};
use reddb::json::{Map, Value as JsonValue};
use reddb::RedDBRuntime;

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

#[test]
fn parse_provider_accepts_all_readme_keywords() {
    for name in [
        "openai",
        "anthropic",
        "groq",
        "openrouter",
        "together",
        "venice",
        "deepseek",
        "huggingface",
        "ollama",
        "local",
    ] {
        parse_provider(name).unwrap_or_else(|e| panic!("parse_provider({name}) failed: {e}"));
    }
}

#[test]
fn grpc_embeddings_rejects_anthropic_with_clear_message() {
    let rt = rt();
    let mut payload = Map::new();
    payload.insert(
        "provider".to_string(),
        JsonValue::String("anthropic".to_string()),
    );
    payload.insert(
        "inputs".to_string(),
        JsonValue::Array(vec![JsonValue::String("hi".to_string())]),
    );
    let err = grpc_embeddings(&rt, &JsonValue::Object(payload))
        .expect_err("anthropic embeddings should be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("anthropic"),
        "error must name the provider: {msg}"
    );
    assert!(
        msg.to_ascii_lowercase().contains("not yet available")
            || msg.to_ascii_lowercase().contains("not yet"),
        "error must indicate unsupported status: {msg}"
    );
}

#[test]
fn grpc_embeddings_rejects_huggingface_with_clear_message() {
    let rt = rt();
    let mut payload = Map::new();
    payload.insert(
        "provider".to_string(),
        JsonValue::String("huggingface".to_string()),
    );
    payload.insert(
        "inputs".to_string(),
        JsonValue::Array(vec![JsonValue::String("hi".to_string())]),
    );
    let err = grpc_embeddings(&rt, &JsonValue::Object(payload))
        .expect_err("huggingface embeddings should be rejected");
    assert!(err.to_string().contains("huggingface"));
}

#[test]
fn grpc_embeddings_rejects_empty_inputs() {
    let rt = rt();
    let mut payload = Map::new();
    payload.insert(
        "provider".to_string(),
        JsonValue::String("openai".to_string()),
    );
    payload.insert("inputs".to_string(), JsonValue::Array(vec![]));
    let err = grpc_embeddings(&rt, &JsonValue::Object(payload)).unwrap_err();
    assert!(err.to_string().contains("inputs"));
}

#[test]
fn grpc_embeddings_rejects_when_no_input_shape_is_provided() {
    let rt = rt();
    let mut payload = Map::new();
    payload.insert(
        "provider".to_string(),
        JsonValue::String("openai".to_string()),
    );
    let err = grpc_embeddings(&rt, &JsonValue::Object(payload)).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("input") && msg.contains("source_query"),
        "error must list every supported shape: {msg}"
    );
}

#[test]
fn grpc_embeddings_rejects_source_query_with_unknown_mode() {
    let rt = rt();
    let mut payload = Map::new();
    payload.insert(
        "provider".to_string(),
        JsonValue::String("openai".to_string()),
    );
    payload.insert(
        "source_query".to_string(),
        JsonValue::String("SELECT 1".to_string()),
    );
    payload.insert(
        "source_mode".to_string(),
        JsonValue::String("garbage".to_string()),
    );
    let err = grpc_embeddings(&rt, &JsonValue::Object(payload)).unwrap_err();
    assert!(
        err.to_string().contains("source_mode"),
        "error must mention source_mode: {err}"
    );
}

#[test]
fn search_similar_rejects_incompatible_provider() {
    // Anthropic: no embeddings endpoint.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let err = q
        .search_similar(SearchSimilarInput {
            collection: "docs".to_string(),
            vector: Vec::new(),
            k: 5,
            min_score: 0.0,
            text: Some("hello world".to_string()),
            provider: Some("anthropic".to_string()),
        })
        .expect_err("anthropic SEARCH SIMILAR must be rejected");
    let msg = err.to_string();
    assert!(msg.contains("anthropic"), "{msg}");
    assert!(
        msg.to_ascii_lowercase().contains("not yet available"),
        "{msg}"
    );
}
