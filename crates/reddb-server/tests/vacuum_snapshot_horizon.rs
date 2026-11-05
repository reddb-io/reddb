//! A reader must retain versions hidden by writers active at snapshot capture.
use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::{RedDBOptions, RedDBRuntime, RuntimeQueryResult};

fn execute(runtime: &RedDBRuntime, connection: u64, sql: &str) -> RuntimeQueryResult {
    set_current_connection_id(connection);
    let result = runtime.execute_query(sql);
    clear_current_connection_id();
    result.expect("valid fixture statement")
}

fn fixture() -> RedDBRuntime {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime opens");
    execute(
        &runtime,
        1,
        "CREATE TABLE snapshot_rows (id TEXT, n INTEGER)",
    );
    execute(
        &runtime,
        1,
        "INSERT INTO snapshot_rows (id, n) VALUES ('row', 1)",
    );
    runtime
}

fn assert_old_row(runtime: &RedDBRuntime) {
    let result = execute(runtime, 3, "SELECT id, n FROM snapshot_rows");
    assert_eq!(
        result.result.records.len(),
        1,
        "reader lost its visible row"
    );
    assert_eq!(
        result.result.records[0]
            .get("n")
            .expect("selected column exists")
            .to_string(),
        "1"
    );
}

fn physical_rows(runtime: &RedDBRuntime) -> usize {
    runtime
        .db()
        .store()
        .get_collection("snapshot_rows")
        .expect("fixture collection exists")
        .query_all(|_| true)
        .len()
}

#[test]
fn vacuum_retains_update_excluded_by_a_newer_snapshot() {
    let runtime = fixture();
    execute(&runtime, 2, "BEGIN");
    execute(&runtime, 3, "BEGIN ISOLATION LEVEL SNAPSHOT");
    execute(
        &runtime,
        2,
        "UPDATE snapshot_rows SET n = 2 WHERE id = 'row'",
    );
    execute(&runtime, 2, "COMMIT");
    assert_old_row(&runtime);
    execute(&runtime, 1, "VACUUM snapshot_rows");
    assert_old_row(&runtime);
    execute(&runtime, 3, "COMMIT");
    execute(&runtime, 1, "VACUUM snapshot_rows");
    assert_eq!(
        physical_rows(&runtime),
        1,
        "completed reader retained history"
    );
}

#[test]
fn vacuum_retains_delete_excluded_by_a_newer_snapshot() {
    let runtime = fixture();
    execute(&runtime, 2, "BEGIN");
    execute(&runtime, 3, "BEGIN ISOLATION LEVEL SNAPSHOT");
    execute(&runtime, 2, "DELETE FROM snapshot_rows WHERE id = 'row'");
    execute(&runtime, 2, "COMMIT");
    assert_old_row(&runtime);
    execute(&runtime, 1, "VACUUM snapshot_rows");
    assert_old_row(&runtime);
    execute(&runtime, 3, "ROLLBACK");
    execute(&runtime, 1, "VACUUM snapshot_rows");
    assert_eq!(
        physical_rows(&runtime),
        0,
        "rolled-back reader retained tombstone"
    );
}

#[test]
fn vacuum_retains_versions_written_by_a_released_savepoint() {
    let runtime = fixture();
    execute(&runtime, 2, "BEGIN");
    execute(&runtime, 2, "SAVEPOINT nested_write");
    execute(&runtime, 3, "BEGIN ISOLATION LEVEL SNAPSHOT");
    execute(
        &runtime,
        2,
        "UPDATE snapshot_rows SET n = 2 WHERE id = 'row'",
    );
    execute(&runtime, 2, "RELEASE SAVEPOINT nested_write");
    execute(&runtime, 2, "COMMIT");
    assert_old_row(&runtime);
    execute(&runtime, 1, "VACUUM snapshot_rows");
    assert_old_row(&runtime);
    execute(&runtime, 3, "COMMIT");
    execute(&runtime, 1, "VACUUM snapshot_rows");
    assert_eq!(physical_rows(&runtime), 1);
}
