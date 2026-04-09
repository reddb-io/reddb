use criterion::{criterion_group, criterion_main, Criterion};
use reddb::application::{
    CreateEdgeInput, CreateKvInput, CreateNodeInput, CreateRowInput, CreateVectorInput,
    ExecuteQueryInput, SearchSimilarInput,
};
use reddb::health::HealthProvider;
use reddb::storage::schema::Value;
use reddb::{EntityUseCases, QueryUseCases, RedDBRuntime, RuntimeEntityPort, RuntimeQueryPort};

// ---------------------------------------------------------------------------
// Embedded profile: insert + query cycle
// ---------------------------------------------------------------------------

fn bench_embedded_insert_query_cycle(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);

    c.bench_function("embedded_insert_query_cycle", |b| {
        b.iter(|| {
            // Insert a row
            uc_entity
                .create_row(CreateRowInput {
                    collection: "cycle_rows".into(),
                    fields: vec![
                        ("name".into(), Value::Text("Alice".into())),
                        ("score".into(), Value::Integer(99)),
                    ],
                    metadata: vec![],
                    node_links: vec![],
                    vector_links: vec![],
                })
                .unwrap();

            // Query it back
            uc_query
                .execute(ExecuteQueryInput {
                    query: "SELECT * FROM cycle_rows".into(),
                })
                .unwrap();
        })
    });
}

// ---------------------------------------------------------------------------
// Embedded profile: vector insert + search cycle
// ---------------------------------------------------------------------------

fn bench_embedded_vector_cycle(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);

    // Seed with some initial vectors so search has data to work with
    for i in 0..20 {
        let dense: Vec<f32> = (0..64)
            .map(|d| ((i * 64 + d) as f32 * 0.01).sin())
            .collect();
        uc_entity
            .create_vector(CreateVectorInput {
                collection: "cycle_vectors".into(),
                dense,
                content: None,
                metadata: vec![],
                link_row: None,
                link_node: None,
            })
            .unwrap();
    }

    let query_vec: Vec<f32> = (0..64).map(|d| (d as f32 * 0.03).cos()).collect();

    c.bench_function("embedded_vector_cycle", |b| {
        b.iter(|| {
            // Insert one more vector
            let dense: Vec<f32> = (0..64).map(|d| (d as f32 * 0.02).sin()).collect();
            uc_entity
                .create_vector(CreateVectorInput {
                    collection: "cycle_vectors".into(),
                    dense,
                    content: Some("cycle doc".into()),
                    metadata: vec![],
                    link_row: None,
                    link_node: None,
                })
                .unwrap();

            // Search similar
            uc_query
                .search_similar(SearchSimilarInput {
                    collection: "cycle_vectors".into(),
                    vector: query_vec.clone(),
                    k: 5,
                    min_score: 0.0,
                })
                .unwrap();
        })
    });
}

// ---------------------------------------------------------------------------
// Embedded profile: graph cycle (2 nodes + 1 edge + neighborhood query)
// ---------------------------------------------------------------------------

fn bench_embedded_graph_cycle(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);

    c.bench_function("embedded_graph_cycle", |b| {
        b.iter(|| {
            // Create two nodes
            let node_a = uc_entity
                .create_node(CreateNodeInput {
                    collection: "cycle_graph".into(),
                    label: "PersonA".into(),
                    node_type: Some("entity".into()),
                    properties: vec![("role".into(), Value::Text("admin".into()))],
                    metadata: vec![],
                    embeddings: vec![],
                    table_links: vec![],
                    node_links: vec![],
                })
                .unwrap();

            let node_b = uc_entity
                .create_node(CreateNodeInput {
                    collection: "cycle_graph".into(),
                    label: "PersonB".into(),
                    node_type: Some("entity".into()),
                    properties: vec![("role".into(), Value::Text("user".into()))],
                    metadata: vec![],
                    embeddings: vec![],
                    table_links: vec![],
                    node_links: vec![],
                })
                .unwrap();

            // Create edge between them
            uc_entity
                .create_edge(CreateEdgeInput {
                    collection: "cycle_graph".into(),
                    label: "KNOWS".into(),
                    from: node_a.id,
                    to: node_b.id,
                    weight: Some(1.0),
                    properties: vec![],
                    metadata: vec![],
                })
                .unwrap();

            // Query the graph
            uc_query
                .execute(ExecuteQueryInput {
                    query: "SELECT * FROM cycle_graph".into(),
                })
                .unwrap();
        })
    });
}

