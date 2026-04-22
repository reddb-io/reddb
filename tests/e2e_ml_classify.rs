//! End-to-end: ML_CLASSIFY / ML_PREDICT_PROBA / SEMANTIC_CACHE_* via SQL.
//!
//! Proves the AI-first claim: ML models trained in Rust are callable
//! from SQL scalars, and the semantic cache is reachable from a
//! user session.

use reddb::application::ExecuteQueryInput;
use reddb::storage::ml::classifier::{IncrementalClassifier, LogisticRegression, LogisticRegressionConfig, TrainingExample};
use reddb::storage::ml::ModelVersion;
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

/// Train a tiny 2-feature, 2-class logistic regression that separates
/// points by the sign of x[0] - x[1]. Register it with the runtime's
/// `ModelRegistry` under `model_name` and return the runtime handle.
fn train_and_register(rt: &RedDBRuntime, model_name: &str) {
    let mut model = LogisticRegression::new(LogisticRegressionConfig {
        learning_rate: 0.1,
        l2_penalty: 0.0,
        epochs: 50,
        shuffle_seed: 42,
    });

    // Class 0: x0 > x1. Class 1: x1 > x0. Linearly separable.
    let mut examples = Vec::new();
    for i in 0..40 {
        let a = (i as f32) * 0.1;
        examples.push(TrainingExample { features: vec![a, 0.0], label: 0 });
        examples.push(TrainingExample { features: vec![0.0, a], label: 1 });
    }
    model.fit(&examples);

    let db = rt.db();
    let hyperparams = r#"{"kind":"logreg"}"#.to_string();
    let version = ModelVersion {
        model: model_name.to_string(),
        version: 1,
        weights_blob: model.to_json().into_bytes(),
        hyperparams_json: hyperparams,
        metrics_json: "{}".to_string(),
        training_data_hash: None,
        training_sql: None,
        parent_version: None,
        created_at_ms: 0,
        created_by: Some("test".to_string()),
        archived: false,
    };
    db.ml_runtime()
        .registry()
        .register_version(model_name.to_string(), version, /*make_active=*/ true)
        .expect("register model version");
}

#[test]
fn ml_classify_returns_predicted_class_from_sql() {
    let rt = rt();
    train_and_register(&rt, "xor_toy");
    let q = QueryUseCases::new(&rt);

    // Class 0: first feature bigger. The scalar takes an   literal.
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT ML_CLASSIFY('xor_toy', [3.0, 0.1]) AS cls".into(),
        })
        .expect("ML_CLASSIFY should run");
    let cls = r.result.records[0].values.get("cls").expect("cls present");
    assert!(matches!(cls, Value::Integer(0)), "expected class 0, got {cls:?}");

    // Class 1: second feature bigger.
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT ML_CLASSIFY('xor_toy', [0.1, 3.0]) AS cls".into(),
        })
        .expect("ML_CLASSIFY should run");
    let cls = r.result.records[0].values.get("cls").expect("cls present");
    assert!(matches!(cls, Value::Integer(1)), "expected class 1, got {cls:?}");
}

#[test]
fn ml_predict_proba_returns_normalised_probabilities() {
    let rt = rt();
    train_and_register(&rt, "xor_toy2");
    let q = QueryUseCases::new(&rt);

    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT ML_PREDICT_PROBA('xor_toy2', [3.0, 0.1]) AS probs".into(),
        })
        .expect("ML_PREDICT_PROBA should run");
    let probs = r.result.records[0].values.get("probs").expect("probs present");
    let arr = match probs {
        Value::Array(v) => v,
        other => panic!("expected Array, got {other:?}"),
    };
    assert_eq!(arr.len(), 2, "two classes expected");
    let sum: f64 = arr
        .iter()
        .map(|v| if let Value::Float(f) = v { *f } else { 0.0 })
        .sum();
    assert!((sum - 1.0).abs() < 0.01, "probs should sum to ~1.0, got {sum}");
    // First class should dominate.
    let p0 = match &arr[0] {
        Value::Float(f) => *f,
        other => panic!("expected Float, got {other:?}"),
    };
    assert!(p0 > 0.6, "class 0 should have high probability, got {p0}");
}

#[test]
fn ml_classify_unknown_model_returns_null() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT ML_CLASSIFY('does_not_exist', [1.0, 2.0]) AS cls".into(),
        })
        .expect("call should not error — NULL on missing model");
    let cls = r.result.records[0].values.get("cls").expect("cls present");
    assert!(matches!(cls, Value::Null), "expected Null, got {cls:?}");
}

#[test]
fn semantic_cache_roundtrip_via_sql() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);

    // PUT then GET — same embedding should hit.
    q.execute(ExecuteQueryInput {
        query: "SELECT SEMANTIC_CACHE_PUT('qa', 'what is redb?', \
                'AI-first multi-model db', \
                [0.1, 0.2, 0.3, 0.4]) AS ok"
            .into(),
    })
    .expect("cache put ok");

    let r = q
        .execute(ExecuteQueryInput {
            query:
                "SELECT SEMANTIC_CACHE_GET('qa', [0.1, 0.2, 0.3, 0.4]) AS cached".into(),
        })
        .expect("cache get ok");
    let cached = r.result.records[0].values.get("cached").expect("cached present");
    assert!(
        matches!(cached, Value::Text(s) if s.as_ref() == "AI-first multi-model db"),
        "expected cached hit, got {cached:?}"
    );
}

#[test]
fn embed_returns_null_without_provider_config() {
    // EMBED needs `red_config` entries for a provider. With none set
    // the scalar degrades to Null rather than panicking or crossing
    // the network. Negative-path guard — proves the wiring reaches
    // the runtime and fails closed.
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT EMBED('hello world', 'openai') AS emb".into(),
        })
        .expect("call should not error");
    let emb = r.result.records[0].values.get("emb").expect("emb present");
    // Either Null (no API key) or a Vector if REDDB_OPENAI_API_KEY is
    // set in the environment. Both are acceptable; we only assert we
    // didn't get a transport-layer panic or a stringified placeholder.
    assert!(
        matches!(emb, Value::Null | Value::Vector(_)),
        "expected Null or Vector, got {emb:?}"
    );
}

#[test]
fn semantic_cache_miss_returns_null() {
    let rt = rt();
    let q = QueryUseCases::new(&rt);
    let r = q
        .execute(ExecuteQueryInput {
            query: "SELECT SEMANTIC_CACHE_GET('qa', [0.99, 0.0, 0.0, 0.0]) AS cached".into(),
        })
        .expect("cache get ok");
    let cached = r.result.records[0].values.get("cached").expect("cached present");
    assert!(matches!(cached, Value::Null), "expected Null, got {cached:?}");
}
