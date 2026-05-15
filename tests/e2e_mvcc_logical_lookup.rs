//! MVCC-correct logical table-row lookups.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn single_i64(rt: &RedDBRuntime, sql: &str, column: &str) -> i64 {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert_eq!(result.result.records.len(), 1, "{sql}");
    match result.result.records[0].get(column) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => *value as i64,
        other => panic!("expected integer {column}, got {other:?}"),
    }
}

fn single_u64(rt: &RedDBRuntime, sql: &str, column: &str) -> u64 {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert_eq!(result.result.records.len(), 1, "{sql}");
    match result.result.records[0].get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => *value as u64,
        other => panic!("expected integer {column}, got {other:?}"),
    }
}

#[test]
fn historical_snapshot_logical_row_lookup_agrees_with_scan() {
    let rt = rt();
    set_current_connection_id(44301);
    exec(&rt, "CREATE TABLE mvcc_lookup_update (id INT, v INT)");
    exec(&rt, "INSERT INTO mvcc_lookup_update (id, v) VALUES (1, 10)");
    let rid = single_u64(
        &rt,
        "SELECT rid FROM mvcc_lookup_update WHERE id = 1",
        "rid",
    );

    set_current_connection_id(44302);
    exec(&rt, "BEGIN");
    assert_eq!(
        single_i64(&rt, "SELECT v FROM mvcc_lookup_update WHERE id = 1", "v"),
        10
    );

    set_current_connection_id(44303);
    exec(&rt, "UPDATE mvcc_lookup_update SET v = 99 WHERE id = 1");

    set_current_connection_id(44302);
    let lookup_v = single_i64(
        &rt,
        &format!("SELECT v FROM mvcc_lookup_update WHERE rid = {rid} OFFSET 0"),
        "v",
    );
    let scan_v = single_i64(&rt, "SELECT v FROM mvcc_lookup_update WHERE id = 1", "v");
    assert_eq!(lookup_v, scan_v);
    assert_eq!(lookup_v, 10);
    exec(&rt, "ROLLBACK");

    clear_current_connection_id();
}