// ---------------------------------------------------------------------------
// Embedded profile: KV set + get + delete cycle
// ---------------------------------------------------------------------------

fn bench_embedded_kv_cycle(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc = EntityUseCases::new(&rt);

    c.bench_function("embedded_kv_cycle", |b| {
        let mut counter = 0u64;
        b.iter(|| {
            let key = format!("cycle_key_{counter}");

            // Set
            uc.create_kv(CreateKvInput {
                collection: "cycle_kv".into(),
                key: key.clone(),
                value: Value::Text("cycle-value".into()),
                metadata: vec![],
            })
            .unwrap();

            // Get
            let _ = uc.get_kv("cycle_kv", &key);

            // Delete
            let _ = uc.delete_kv("cycle_kv", &key);

            counter += 1;
        })
    });
}

// ---------------------------------------------------------------------------
// Embedded profile: mixed workload (50% reads + 50% writes interleaved)
// ---------------------------------------------------------------------------

fn bench_embedded_mixed_workload(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);

    // Pre-populate so reads have data
    for i in 0..50 {
        uc_entity
            .create_row(CreateRowInput {
                collection: "mixed_workload".into(),
                fields: vec![
                    ("name".into(), Value::Text(format!("Seed_{i}"))),
                    ("score".into(), Value::Integer(i)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap();
    }

    c.bench_function("embedded_mixed_workload_100ops", |b| {
        b.iter(|| {
            for i in 0..50 {
                // Write
                uc_entity
                    .create_row(CreateRowInput {
                        collection: "mixed_workload".into(),
                        fields: vec![
                            ("name".into(), Value::Text(format!("Mixed_{i}"))),
                            ("score".into(), Value::Integer(i + 1000)),
                        ],
                        metadata: vec![],
                        node_links: vec![],
                        vector_links: vec![],
                    })
                    .unwrap();

                // Read
                uc_query
                    .execute(ExecuteQueryInput {
                        query: "SELECT * FROM mixed_workload LIMIT 10".into(),
                    })
                    .unwrap();
            }
        })
    });
}

// ---------------------------------------------------------------------------
// Serverless profile: cold start (runtime creation)
// ---------------------------------------------------------------------------

fn bench_serverless_startup(c: &mut Criterion) {
    c.bench_function("serverless_cold_start", |b| {
        b.iter(|| {
            let _rt = RedDBRuntime::in_memory().unwrap();
        })
    });
}

// ---------------------------------------------------------------------------
// Serverless profile: warmup (health + catalog snapshot)
// ---------------------------------------------------------------------------

fn bench_serverless_warmup(c: &mut Criterion) {
    let rt = RedDBRuntime::in_memory().unwrap();

    // Populate with some data to make warmup realistic
    let uc = EntityUseCases::new(&rt);
    for i in 0..20 {
        uc.create_row(CreateRowInput {
            collection: "warmup_data".into(),
            fields: vec![("idx".into(), Value::Integer(i))],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .unwrap();
    }

    c.bench_function("serverless_warmup", |b| {
        b.iter(|| {
            // Health check (serverless readiness probe)
            let _report = rt.health();

            // Catalog snapshot (serverless state sync)
            let _catalog = rt.catalog();
        })
    });
}

// ---------------------------------------------------------------------------
// Criterion groups
// ---------------------------------------------------------------------------

criterion_group!(
    embedded_profiles,
    bench_embedded_insert_query_cycle,
    bench_embedded_vector_cycle,
    bench_embedded_graph_cycle,
    bench_embedded_kv_cycle,
    bench_embedded_mixed_workload,
);

criterion_group!(
    serverless_profiles,
    bench_serverless_startup,
    bench_serverless_warmup,
);

criterion_main!(embedded_profiles, serverless_profiles);
