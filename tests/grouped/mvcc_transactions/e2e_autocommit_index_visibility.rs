//! Concurrent autocommit reads must retain a visible row while it is updated.
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};
use std::sync::{Arc, Barrier};

#[test]
fn indexed_reads_keep_row_visible_during_autocommit_updates() {
    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime"));
    runtime
        .execute_query(
            "CREATE TABLE concurrent_snapshot_read (id TEXT PRIMARY KEY, revision BIGINT NOT NULL)",
        )
        .expect("table");
    runtime
        .execute_query("INSERT INTO concurrent_snapshot_read (id, revision) VALUES ('routing', 0)")
        .expect("seed");
    let start = Arc::new(Barrier::new(4));
    let mut workers = Vec::new();
    for worker in 0..4 {
        let runtime = Arc::clone(&runtime);
        let start = Arc::clone(&start);
        workers.push(std::thread::spawn(move || {
            set_current_connection_id(90000 + worker);
            start.wait();
            for _ in 0..120 {
                if worker < 2 {
                    let result = runtime.execute_query("SELECT revision FROM concurrent_snapshot_read WHERE id = 'routing'").expect("read");
                    assert_eq!(result.result.records.len(), 1, "a committed row became invisible");
                } else {
                    runtime.execute_query("UPDATE concurrent_snapshot_read SET revision = revision + 1 WHERE id = 'routing'").expect("update");
                }
            }
            clear_current_connection_id();
        }));
    }
    for worker in workers {
        worker.join().expect("worker");
    }
    let final_read = runtime
        .execute_query("SELECT revision FROM concurrent_snapshot_read WHERE id = 'routing'")
        .expect("final read");
    assert_eq!(final_read.result.records.len(), 1);
}
