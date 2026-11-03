use super::*;
use crate::runtime::RuntimeQueryResult;
use crate::RedDBOptions;

fn fixture() -> (RedDBRuntime, PreparedRegistry, Vec<u8>) {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(1024 * 1024))
            .expect("runtime");
    runtime
        .execute_query("CREATE TABLE owned_results (payload BLOB)")
        .expect("table");
    let payload = vec![0x71; 64 * 1024];
    runtime
        .execute_query_with_params(
            "INSERT INTO owned_results (payload) VALUES ($1)",
            &[Value::Blob(payload.clone())],
        )
        .expect("seed fits");
    (runtime, PreparedRegistry::new(), payload)
}

#[test]
fn retained_request_results_compete_for_shared_memory_and_release_on_last_drop() {
    let (runtime, prepared, payload) = fixture();
    let executor = QueryRequestExecutor::new(&runtime, &prepared);
    let request = || QueryRequest::sql("SELECT payload FROM owned_results", Vec::new());
    let mut results = Vec::new();
    let mut refused = false;
    for _ in 0..32 {
        match executor.execute(request()) {
            Ok(result) => results.push(result),
            Err(_) => {
                refused = true;
                break;
            }
        }
    }
    assert!(
        refused,
        "retained runtime results must share one budget after execution finishes"
    );
    assert!(!results.is_empty(), "a fitting read succeeds");
    let copies: Vec<_> = results.iter().cloned().collect();
    drop(results);
    assert!(
        executor.execute(request()).is_err(),
        "copies retain the result charge"
    );
    assert!(executor
        .execute(QueryRequest::sql("SELECT 1 AS healthy", Vec::new()))
        .is_ok());
    runtime.refresh_memory_accounting();
    assert!(
        runtime.memory_accounting().total_used_bytes() <= runtime.memory_budget().resolved_bytes
    );
    drop(copies);
    let retried = executor
        .execute(request())
        .expect("last owner returns headroom");
    assert_eq!(
        retried.result.records.len(),
        1,
        "refusal preserves the original row"
    );
    assert_eq!(
        retried.result.records[0].get("payload"),
        Some(&Value::Blob(payload))
    );
}

#[test]
fn request_result_clones_share_records_and_binary_payload_instead_of_deep_copying() {
    let (runtime, prepared, _) = fixture();
    let result = QueryRequestExecutor::new(&runtime, &prepared)
        .execute(QueryRequest::sql(
            "SELECT payload FROM owned_results",
            Vec::new(),
        ))
        .expect("read");
    let (copy, allocated) =
        crate::storage::no_malloc_hot_paths::measure_allocations(|| result.clone());
    assert_eq!(
        allocated.allocated_bytes, 0,
        "cloning a result must not allocate payloads or metadata"
    );
    assert_eq!(
        result.result.records.as_ptr(),
        copy.result.records.as_ptr(),
        "result clones must share their record allocation"
    );
    let (Some(Value::Blob(original)), Some(Value::Blob(cloned))) = (
        result.result.records[0].get("payload"),
        copy.result.records[0].get("payload"),
    ) else {
        panic!("binary payload");
    };
    assert_eq!(
        original.as_ptr(),
        cloned.as_ptr(),
        "binary payload shares ownership too"
    );
    drop(result);
    assert_eq!(cloned.len(), 64 * 1024);
}

