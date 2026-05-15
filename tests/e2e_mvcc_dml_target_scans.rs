//! MVCC-correct table-row DML target selection.

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

fn row_count(rt: &RedDBRuntime, sql: &str) -> usize {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .result
        .records
        .len()
}

#[test]
fn snapshot_update_targets_the_same_indexed_row_version_select_sees() {
    let rt = rt();
    set_current_connection_id(51201);
    exec(
        &rt,
        "CREATE TABLE mvcc_dml_update (id INT, v INT, touched INT)",
    );
    exec(
        &rt,
        "INSERT INTO mvcc_dml_update (id, v, touched) VALUES (1, 10, 0)",
    );
    exec(
        &rt,
        "CREATE INDEX idx_mvcc_dml_update_id ON mvcc_dml_update (id) USING HASH",
    );

    set_current_connection_id(51202);
    exec(&rt, "BEGIN");
    assert_eq!(
        single_i64(&rt, "SELECT v FROM mvcc_dml_update WHERE id = 1", "v"),
        10
    );

    set_current_connection_id(51203);
    exec(&rt, "UPDATE mvcc_dml_update SET v = 99 WHERE id = 1");

    set_current_connection_id(51202);
    assert_eq!(
        single_i64(&rt, "SELECT v FROM mvcc_dml_update WHERE id = 1", "v"),
        10
    );
    let updated = rt
        .execute_query("UPDATE mvcc_dml_update SET touched = 1 WHERE id = 1")
        .expect("snapshot update");
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(
        single_i64(&rt, "SELECT v FROM mvcc_dml_update WHERE id = 1", "v"),
        10,
        "UPDATE must target the row version visible to the statement snapshot"
    );
    exec(&rt, "ROLLBACK");

    clear_current_connection_id();
}

#[test]
fn snapshot_delete_targets_the_same_indexed_row_version_select_sees() {
    let rt = rt();
    set_current_connection_id(51211);
    exec(&rt, "CREATE TABLE mvcc_dml_delete (id INT, v INT)");
    exec(&rt, "INSERT INTO mvcc_dml_delete (id, v) VALUES (1, 10)");
    exec(
        &rt,
        "CREATE INDEX idx_mvcc_dml_delete_id ON mvcc_dml_delete (id) USING HASH",
    );

    set_current_connection_id(51212);
    exec(&rt, "BEGIN");
    assert_eq!(
        single_i64(&rt, "SELECT v FROM mvcc_dml_delete WHERE id = 1", "v"),
        10
    );

    set_current_connection_id(51213);
    exec(&rt, "UPDATE mvcc_dml_delete SET v = 99 WHERE id = 1");

    set_current_connection_id(51212);
    assert_eq!(
        single_i64(&rt, "SELECT v FROM mvcc_dml_delete WHERE id = 1", "v"),
        10
    );
    let deleted = rt
        .execute_query("DELETE FROM mvcc_dml_delete WHERE id = 1")
        .expect("snapshot delete");
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(
        row_count(&rt, "SELECT v FROM mvcc_dml_delete WHERE id = 1"),
        0,
        "DELETE must remove the row version visible to the statement snapshot"
    );
    exec(&rt, "ROLLBACK");

    clear_current_connection_id();
}
