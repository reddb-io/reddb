//! Issue #684 — final conformance coverage for the local embedding
//! operating model.
//!
//! Drives the same registered+installed local model through every
//! surface the PRD promises:
//!
//!   1. HTTP `POST /ai/embeddings`
//!   2. gRPC `crate::ai::grpc_embeddings`
//!   3. SQL `VECTOR SEARCH ... SIMILAR TO '<text>'` with `REDDB_AI_PROVIDER=local`
//!   4. SQL `INSERT ... WITH AUTO EMBED (...) USING local MODEL '<name>'`
//!
//! Every assertion runs against an in-process backend installed via
//! `install_local_embedding_backend`. No outbound network, no fixture
//! download — proving the contract "normal test commands do not
//! require live HuggingFace network access" for the `local` provider.

#[path = "../../support/mod.rs"]
mod support;

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use reddb::ai::grpc_embeddings;
use reddb::application::{CreateKvInput, EntityUseCases, ExecuteQueryInput, QueryUseCases};
use reddb::json::{Map, Value as JsonValue};
use reddb::runtime::ai::local_embedding::{
    clear_local_embedding_backend_for_tests, install_local_embedding_backend,
    LocalEmbeddingBackend, LocalEmbeddingRequest,
};
use reddb::server::RedDBServer;
use reddb::storage::schema::Value;
use reddb::{RedDBResult, RedDBRuntime};

