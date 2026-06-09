//! Local AI embedding model artifact cache tests (#679).
//!
//! Exercises the pull / inspect / drop lifecycle layered over the
//! #678 registry. Artifact acquisition is fixture-based — these
//! tests never reach out to HuggingFace.

#[allow(dead_code)]
mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
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
        .set_read_timeout(Some(Duration::from_secs(10)))
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

fn unique_temp_dir(label: &str) -> support::TempDataDir {
    support::temp_data_dir(&format!("ai-cache-{label}"))
}

fn make_fixture(label: &str) -> support::TempDataDir {
    let dir = unique_temp_dir(label);
    fs::write(dir.join("config.json"), b"{\"model_type\":\"bert\"}").unwrap();
    fs::write(dir.join("model.safetensors"), b"fake-weights-bytes").unwrap();
    fs::write(dir.join("tokenizer.json"), b"{\"vocab\":[]}").unwrap();
    dir
}

fn register(addr: &str) {
    let (status, body) = send(addr, "POST", "/ai/models", MINIMAL_VALID);
    assert_eq!(status, 201, "register failed: {body}");
}

fn configure_cache_dir(addr: &str, path: &Path) {
    let body = format!(r#"{{"value":"{}"}}"#, path.display());
    let (status, _) = send(addr, "PUT", "/config/red.config.ai.local.cache_dir", &body);
    assert_eq!(status, 200);
}

#[test]
fn pull_installs_artifact_from_fixture_and_writes_manifest() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let cache_dir = unique_temp_dir("pull_cache");
    configure_cache_dir(&addr, &cache_dir);
    register(&addr);

    // Cache should report missing before pull.
    let (status, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2/cache", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"missing\""), "{body}");

    let fixture = make_fixture("pull");
    let body_json = format!(r#"{{"fixture_dir":"{}"}}"#, fixture.display());
    let (status, body) = send(&addr, "POST", "/ai/models/minilm-l6-v2/pull", &body_json);
    assert_eq!(status, 200, "pull failed: {body}");
    assert!(body.contains("\"status\":\"installed\""), "{body}");
    assert!(body.contains("\"task\":\"embedding\""), "{body}");
    assert!(body.contains("\"engine\":\"candle\""), "{body}");
    assert!(body.contains("\"dimensions\":384"), "{body}");
    assert!(
        body.contains("\"revision\":\"fa97f6e7cb1a59073dff9e6b13e2715cf7475ac9\""),
        "{body}"
    );
    assert!(body.contains("\"sha256\""), "{body}");
    assert!(body.contains("\"installed_at_unix_ms\""), "{body}");

    // Cache report and on-disk manifest.
    let (status, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2/cache", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"installed\""), "{body}");
    assert!(body.contains("\"footprint_bytes\""), "{body}");

    // The registry-level inspect reflects installed status.
    let (status, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"installed\""), "{body}");
    assert!(body.contains("\"installed_at_unix_ms\""), "{body}");

    let manifest = cache_dir.join("minilm-l6-v2").join("manifest.json");
    assert!(manifest.exists(), "manifest must exist at {manifest:?}");
    let manifest_text = fs::read_to_string(&manifest).unwrap();
    assert!(manifest_text.contains("\"source\""), "{manifest_text}");
    assert!(manifest_text.contains("\"sha256\""), "{manifest_text}");
}

#[test]
fn pull_without_fixture_returns_actionable_error() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let cache_dir = unique_temp_dir("no_fixture_cache");
    configure_cache_dir(&addr, &cache_dir);
    register(&addr);

    let (status, body) = send(&addr, "POST", "/ai/models/minilm-l6-v2/pull", "{}");
    assert_eq!(status, 400, "{body}");
    let lower = body.to_ascii_lowercase();
    assert!(
        lower.contains("fixture_dir") || lower.contains("huggingface"),
        "should mention fixture_dir or huggingface: {body}"
    );
}

#[test]
fn pull_with_missing_fixture_dir_returns_400() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let cache_dir = unique_temp_dir("missing_fixture_cache");
    configure_cache_dir(&addr, &cache_dir);
    register(&addr);

    let bogus = std::env::temp_dir().join(format!("does_not_exist_{}", std::process::id()));
    let body_json = format!(r#"{{"fixture_dir":"{}"}}"#, bogus.display());
    let (status, body) = send(&addr, "POST", "/ai/models/minilm-l6-v2/pull", &body_json);
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("does not exist") || body.contains("not a directory"),
        "{body}"
    );

    // Cache stays missing.
    let (status, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2/cache", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"missing\""), "{body}");
}

#[test]
fn pull_against_unregistered_model_returns_404() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let cache_dir = unique_temp_dir("unreg_cache");
    configure_cache_dir(&addr, &cache_dir);
    let fixture = make_fixture("unreg");
    let body_json = format!(r#"{{"fixture_dir":"{}"}}"#, fixture.display());
    let (status, body) = send(&addr, "POST", "/ai/models/ghost/pull", &body_json);
    assert_eq!(status, 404, "{body}");
}

