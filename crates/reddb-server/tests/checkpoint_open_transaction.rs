//! A checkpoint must not commit an unfinished transaction across a process crash.
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

const CHILD_PATH: &str = "REDDB_TEST_OPEN_TRANSACTION_CHECKPOINT_PATH";

fn open(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(path)).expect("open runtime")
}

fn seed(path: &Path, paged: bool) {
    if paged {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(path)
                .with_storage_profile(StorageDeployPreset::Serverless.selection())
                .expect("paged profile"),
        )
        .expect("paged runtime");
        runtime.checkpoint().expect("initialize file");
    }
    let runtime = open(path);
    for sql in [
        "CREATE TABLE tx_rows (id INTEGER, n INTEGER)",
        "CREATE UNIQUE INDEX tx_rows_id ON tx_rows (id)",
        "INSERT INTO tx_rows (id,n) VALUES (1,7),(4,8)",
    ] {
        runtime
            .execute_query(sql)
            .expect("seed transaction fixture");
    }
    runtime.checkpoint().expect("committed seed");
}

fn verify_values(runtime: &RedDBRuntime, first_value: i64) {
    let rows = runtime
        .execute_query("SELECT id,n FROM tx_rows ORDER BY id")
        .expect("original rows");
    assert_eq!(rows.result.records.len(), 2);
    for (row, (id, value)) in rows.result.records.iter().zip([(1, first_value), (4, 8)]) {
        assert_eq!(row.get("id").expect("id").to_string(), id.to_string());
        assert_eq!(row.get("n").expect("n").to_string(), value.to_string());
        assert_eq!(
            runtime
                .execute_query(&format!("SELECT n FROM tx_rows WHERE id={id}"))
                .expect("indexed original")
                .result
                .records
                .len(),
            1
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

fn crash_case(name: &str, mutation: &str, commit: bool, expected_value: i64) {
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let path = Path::new(&path);
        let runtime = open(path);
        set_current_connection_id(42);
        runtime
            .execute_query("BEGIN")
            .expect("begin unfinished transaction");
        runtime
            .execute_query(mutation)
            .expect("uncommitted mutation");
        clear_current_connection_id();
        runtime
            .checkpoint()
            .expect("checkpoint while transaction is open");
        if commit {
            set_current_connection_id(42);
            runtime
                .execute_query("COMMIT")
                .expect("durable commit after checkpoint");
            clear_current_connection_id();
        }
        std::fs::write(path.with_extension("ready"), b"checkpoint completed")
            .expect("crash marker");
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    for paged in [false, true] {
        run_crash_case(name, paged, expected_value);
    }
}

fn run_crash_case(name: &str, paged: bool, expected_value: i64) {
    let directory = tempfile::tempdir().expect("isolated fixture");
    let path = directory.path().join("data.rdb");
    seed(&path, paged);
    let mut child = CrashChild(
        Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", name, "--nocapture"])
            .env(CHILD_PATH, &path)
            .spawn()
            .expect("spawn checkpoint child"),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while !path.with_extension("ready").exists() && Instant::now() < deadline {
        assert!(
            child.0.try_wait().expect("child status").is_none(),
            "checkpoint child exited early"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        path.with_extension("ready").exists(),
        "checkpoint child timed out"
    );
    child.0.kill().expect("force process termination");
    let status = child.0.wait().expect("wait for termination");
    assert!(!status.success());
    let restored = open(&path);
    verify_values(&restored, expected_value);
    restored.checkpoint().expect("checkpoint recovered state");
    drop(restored);
    verify_values(&open(&path), expected_value);
}

#[test]
fn unfinished_insert_does_not_survive_checkpoint_and_crash() {
    crash_case(
        "unfinished_insert_does_not_survive_checkpoint_and_crash",
        "INSERT INTO tx_rows (id,n) VALUES (3,99)",
        false,
        7,
    );
}

#[test]
fn unfinished_update_does_not_survive_checkpoint_and_crash() {
    crash_case(
        "unfinished_update_does_not_survive_checkpoint_and_crash",
        "UPDATE tx_rows SET id=2,n=99 WHERE id=1",
        false,
        7,
    );
}

#[test]
fn unfinished_delete_does_not_survive_checkpoint_and_crash() {
    crash_case(
        "unfinished_delete_does_not_survive_checkpoint_and_crash",
        "DELETE FROM tx_rows WHERE id=4",
        false,
        7,
    );
}

#[test]
fn transaction_can_commit_after_its_versions_were_excluded_from_checkpoint() {
    for paged in [false, true] {
        commit_after_checkpoint(paged);
    }
}

fn commit_after_checkpoint(paged: bool) {
    let directory = tempfile::tempdir().expect("isolated fixture");
    let path = directory.path().join("data.rdb");
    seed(&path, paged);
    {
        let runtime = open(&path);
        set_current_connection_id(42);
        runtime.execute_query("BEGIN").expect("begin");
        runtime
            .execute_query("UPDATE tx_rows SET n=9 WHERE id=1")
            .expect("update");
        clear_current_connection_id();
        runtime.checkpoint().expect("checkpoint unfinished version");
        set_current_connection_id(42);
        runtime
            .execute_query("COMMIT")
            .expect("commit after checkpoint");
        clear_current_connection_id();
        runtime.checkpoint().expect("checkpoint committed version");
    }
    let restored = open(&path);
    let rows = restored
        .execute_query("SELECT n FROM tx_rows WHERE id=1")
        .expect("committed value");
    assert_eq!(rows.result.records[0].get("n").expect("n").to_string(), "9");
}

#[test]
fn commit_after_checkpoint_survives_crash_without_another_checkpoint() {
    crash_case(
        "commit_after_checkpoint_survives_crash_without_another_checkpoint",
        "UPDATE tx_rows SET n=9 WHERE id=1",
        true,
        9,
    );
}
