use criterion::{criterion_group, criterion_main, Criterion};
use reddb::application::{
    CreateDocumentInput, CreateEdgeInput, CreateKvInput, CreateNodeInput, CreateRowInput,
    CreateVectorInput, ExecuteQueryInput, SearchSimilarInput,
};
use reddb::storage::schema::Value;
use reddb::{EntityUseCases, QueryUseCases, RedDBRuntime};

// ---------------------------------------------------------------------------
// Row benchmarks
// ---------------------------------------------------------------------------

fn bench_row_insert(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc = EntityUseCases::new(&rt);

    c.bench_function("row_insert", |b| {
        b.iter(|| {
            uc.create_row(CreateRowInput {
                collection: "bench_rows".into(),
                fields: vec![
                    ("name".into(), Value::Text("Alice".into())),
                    ("age".into(), Value::Integer(30)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
        })
    });
}

fn bench_row_insert_batch(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc = EntityUseCases::new(&rt);

    c.bench_function("row_insert_batch_100", |b| {
        b.iter(|| {
            for i in 0..100 {
                uc.create_row(CreateRowInput {
                    collection: "bench_rows_batch".into(),
                    fields: vec![
                        ("name".into(), Value::Text(format!("User_{i}"))),
                        ("age".into(), Value::Integer(20 + (i % 50) as i64)),
                        ("active".into(), Value::Boolean(i % 2 == 0)),
                    ],
                    metadata: vec![],
                    node_links: vec![],
                    vector_links: vec![],
                })
                .unwrap();
            }
        })
    });
}

// ---------------------------------------------------------------------------
// KV benchmarks
// ---------------------------------------------------------------------------

fn bench_kv_set(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc = EntityUseCases::new(&rt);

    c.bench_function("kv_set", |b| {
        let mut counter = 0u64;
        b.iter(|| {
            uc.create_kv(CreateKvInput {
                collection: "bench_kv".into(),
                key: format!("key_{counter}"),
                value: Value::Text("some-value".into()),
                metadata: vec![],
            })
            .unwrap();
            counter += 1;
        })
    });
}

fn bench_kv_get(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc = EntityUseCases::new(&rt);

    // Pre-populate
    for i in 0..100 {
        uc.create_kv(CreateKvInput {
            collection: "bench_kv_get".into(),
            key: format!("key_{i}"),
            value: Value::Text(format!("value_{i}")),
            metadata: vec![],
        })
        .unwrap();
    }

    c.bench_function("kv_get", |b| {
        let mut idx = 0u64;
        b.iter(|| {
            let key = format!("key_{}", idx % 100);
            let _ = uc.get_kv("bench_kv_get", &key);
            idx += 1;
        })
    });
}

// ---------------------------------------------------------------------------
// Document benchmarks
// ---------------------------------------------------------------------------

fn bench_document_insert(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc = EntityUseCases::new(&rt);

    c.bench_function("document_insert", |b| {
        b.iter(|| {
            let body = reddb::json!({
                "title": "Benchmark Document",
                "category": "performance",
                "score": 42.5
            });
            uc.create_document(CreateDocumentInput {
                collection: "bench_docs".into(),
                body,
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
        })
    });
}

// ---------------------------------------------------------------------------
// Graph benchmarks
// ---------------------------------------------------------------------------

fn bench_node_insert(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc = EntityUseCases::new(&rt);

    c.bench_function("node_insert", |b| {
        b.iter(|| {
            uc.create_node(CreateNodeInput {
                collection: "bench_graph".into(),
                label: "Person".into(),
                node_type: Some("entity".into()),
                properties: vec![
                    ("name".into(), Value::Text("Alice".into())),
                    ("score".into(), Value::Float(0.95)),
                ],
                metadata: vec![],
                embeddings: vec![],
                table_links: vec![],
                node_links: vec![],
            })
            .unwrap();
        })
    });
}

fn bench_edge_insert(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc = EntityUseCases::new(&rt);

    // Pre-create two nodes
    let node_a = uc
        .create_node(CreateNodeInput {
            collection: "bench_graph_edges".into(),
            label: "NodeA".into(),
            node_type: None,
            properties: vec![],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .unwrap();

    let node_b = uc
        .create_node(CreateNodeInput {
            collection: "bench_graph_edges".into(),
            label: "NodeB".into(),
            node_type: None,
            properties: vec![],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .unwrap();

    c.bench_function("edge_insert", |b| {
        b.iter(|| {
            uc.create_edge(CreateEdgeInput {
                collection: "bench_graph_edges".into(),
                label: "KNOWS".into(),
                from: node_a.id,
                to: node_b.id,
                weight: Some(1.0),
                properties: vec![],
                metadata: vec![],
            })
            .unwrap();
        })
    });
}

// ---------------------------------------------------------------------------
// Vector benchmarks
// ---------------------------------------------------------------------------

fn bench_vector_insert(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc = EntityUseCases::new(&rt);

    c.bench_function("vector_insert_128d", |b| {
        let dense: Vec<f32> = (0..128).map(|i| (i as f32) * 0.01).collect();
        b.iter(|| {
            uc.create_vector(CreateVectorInput {
                collection: "bench_vectors".into(),
                dense: dense.clone(),
                content: Some("benchmark vector".into()),
                metadata: vec![],
                link_row: None,
                link_node: None,
            })
            .unwrap();
        })
    });
}

// ---------------------------------------------------------------------------
// Query benchmarks
// ---------------------------------------------------------------------------

fn bench_query_select(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);

    // Pre-populate with 100 rows
    for i in 0..100 {
        uc_entity
            .create_row(CreateRowInput {
                collection: "bench_select".into(),
                fields: vec![
                    ("name".into(), Value::Text(format!("User_{i}"))),
                    ("age".into(), Value::Integer(18 + (i % 60) as i64)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }

    c.bench_function("query_select_all", |b| {
        b.iter(|| {
            uc_query
                .execute(ExecuteQueryInput {
                    query: "SELECT * FROM bench_select".into(),
                })
                .unwrap();
        })
    });
}

fn bench_query_select_filter(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);

    // Pre-populate with 100 rows
    for i in 0..100 {
        uc_entity
            .create_row(CreateRowInput {
                collection: "bench_filter".into(),
                fields: vec![
                    ("name".into(), Value::Text(format!("User_{i}"))),
                    ("age".into(), Value::Integer(18 + (i % 60) as i64)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }

    c.bench_function("query_select_filter", |b| {
        b.iter(|| {
            uc_query
                .execute(ExecuteQueryInput {
                    query: "SELECT * FROM bench_filter WHERE age > 50".into(),
                })
                .unwrap();
        })
    });
}

fn bench_query_universal(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);

    // Pre-populate with mixed entity types
    for i in 0..30 {
        uc_entity
            .create_row(CreateRowInput {
                collection: "bench_universal".into(),
                fields: vec![("idx".into(), Value::Integer(i))],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }
    for i in 0..20 {
        uc_entity
            .create_node(CreateNodeInput {
                collection: "bench_universal".into(),
                label: format!("Node_{i}"),
                node_type: Some("entity".into()),
                properties: vec![],
                metadata: vec![],
                embeddings: vec![],
                table_links: vec![],
                node_links: vec![],
            })
            .unwrap();
    }
    let dense: Vec<f32> = (0..64).map(|i| (i as f32) * 0.02).collect();
    for _ in 0..10 {
        uc_entity
            .create_vector(CreateVectorInput {
                collection: "bench_universal".into(),
                dense: dense.clone(),
                content: None,
                metadata: vec![],
                link_row: None,
                link_node: None,
            })
            .unwrap();
    }

    c.bench_function("query_select_universal", |b| {
        b.iter(|| {
            uc_query
                .execute(ExecuteQueryInput {
                    query: "SELECT * FROM any".into(),
                })
                .unwrap();
        })
    });
}

// ---------------------------------------------------------------------------
// Vector search benchmarks
// ---------------------------------------------------------------------------

fn bench_vector_search_brute(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);

    // Pre-populate with 50 vectors (below HNSW threshold, brute-force path)
    for i in 0..50 {
        let dense: Vec<f32> = (0..128)
            .map(|d| ((i * 128 + d) as f32 * 0.001).sin())
            .collect();
        uc_entity
            .create_vector(CreateVectorInput {
                collection: "bench_vec_brute".into(),
                dense,
                content: None,
                metadata: vec![],
                link_row: None,
                link_node: None,
            })
            .unwrap();
    }

    let query_vec: Vec<f32> = (0..128).map(|d| (d as f32 * 0.005).cos()).collect();

    c.bench_function("vector_search_brute_50", |b| {
        b.iter(|| {
            uc_query
                .search_similar(SearchSimilarInput {
                    collection: "bench_vec_brute".into(),
                    vector: query_vec.clone(),
                    k: 10,
                    min_score: 0.0,
                    text: None,
                    provider: None,
                })
                .unwrap();
        })
    });
}

fn bench_vector_search_hnsw(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);

    // Pre-populate with 200 vectors (above 100 threshold, HNSW path)
    for i in 0..200 {
        let dense: Vec<f32> = (0..128)
            .map(|d| ((i * 128 + d) as f32 * 0.001).sin())
            .collect();
        uc_entity
            .create_vector(CreateVectorInput {
                collection: "bench_vec_hnsw".into(),
                dense,
                content: None,
                metadata: vec![],
                link_row: None,
                link_node: None,
            })
            .unwrap();
    }

    let query_vec: Vec<f32> = (0..128).map(|d| (d as f32 * 0.005).cos()).collect();

    c.bench_function("vector_search_hnsw_200", |b| {
        b.iter(|| {
            uc_query
                .search_similar(SearchSimilarInput {
                    collection: "bench_vec_hnsw".into(),
                    vector: query_vec.clone(),
                    k: 10,
                    min_score: 0.0,
                    text: None,
                    provider: None,
                })
                .unwrap();
        })
    });
}

// ---------------------------------------------------------------------------
// Criterion groups
// ---------------------------------------------------------------------------

criterion_group!(
    entity_benches,
    bench_row_insert,
    bench_row_insert_batch,
    bench_kv_set,
    bench_kv_get,
    bench_document_insert,
    bench_node_insert,
    bench_edge_insert,
    bench_vector_insert,
);

fn bench_query_5k_point(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);
    for i in 0..5000 {
        uc_entity
            .create_row(CreateRowInput {
                collection: "users5k".into(),
                fields: vec![
                    ("name".into(), Value::Text(format!("User_{i}"))),
                    ("age".into(), Value::Integer(18 + (i % 63) as i64)),
                    (
                        "city".into(),
                        Value::Text(["NYC", "London", "Tokyo", "Paris", "Berlin"][i % 5].into()),
                    ),
                    ("email".into(), Value::Text(format!("u{i}@t.com"))),
                    ("score".into(), Value::Float(i as f64 * 0.02)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }
    c.bench_function("query_5k_point_lookup", |b| {
        let mut id = 1u64;
        b.iter(|| {
            uc_query
                .execute(ExecuteQueryInput {
                    query: format!("SELECT * FROM users5k WHERE _entity_id = {id}"),
                })
                .unwrap();
            id = (id % 5000) + 1;
        })
    });
}

fn bench_query_5k_range(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);
    for i in 0..5000 {
        uc_entity
            .create_row(CreateRowInput {
                collection: "users5kr".into(),
                fields: vec![
                    ("name".into(), Value::Text(format!("User_{i}"))),
                    ("age".into(), Value::Integer(18 + (i % 63) as i64)),
                    (
                        "city".into(),
                        Value::Text(["NYC", "London", "Tokyo", "Paris", "Berlin"][i % 5].into()),
                    ),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }
    c.bench_function("query_5k_range", |b| {
        b.iter(|| {
            uc_query
                .execute(ExecuteQueryInput {
                    query: "SELECT * FROM users5kr WHERE age BETWEEN 25 AND 55".into(),
                })
                .unwrap();
        })
    });
}

fn bench_query_5k_filtered(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);
    for i in 0..5000 {
        uc_entity
            .create_row(CreateRowInput {
                collection: "users5kf".into(),
                fields: vec![
                    ("name".into(), Value::Text(format!("User_{i}"))),
                    ("age".into(), Value::Integer(18 + (i % 63) as i64)),
                    (
                        "city".into(),
                        Value::Text(["NYC", "London", "Tokyo", "Paris", "Berlin"][i % 5].into()),
                    ),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }
    c.bench_function("query_5k_filtered", |b| {
        b.iter(|| {
            uc_query
                .execute(ExecuteQueryInput {
                    query: "SELECT * FROM users5kf WHERE city = 'NYC' AND age > 30".into(),
                })
                .unwrap();
        })
    });
}

criterion_group!(
    query_benches,
    bench_query_select,
    bench_query_select_filter,
    bench_query_universal,
    bench_vector_search_brute,
    bench_vector_search_hnsw,
    bench_query_5k_point,
    bench_query_5k_range,
    bench_query_5k_filtered,
);

criterion_main!(entity_benches, query_benches);
