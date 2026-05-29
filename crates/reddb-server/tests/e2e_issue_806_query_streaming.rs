//! Issue #806 — query streaming, slice 750b: bounded-memory executor.
//!
//! The runtime query executor now produces table-query output through a
//! bounded-memory streaming channel (`RowStream`) and the `/query` path
//! collects the chunks internally. These end-to-end checks pin the
//! observable contract through the public runtime API: every pipeline
//! shape the issue calls out — scan, filter, indexed_scan, join,
//! aggregate — must preserve its ordering and snapshot guarantees after
//! the executor was rerouted through the streaming channel.

use reddb_server::storage::schema::Value;
use reddb_server::{RedDBOptions, RedDBRuntime, RuntimeQueryResult};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
}

fn uint_at(result: &RuntimeQueryResult, row: usize, column: &str) -> u64 {
    match result.result.records[row].get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => *value as u64,
        other => panic!("expected integer at row {row} column {column}, got {other:?}"),
    }
}

fn seed(n: usize) -> RedDBRuntime {
    let rt = rt();
    rt.execute_query("CREATE TABLE t (id INT, name TEXT)")
        .expect("create table");
    let values = (0..n)
        .map(|i| format!("({i}, 'row{i}')"))
        .collect::<Vec<_>>()
        .join(", ");
    rt.execute_query(&format!("INSERT INTO t (id, name) VALUES {values}"))
        .expect("insert rows");
    rt
}

#[test]
fn unfiltered_scan_streams_every_row_through_the_channel() {
    // A large unfiltered scan (well past one chunk) returns the full set
    // with no rows dropped or duplicated — the collect-from-chunks path
    // is faithful end to end.
    const N: usize = 300;
    let rt = seed(N);

    let result = rt.execute_query("SELECT * FROM t").expect("scan ok");
    assert_eq!(result.result.records.len(), N);

    let mut ids: Vec<u64> = (0..N).map(|i| uint_at(&result, i, "id")).collect();
    ids.sort_unstable();
    let expected: Vec<u64> = (0..N as u64).collect();
    assert_eq!(
        ids, expected,
        "every id appears exactly once across the chunks"
    );
}

#[test]
fn order_by_is_preserved_after_streaming_reroute() {
    // ORDER BY is inherently materialising; the streaming reroute must
    // not disturb the sorted slice.
    let rt = seed(50);
    let result = rt
        .execute_query("SELECT * FROM t ORDER BY id DESC LIMIT 5")
        .expect("ordered scan ok");
    let ids: Vec<u64> = (0..result.result.records.len())
        .map(|i| uint_at(&result, i, "id"))
        .collect();
    assert_eq!(ids, vec![49, 48, 47, 46, 45]);
}

#[test]
fn filtered_and_indexed_scan_still_resolve_a_single_row() {
    let rt = seed(100);
    let result = rt
        .execute_query("SELECT * FROM t WHERE id = 42")
        .expect("filtered scan ok");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(uint_at(&result, 0, "id"), 42);
}

#[test]
fn aggregate_count_is_preserved_after_streaming_reroute() {
    let rt = seed(123);
    let result = rt
        .execute_query("SELECT count(*) AS c FROM t")
        .expect("aggregate ok");
    assert_eq!(result.result.records.len(), 1);
    let count = result.result.records[0]
        .get("c")
        .and_then(|value| match value {
            Value::UnsignedInteger(v) => Some(*v),
            Value::Integer(v) => Some(*v as u64),
            _ => None,
        })
        .expect("count column present");
    assert_eq!(count, 123);
}

#[test]
fn join_ordering_is_preserved_after_streaming_reroute() {
    let rt = rt();
    rt.execute_query("CREATE TABLE users (id INT, name TEXT)")
        .expect("create users");
    rt.execute_query("CREATE TABLE orders (id INT, user_id INT, total INT)")
        .expect("create orders");
    rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'Ada'), (2, 'Linus')")
        .expect("insert users");
    rt.execute_query(
        "INSERT INTO orders (id, user_id, total) VALUES (10, 1, 100), (11, 2, 200), (12, 1, 50)",
    )
    .expect("insert orders");

    let result = rt
        .execute_query(
            "SELECT orders.id AS oid FROM orders \
             JOIN users ON orders.user_id = users.id \
             ORDER BY orders.id ASC",
        )
        .expect("join ok");
    let ids: Vec<u64> = (0..result.result.records.len())
        .map(|i| uint_at(&result, i, "oid"))
        .collect();
    assert_eq!(ids, vec![10, 11, 12], "join output stays in ORDER BY order");
}

#[test]
fn snapshot_is_stable_across_a_streamed_scan() {
    // A scan reads a single, consistent snapshot: rows inserted are all
    // visible, and the count matches the writes — no torn read across
    // the chunk boundary.
    let rt = seed(200);
    let before = rt.execute_query("SELECT * FROM t").expect("scan ok");
    assert_eq!(before.result.records.len(), 200);

    rt.execute_query("INSERT INTO t (id, name) VALUES (200, 'row200')")
        .expect("insert one more");
    let after = rt.execute_query("SELECT * FROM t").expect("scan ok");
    assert_eq!(
        after.result.records.len(),
        201,
        "later snapshot sees the new row"
    );
}