#[test]
fn retained_capacity_cannot_hide_in_nested_values_graphs_vectors_or_metadata() {
    use crate::storage::memory_pools::MemoryPool;
    use crate::storage::query::unified::{
        GraphPath, MatchedNode, VectorQueryStats, VectorSearchResult,
    };
    use std::collections::HashMap;
    const LARGE: usize = 2 * 1024 * 1024;
    fn spare_text() -> String {
        let mut text = String::with_capacity(LARGE);
        text.push('x');
        text
    }
    fn add(result: &mut RuntimeQueryResult, value: Value) {
        let mut record = super::super::UnifiedRecord::new();
        record.set("payload", value);
        result.result.push(record);
    }
    let cases: &[(&str, fn(&mut RuntimeQueryResult))] = &[
        ("binary spare capacity", |result| {
            let mut value = Vec::with_capacity(LARGE);
            value.push(1);
            add(result, Value::Blob(value));
        }),
        ("vector spare capacity", |result| {
            let mut value = Vec::with_capacity(LARGE / 4);
            value.push(1.0);
            add(result, Value::Vector(value));
        }),
        ("nested array spare slots", |result| {
            let mut values = Vec::with_capacity(LARGE / std::mem::size_of::<Value>());
            values.push(Value::Integer(1));
            add(result, Value::Array(vec![Value::Array(values)]));
        }),
        ("column metadata", |result| {
            result.result.columns.push(spare_text());
        }),
        ("query metadata", |result| {
            result.query = spare_text();
        }),
        ("bookmark metadata", |result| {
            result.bookmark = Some(spare_text());
        }),
        ("notice metadata", |result| {
            result.notice = Some(spare_text());
        }),
        ("pre-serialized result", |result| {
            result.result.pre_serialized_json = Some(spare_text());
        }),
        ("graph node label", |result| {
            let mut record = super::super::UnifiedRecord::new();
            record.nodes.insert(
                "n".into(),
                MatchedNode {
                    id: "1".into(),
                    label: spare_text(),
                    node_label: String::new(),
                    properties: HashMap::new(),
                },
            );
            result.result.push(record);
        }),
        ("graph path node", |result| {
            let mut record = super::super::UnifiedRecord::new();
            record.paths.push(GraphPath {
                nodes: vec![spare_text()],
                edges: Vec::new(),
                total_weight: 0.0,
            });
            result.result.push(record);
        }),
        ("vector result metadata", |result| {
            let mut record = super::super::UnifiedRecord::new();
            record
                .vector_results
                .push(VectorSearchResult::new(1, spare_text(), 0.0));
            result.result.push(record);
        }),
        ("operator metadata", |result| {
            result.result.stats.vector = Some(VectorQueryStats {
                access_path: spare_text(),
                ..Default::default()
            });
        }),
        ("record vector spare slots", |result| {
            result.result.records =
                Vec::with_capacity(LARGE / std::mem::size_of::<super::super::UnifiedRecord>());
        }),
        ("column vector spare slots", |result| {
            result.result.columns = Vec::with_capacity(LARGE / std::mem::size_of::<String>());
        }),
        ("shared schema spare slots", |result| {
            let mut schema = Vec::with_capacity(LARGE / std::mem::size_of::<std::sync::Arc<str>>());
            schema.push(std::sync::Arc::<str>::from("payload"));
            result.result.push(super::super::UnifiedRecord::with_schema(
                std::sync::Arc::new(schema),
                vec![Value::Integer(1)],
            ));
        }),
        ("record value spare slots", |result| {
            let schema = std::sync::Arc::new(vec![std::sync::Arc::<str>::from("payload")]);
            let mut values = Vec::with_capacity(LARGE / std::mem::size_of::<Value>());
            values.push(Value::Integer(1));
            result
                .result
                .push(super::super::UnifiedRecord::with_schema(schema, values));
        }),
        ("sparse graph property buckets", |result| {
            let mut record = super::super::UnifiedRecord::new();
            let mut properties =
                HashMap::with_capacity(LARGE / std::mem::size_of::<(String, Value)>());
            properties.insert("k".into(), Value::Integer(1));
            record.nodes.insert(
                "n".into(),
                MatchedNode {
                    id: "1".into(),
                    label: "n".into(),
                    node_label: "n".into(),
                    properties,
                },
            );
            result.result.push(record);
        }),
        ("path edge properties", |result| {
            let mut record = super::super::UnifiedRecord::new();
            let mut edge =
                crate::storage::query::unified::MatchedEdge::from_tuple("a", "e", "b", 1.0);
            edge.properties
                .insert("payload".into(), Value::Json(spare_text().into_bytes()));
            record.paths.push(GraphPath {
                nodes: vec!["a".into(), "b".into()],
                edges: vec![edge],
                total_weight: 1.0,
            });
            result.result.push(record);
        }),
        ("too-deep API array", |result| {
            let mut value = Value::Null;
            for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH + 2 {
                value = Value::Array(vec![value]);
            }
            add(result, value);
        }),
    ];
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(1024 * 1024))
            .expect("runtime");
    for (name, populate) in cases {
        let mut result = RuntimeQueryResult::ok_records(
            "SELECT fixture".into(),
            vec!["payload".into()],
            Vec::new(),
            "select",
        );
        populate(&mut result);
        let error = runtime.own_query_result(result).expect_err(name);
        assert!(
            matches!(
                error,
                RedDBError::MaterializationLimitExceeded {
                    executor: super::super::owned_query_result::SHARED_RESULT_EXECUTOR,
                    ..
                }
            ),
            "{name}: {error}"
        );
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0,
            "{name}: failed admission retains nothing"
        );
    }
    let prepared = PreparedRegistry::new();
    QueryRequestExecutor::new(&runtime, &prepared)
        .execute(QueryRequest::sql("SELECT 1 AS healthy", Vec::new()))
        .expect("a healthy read still succeeds");
}

