//! Committed payloads must grow past RAM; checkpoints must retain live writers.
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;
use std::path::Path;

const BUDGET_BYTES: u64 = 32 * 1024 * 1024;
const PAYLOAD_BYTES: usize = 64 * 1024;
const ROWS: usize = 2048;
const CHILD_PATH: &str = "REDDB_TEST_BOUNDED_WRITE_PROGRESS_PATH";

fn options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_memory_budget(BUDGET_BYTES)
        .with_bounded_disk_residency(true)
}

fn payload(id: usize) -> String {
    // Deterministic noise keeps compression from concealing physical growth.
    let mut state = id as u64 + 1;
    (0..PAYLOAD_BYTES)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (b'a' + (state % 26) as u8) as char
        })
        .collect()
}

fn write_rows(runtime: &RedDBRuntime, start: usize, end: usize) {
    let started = std::time::Instant::now();
    let mut maximum_resident = 0;
    for id in start..end {
        let statement = format!(
            "INSERT INTO sustained_rows (id, payload) VALUES ({id}, '{}')",
            payload(id)
        );
        if let Err(error) = runtime.execute_query(&statement) {
            panic!(
                "committed write {id}/{ROWS} failed after {} logical payload bytes with a {BUDGET_BYTES}-byte budget: {error}",
                id * PAYLOAD_BYTES,
            );
        }
        maximum_resident = maximum_resident.max(runtime.db().store().segment_memory_bytes());
        assert!(maximum_resident < BUDGET_BYTES);
    }
    eprintln!(
        "committed={} logical_payload_bytes={} peak_resident_segment_bytes={maximum_resident} elapsed_ms={}",
        end - start, (end - start) * PAYLOAD_BYTES, started.elapsed().as_millis(),
    );
}

fn verify(runtime: &RedDBRuntime, ids: impl IntoIterator<Item = usize>) {
    let result = runtime
        .execute_query("SELECT COUNT(*) AS n FROM sustained_rows")
        .expect("all acknowledged rows are readable");
    assert_eq!(
        result.result.records[0].get("n").unwrap().display_string(),
        ROWS.to_string(),
    );
    for id in ids {
        let result = runtime
            .execute_query(&format!(
                "SELECT payload FROM sustained_rows WHERE id = {id}"
            ))
            .expect("read persisted payload");
        assert_eq!(result.result.records.len(), 1, "row {id}");
        assert_eq!(
            result.result.records[0].get("payload"),
            Some(&Value::text(payload(id).as_str())),
            "row {id}",
        );
    }
}

fn verify_all(runtime: &RedDBRuntime) {
    // Validate every acknowledged payload with one ordered bounded scan. The
    // storage contract is about recovering all rows; issuing ROWS distinct SQL
    // strings also benchmarks planning, index restoration and durable result
    // caching, and can exhaust the CI timeout without strengthening recovery
    // coverage.
    let result = runtime
        .execute_query("SELECT id, payload FROM sustained_rows ORDER BY id")
        .expect("all acknowledged rows are readable");
    assert_eq!(result.result.records.len(), ROWS);
    for (id, row) in result.result.records.iter().enumerate() {
        assert_eq!(
            row.get("id").map(Value::display_string),
            Some(id.to_string()),
            "row order at {id}",
        );
        assert_eq!(
            row.get("payload"),
            Some(&Value::text(payload(id).as_str())),
            "row {id}",
        );
    }
}

#[test]
fn bounded_write_crash_child() {
    let Some(path) = std::env::var_os(CHILD_PATH) else {
        return;
    };
    let runtime = RedDBRuntime::with_options(options(Path::new(&path))).expect("bounded runtime");
    runtime
        .execute_query("CREATE TABLE sustained_rows (id INTEGER PRIMARY KEY, payload TEXT)")
        .unwrap();
    write_rows(&runtime, 0, ROWS);
    verify(&runtime, [0, ROWS / 2, ROWS - 1]);
    // Bypass Drop and its final flush: only automatic drains and the WAL survive.
    std::process::exit(73);
}

