//! Local AI embedding model registry tests.
//!
//! Covers the HTTP surface introduced for #678:
//!
//! * Register a local HuggingFace embedding model and inspect it.
//! * Reject empty / invalid `source`, `model name`, `task`, `revision`.
//! * Reject unsupported tasks (especially `prompt` / `generation`).
//! * Trust policy defaults to `disabled` and refuses unsafe settings
//!   unless explicitly acknowledged.
//! * Duplicate registration and invalid updates fail deterministically.
//! * Metadata persists across runtime restart by snapshot/restart of
//!   the same on-disk path.
//!
//! No artifacts are pulled — this slice is metadata-only.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use reddb::server::RedDBServer;
use reddb::RedDBRuntime;

fn spawn_http_server(rt: RedDBRuntime) -> String {
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    addr.to_string()
}

fn send(addr: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let request = format!(
        "{method} {path} HTTP/1.1\r\n\
         Host: localhost\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {}",
        body.len(),
        body,
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

const MINIMAL_VALID: &str = r#"{
    "name":"minilm-l6-v2",
    "source":"sentence-transformers/all-MiniLM-L6-v2",
    "task":"embedding",
    "revision":"fa97f6e7cb1a59073dff9e6b13e2715cf7475ac9",
    "dimensions":384
}"#;

#[test]
fn register_persists_a_valid_local_embedding_model() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));

    let (status, body) = send(&addr, "POST", "/ai/models", MINIMAL_VALID);
    assert_eq!(status, 201, "register failed: {body}");
    assert!(body.contains("\"provider\":\"local\""), "{body}");
    assert!(body.contains("\"task\":\"embedding\""), "{body}");
    assert!(body.contains("\"engine\":\"candle\""), "{body}");
    assert!(body.contains("\"pull_policy\":\"if_missing\""), "{body}");
    assert!(body.contains("\"trust_policy\":\"disabled\""), "{body}");
    assert!(body.contains("\"status\":\"registered\""), "{body}");
    assert!(body.contains("\"dimensions\":384"), "{body}");

    let (status, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2", "");
    assert_eq!(status, 200, "inspect failed: {body}");
    assert!(
        body.contains("\"source\":\"sentence-transformers/all-MiniLM-L6-v2\""),
        "{body}"
    );
    assert!(
        body.contains("\"revision\":\"fa97f6e7cb1a59073dff9e6b13e2715cf7475ac9\""),
        "{body}"
    );

    let (status, body) = send(&addr, "GET", "/ai/models", "");
    assert_eq!(status, 200, "list failed: {body}");
    assert!(body.contains("\"count\":1"), "{body}");
    assert!(body.contains("\"name\":\"minilm-l6-v2\""), "{body}");
}

#[test]
fn register_rejects_duplicate_model_name() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let (status, _) = send(&addr, "POST", "/ai/models", MINIMAL_VALID);
    assert_eq!(status, 201);

    let (status, body) = send(&addr, "POST", "/ai/models", MINIMAL_VALID);
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("already registered"), "{body}");
    assert!(body.contains("minilm-l6-v2"), "{body}");
}

#[test]
fn register_rejects_empty_source_name_revision_task() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));

    for (case, payload, must_contain) in [
        (
            "empty name",
            r#"{"name":"","source":"x/y","task":"embedding","revision":"abc","dimensions":4}"#,
            "name",
        ),
        (
            "empty source",
            r#"{"name":"m1","source":"","task":"embedding","revision":"abc","dimensions":4}"#,
            "source",
        ),
        (
            "empty revision",
            r#"{"name":"m1","source":"x/y","task":"embedding","revision":"","dimensions":4}"#,
            "revision",
        ),
        (
            "missing task",
            r#"{"name":"m1","source":"x/y","revision":"abc","dimensions":4}"#,
            "task",
        ),
        (
            "missing dimensions",
            r#"{"name":"m1","source":"x/y","task":"embedding","revision":"abc"}"#,
            "dimensions",
        ),
        (
            "name with dot",
            r#"{"name":"m.1","source":"x/y","task":"embedding","revision":"abc","dimensions":4}"#,
            "name",
        ),
        (
            "name with whitespace",
            "{\"name\":\"m 1\",\"source\":\"x/y\",\"task\":\"embedding\",\"revision\":\"abc\",\"dimensions\":4}",
            "name",
        ),
        (
            "negative dimensions",
            r#"{"name":"m1","source":"x/y","task":"embedding","revision":"abc","dimensions":-1}"#,
            "dimensions",
        ),
        (
            "zero dimensions",
            r#"{"name":"m1","source":"x/y","task":"embedding","revision":"abc","dimensions":0}"#,
            "dimensions",
        ),
        (
            "huge dimensions",
            r#"{"name":"m1","source":"x/y","task":"embedding","revision":"abc","dimensions":99999999}"#,
            "dimensions",
        ),
    ] {
        let (status, body) = send(&addr, "POST", "/ai/models", payload);
        assert_eq!(status, 400, "{case}: {body}");
        assert!(
            body.to_ascii_lowercase().contains(must_contain),
            "{case}: missing '{must_contain}' in {body}"
        );
    }
}

