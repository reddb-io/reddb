//! Incidental pager flushes must not publish unfinished transaction changes.
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

const CHILD_PATH: &str = "REDDB_TEST_TRANSACTION_FLUSH_PATH";
const CHILD_END: &str = "REDDB_TEST_TRANSACTION_FLUSH_END";

fn open(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(path)).expect("open runtime")
}

fn seed(path: &Path) {
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile"),
    )
    .expect("paged runtime");
    for sql in [
        "CREATE TABLE tx_rows (id INTEGER, n INTEGER)",
        "CREATE UNIQUE INDEX tx_rows_id ON tx_rows (id)",
        "INSERT INTO tx_rows (id,n) VALUES (1,7),(4,8)",
    ] {
        runtime.execute_query(sql).expect("seed fixture");
    }
    runtime.checkpoint().expect("committed seed");
}

fn verify(runtime: &RedDBRuntime, expected: &[(i64, i64)]) {
    let rows = runtime
        .execute_query("SELECT id,n FROM tx_rows ORDER BY id")
        .expect("read recovered rows");
    assert_eq!(rows.result.records.len(), expected.len());
    for (row, (id, value)) in rows.result.records.iter().zip(expected) {
        assert_eq!(row.get("id").expect("id").to_string(), id.to_string());
        assert_eq!(row.get("n").expect("n").to_string(), value.to_string());
        let indexed = runtime
            .execute_query(&format!("SELECT n FROM tx_rows WHERE id={id}"))
            .expect("indexed recovered row");
        assert_eq!(indexed.result.records.len(), 1);
        assert_eq!(
            indexed.result.records[0].get("n").expect("n").to_string(),
            value.to_string()
        );
    }
}

struct CrashChild(Child);
impl Drop for CrashChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn crash_case(name: &str, mutation: &str, committed: &[(i64, i64)]) {
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let path = Path::new(&path);
        let runtime = open(path);
        set_current_connection_id(42);
        runtime.execute_query("BEGIN").expect("begin");
        runtime.execute_query(mutation).expect("mutation");
        clear_current_connection_id();
        let flush = || {
            runtime
                .db()
                .store()
                .pager()
                .expect("paged fixture")
                .flush()
                .expect("incidental flush");
        };
        flush();
        let end = std::env::var(CHILD_END).expect("transaction outcome");
        if !end.is_empty() {
            set_current_connection_id(42);
            runtime.execute_query(&end).expect("end transaction");
            clear_current_connection_id();
            flush();
        }
        std::fs::write(path.with_extension("ready"), b"flushed").expect("crash marker");
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    for end in ["", "ROLLBACK", "COMMIT"] {
        let directory = tempfile::tempdir().expect("isolated fixture");
        let path = directory.path().join("data.rdb");
        seed(&path);
        let mut child = CrashChild(
            Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", name, "--nocapture"])
                .env(CHILD_PATH, &path)
                .env(CHILD_END, end)
                .spawn()
                .expect("spawn crash child"),
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        while !path.with_extension("ready").exists() && Instant::now() < deadline {
            assert!(
                child.0.try_wait().expect("child status").is_none(),
                "child exited early"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(path.with_extension("ready").exists(), "child timed out");
        child.0.kill().expect("force termination");
        assert!(!child.0.wait().expect("wait for termination").success());
        let expected = if end == "COMMIT" {
            committed
        } else {
            &[(1, 7), (4, 8)]
        };
        let runtime = open(&path);
        verify(&runtime, expected);
        runtime.checkpoint().expect("checkpoint recovered state");
        drop(runtime);
        verify(&open(&path), expected);
    }
}

#[test]
fn insert_survives_incidental_flush_only_after_commit() {
    crash_case(
        "insert_survives_incidental_flush_only_after_commit",
        "INSERT INTO tx_rows (id,n) VALUES (3,99)",
        &[(1, 7), (3, 99), (4, 8)],
    );
}

#[test]
fn bulk_insert_survives_incidental_flush_only_after_commit() {
    crash_case(
        "bulk_insert_survives_incidental_flush_only_after_commit",
        "INSERT INTO tx_rows (id,n) VALUES (3,99),(5,100)",
        &[(1, 7), (3, 99), (4, 8), (5, 100)],
    );
}

#[test]
fn update_survives_incidental_flush_only_after_commit() {
    crash_case(
        "update_survives_incidental_flush_only_after_commit",
        "UPDATE tx_rows SET id=2,n=99 WHERE id=1",
        &[(2, 99), (4, 8)],
    );
}

#[test]
fn delete_survives_incidental_flush_only_after_commit() {
    crash_case(
        "delete_survives_incidental_flush_only_after_commit",
        "DELETE FROM tx_rows WHERE id=4",
        &[(1, 7)],
    );
}
