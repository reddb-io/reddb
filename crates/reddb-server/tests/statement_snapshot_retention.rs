//! VACUUM must retain the snapshot of an ordinary query waiting for a lock.
use std::sync::Arc;
use std::time::{Duration, Instant};

use reddb_server::runtime::lock_manager::LockMode;
use reddb_server::runtime::locking::{LockerGuard, Resource};
use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::{RedDBOptions, RedDBRuntime, RuntimeQueryResult};

fn execute(runtime: &RedDBRuntime, connection: u64, query: &str) -> RuntimeQueryResult {
    set_current_connection_id(connection);
    let result = runtime.execute_query(query);
    clear_current_connection_id();
    result.expect("valid snapshot retention fixture statement")
}

fn verify_waiting_statement_retains_history(delete: bool) {
    let runtime = Arc::new(
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime opens"),
    );
    execute(&runtime, 1, "CREATE TABLE frame_rows (id TEXT, n INTEGER)");
    execute(&runtime, 1, "CREATE TABLE frame_gate (id TEXT)");
    execute(
        &runtime,
        1,
        "INSERT INTO frame_rows (id, n) VALUES ('row', 1)",
    );
    execute(&runtime, 1, "INSERT INTO frame_gate (id) VALUES ('row')");
    execute(&runtime, 2, "BEGIN");

    let locks = runtime.lock_manager();
    let mut gate = LockerGuard::new(locks.clone());
    gate.acquire(
        Resource::Collection("frame_gate".to_string()),
        LockMode::Exclusive,
    )
    .expect("fixture holds the gate collection");
    let reader_runtime = runtime.clone();
    let reader = std::thread::spawn(move || {
        execute(
        &reader_runtime, 3,
        "SELECT frame_rows.id AS id, frame_rows.n AS n FROM frame_rows JOIN frame_gate ON frame_rows.id = frame_gate.id",
    )
    });
    // Waiting on the real dispatch lock proves the query has already built
    // and installed its frame. No timing assumption about scan progress.
    let deadline = Instant::now() + Duration::from_secs(5);
    while locks.stats().waiting == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let waited = locks.stats().waiting > 0;
    if waited {
        execute(
            &runtime,
            2,
            if delete {
                "DELETE FROM frame_rows WHERE id = 'row'"
            } else {
                "UPDATE frame_rows SET n = 2 WHERE id = 'row'"
            },
        );
        execute(&runtime, 2, "COMMIT");
        execute(&runtime, 1, "VACUUM frame_rows");
    }
    drop(gate);
    let result = reader.join().expect("waiting reader terminates");
    assert!(waited, "query never reached its dispatch lock");
    assert_eq!(
        result.result.records.len(),
        1,
        "VACUUM reclaimed the waiting query's visible row"
    );
    assert_eq!(
        result.result.records[0]
            .get("n")
            .expect("projected value exists")
            .to_string(),
        "1"
    );
    execute(&runtime, 1, "VACUUM frame_rows");
    let physical = runtime
        .db()
        .store()
        .get_collection("frame_rows")
        .expect("fixture collection remains")
        .query_all(|_| true)
        .len();
    assert_eq!(
        physical,
        usize::from(!delete),
        "completed query retained obsolete history"
    );
}

#[test]
fn waiting_autocommit_query_retains_updated_version() {
    verify_waiting_statement_retains_history(false);
}

#[test]
fn waiting_autocommit_query_retains_deleted_version() {
    verify_waiting_statement_retains_history(true);
}
