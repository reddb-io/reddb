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

use std::sync::{Arc, Mutex};

use reddb::application::{CreateKvInput, EntityUseCases, ExecuteQueryInput, QueryUseCases};
use reddb::runtime::ai::cdc_enrichment::{CdcEnrichmentConsumer, EnrichmentConfig};
use reddb::runtime::ai::local_embedding::{
    clear_local_embedding_backend_for_tests, install_local_embedding_backend,
    LocalEmbeddingBackend, LocalEmbeddingRequest,
};
use reddb::storage::schema::Value;
use reddb::{RedDBError, RedDBResult, RedDBRuntime};

use super::support::backend_lock;

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

/// Backend that fails its first `fail_remaining` calls before producing a
/// fixed vector. Drives the consumer's retry → dead-letter path with a
/// controllable provider outage.
struct FailingBackend {
    fail_remaining: Arc<Mutex<u32>>,
    vector: Vec<f32>,
}

impl LocalEmbeddingBackend for FailingBackend {
    fn embed(&self, request: &LocalEmbeddingRequest) -> RedDBResult<Vec<Vec<f32>>> {
        let mut remaining = self.fail_remaining.lock().unwrap();
        if *remaining > 0 {
            *remaining -= 1;
            return Err(RedDBError::Query(
                "mock embedding provider is down".to_string(),
            ));
        }
        Ok(request.inputs.iter().map(|_| self.vector.clone()).collect())
    }
}

/// Run a query and report how many records it surfaced, treating any error
/// (e.g. a collection with no attached vectors yet) as zero hits — a
/// pending row is simply not returned by `VECTOR SEARCH`.
fn search_hits(rt: &RedDBRuntime, sql: &str) -> usize {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .map(|result| result.result.records.len())
        .unwrap_or(0)
}

/// `CREATE TABLE docs ... WITH (EMBED (...))` declaring a per-collection
/// embed policy over the local provider (issue #1271 DDL, #1272 enrichment).
fn create_docs_with_embed_policy(rt: &RedDBRuntime) {
    exec(
        rt,
        "CREATE TABLE docs (id INT, body TEXT) WITH ( \
           EMBED (fields = ('body'), provider = 'local', model = 'mini') \
         )",
    );
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

/// Issue #1272: a collection with an embed policy auto-vectorises the
/// declared fields *asynchronously* over CDC — the INSERT itself carries no
/// `WITH AUTO EMBED` clause and does not block on the provider. The row is
/// excluded from `VECTOR SEARCH` until the enrichment consumer attaches the
/// vector, then included once attached.
#[test]
fn auto_embed_over_cdc_attaches_after_commit_and_excludes_pending() {
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
        calls: Arc::new(Mutex::new(Vec::new())),
    }));

    let rt = rt();
    register_installed_local_model(&rt, "mini", 2);
    create_docs_with_embed_policy(&rt);

    // Plain INSERT — the policy drives enrichment; no per-request clause.
    exec(&rt, "INSERT INTO docs (id, body) VALUES (1, 'alpha')");

    // Before the consumer runs, the row has no vector and is therefore
    // excluded from vector search (it is pending).
    assert_eq!(
        search_hits(&rt, "VECTOR SEARCH docs SIMILAR TO [1.0, 0.0] LIMIT 5"),
        0,
        "row must be excluded from vector search while enrichment is pending"
    );

    let mut consumer = CdcEnrichmentConsumer::with_defaults();
    let stats = consumer.tick(&rt, 0).expect("enrichment tick");
    assert!(stats.ingested >= 1, "insert event should be ingested");
    assert_eq!(stats.attached, 1, "one vector should be attached");

    // Once attached, the row is searchable.
    let result = exec(&rt, "VECTOR SEARCH docs SIMILAR TO [1.0, 0.0] LIMIT 5");
    assert_eq!(
        result.result.records.len(),
        1,
        "row must be included in vector search once enrichment attaches"
    );

    clear_local_embedding_backend_for_tests();
}

