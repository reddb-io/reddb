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

#[path = "../../support/mod.rs"]
mod support;

use reddb::ai::{grpc_embeddings, parse_provider, AiProvider};
use reddb::application::{ExecuteQueryInput, QueryUseCases, SearchSimilarInput};
use reddb::json::{Map, Value as JsonValue};
use reddb::runtime::ai::local_embedding::clear_local_embedding_backend_for_tests;
use reddb::runtime::ai::provider_capabilities::{Modality, Registry};
use reddb::server::RedDBServer;
use reddb::RedDBRuntime;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn assert_local_models_disabled_error(msg: &str) {
    let lower = msg.to_ascii_lowercase();
    assert!(lower.contains("local"), "{msg}");
    assert!(lower.contains("local-models"), "{msg}");
    assert!(lower.contains("feature"), "{msg}");
    assert!(lower.contains("ollama"), "{msg}");
}

fn spawn_http_server(rt: RedDBRuntime) -> String {
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    addr.to_string()
}

fn spawn_persistent_http_server(tag: &str) -> (support::TempDbFile, String) {
    let (db, rt) = support::persistent_runtime(tag);
    let addr = spawn_http_server(rt);
    (db, addr)
}

fn post_json(addr: &str, path: &str, body: &str) -> (u16, String) {
    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body
    );

    let mut stream = TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("missing status in response: {response}"));
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
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
        "minimax",
        "huggingface",
        "ollama",
        "local",
    ] {
        parse_provider(name).unwrap_or_else(|e| panic!("parse_provider({name}) failed: {e}"));
    }
}

#[test]
fn parse_provider_accepts_minimax_and_resolves_openai_compatible_base() {
    // MiniMax is OpenAI-compatible transport: parse must succeed, the
    // provider must report itself as compatible, and resolve a v1 base.
    let provider = parse_provider("minimax").expect("minimax should parse");
    assert_eq!(provider, AiProvider::MiniMax);
    assert_eq!(provider.token(), "minimax");
    assert!(
        provider.is_openai_compatible(),
        "minimax uses the OpenAI-compatible transport"
    );
    let base = provider.default_api_base();
    assert!(base.starts_with("https://"), "{base}");
    assert!(base.ends_with("/v1"), "{base}");
    // Snake-case alias also resolves.
    assert_eq!(
        parse_provider("mini_max").expect("mini_max alias"),
        AiProvider::MiniMax
    );
}

#[test]
fn ddl_modality_gate_rejects_incapable_and_accepts_capable() {
    // The DDL-time validation entry point: a policy that wires an
    // embeddings-only `local` backend to a generation job is rejected
    // with a clear, operator-actionable message; a capable pairing is
    // admitted. Mirrors the provider-gate style of the other contracts.
    let registry = Registry::new();

    let err = registry
        .validate_policy_modality("local", "all-MiniLM-L6-v2", Modality::Generate)
        .expect_err("local cannot generate");
    let msg = err.to_string();
    let lower = msg.to_ascii_lowercase();
    assert!(
        lower.contains("local"),
        "error must name the provider: {msg}"
    );
    assert!(
        lower.contains("generate"),
        "error must name the modality: {msg}"
    );
    assert!(
        lower.contains("cannot serve") || lower.contains("invalid"),
        "error must explain the rejection: {msg}"
    );

    // MiniMax can serve embeddings, generation, and vision.
    registry
        .validate_policy_modality("minimax", "abab6.5s-chat", Modality::Vision)
        .expect("minimax serves vision");
    registry
        .validate_policy_modality("minimax", "embo-01", Modality::Embed)
        .expect("minimax serves embeddings");
}

#[test]
fn ddl_modality_gate_honours_override_and_unknown_conservative_default() {
    // Unknown provider falls back to conservative defaults: the two
    // universal text modalities are allowed, specialised ones denied.
    let registry = Registry::new();
    assert!(registry.can_serve("brand-new-llm", "m", Modality::Generate));
    assert!(registry.can_serve("brand-new-llm", "m", Modality::Embed));
    assert!(!registry.can_serve("brand-new-llm", "m", Modality::Vision));

    // A per-deployment override is honoured by the gate.
    let upgraded = reddb::runtime::ai::provider_capabilities::Modalities {
        embed: false,
        generate: true,
        vision: true,
        moderate: false,
    };
    let registry = registry.with_modality_override("brand-new-llm", upgraded);
    registry
        .validate_policy_modality("brand-new-llm", "m", Modality::Vision)
        .expect("override grants vision");
    assert!(registry
        .validate_policy_modality("brand-new-llm", "m", Modality::Embed)
        .is_err());
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
#[cfg(not(feature = "local-models"))]
fn http_embeddings_rejects_local_when_local_models_feature_is_disabled() {
    let _bg = super::support::backend_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    clear_local_embedding_backend_for_tests();

    let (_db, addr) = spawn_persistent_http_server("ai-multi-local-embeddings-http");
    let (status, body) = post_json(
        &addr,
        "/ai/embeddings",
        r#"{"provider":"local","inputs":["hello"]}"#,
    );

    assert_eq!(status, 501, "unexpected response body: {body}");
    assert_local_models_disabled_error(&body);
    clear_local_embedding_backend_for_tests();
}

#[test]
fn http_prompt_rejects_local_because_generation_is_out_of_scope() {
    let (_db, addr) = spawn_persistent_http_server("ai-multi-local-prompt-http");
    let (status, body) = post_json(
        &addr,
        "/ai/prompt",
        r#"{"provider":"local","prompt":"hello"}"#,
    );

    assert_eq!(status, 400, "unexpected response body: {body}");
    let lower = body.to_ascii_lowercase();
    assert!(lower.contains("out of scope"), "{body}");
    assert!(lower.contains("embeddings-only"), "{body}");
}

#[test]
#[cfg(not(feature = "local-models"))]
fn grpc_embeddings_rejects_local_when_local_models_feature_is_disabled() {
    let _bg = super::support::backend_lock()
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    clear_local_embedding_backend_for_tests();

    let rt = rt();
    let mut payload = Map::new();
    payload.insert(
        "provider".to_string(),
        JsonValue::String("local".to_string()),
    );
    payload.insert(
        "inputs".to_string(),
        JsonValue::Array(vec![JsonValue::String("hi".to_string())]),
    );

    let err = grpc_embeddings(&rt, &JsonValue::Object(payload))
        .expect_err("local embeddings should require the local-models feature");
    assert_local_models_disabled_error(&err.to_string());
    clear_local_embedding_backend_for_tests();
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

#[test]
#[cfg(not(feature = "local-models"))]
fn search_similar_rejects_local_when_local_models_feature_is_disabled() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let err = q
        .search_similar(SearchSimilarInput {
            collection: "docs".to_string(),
            vector: Vec::new(),
            k: 5,
            min_score: 0.0,
            text: Some("hello world".to_string()),
            provider: Some("local".to_string()),
        })
        .expect_err("local SEARCH SIMILAR must require the local-models feature");

    assert_local_models_disabled_error(&err.to_string());
}

#[test]
#[cfg(not(feature = "local-models"))]
fn auto_embed_insert_rejects_local_when_local_models_feature_is_disabled() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let err = q
        .execute(ExecuteQueryInput {
            query: "INSERT INTO docs (body) VALUES ('hello') WITH AUTO EMBED (body) USING local"
                .to_string(),
        })
        .expect_err("local AUTO EMBED must require the local-models feature");

    assert_local_models_disabled_error(&err.to_string());
}
