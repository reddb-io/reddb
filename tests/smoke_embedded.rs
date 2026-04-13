//! Smoke tests for RedDB v1 — Embedded profile
//!
//! Validates core operations across entity domains using the embedded (in-process) profile.

use reddb::application::{
    CreateDocumentInput, CreateEdgeInput, CreateKvInput, CreateNodeInput, CreateRowInput,
    CreateVectorInput, ExecuteQueryInput, ExplainQueryInput, SearchSimilarInput,
};
use reddb::json::Value as JsonValue;
use reddb::storage::schema::Value;
use reddb::{ArtifactState, EntityUseCases, NativeUseCases, QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("failed to create in-memory runtime")
}

// ---------------------------------------------------------------------------
// Sprint 1: Health and catalog
// ---------------------------------------------------------------------------

#[test]
fn smoke_health_report() {
    let rt = rt();
    let native = NativeUseCases::new(&rt);
    let report = native.health();
    assert!(
        report.is_healthy() || matches!(report.state, reddb::HealthState::Degraded),
        "fresh runtime should be healthy or degraded, got {:?}",
        report.state
    );
}

#[test]
fn smoke_catalog_snapshot() {
    let rt = rt();
    let native = NativeUseCases::new(&rt);
    let _report = native.health();
}

// ---------------------------------------------------------------------------
// Sprint 2: Storage domains — rows
// ---------------------------------------------------------------------------

#[test]
fn smoke_row_crud() {
    let rt = rt();
    let uc = EntityUseCases::new(&rt);

    let out = uc.create_row(CreateRowInput {
        collection: "users".into(),
        fields: vec![
            ("name".into(), Value::Text("Alice".into())),
            ("age".into(), Value::Integer(30)),
        ],
        metadata: vec![],
        node_links: vec![],
        vector_links: vec![],
    });
    assert!(out.is_ok(), "create_row should succeed: {:?}", out.err());
    let _id = out.unwrap().id;
}

// ---------------------------------------------------------------------------
// Sprint 2: Storage domains — vectors
// ---------------------------------------------------------------------------

#[test]
fn smoke_vector_insert_and_search() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    for v in [
        vec![1.0f32, 0.0, 0.0],
        vec![0.0, 1.0, 0.0],
        vec![0.9, 0.1, 0.0],
    ] {
        entity
            .create_vector(CreateVectorInput {
                collection: "embeddings".into(),
                dense: v,
                content: None,
                metadata: vec![],
                link_row: None,
                link_node: None,
            })
            .unwrap();
    }

    let results = query.search_similar(SearchSimilarInput {
        collection: "embeddings".into(),
        vector: vec![1.0, 0.0, 0.0],
        k: 3,
        min_score: 0.0,
        text: None,
        provider: None,
    });
    assert!(
        results.is_ok(),
        "search_similar should succeed: {:?}",
        results.err()
    );
}

// ---------------------------------------------------------------------------
// Sprint 2: Storage domains — graph nodes/edges
// ---------------------------------------------------------------------------