#[test]
fn committed_data_can_outgrow_ram_without_manual_checkpoints() {
    let directory = tempfile::tempdir().expect("private write-pressure fixture");
    let path = directory.path().join("pressure.rdb");
    assert!(ROWS * PAYLOAD_BYTES >= 4 * BUDGET_BYTES as usize);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "bounded_write_crash_child", "--nocapture"])
        .env(CHILD_PATH, &path)
        .status()
        .expect("run crash child");
    assert_eq!(
        status.code(),
        Some(73),
        "writer must acknowledge every row before crash"
    );
    let runtime = RedDBRuntime::with_options(options(&path)).expect("recover acknowledged writes");
    verify_all(&runtime);
    runtime.checkpoint().expect("checkpoint recovered state");
    drop(runtime);
    let runtime = RedDBRuntime::with_options(options(&path)).expect("reopen recovered checkpoint");
    verify(&runtime, [0, ROWS / 2, ROWS - 1]);
}

#[test]
fn concurrent_committed_writers_can_outgrow_ram() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("concurrent.rdb");
    let runtime = RedDBRuntime::with_options(options(&path)).unwrap();
    runtime
        .execute_query("CREATE TABLE sustained_rows (id INTEGER PRIMARY KEY, payload TEXT)")
        .unwrap();
    std::thread::scope(|scope| {
        for client in 0..4 {
            let runtime = &runtime;
            scope.spawn(move || write_rows(runtime, client * ROWS / 4, (client + 1) * ROWS / 4));
        }
    });
    verify(&runtime, [0, ROWS / 2, ROWS - 1]);
    drop(runtime);
    let runtime = RedDBRuntime::with_options(options(&path)).unwrap();
    verify_all(&runtime);
}

#[test]
fn checkpoint_keeps_uncommitted_resident_rows_until_transaction_finishes() {
    for commit in [false, true] {
        let directory = tempfile::tempdir().expect("private transaction fixture");
        let path = directory.path().join("transaction.rdb");
        let runtime = RedDBRuntime::with_options(options(&path)).expect("bounded runtime");
        runtime
            .execute_query("CREATE TABLE pending_rows (id INTEGER PRIMARY KEY, payload TEXT)")
            .unwrap();
        runtime
            .execute_query(
                "INSERT INTO pending_rows (id, payload) VALUES (1, 'committed'), (3, 'keep')",
            )
            .unwrap();
        runtime.execute_query("BEGIN").unwrap();
        runtime
            .execute_query("INSERT INTO pending_rows (id, payload) VALUES (2, 'pending')")
            .unwrap();
        runtime
            .execute_query("UPDATE pending_rows SET payload = 'updated' WHERE id = 1")
            .unwrap();
        runtime
            .execute_query("DELETE FROM pending_rows WHERE id = 3")
            .unwrap();
        // Repeated checkpoints exercise both replacement and sparse reuse.
        for _ in 0..2 {
            runtime.checkpoint().expect("checkpoint during transaction");
            let result = runtime
                .execute_query("SELECT id, payload FROM pending_rows ORDER BY id")
                .unwrap();
            assert_eq!(
                result.result.records.len(),
                2,
                "retain the live transaction's rows"
            );
            assert_eq!(
                result.result.records[0].get("payload"),
                Some(&Value::text("updated"))
            );
            assert_eq!(
                result.result.records[1].get("payload"),
                Some(&Value::text("pending"))
            );
        }
        runtime
            .execute_query(if commit { "COMMIT" } else { "ROLLBACK" })
            .unwrap();
        runtime.checkpoint().unwrap();
        drop(runtime);
        let runtime = RedDBRuntime::with_options(options(&path)).unwrap();
        let result = runtime
            .execute_query("SELECT id, payload FROM pending_rows ORDER BY id")
            .unwrap();
        assert_eq!(result.result.records.len(), 2);
        let expected = if commit {
            ["updated", "pending"]
        } else {
            ["committed", "keep"]
        };
        for (row, value) in result.result.records.iter().zip(expected) {
            assert_eq!(row.get("payload"), Some(&Value::text(value)));
        }
    }
}
