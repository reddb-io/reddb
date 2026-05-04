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
    // Issue #36 nailed down the policy: Anthropic has no embeddings
    // product and RedDB rejects the request explicitly rather than
    // silently re-routing to a different provider. Pin the error
    // copy is operator-actionable: names the provider, says they
    // don't have embeddings, and points at the alternatives.
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
    let lower = msg.to_ascii_lowercase();
    assert!(
        lower.contains("anthropic"),
        "error must name the provider: {msg}"
    );
    assert!(
        lower.contains("does not offer") || lower.contains("does not"),
        "error must explain that anthropic has no embeddings product: {msg}"
    );
    assert!(
        lower.contains("openai") || lower.contains("compatible"),
        "error must point operator at an alternative: {msg}"
    );
}

#[test]
fn grpc_embeddings_huggingface_dispatches_to_hf_client() {
    // Issue #36: HuggingFace embeddings now route to the dedicated
    // `huggingface_embeddings()` client instead of being rejected.
    // Without an HTTP server here the request will fail at the
    // transport layer — but the failure message must come from the
    // HF code path, not from the old "not yet available" reject.
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
        .expect_err("hf embeddings should fail without an api key / server");
    let lower = err.to_string().to_ascii_lowercase();
    assert!(
        !lower.contains("not yet available"),
        "must NOT use the legacy unsupported-provider message: {err}",
    );
    // Either the API-key resolution failed or the HF transport did.
    // Both paths name the provider in the error.
    assert!(
        lower.contains("huggingface") || lower.contains("api key"),
        "error must surface the HF dispatch path: {err}"
    );
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
