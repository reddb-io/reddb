//! Issue #682 — INSERT ... WITH AUTO EMBED routed through the local
//! AI provider.
//!
//! Drives `INSERT ... WITH AUTO EMBED (...) USING local MODEL '<name>'`
//! against a registered+installed local embedding model. A controllable
//! in-process backend stands in for a real candle/onnx engine so the
//! write contract is exercised without downloading anything.
//!
//! Covers:
//!   - happy path: local backend produces vectors, vectors persisted
//!     into the target collection, dense + content surfaced through
//!     `VECTOR SEARCH`.
//!   - pre-flight: missing model fails the statement before any row
//!     is written (no partial writes).
//!   - pre-flight: dimension mismatch between backend output and
//!     registry fails before vector persistence; rows already written
//!     match existing OpenAI-path semantics, so the assertion is on
//!     the deterministic error variant.

use std::sync::{Arc, Mutex, OnceLock};

use reddb::application::{CreateKvInput, EntityUseCases, ExecuteQueryInput, QueryUseCases};
use reddb::runtime::ai::local_embedding::{
    clear_local_embedding_backend_for_tests, install_local_embedding_backend,
    LocalEmbeddingBackend, LocalEmbeddingRequest,
};
use reddb::storage::schema::Value;
use reddb::{RedDBError, RedDBResult, RedDBRuntime};

fn backend_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
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

/// Backend that emits a fixed vector for every input. Lets the tests
/// assert that the auto-embed write path persisted exactly what the
/// local backend returned.
struct FixedBackend {
    vector: Vec<f32>,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl LocalEmbeddingBackend for FixedBackend {
    fn embed(&self, request: &LocalEmbeddingRequest) -> RedDBResult<Vec<Vec<f32>>> {
        self.calls.lock().unwrap().push(request.inputs.clone());
        Ok(request.inputs.iter().map(|_| self.vector.clone()).collect())
    }
}

#[test]
fn auto_embed_insert_with_local_provider_persists_backend_vectors() {
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    let calls = Arc::new(Mutex::new(Vec::new()));
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
        calls: Arc::clone(&calls),
    }));

    let rt = rt();
    register_installed_local_model(&rt, "mini", 2);

    exec(
        &rt,
        "INSERT INTO docs (id, body) VALUES (1, 'alpha') \
         WITH AUTO EMBED (body) USING local MODEL 'mini'",
    );

    // Single round-trip into the in-process local backend: one call,
    // one input.
    let observed = calls.lock().unwrap().clone();
    assert_eq!(observed.len(), 1, "expected one batched local call");
    assert_eq!(observed[0], vec!["alpha".to_string()]);

    // The vector persisted by the auto-embed write path is reachable
    // via VECTOR SEARCH against the registered collection. The fixed
    // backend always returns `[1.0, 0.0]`, so a search against the
    // same vector resolves to the inserted document.
    let result = exec(&rt, "VECTOR SEARCH docs SIMILAR TO [1.0, 0.0] LIMIT 5");
    assert_eq!(
        result.result.records.len(),
        1,
        "auto-embedded vector should be searchable"
    );

    clear_local_embedding_backend_for_tests();
}

#[test]
fn auto_embed_insert_with_local_provider_fails_before_writing_when_model_missing() {
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
        calls: Arc::new(Mutex::new(Vec::new())),
    }));

    let rt = rt();
    // Deliberately do NOT register a model — the pre-flight must
    // refuse the statement and leave `docs` empty.

    let err = exec_err(
        &rt,
        "INSERT INTO docs (id, body) VALUES (1, 'x') \
         WITH AUTO EMBED (body) USING local MODEL 'nope'",
    );
    let message = format!("{err:?}");
    assert!(
        matches!(err, RedDBError::NotFound(_)),
        "expected NotFound, got: {message}"
    );
    assert!(
        message.contains("nope") && message.contains("not registered"),
        "error should name missing model and mention registration: {message}"
    );

    // No row was written — the target collection is untouched.
    // (`docs` was never created because pre-flight short-circuited
    // before the auto-create on first insert.)
    let select_err = exec_err(&rt, "SELECT * FROM docs");
    assert!(
        matches!(select_err, RedDBError::NotFound(_)),
        "no rows must be written when local pre-flight fails: {select_err:?}"
    );

    clear_local_embedding_backend_for_tests();
}

#[test]
fn auto_embed_insert_with_local_provider_fails_on_backend_dimension_mismatch() {
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    // Backend returns a 4-d vector; the registry pins dim=2. The
    // shape validator inside `embed_local_with_db` must reject the
    // backend output before any `create_vector` runs.
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0, 0.0, 0.0],
        calls: Arc::new(Mutex::new(Vec::new())),
    }));

    let rt = rt();
    register_installed_local_model(&rt, "narrow", 2);

    let err = exec_err(
        &rt,
        "INSERT INTO docs (id, body) VALUES (1, 'x') \
         WITH AUTO EMBED (body) USING local MODEL 'narrow'",
    );
    let message = format!("{err:?}");
    assert!(
        matches!(err, RedDBError::Query(_)),
        "expected Query error, got: {message}"
    );
    assert!(
        message.contains("registered with dimensions=2"),
        "error should call out the dimension contract: {message}"
    );

    clear_local_embedding_backend_for_tests();
}

#[test]
fn auto_embed_insert_with_local_provider_requires_model_clause() {
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
        calls: Arc::new(Mutex::new(Vec::new())),
    }));

    let rt = rt();
    // The local provider has no implicit default model — omitting
    // MODIFIER must be a deterministic, pre-flight error rather than
    // a silent fallback to an OpenAI default.
    let err = exec_err(
        &rt,
        "INSERT INTO docs (id, body) VALUES (1, 'x') \
         WITH AUTO EMBED (body) USING local",
    );
    let message = format!("{err:?}");
    assert!(
        matches!(err, RedDBError::Query(_)),
        "expected Query error, got: {message}"
    );
    assert!(
        message.contains("MODEL") && message.contains("local"),
        "error should explain the local provider needs MODEL '<name>': {message}"
    );

    let select_err = exec_err(&rt, "SELECT * FROM docs");
    assert!(
        matches!(select_err, RedDBError::NotFound(_)),
        "no rows must be written when MODEL clause is missing: {select_err:?}"
    );

    clear_local_embedding_backend_for_tests();
}
