//! Idle maintenance must reclaim obsolete versions without an explicit VACUUM.
use std::time::{Duration, Instant};

use reddb_server::{RedDBOptions, RedDBRuntime};

fn physical_rows(runtime: &RedDBRuntime, table: &str) -> usize {
    let mut count = 0;
    runtime
        .db()
        .store()
        .get_collection(table)
        .expect("fixture collection exists")
        .for_each_entity(|_| {
            count += 1;
            true
        });
    count
}

fn await_history_reclamation(runtime: &RedDBRuntime, table: &str, maximum: usize) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while physical_rows(runtime, table) > maximum && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let physical = physical_rows(runtime, table);
    assert!(
        physical <= maximum,
        "idle maintenance retained {physical} versions, expected <= {maximum}"
    );
}

fn value(runtime: &RedDBRuntime, connection: u64) -> String {
    use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    set_current_connection_id(connection);
    let result = runtime.execute_query("SELECT n FROM reader_rows WHERE id = 1");
    clear_current_connection_id();
    result.expect("read row").result.records[0]
        .get("n")
        .expect("value exists")
        .to_string()
}

#[test]
fn fixed_logical_data_does_not_retain_every_update_forever() {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime opens");
    runtime
        .execute_query("CREATE TABLE churn_rows (id INTEGER, n INTEGER)")
        .expect("create churn table");
    let values = (0..128)
        .map(|id| format!("({id}, 0)"))
        .collect::<Vec<_>>()
        .join(",");
    runtime
        .execute_query(&format!("INSERT INTO churn_rows (id, n) VALUES {values}"))
        .expect("seed fixed logical data");
    for _ in 0..10 {
        runtime
            .execute_query("UPDATE churn_rows SET n = n + 1")
            .expect("update fixed logical data");
    }
    let manager = runtime
        .db()
        .store()
        .get_collection("churn_rows")
        .expect("churn table exists");
    let deadline = Instant::now() + Duration::from_secs(15);
    while manager.query_all(|_| true).len() > 256 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let physical = manager.query_all(|_| true).len();
    assert!(
        physical <= 256,
        "128 live rows retained {physical} physical versions after idle maintenance"
    );
    let result = runtime
        .execute_query("SELECT COUNT(*) AS total FROM churn_rows WHERE n = 10")
        .expect("read current values");
    assert_eq!(
        result.result.records[0]
            .get("total")
            .expect("count exists")
            .to_string(),
        "128"
    );
}

#[test]
fn automatic_reclamation_preserves_an_active_snapshot_reader() {
    use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime opens");
    runtime
        .execute_query("CREATE TABLE reader_rows (id INTEGER, n INTEGER)")
        .expect("create table");
    runtime
        .execute_query("INSERT INTO reader_rows (id, n) VALUES (1, 0)")
        .expect("seed row");
    set_current_connection_id(42);
    let result = runtime.execute_query("BEGIN ISOLATION LEVEL SNAPSHOT");
    clear_current_connection_id();
    result.expect("reader begins");
    assert_eq!(value(&runtime, 42), "0");
    for _ in 0..20 {
        runtime
            .execute_query("UPDATE reader_rows SET n = n + 1 WHERE id = 1")
            .expect("update row");
    }
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        value(&runtime, 42),
        "0",
        "background cleanup changed reader visibility"
    );
    set_current_connection_id(42);
    let result = runtime.execute_query("COMMIT");
    clear_current_connection_id();
    result.expect("reader completes");
    await_history_reclamation(&runtime, "reader_rows", 1);
    assert_eq!(value(&runtime, 1), "20");
}

#[test]
fn disabled_collector_preserves_history_and_can_resume() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime opens");
    runtime
        .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
        .expect("disable collector");
    runtime
        .execute_query("CREATE TABLE reader_rows (id INTEGER, n INTEGER)")
        .expect("create table");
    runtime
        .execute_query("INSERT INTO reader_rows (id, n) VALUES (1, 0)")
        .expect("seed row");
    runtime
        .execute_query("UPDATE reader_rows SET n = 1")
        .expect("update row");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(physical_rows(&runtime, "reader_rows"), 2);
    runtime
        .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = true")
        .expect("enable collector");
    await_history_reclamation(&runtime, "reader_rows", 1);
    assert_eq!(value(&runtime, 1), "1");
}

#[test]
fn indexed_rows_survive_reclamation_checkpoint_and_reopen() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("reclaimed.rdb");
    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime opens");
        runtime
            .execute_query("CREATE TABLE reader_rows (id INTEGER, n INTEGER)")
            .expect("create table");
        runtime
            .execute_query("CREATE INDEX reader_rows_id ON reader_rows (id)")
            .expect("create index");
        runtime
            .execute_query("INSERT INTO reader_rows (id, n) VALUES (1, 0)")
            .expect("seed row");
        for _ in 0..20 {
            runtime
                .execute_query("UPDATE reader_rows SET n = n + 1 WHERE id = 1")
                .expect("update row");
        }
        await_history_reclamation(&runtime, "reader_rows", 1);
        assert_eq!(value(&runtime, 1), "20");
        runtime.flush().expect("checkpoint reclaimed state");
    }
    let restored =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen checkpoint");
    assert_eq!(value(&restored, 1), "20");
    assert_eq!(physical_rows(&restored, "reader_rows"), 1);
}

#[test]
fn aborted_indexed_writes_remain_invisible_and_the_key_can_be_reused() {
    use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime opens");
    runtime
        .execute_query("CREATE TABLE reader_rows (id INTEGER, n INTEGER)")
        .expect("create table");
    runtime
        .execute_query("CREATE UNIQUE INDEX reader_rows_id ON reader_rows (id)")
        .expect("create index");
    set_current_connection_id(42);
    runtime
        .execute_query("BEGIN")
        .expect("begin aborted insert");
    runtime
        .execute_query("INSERT INTO reader_rows (id, n) VALUES (1, 99)")
        .expect("insert tentative row");
    let result = runtime.execute_query("ROLLBACK");
    clear_current_connection_id();
    result.expect("rollback insert");
    await_history_reclamation(&runtime, "reader_rows", 0);
    assert!(runtime
        .execute_query("SELECT n FROM reader_rows WHERE id = 1")
        .expect("read after rollback")
        .result
        .records
        .is_empty());
    runtime
        .execute_query("INSERT INTO reader_rows (id, n) VALUES (1, 7)")
        .expect("reuse rolled-back unique key");
    set_current_connection_id(42);
    runtime
        .execute_query("BEGIN")
        .expect("begin aborted update");
    runtime
        .execute_query("UPDATE reader_rows SET id = 2, n = 8 WHERE id = 1")
        .expect("change indexed key tentatively");
    let result = runtime.execute_query("ROLLBACK");
    clear_current_connection_id();
    result.expect("rollback indexed update");
    await_history_reclamation(&runtime, "reader_rows", 1);
    assert_eq!(value(&runtime, 1), "7");
    assert!(runtime
        .execute_query("SELECT n FROM reader_rows WHERE id = 2")
        .expect("aborted key remains absent")
        .result
        .records
        .is_empty());
}
