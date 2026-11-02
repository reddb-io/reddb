use super::BinaryQueryResponse;
use crate::runtime::resource_limits::ResourceLimits;
use crate::runtime::RedDBRuntime;
use crate::storage::memory_pools::MemoryPool;
use crate::wire::query_direct::try_handle_query_binary_direct;
use crate::RedDBOptions;
use reddb_types::Value;

fn fixture(result_limit: Option<usize>) -> RedDBRuntime {
    let runtime = RedDBRuntime::with_pool_and_limits(
        RedDBOptions::in_memory().with_memory_budget(4 * 1024 * 1024),
        crate::runtime::ConnectionPoolConfig::default(),
        ResourceLimits {
            max_result_bytes: result_limit.map(|value| value as u64),
            ..Default::default()
        },
    )
    .unwrap();
    for sql in [
        "CREATE TABLE admission (id INT, age INT, city TEXT, score FLOAT, payload TEXT)",
        "CREATE INDEX admission_id ON admission (id) USING HASH",
        "CREATE INDEX admission_age ON admission (age) USING BTREE",
        "CREATE INDEX admission_city_age ON admission (city, age) USING BTREE",
    ] {
        runtime.execute_query(sql).unwrap();
    }
    for id in 1..=3 {
        runtime.execute_query_with_params(
            "INSERT INTO admission (id, age, city, score, payload) VALUES ($1, 30, 'NYC', $2, $3)",
            &[Value::Integer(id), Value::Float(id as f64 * 10.0), Value::text("x".repeat(64 * 1024))],
        ).unwrap();
    }
    runtime
}

const QUERIES: &[&str] = &[
    "SELECT id, payload FROM admission LIMIT 3",
    "SELECT id, payload FROM admission WHERE id = 2",
    "SELECT id, payload FROM admission WHERE age BETWEEN 20 AND 40",
    "SELECT id, payload FROM admission WHERE city = 'NYC' AND age > 20",
    "SELECT id, payload FROM admission WHERE city = 'NYC' AND age BETWEEN 20 AND 40 AND score > 0 ORDER BY score DESC LIMIT 3",
    "SELECT id AS selected, payload FROM admission WHERE age BETWEEN 20 AND 40 LIMIT 3",
    "SELECT * FROM admission WHERE age BETWEEN 20 AND 40 LIMIT 3",
];

fn decoded(response: &[u8]) -> (Vec<String>, Vec<Vec<Value>>) {
    assert_eq!(response[4], reddb_wire::legacy::MSG_RESULT);
    assert_eq!(
        u32::from_le_bytes(response[..4].try_into().unwrap()) as usize,
        response.len() - 4
    );
    let body = &response[5..];
    let count = u16::from_le_bytes(body[..2].try_into().unwrap()) as usize;
    let mut position = 2;
    let mut columns = Vec::new();
    for _ in 0..count {
        let length = u16::from_le_bytes(body[position..position + 2].try_into().unwrap()) as usize;
        position += 2;
        columns.push(String::from_utf8(body[position..position + length].to_vec()).unwrap());
        position += length;
    }
    let rows = u32::from_le_bytes(body[position..position + 4].try_into().unwrap());
    position += 4;
    let mut values = Vec::new();
    for _ in 0..rows {
        values.push(
            (0..count)
                .map(|_| crate::wire::protocol::decode_value(body, &mut position))
                .collect(),
        );
    }
    assert_eq!(position, body.len());
    (columns, values)
}

#[test]
fn all_direct_shapes_refuse_under_pressure_then_retry_without_changing_results() {
    let runtime = fixture(None);
    for sql in QUERIES {
        let response = try_handle_query_binary_direct(&runtime, sql).expect(sql);
        let expected = decoded(&response);
        drop(response);
        runtime.refresh_memory_accounting();
        let available =
            runtime.memory_budget().resolved_bytes - runtime.memory_accounting().total_used_bytes();
        let held = runtime
            .serialize_query_buffer(available as usize - 16 * 1024, 0, |_| {})
            .unwrap();
        let response =
            try_handle_query_binary_direct(&runtime, sql).expect("denial cannot fall back");
        assert_eq!(response[4], reddb_wire::legacy::MSG_ERROR, "{sql}");
        assert!(String::from_utf8_lossy(&response[5..]).contains("memory"));
        drop(response);
        drop(held);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0,
            "{sql}"
        );
        let response = try_handle_query_binary_direct(&runtime, sql).expect(sql);
        assert_eq!(decoded(&response), expected, "{sql}");
        assert!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers)
                >= response.len() as u64
        );
        drop(response);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }
}

#[test]
fn direct_shapes_discard_accepted_prefix_when_result_limit_is_exceeded() {
    // One row fits. Multirow scans must discard their accepted prefix; the
    // ordered path must also release all individually encoded row owners.
    let runtime = fixture(Some(80 * 1024));
    for sql in QUERIES.iter().filter(|sql| !sql.contains("id = 2")) {
        let response = try_handle_query_binary_direct(&runtime, sql).expect(sql);
        assert_eq!(response[4], reddb_wire::legacy::MSG_ERROR, "{sql}");
        assert!(String::from_utf8_lossy(&response[5..]).contains("size limit"));
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }
    let response = try_handle_query_binary_direct(&runtime, QUERIES[1]).unwrap();
    assert_eq!(decoded(&response).1.len(), 1);
}

#[test]
fn direct_empty_results_and_zero_limits_keep_admitted_headers() {
    let runtime = fixture(None);
    for sql in [
        "SELECT id, payload FROM admission LIMIT 0",
        "SELECT id, payload FROM admission WHERE id = 999",
        "SELECT id, payload FROM admission WHERE age BETWEEN 100 AND 200",
        "SELECT id, payload FROM admission WHERE city = 'NYC' AND age > 200",
        "SELECT id, payload FROM admission WHERE city = 'NYC' AND age BETWEEN 20 AND 40 AND score > 100 ORDER BY score DESC LIMIT 3",
    ] {
        let response = try_handle_query_binary_direct(&runtime, sql).expect(sql);
        let (columns, rows) = decoded(&response);
        assert_eq!(columns, ["id", "payload"]);
        assert!(rows.is_empty());
        assert!(matches!(response, BinaryQueryResponse::Admitted(_)));
        drop(response);
        assert_eq!(runtime.memory_accounting().used_bytes(MemoryPool::QueryBuffers), 0);
    }
}

#[test]
fn ordered_results_retain_candidate_order_and_response_clone_ownership() {
    let runtime = fixture(None);
    let response = try_handle_query_binary_direct(&runtime, QUERIES[4]).unwrap();
    let (_, rows) = decoded(&response);
    assert_eq!(
        rows.iter().map(|row| &row[0]).collect::<Vec<_>>(),
        [&Value::Integer(3), &Value::Integer(2), &Value::Integer(1)]
    );
    let BinaryQueryResponse::Admitted(bytes) = response else {
        panic!("direct scan must admit response")
    };
    let clone = bytes.clone();
    let charge = runtime
        .memory_accounting()
        .used_bytes(MemoryPool::QueryBuffers);
    drop(bytes);
    runtime.refresh_memory_accounting();
    assert_eq!(
        runtime
            .memory_accounting()
            .used_bytes(MemoryPool::QueryBuffers),
        charge
    );
    drop(clone);
    assert_eq!(
        runtime
            .memory_accounting()
            .used_bytes(MemoryPool::QueryBuffers),
        0
    );
}