/// Issue #1272: an update to an embedded field re-vectorises over CDC.
#[test]
fn auto_embed_over_cdc_revectorizes_on_update() {
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    let calls = Arc::new(Mutex::new(Vec::new()));
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
        calls: Arc::clone(&calls),
    }));

    let rt = rt();
    register_installed_local_model(&rt, "mini", 2);
    create_docs_with_embed_policy(&rt);

    exec(&rt, "INSERT INTO docs (id, body) VALUES (1, 'alpha')");
    let mut consumer = CdcEnrichmentConsumer::with_defaults();
    consumer.tick(&rt, 0).expect("initial enrichment tick");
    let calls_after_insert = calls.lock().unwrap().len();
    assert!(calls_after_insert >= 1, "insert should embed the row");

    // Mutate the embedded field — the consumer must re-embed it.
    exec(&rt, "UPDATE docs SET body = 'beta' WHERE id = 1");
    let stats = consumer.tick(&rt, 1).expect("update enrichment tick");
    assert!(
        stats.attached >= 1,
        "update to an embedded field should re-vectorise: {stats:?}"
    );
    let observed: Vec<Vec<String>> = calls.lock().unwrap().clone();
    assert!(
        observed
            .iter()
            .any(|inputs| inputs == &vec!["beta".to_string()]),
        "the updated field text must be re-embedded: {observed:?}"
    );

    clear_local_embedding_backend_for_tests();
}

/// Issue #1272: provider failure retries with backoff and dead-letters
/// after a bounded number of attempts; the re-drive path then re-enqueues
/// the work and a healthy provider completes it.
#[test]
fn auto_embed_over_cdc_retries_then_dead_letters_then_redrives() {
    let _bg = backend_lock().lock().unwrap_or_else(|p| p.into_inner());
    install_local_embedding_backend(Arc::new(FailingBackend {
        // Always fail through the dead-letter phase.
        fail_remaining: Arc::new(Mutex::new(1_000)),
        vector: vec![1.0, 0.0],
    }));

    let rt = rt();
    register_installed_local_model(&rt, "mini", 2);
    create_docs_with_embed_policy(&rt);

    exec(&rt, "INSERT INTO docs (id, body) VALUES (1, 'alpha')");

    let mut consumer = CdcEnrichmentConsumer::new(EnrichmentConfig {
        max_attempts: 3,
        base_backoff_ms: 10,
        poll_max: 1024,
    });

    // Attempt 1 fails → scheduled with backoff (not_before = 10).
    let s1 = consumer.tick(&rt, 0).expect("tick 1");
    assert_eq!(s1.retried, 1, "first failure retries: {s1:?}");
    assert_eq!(consumer.pending_len(), 1, "row stays pending after retry");
    assert!(consumer.dead_letters().is_empty());

    // Backoff is honoured: a tick before `not_before` does nothing.
    let s_early = consumer.tick(&rt, 5).expect("early tick");
    assert_eq!(
        s_early,
        Default::default(),
        "backoff must defer the retry: {s_early:?}"
    );
    assert_eq!(consumer.pending_len(), 1);

    // Attempt 2 fails → backoff grows (not_before = 30).
    let s2 = consumer.tick(&rt, 10).expect("tick 2");
    assert_eq!(s2.retried, 1, "second failure retries: {s2:?}");

    // Attempt 3 exhausts the budget → dead-lettered.
    let s3 = consumer.tick(&rt, 30).expect("tick 3");
    assert_eq!(s3.dead_lettered, 1, "third failure dead-letters: {s3:?}");
    assert_eq!(
        consumer.pending_len(),
        0,
        "dead-lettered work leaves pending"
    );
    assert_eq!(consumer.dead_letters().len(), 1);
    assert_eq!(consumer.dead_letters()[0].collection, "docs");
    assert_eq!(consumer.dead_letters()[0].attempts, 3);
    assert_eq!(
        search_hits(&rt, "VECTOR SEARCH docs SIMILAR TO [1.0, 0.0] LIMIT 5"),
        0,
        "a dead-lettered row never enters vector search"
    );

    // Provider recovers; ops re-drive re-enqueues the dead-letter.
    install_local_embedding_backend(Arc::new(FixedBackend {
        vector: vec![1.0, 0.0],
        calls: Arc::new(Mutex::new(Vec::new())),
    }));
    let redriven = consumer.redrive();
    assert_eq!(redriven, 1, "re-drive returns the dead-letter to pending");
    assert!(consumer.dead_letters().is_empty());
    assert_eq!(consumer.pending_len(), 1);

    let s4 = consumer.tick(&rt, 40).expect("tick after redrive");
    assert_eq!(
        s4.attached, 1,
        "healthy provider completes the work: {s4:?}"
    );
    assert_eq!(
        search_hits(&rt, "VECTOR SEARCH docs SIMILAR TO [1.0, 0.0] LIMIT 5"),
        1,
        "re-driven row is searchable once enriched"
    );

    clear_local_embedding_backend_for_tests();
}