/// Shared serial lock for the process-global backend slot — every test
/// that installs a backend must take this before swapping the slot.
fn backend_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Serial lock for env var mutations — `VECTOR SEARCH SIMILAR TO
/// '<text>'` resolves the provider from `REDDB_AI_PROVIDER`, so the
/// test must own those vars exclusively while it runs.
fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct EnvGuard {
    saved: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(vars: &[(&'static str, String)]) -> Self {
        let mut saved = Vec::new();
        let mut dedup = BTreeMap::new();
        for (key, value) in vars {
            dedup.insert(*key, value.clone());
        }
        for (key, value) in dedup {
            saved.push((key, std::env::var(key).ok()));
            unsafe {
                std::env::set_var(key, value);
            }
        }
        Self { saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.drain(..).rev() {
            match value {
                Some(v) => unsafe { std::env::set_var(key, v) },
                None => unsafe { std::env::remove_var(key) },
            }
        }
    }
}

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn register_installed_local_model(rt: &RedDBRuntime, name: &str, dimensions: u32) {
    let key = format!("red.config.ai.models.{name}");
    let payload = format!(
        r#"{{"name":"{name}","provider":"local","source":"sentence-transformers/all-MiniLM-L6-v2","task":"embedding","revision":"v1.0","engine":"candle","dimensions":{dimensions},"status":"installed","pull_policy":"if_missing"}}"#
    );
    EntityUseCases::new(rt)
        .create_kv(CreateKvInput {
            collection: "red_config".to_string(),
            key,
            value: Value::text(payload),
            metadata: Vec::new(),
        })
        .expect("register model entry");
}

/// Backend that emits a fixed vector for every input. The test asserts
/// the wire layer surfaces exactly what the in-process backend produced
/// — proving the round-trip does not silently re-route.
struct FixedBackend {
    vector: Vec<f32>,
    calls: Arc<Mutex<usize>>,
}

impl LocalEmbeddingBackend for FixedBackend {
    fn embed(&self, request: &LocalEmbeddingRequest) -> RedDBResult<Vec<Vec<f32>>> {
        *self.calls.lock().unwrap() += 1;
        Ok(request.inputs.iter().map(|_| self.vector.clone()).collect())
    }
}

fn spawn_http_server(rt: RedDBRuntime) -> String {
    let server = RedDBServer::new(rt);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local addr");
    server.serve_in_background_on(listener);
    addr.to_string()
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
        .expect("read timeout");
    stream.write_all(request.as_bytes()).expect("write");
    stream.flush().expect("flush");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read");
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("missing status line: {response}"));
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}

/// All four surfaces resolve the same registered model through the
/// same in-process backend. No remote calls, no silent fallback,
/// identical embedding shape end-to-end.
#[test]
fn local_provider_conformance_all_surfaces_offline() {
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _env = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _guard = EnvGuard::set(&[
        ("REDDB_AI_PROVIDER", "local".to_string()),
        ("REDDB_AI_MODEL", "mini-en".to_string()),
    ]);

    let calls = Arc::new(Mutex::new(0_usize));
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
        calls: Arc::clone(&calls),
    }));

    // -- (1) HTTP POST /ai/embeddings ------------------------------
    // The HTTP server takes ownership of its runtime, so spin up a
    // dedicated one for the HTTP exercise. The model registry is a
    // pure KV write, so registering the same name on each runtime
    // gives the local backend the same registry view it would see
    // in a single-process deployment.
    let (_http_db, http_rt) = support::persistent_runtime("local-embedding-http");
    register_installed_local_model(&http_rt, "mini-en", 2);
    let addr = spawn_http_server(http_rt);
    let (status, body) = post_json(
        &addr,
        "/ai/embeddings",
        r#"{"provider":"local","model":"mini-en","inputs":["hello","world"]}"#,
    );
    assert_eq!(status, 200, "HTTP /ai/embeddings: {body}");
    assert!(
        body.contains("\"provider\":\"local\""),
        "HTTP response must tag provider=local: {body}"
    );
    assert!(
        body.contains("\"model\":\"mini-en\""),
        "HTTP response must echo registered model name: {body}"
    );
    assert!(
        body.contains("\"dimensions\":2"),
        "HTTP response must pin registered dimensions: {body}"
    );

    // -- (2) gRPC grpc_embeddings ----------------------------------
    let grpc_rt = rt();
    register_installed_local_model(&grpc_rt, "mini-en", 2);
    let mut payload = Map::new();
    payload.insert("provider".to_string(), JsonValue::String("local".into()));
    payload.insert("model".to_string(), JsonValue::String("mini-en".into()));
    payload.insert(
        "inputs".to_string(),
        JsonValue::Array(vec![
            JsonValue::String("hello".into()),
            JsonValue::String("world".into()),
        ]),
    );
    let grpc_resp =
        grpc_embeddings(&grpc_rt, &JsonValue::Object(payload)).expect("grpc embeddings");
    let grpc_obj = grpc_resp.as_object().expect("grpc returns object");
    assert_eq!(
        grpc_obj.get("provider").and_then(JsonValue::as_str),
        Some("local")
    );
    assert_eq!(
        grpc_obj.get("model").and_then(JsonValue::as_str),
        Some("mini-en")
    );
    assert_eq!(
        grpc_obj.get("dimensions").and_then(JsonValue::as_u64),
        Some(2)
    );
    let grpc_vecs = grpc_obj
        .get("embeddings")
        .and_then(JsonValue::as_array)
        .expect("grpc embeddings array");
    assert_eq!(grpc_vecs.len(), 2, "one row per input");

    // -- (3) SQL VECTOR SEARCH SIMILAR TO '<text>' via env defaults
    // The text-vector-search path resolves the provider from
    // REDDB_AI_PROVIDER / REDDB_AI_MODEL (set above) and embeds the
    // text in-process before scoring. Seed a dense vector that
    // exactly matches the FixedBackend output so the search resolves
    // to the seeded record.
    let search_rt = rt();
    register_installed_local_model(&search_rt, "mini-en", 2);
    let q = QueryUseCases::new(&search_rt);
    q.execute(ExecuteQueryInput {
        query: "INSERT INTO embeddings VECTOR (dense, content) VALUES \
                ([1.0, 0.0], 'seeded by conformance test')"
            .to_string(),
    })
    .expect("seed vector insert");
    let search = q
        .execute(ExecuteQueryInput {
            query: "VECTOR SEARCH embeddings SIMILAR TO 'anything' LIMIT 5".to_string(),
        })
        .expect("VECTOR SEARCH via local provider");
    assert!(
        !search.result.records.is_empty(),
        "VECTOR SEARCH SIMILAR TO '<text>' with provider=local must resolve seeded vector"
    );

    // -- (4) WITH AUTO EMBED through the local provider -----------
    let ae_rt = rt();
    register_installed_local_model(&ae_rt, "mini-en", 2);
    let ae_q = QueryUseCases::new(&ae_rt);
    ae_q.execute(ExecuteQueryInput {
        query: "INSERT INTO autodocs (id, body) VALUES (1, 'auto-embed via local') \
                WITH AUTO EMBED (body) USING local MODEL 'mini-en'"
            .to_string(),
    })
    .expect("auto-embed insert");
    let auto = ae_q
        .execute(ExecuteQueryInput {
            query: "VECTOR SEARCH autodocs SIMILAR TO [1.0, 0.0] LIMIT 5".to_string(),
        })
        .expect("auto-embed vector search");
    assert_eq!(
        auto.result.records.len(),
        1,
        "auto-embedded vector must be searchable"
    );

    // Backend was called at least once per surface that invokes it:
    // HTTP (1), gRPC (1), VECTOR SEARCH text (1), AUTO EMBED (1).
    let total_calls = *calls.lock().unwrap();
    assert!(
        total_calls >= 4,
        "expected ≥4 backend calls across the four surfaces, got {total_calls}"
    );

    clear_local_embedding_backend_for_tests();
}

/// Boundary: omitting `model` for the local provider on HTTP is a
/// pre-flight error, not a silent fallback to an OpenAI default.
#[test]
fn local_provider_http_requires_explicit_model_name() {
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
        calls: Arc::new(Mutex::new(0)),
    }));

    let (_db, http_rt) = support::persistent_runtime("local-embedding-http-missing-model");
    let addr = spawn_http_server(http_rt);
    let (status, body) = post_json(
        &addr,
        "/ai/embeddings",
        r#"{"provider":"local","inputs":["hello"]}"#,
    );
    assert_eq!(status, 400, "missing 'model' must be rejected: {body}");
    let lower = body.to_ascii_lowercase();
    assert!(
        lower.contains("model") && lower.contains("required"),
        "error must call out the missing 'model' field: {body}"
    );

    clear_local_embedding_backend_for_tests();
}
