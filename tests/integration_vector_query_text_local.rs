//! Issue #681 — text vector search routed through the local AI provider.
//!
//! Drives `VECTOR SEARCH ... SIMILAR TO '<text>'` with the default
//! provider set to `local`, against a registered+installed local
//! embedding model. The deterministic fake backend stands in for a
//! real candle/onnx engine so the runtime contract is exercised
//! without downloading anything.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use reddb::application::{CreateKvInput, EntityUseCases, ExecuteQueryInput, QueryUseCases};
use reddb::runtime::ai::local_embedding::{
    clear_local_embedding_backend_for_tests, install_local_embedding_backend,
    LocalEmbeddingBackend, LocalEmbeddingRequest,
};
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{RedDBError, RedDBResult, RedDBRuntime};

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
                Some(value) => unsafe {
                    std::env::set_var(key, value);
                },
                None => unsafe {
                    std::env::remove_var(key);
                },
            }
        }
    }
}

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("query should succeed: {sql}\nerror: {err:?}"))
}

fn exec_err(rt: &RedDBRuntime, sql: &str) -> RedDBError {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .err()
        .unwrap_or_else(|| panic!("query should fail: {sql}"))
}

fn text(record: &UnifiedRecord, column: &str) -> String {
    match record.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text value for {column}, got {other:?}"),
    }
}

/// Register a local embedding model entry in `red_config` and stamp it
/// `installed`. Mirrors what `POST /ai/models` + the cache pull would
/// land, without going through HTTP — the embedding routing path only
/// reads the registry KV.
fn register_installed_local_model(rt: &RedDBRuntime, name: &str, dimensions: u32) {
    let key = format!("red.config.ai.models.{name}");
    let payload = format!(
        r#"{{"name":"{name}","provider":"local","source":"sentence-transformers/all-MiniLM-L6-v2","task":"embedding","revision":"main","engine":"candle","dimensions":{dimensions},"status":"installed","pull_policy":"if_missing"}}"#
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

/// Backend that emits a fixed unit vector aligned with `[1.0, 0.0]`,
/// regardless of input. Lets the test assert which collection row the
/// search resolves to.
struct FixedBackend {
    vector: Vec<f32>,
}

impl LocalEmbeddingBackend for FixedBackend {
    fn embed(&self, request: &LocalEmbeddingRequest) -> RedDBResult<Vec<Vec<f32>>> {
        Ok(request.inputs.iter().map(|_| self.vector.clone()).collect())
    }
}

/// Process-global backend slot serialisation. Mirrors the unit-test
/// strategy in `handlers_ai.rs::tests`.
fn backend_lock() -> &'static Mutex<()> {
    static L: OnceLock<Mutex<()>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

#[test]
fn text_vector_search_routes_through_local_provider_when_default_provider_is_local() {
    let _env = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _guard = EnvGuard::set(&[
        ("REDDB_AI_PROVIDER", "local".to_string()),
        ("REDDB_AI_MODEL", "mini".to_string()),
    ]);
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
    }));

    let rt = rt();
    register_installed_local_model(&rt, "mini", 2);

    exec(
        &rt,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'match-a')",
    );
    exec(
        &rt,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([0.0, 1.0], 'match-b')",
    );

    let result = exec(
        &rt,
        "VECTOR SEARCH embeddings SIMILAR TO 'anything' LIMIT 1",
    );

    assert_eq!(result.result.records.len(), 1);
    assert_eq!(text(&result.result.records[0], "content"), "match-a");
    assert_eq!(text(&result.result.records[0], "red_entity_type"), "vector");

    clear_local_embedding_backend_for_tests();
}

