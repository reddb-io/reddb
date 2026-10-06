//! Regression for #2376: concurrent DDL corrupted the system B-trees
//! (`red.control_events`, `red_index_registry`, `red_stats`) and left
//! every later CREATE/ALTER TABLE failing.
//!
//! Several workers replay the same idempotent migration at once, the
//! shape that broke the engine in the field: all CREATE TABLEs, then
//! ALTER TABLE ADD COLUMN, then the indexes. Afterwards new DDL must
//! still work, and the database must reopen with its rows readable and
//! DDL still working.

use std::path::Path;
use std::sync::{Arc, Barrier};

use reddb_server::storage::{DeployProfile, StoragePackaging, StorageProfileSelection};
use reddb_server::{RedDBOptions, RedDBRuntime};

const TABLE_COUNT: usize = 35;
const ALTER_COUNT: usize = 17;
const WORKER_COUNT: usize = 4;
const ROUND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// The server's `operational-directory` packaging: a paged store whose
/// collections (system ones included) live in pager B-trees. The default
/// embedded single-file packaging has no pager and never raced.
fn open_paged(path: &Path) -> RedDBRuntime {
    let profile = StorageProfileSelection {
        deploy_profile: DeployProfile::Embedded,
        packaging: StoragePackaging::OperationalDirectory,
        replica_count: 0,
        managed_backup: false,
        wal_retention: false,
    };
    let options = RedDBOptions::persistent(path)
        .with_storage_profile(profile)
        .expect("operational-directory profile");
    RedDBRuntime::with_options(options).expect("runtime boots")
}

fn migration() -> Vec<String> {
    let mut statements = Vec::new();
    for i in 0..TABLE_COUNT {
        statements.push(format!(
            "CREATE TABLE IF NOT EXISTS t{i} (id TEXT, a TEXT, b TEXT, c TEXT)"
        ));
    }
    for i in 0..ALTER_COUNT {
        statements.push(format!("ALTER TABLE t{i} ADD COLUMN d TEXT"));
    }
    for i in 0..TABLE_COUNT {
        statements.push(format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS t{i}_id ON t{i} (id)"
        ));
    }
    for i in 0..TABLE_COUNT {
        statements.push(format!("CREATE INDEX IF NOT EXISTS t{i}_a ON t{i} (a)"));
    }
    statements
}

/// Replays the migration from `WORKER_COUNT` threads at once and returns
/// every error that is not the expected idempotency outcome of a racing
/// `ALTER TABLE ... ADD COLUMN` (only one worker can add column `d`).
fn replay_concurrently(runtime: &RedDBRuntime) -> Vec<String> {
    let statements = Arc::new(migration());
    let barrier = Arc::new(Barrier::new(WORKER_COUNT));
    let workers: Vec<_> = (0..WORKER_COUNT)
        .map(|_| {
            let runtime = runtime.clone();
            let statements = Arc::clone(&statements);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                let mut errors = Vec::new();
                for statement in statements.iter() {
                    if let Err(err) = runtime.execute_query(statement) {
                        let message = err.to_string();
                        let duplicate_column = statement.starts_with("ALTER TABLE")
                            && message.contains("already exists");
                        if !duplicate_column {
                            errors.push(format!("{statement}: {message}"));
                        }
                    }
                }
                errors
            })
        })
        .collect();
    workers
        .into_iter()
        .flat_map(|worker| worker.join().expect("worker thread panicked"))
        .collect()
}

fn assert_ddl_and_reads_work(runtime: &RedDBRuntime, new_column: &str, label: &str) {
    for statement in [
        "CREATE TABLE IF NOT EXISTS after_race (id TEXT)".to_string(),
        format!("ALTER TABLE t20 ADD COLUMN {new_column} TEXT"),
        "CREATE INDEX IF NOT EXISTS after_race_id ON after_race (id)".to_string(),
        "DROP TABLE IF EXISTS after_race".to_string(),
        "SELECT * FROM t0".to_string(),
    ] {
        runtime
            .execute_query(&statement)
            .unwrap_or_else(|err| panic!("{label}: `{statement}` failed: {err}"));
    }
}

fn insert_one_row_per_table(runtime: &RedDBRuntime, label: &str) {
    for i in 0..TABLE_COUNT {
        runtime
            .execute_query(&format!(
                "INSERT INTO t{i} (id, a, b, c) VALUES ('row{i}', 'a', 'b', 'c')"
            ))
            .unwrap_or_else(|err| panic!("{label}: insert into t{i} failed: {err}"));
    }
}

fn assert_rows_intact(runtime: &RedDBRuntime, label: &str) {
    for i in 0..TABLE_COUNT {
        let rows = runtime
            .execute_query(&format!("SELECT id FROM t{i} WHERE id = 'row{i}'"))
            .unwrap_or_else(|err| panic!("{label}: select from t{i} failed: {err}"));
        assert_eq!(rows.result.records.len(), 1, "{label}: t{i} row lookup");
    }
}

fn replay_round(round: u32) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("data.rdb");
    {
        let runtime = open_paged(&db_path);
        let errors = replay_concurrently(&runtime);
        assert!(
            errors.is_empty(),
            "round {round}: concurrent migration replay produced errors:\n{}",
            errors.join("\n")
        );
        assert_ddl_and_reads_work(&runtime, "e", &format!("round {round} after race"));
        insert_one_row_per_table(&runtime, &format!("round {round} after race"));
        runtime.checkpoint().expect("checkpoint after race");
    }

    // Recovery: the file written under concurrent DDL must reopen with
    // healthy system trees, its rows readable and DDL still working.
    let runtime = open_paged(&db_path);
    assert_rows_intact(&runtime, &format!("round {round} after reopen"));
    assert_ddl_and_reads_work(&runtime, "f", &format!("round {round} after reopen"));
}

#[test]
fn concurrent_migration_replay_keeps_system_trees_and_ddl_healthy() {
    // Two rounds make the regression near-certain to surface. A torn tree
    // can also send the rebuild into an endless leaf walk (the server
    // "wedge" in #2376), so each round runs under a watchdog.
    for round in 0..2 {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            replay_round(round);
            let _ = done_tx.send(());
        });
        match done_rx.recv_timeout(ROUND_DEADLINE) {
            Ok(()) => worker.join().expect("round thread"),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                // The round panicked: surface its assertion message.
                if let Err(panic) = worker.join() {
                    std::panic::resume_unwind(panic);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("round {round}: engine wedged for {ROUND_DEADLINE:?} under concurrent DDL")
            }
        }
    }
}