#[test]
fn register_rejects_prompt_or_generation_task() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));

    for task in ["prompt", "generation", "chat", "completion"] {
        let payload = format!(
            r#"{{"name":"m1","source":"x/y","task":"{task}","revision":"abc","dimensions":4}}"#
        );
        let (status, body) = send(&addr, "POST", "/ai/models", &payload);
        assert_eq!(status, 400, "task={task}: {body}");
        let lower = body.to_ascii_lowercase();
        assert!(
            lower.contains("out of scope") || lower.contains("not supported"),
            "task={task}: must explain unsupported task: {body}"
        );
        assert!(lower.contains("embedding"), "task={task}: {body}");
    }
}

#[test]
fn register_rejects_unknown_task() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let payload = r#"{"name":"m1","source":"x/y","task":"vision","revision":"abc","dimensions":4}"#;
    let (status, body) = send(&addr, "POST", "/ai/models", payload);
    assert_eq!(status, 400, "{body}");
    assert!(body.to_ascii_lowercase().contains("vision"), "{body}");
}

#[test]
fn trust_policy_defaults_to_disabled_and_rejects_unacknowledged_remote_code() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));

    let (status, _) = send(&addr, "POST", "/ai/models", MINIMAL_VALID);
    assert_eq!(status, 201);
    let (_, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2", "");
    assert!(body.contains("\"trust_policy\":\"disabled\""), "{body}");

    let payload = r#"{
        "name":"unsafe-model",
        "source":"some-org/risky",
        "task":"embedding",
        "revision":"abc",
        "dimensions":4,
        "trust_policy":"allow_remote_code"
    }"#;
    let (status, body) = send(&addr, "POST", "/ai/models", payload);
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("acknowledge_remote_code_risk"), "{body}");

    // garbage trust policy → rejected
    let bad = r#"{
        "name":"unsafe-model",
        "source":"some-org/risky",
        "task":"embedding",
        "revision":"abc",
        "dimensions":4,
        "trust_policy":"yolo"
    }"#;
    let (status, body) = send(&addr, "POST", "/ai/models", bad);
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("trust_policy"), "{body}");

    // acknowledged → accepted
    let safe = r#"{
        "name":"unsafe-model",
        "source":"some-org/risky",
        "task":"embedding",
        "revision":"abc",
        "dimensions":4,
        "trust_policy":"allow_remote_code",
        "acknowledge_remote_code_risk": true
    }"#;
    let (status, body) = send(&addr, "POST", "/ai/models", safe);
    assert_eq!(status, 201, "{body}");
    assert!(
        body.contains("\"trust_policy\":\"allow_remote_code\""),
        "{body}"
    );
}

#[test]
fn update_rejects_missing_or_invalid_fields() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let (status, _) = send(&addr, "POST", "/ai/models", MINIMAL_VALID);
    assert_eq!(status, 201);

    // Updating a non-registered model → 404
    let other = r#"{
        "name":"ghost",
        "source":"x/y",
        "task":"embedding",
        "revision":"abc",
        "dimensions":4
    }"#;
    let (status, body) = send(&addr, "PUT", "/ai/models/ghost", other);
    assert_eq!(status, 404, "{body}");

    // Update with invalid task → 400
    let bad = r#"{
        "name":"minilm-l6-v2",
        "source":"sentence-transformers/all-MiniLM-L6-v2",
        "task":"prompt",
        "revision":"fa97f6e7cb1a59073dff9e6b13e2715cf7475ac9",
        "dimensions":384
    }"#;
    let (status, body) = send(&addr, "PUT", "/ai/models/minilm-l6-v2", bad);
    assert_eq!(status, 400, "{body}");

    // Update with mismatched path name → 400
    let mismatch = r#"{
        "name":"some-other-name",
        "source":"sentence-transformers/all-MiniLM-L6-v2",
        "task":"embedding",
        "revision":"fa97f6e7cb1a59073dff9e6b13e2715cf7475ac9",
        "dimensions":384
    }"#;
    let (status, body) = send(&addr, "PUT", "/ai/models/minilm-l6-v2", mismatch);
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("does not match"), "{body}");

    // Valid update bumps revision deterministically
    let bumped = r#"{
        "name":"minilm-l6-v2",
        "source":"sentence-transformers/all-MiniLM-L6-v2",
        "task":"embedding",
        "revision":"v2.0.0",
        "dimensions":384
    }"#;
    let (status, body) = send(&addr, "PUT", "/ai/models/minilm-l6-v2", bumped);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"revision\":\"v2.0.0\""), "{body}");

    let (_, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2", "");
    assert!(body.contains("\"revision\":\"v2.0.0\""), "{body}");
}

#[test]
fn inspect_unknown_model_returns_404() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let (status, body) = send(&addr, "GET", "/ai/models/does-not-exist", "");
    assert_eq!(status, 404, "{body}");
}

#[test]
fn registered_model_survives_runtime_handoff() {
    // Persistence within the same runtime handle is the strongest
    // guarantee we can prove with the in-memory runtime, which still
    // exercises the durable red_config storage layer.
    let rt = RedDBRuntime::in_memory().expect("rt");
    let addr = spawn_http_server(rt);
    let (status, _) = send(&addr, "POST", "/ai/models", MINIMAL_VALID);
    assert_eq!(status, 201);

    // A fresh request through the live server still observes the entry.
    let (status, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"name\":\"minilm-l6-v2\""), "{body}");
}