#[test]
fn admitted_result_rendering_keeps_ownership_on_success_and_unwind() {
    use crate::storage::memory_pools::MemoryPool;
    let (runtime, prepared, _) = fixture();
    let execute = || {
        QueryRequestExecutor::new(&runtime, &prepared)
            .execute(QueryRequest::sql(
                "SELECT payload FROM owned_results",
                Vec::new(),
            ))
            .expect("read")
    };
    execute().render(|value| {
        assert!(
            matches!(value, std::borrow::Cow::Borrowed(_)),
            "admitted allocations cannot move past their lease"
        );
        assert!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers)
                >= 64 * 1024
        );
    });
    assert_eq!(
        runtime
            .memory_accounting()
            .used_bytes(MemoryPool::QueryBuffers),
        0
    );
    let result = execute();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        result.render(|_| {
            panic!("render unwind fixture");
        })
    }));
    assert!(panic.is_err());
    assert_eq!(
        runtime
            .memory_accounting()
            .used_bytes(MemoryPool::QueryBuffers),
        0
    );
    execute();
}

#[test]
fn prepared_and_parameterized_requests_keep_the_same_owned_admission() {
    let (runtime, prepared, _) = fixture();
    let statement = prepared
        .prepare(
            &runtime,
            "SELECT payload FROM owned_results WHERE payload = $1",
        )
        .expect("prepare");
    let executor = QueryRequestExecutor::new(&runtime, &prepared);
    let first = executor
        .execute(QueryRequest::prepared(
            statement.id,
            vec![ParamValue::Bytes(vec![0x71; 64 * 1024])],
        ))
        .expect("prepared read");
    let second = executor
        .execute(QueryRequest::sql(
            "SELECT payload FROM owned_results WHERE payload = $1",
            vec![ParamValue::Bytes(vec![0x71; 64 * 1024])],
        ))
        .expect("parameterized read");
    assert_eq!(
        first.result.records[0].get("payload"),
        second.result.records[0].get("payload")
    );
    let pool = crate::storage::memory_pools::MemoryPool::QueryBuffers;
    let before = runtime.memory_accounting().used_bytes(pool);
    assert!(before >= 2 * 64 * 1024);
    let copy = second.clone();
    drop(second);
    assert_eq!(runtime.memory_accounting().used_bytes(pool), before);
    drop(first);
    assert!(runtime.memory_accounting().used_bytes(pool) >= 64 * 1024);
    drop(copy);
    assert_eq!(runtime.memory_accounting().used_bytes(pool), 0);
}

#[test]
fn retained_memory_bound_covers_real_allocations_for_many_short_text_values() {
    use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
    let (values, measured) = crate::storage::no_malloc_hot_paths::measure_allocations(|| {
        (0..4096).map(|_| Value::text("x")).collect::<Vec<_>>()
    });
    let schema = std::sync::Arc::new(vec![std::sync::Arc::<str>::from("payload")]);
    let record = UnifiedRecord::with_schema(schema, vec![Value::Array(values)]);
    let mut result = UnifiedResult::with_columns(vec!["payload".into()]);
    result.push(record);
    let retained_bytes = result.retained_memory_bytes();
    assert!(
        retained_bytes >= measured.allocated_bytes,
        "result allowance must cover real allocations: estimated={retained_bytes}, allocated={}",
        measured.allocated_bytes
    );
}