#[test]
fn cache_drop_removes_artifacts_but_keeps_registration() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let cache_dir = unique_temp_dir("drop_cache");
    configure_cache_dir(&addr, &cache_dir);
    register(&addr);

    let fixture = make_fixture("drop");
    let body_json = format!(r#"{{"fixture_dir":"{}"}}"#, fixture.display());
    let (status, _) = send(&addr, "POST", "/ai/models/minilm-l6-v2/pull", &body_json);
    assert_eq!(status, 200);

    let model_dir = cache_dir.join("minilm-l6-v2");
    assert!(model_dir.is_dir(), "{model_dir:?}");

    let (status, body) = send(&addr, "DELETE", "/ai/models/minilm-l6-v2/cache", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"removed\":true"), "{body}");
    assert!(body.contains("\"status\":\"registered\""), "{body}");
    assert!(!model_dir.exists(), "model dir must be gone: {model_dir:?}");

    // Registration still present after drop.
    let (status, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"name\":\"minilm-l6-v2\""), "{body}");
    assert!(body.contains("\"status\":\"registered\""), "{body}");
    assert!(!body.contains("\"installed_at_unix_ms\""), "{body}");

    // Cache status reports missing now.
    let (status, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2/cache", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"missing\""), "{body}");

    // Drop again is idempotent: returns 200 and removed=false.
    let (status, body) = send(&addr, "DELETE", "/ai/models/minilm-l6-v2/cache", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"removed\":false"), "{body}");
}

#[test]
fn corrupt_manifest_makes_cache_unhealthy() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let cache_dir = unique_temp_dir("corrupt_cache");
    configure_cache_dir(&addr, &cache_dir);
    register(&addr);

    let fixture = make_fixture("corrupt");
    let body_json = format!(r#"{{"fixture_dir":"{}"}}"#, fixture.display());
    let (status, _) = send(&addr, "POST", "/ai/models/minilm-l6-v2/pull", &body_json);
    assert_eq!(status, 200);

    // Corrupt the manifest on disk.
    let manifest = cache_dir.join("minilm-l6-v2").join("manifest.json");
    fs::write(&manifest, b"not json at all").unwrap();

    let (status, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2/cache", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"unhealthy\""), "{body}");
    assert!(body.contains("manifest"), "{body}");
}

#[test]
fn missing_artifact_file_makes_cache_unhealthy() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let cache_dir = unique_temp_dir("missing_file_cache");
    configure_cache_dir(&addr, &cache_dir);
    register(&addr);

    let fixture = make_fixture("missing_file");
    let body_json = format!(r#"{{"fixture_dir":"{}"}}"#, fixture.display());
    let (status, _) = send(&addr, "POST", "/ai/models/minilm-l6-v2/pull", &body_json);
    assert_eq!(status, 200);

    let _ = fs::remove_file(cache_dir.join("minilm-l6-v2").join("model.safetensors"));
    let (status, body) = send(&addr, "GET", "/ai/models/minilm-l6-v2/cache", "");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"unhealthy\""), "{body}");
    assert!(body.contains("model.safetensors"), "{body}");
}

#[test]
fn pull_is_idempotent_and_overwrites_existing_install() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let cache_dir = unique_temp_dir("idempotent_cache");
    configure_cache_dir(&addr, &cache_dir);
    register(&addr);

    let first = make_fixture("idem_one");
    let body_json = format!(r#"{{"fixture_dir":"{}"}}"#, first.display());
    let (status, _) = send(&addr, "POST", "/ai/models/minilm-l6-v2/pull", &body_json);
    assert_eq!(status, 200);

    let second = unique_temp_dir("idem_two");
    fs::write(second.join("config.json"), b"{\"different\":true}").unwrap();
    fs::write(second.join("model.safetensors"), b"different-weights").unwrap();
    let body_json = format!(r#"{{"fixture_dir":"{}"}}"#, second.display());
    let (status, body) = send(&addr, "POST", "/ai/models/minilm-l6-v2/pull", &body_json);
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"installed\""), "{body}");

    let config = fs::read_to_string(cache_dir.join("minilm-l6-v2").join("config.json")).unwrap();
    assert!(
        config.contains("different"),
        "second pull must replace files: {config}"
    );

    // tokenizer.json from the first install must be gone after replacement.
    assert!(!cache_dir
        .join("minilm-l6-v2")
        .join("tokenizer.json")
        .exists());
}

#[test]
fn fixture_dir_via_config_works_when_request_omits_it() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let cache_dir = unique_temp_dir("config_fixture_cache");
    configure_cache_dir(&addr, &cache_dir);
    register(&addr);

    let fixture = make_fixture("config_fixture");
    let body = format!(r#"{{"value":"{}"}}"#, fixture.display());
    let (status, _) = send(
        &addr,
        "PUT",
        "/config/red.config.ai.local.fixture_dir",
        &body,
    );
    assert_eq!(status, 200);

    let (status, body) = send(&addr, "POST", "/ai/models/minilm-l6-v2/pull", "{}");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("\"status\":\"installed\""), "{body}");
}

#[test]
fn cache_status_404_for_unregistered_model() {
    let addr = spawn_http_server(RedDBRuntime::in_memory().expect("rt"));
    let (status, body) = send(&addr, "GET", "/ai/models/does-not-exist/cache", "");
    assert_eq!(status, 404, "{body}");
}