#[test]
fn text_vector_search_with_local_provider_fails_on_missing_model() {
    let _env = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _guard = EnvGuard::set(&[
        ("REDDB_AI_PROVIDER", "local".to_string()),
        ("REDDB_AI_MODEL", "nope".to_string()),
    ]);
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
    }));

    let rt = rt();
    // Collection exists but the named model is not in the registry.
    exec(
        &rt,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'a')",
    );

    let err = exec_err(
        &rt,
        "VECTOR SEARCH embeddings SIMILAR TO 'whatever' LIMIT 1",
    );
    let message = format!("{err:?}");
    assert!(
        matches!(err, RedDBError::NotFound(_)),
        "expected NotFound, got: {message}"
    );
    assert!(
        message.contains("nope") && message.contains("not registered"),
        "error should name the missing model and call out registration: {message}"
    );

    clear_local_embedding_backend_for_tests();
}

#[test]
fn text_vector_search_with_local_provider_fails_on_dimension_mismatch() {
    let _env = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _guard = EnvGuard::set(&[
        ("REDDB_AI_PROVIDER", "local".to_string()),
        ("REDDB_AI_MODEL", "wide".to_string()),
    ]);
    // Backend returns a 4-d vector; the collection contract pins dim=2
    // (inferred from the first INSERT). The shape validator inside the
    // vector executor must reject the search before it scores anything.
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0, 0.0, 0.0],
    }));

    let rt = rt();
    register_installed_local_model(&rt, "wide", 4);
    // Pin the collection's dimension contract to 2 via a turbo
    // declaration. `validate_vector_query_shape` reads the contract;
    // a plain `INSERT INTO ... VECTOR` does not set it, so without an
    // explicit DDL the 4-d/2-d mismatch would silently return zero
    // matches instead of erroring.
    exec(
        &rt,
        "CREATE COLLECTION embeddings KIND vector.turbo DIM 2 METRIC COSINE",
    );
    exec(
        &rt,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'a')",
    );

    let err = exec_err(
        &rt,
        "VECTOR SEARCH embeddings SIMILAR TO 'whatever' LIMIT 1",
    );
    let message = format!("{err:?}");
    assert!(
        matches!(err, RedDBError::Query(_)),
        "expected Query error, got: {message}"
    );
    assert!(
        message.contains("dimension mismatch"),
        "error should describe a dimension mismatch: {message}"
    );

    clear_local_embedding_backend_for_tests();
}

#[test]
fn text_vector_search_with_local_provider_fails_on_uninstalled_model() {
    let _env = env_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _guard = EnvGuard::set(&[
        ("REDDB_AI_PROVIDER", "local".to_string()),
        ("REDDB_AI_MODEL", "pending".to_string()),
    ]);
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
    }));

    let rt = rt();
    // Register but leave status != "installed". The runtime path must
    // never silently fall back to a remote provider when the local
    // artifacts are missing.
    let payload = r#"{"name":"pending","provider":"local","source":"sentence-transformers/all-MiniLM-L6-v2","task":"embedding","revision":"main","engine":"candle","dimensions":2,"status":"registered","pull_policy":"never"}"#;
    EntityUseCases::new(&rt)
        .create_kv(CreateKvInput {
            collection: "red_config".to_string(),
            key: "red.config.ai.models.pending".to_string(),
            value: Value::text(payload.to_string()),
            metadata: Vec::new(),
        })
        .expect("register pending model");

    exec(
        &rt,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'a')",
    );

    let err = exec_err(
        &rt,
        "VECTOR SEARCH embeddings SIMILAR TO 'whatever' LIMIT 1",
    );
    let message = format!("{err:?}");
    assert!(
        matches!(err, RedDBError::NotFound(_)),
        "expected NotFound for uninstalled local model, got: {message}"
    );
    // No silent remote fallback: the error must name the local pull
    // path, not point operators at OpenAI / HuggingFace.
    assert!(
        !message.to_ascii_lowercase().contains("openai"),
        "{message}"
    );
    assert!(
        !message.to_ascii_lowercase().contains("huggingface"),
        "{message}"
    );
    assert!(
        message.contains("/ai/models/pending/pull"),
        "error should point at the explicit pull endpoint: {message}"
    );

    clear_local_embedding_backend_for_tests();
}