#[test]
fn smoke_graph_crud() {
    let rt = rt();
    let uc = EntityUseCases::new(&rt);

    let node_a = uc
        .create_node(CreateNodeInput {
            collection: "network".into(),
            label: "host_a".into(),
            node_type: Some("Host".into()),
            properties: vec![("ip".into(), Value::Text("192.168.1.1".into()))],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .unwrap();

    let node_b = uc
        .create_node(CreateNodeInput {
            collection: "network".into(),
            label: "host_b".into(),
            node_type: Some("Host".into()),
            properties: vec![("ip".into(), Value::Text("10.0.0.1".into()))],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .unwrap();

    let edge = uc.create_edge(CreateEdgeInput {
        collection: "network".into(),
        label: "connects_to".into(),
        from: node_a.id,
        to: node_b.id,
        weight: Some(1.0),
        properties: vec![],
        metadata: vec![],
    });
    assert!(edge.is_ok(), "create_edge should succeed: {:?}", edge.err());
}

// ---------------------------------------------------------------------------
// Sprint 3: Universal query
// ---------------------------------------------------------------------------

#[test]
fn smoke_query_select() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    entity
        .create_row(CreateRowInput {
            collection: "hosts".into(),
            fields: vec![
                ("ip".into(), Value::Text("192.168.1.1".into())),
                ("os".into(), Value::Text("Linux".into())),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .unwrap();

    let result = query.execute(ExecuteQueryInput {
        query: "SELECT * FROM hosts".into(),
    });
    assert!(result.is_ok(), "SELECT should succeed: {:?}", result.err());
}

#[test]
fn smoke_query_explain_universal() {
    let rt = rt();
    let query = QueryUseCases::new(&rt);

    let explain = query.explain(ExplainQueryInput {
        query: "SELECT * FROM any".into(),
    });
    assert!(
        explain.is_ok(),
        "explain should succeed: {:?}",
        explain.err()
    );
    let explain = explain.unwrap();
    assert!(explain.is_universal, "FROM any should be universal");
}

// ---------------------------------------------------------------------------
// Sprint 4: Artifact lifecycle
// ---------------------------------------------------------------------------

#[test]
fn smoke_artifact_state_machine() {
    assert!(ArtifactState::Ready.is_queryable());
    assert!(!ArtifactState::Building.is_queryable());
    assert!(!ArtifactState::Failed.is_queryable());

    assert!(ArtifactState::Declared.can_rebuild());
    assert!(ArtifactState::Failed.can_rebuild());
    assert!(!ArtifactState::Ready.can_rebuild());

    assert_eq!(
        ArtifactState::from_build_state("ready", true),
        ArtifactState::Ready
    );
    assert_eq!(
        ArtifactState::from_build_state("ready", false),
        ArtifactState::Disabled
    );
    assert_eq!(
        ArtifactState::from_build_state("failed", true),
        ArtifactState::Failed
    );
    assert_eq!(
        ArtifactState::from_build_state("stale", true),
        ArtifactState::Stale
    );

    assert_eq!(ArtifactState::Ready.to_string(), "ready");
    assert_eq!(
        ArtifactState::RequiresRebuild.to_string(),
        "requires_rebuild"
    );
}

// ---------------------------------------------------------------------------
// Key-Value first-class API
// ---------------------------------------------------------------------------

#[test]
fn smoke_kv_crud() {
    let rt = rt();
    let uc = EntityUseCases::new(&rt);

    // Set a key
    let out = uc.create_kv(CreateKvInput {
        collection: "config".into(),
        key: "app.name".into(),
        value: Value::Text("RedDB".into()),
        metadata: vec![],
    });
    assert!(out.is_ok(), "create_kv should succeed: {:?}", out.err());

    // Get the key
    let val = uc.get_kv("config", "app.name");
    assert!(val.is_ok(), "get_kv should succeed: {:?}", val.err());
    let val = val.unwrap();
    assert!(val.is_some(), "key should exist");
    let (value, _id) = val.unwrap();
    assert!(
        matches!(value, Value::Text(ref s) if s == "RedDB"),
        "value should be RedDB"
    );

    // Delete the key
    let deleted = uc.delete_kv("config", "app.name");
    assert!(
        deleted.is_ok(),
        "delete_kv should succeed: {:?}",
        deleted.err()
    );
    assert!(deleted.unwrap(), "should have deleted something");

    // Confirm deleted
    let val = uc.get_kv("config", "app.name").unwrap();
    assert!(val.is_none(), "key should be gone after delete");
}

// ---------------------------------------------------------------------------
// Document first-class API
// ---------------------------------------------------------------------------

#[test]
fn smoke_document_crud() {
    let rt = rt();
    let uc = EntityUseCases::new(&rt);

    let mut body = reddb::json::Map::new();
    body.insert("name".into(), JsonValue::String("Alice".into()));
    body.insert("age".into(), JsonValue::Number(30.0));
    body.insert("active".into(), JsonValue::Bool(true));

    let out = uc.create_document(CreateDocumentInput {
        collection: "profiles".into(),
        body: JsonValue::Object(body),
        metadata: vec![],
        node_links: vec![],
        vector_links: vec![],
    });
    assert!(
        out.is_ok(),
        "create_document should succeed: {:?}",
        out.err()
    );

    // Query via table (documents are enriched rows)
    let result = QueryUseCases::new(&rt).execute(ExecuteQueryInput {
        query: "SELECT * FROM profiles".into(),
    });
    assert!(
        result.is_ok(),
        "query documents should succeed: {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// Vector search with HNSW indexing
// ---------------------------------------------------------------------------

#[test]
fn smoke_vector_hnsw_search() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    // Insert enough vectors to trigger HNSW (>=100 for index build)
    for i in 0..120 {
        let angle = (i as f32) * std::f32::consts::PI * 2.0 / 120.0;
        entity
            .create_vector(CreateVectorInput {
                collection: "hnsw_test".into(),
                dense: vec![angle.cos(), angle.sin(), 0.0],
                content: Some(format!("vector_{}", i)),
                metadata: vec![],
                link_row: None,
                link_node: None,
            })
            .unwrap();
    }

    // Search should use HNSW index (>100 vectors)
    let results = query.search_similar(SearchSimilarInput {
        collection: "hnsw_test".into(),
        vector: vec![1.0, 0.0, 0.0],
        k: 5,
        min_score: 0.0,
        text: None,
        provider: None,
    });
    assert!(
        results.is_ok(),
        "HNSW search should succeed: {:?}",
        results.err()
    );
    let results = results.unwrap();
    assert!(!results.is_empty(), "should find similar vectors via HNSW");
}
