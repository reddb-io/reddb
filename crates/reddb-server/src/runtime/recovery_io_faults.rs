//! Recovery must retain its evidence across a second storage failure and process death.
use super::*;
use crate::storage::engine::{pager, PAGE_SIZE};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TEST: &str =
    "runtime::recovery_io_faults::interrupted_dwb_recovery_preserves_rows_indexes_and_progress";
const CHILD_PATH: &str = "REDDB_RECOVERY_IO_PATH";
const CHILD_STAGE: &str = "REDDB_RECOVERY_IO_STAGE";
const CHILD_FAULT: &str = "REDDB_RECOVERY_IO_FAULT";

fn open(path: &Path) -> RedDBResult<RedDBRuntime> {
    let runtime = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
        RedDBRuntime::with_options(
            RedDBOptions::persistent(path)
                .with_memory_budget(64 * 1024 * 1024)
                .with_storage_profile(
                    crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
                )
                .expect("profile"),
        )
    })?;
    runtime
        .background_tasks
        .as_ref()
        .expect("workers")
        .shutdown();
    Ok(runtime)
}

fn assert_rows(runtime: &RedDBRuntime, retry: bool) {
    // Cold HASH lookups and a BTREE range precede the authoritative full scan.
    for (id, expected) in [(1, "committed"), (2, "keep"), (3, "new")] {
        let result = runtime
            .execute_query(&format!(
                "SELECT payload FROM recovery_rows WHERE id = {id}"
            ))
            .expect("cold HASH lookup");
        assert_eq!(result.result.records.len(), 1, "id={id}");
        assert_eq!(
            result.result.records[0].get("payload"),
            Some(&Value::text(expected))
        );
    }
    let old = runtime
        .execute_query("SELECT id FROM recovery_rows WHERE org_id = 'org-a'")
        .expect("old BTREE key");
    assert!(
        old.result.records.is_empty(),
        "old MVCC key must stay absent"
    );
    let indexed = runtime
        .execute_query("SELECT id, org_id, payload FROM recovery_rows WHERE org_id >= 'org-a' AND org_id < 'org-z' ORDER BY id")
        .expect("cold BTREE range");
    let scan = runtime
        .execute_query("SELECT id, org_id, payload FROM recovery_rows ORDER BY id")
        .expect("scan");
    let expected = if retry { 4 } else { 3 };
    for result in [&indexed, &scan] {
        assert_eq!(result.result.records.len(), expected);
        for (record, (id, org, payload)) in result.result.records.iter().zip([
            (1, "org-x", "committed"),
            (2, "org-b", "keep"),
            (3, "org-c", "new"),
            (4, "org-d", "retry"),
        ]) {
            assert_eq!(record.get("id"), Some(&Value::Integer(id)));
            assert_eq!(record.get("org_id"), Some(&Value::text(org)));
            assert_eq!(record.get("payload"), Some(&Value::text(payload)));
        }
    }
}

fn child(path: &Path, stage: &str, fault: &str) {
    match stage {
        "seed" => {
            let runtime = open(path).expect("fixture runtime");
            for sql in [
                "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
                "CREATE TABLE recovery_rows (id INT, org_id TEXT, payload TEXT)",
                "CREATE UNIQUE INDEX recovery_id ON recovery_rows (id) USING HASH",
                "CREATE INDEX recovery_org ON recovery_rows (org_id) USING BTREE",
                "INSERT INTO recovery_rows (id,org_id,payload) VALUES (1,'org-a','original'),(2,'org-b','keep')",
            ] {
                runtime.execute_query(sql).expect(sql);
            }
            runtime.checkpoint().expect("baseline checkpoint");
            for sql in [
                "BEGIN",
                "UPDATE recovery_rows SET org_id = 'org-x', payload = 'committed' WHERE id = 1",
                "INSERT INTO recovery_rows (id,org_id,payload) VALUES (3,'org-c','new')",
                "COMMIT",
            ] {
                runtime.execute_query(sql).expect(sql);
            }
            pager::test_fail_next_pager_write(Some(PAGE_SIZE as u64), 17);
            let error = runtime
                .checkpoint()
                .expect_err("leave a durable DWB catalog frame");
            assert!(
                error.to_string().contains("injected pager disk full"),
                "{error}"
            );
            std::process::exit(199);
        }
        "recover" => {
            let home = Some(PAGE_SIZE as u64);
            let clear = Some(reddb_engine::storage::pager_io::dwb_zone_offset());
            match fault {
                "home-empty" => pager::test_fail_next_pager_write(home, 0),
                "home-partial" => pager::test_fail_next_pager_write(home, 17),
                "home-sync" => pager::test_fail_pager_sync_after(0),
                "clear-empty" => pager::test_fail_next_pager_write(clear, 0),
                "clear-partial" => pager::test_fail_next_pager_write(clear, 17),
                "clear-sync" => pager::test_fail_pager_sync_after(1),
                _ => panic!("unknown recovery fault {fault}"),
            }
            let error = match open(path) {
                Ok(_) => panic!("fault must prevent the recovering runtime from serving requests"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("injected pager"), "{error}");
        }
        "verify" => {
            {
                let runtime = open(path).expect("recovery after storage is healthy");
                assert_rows(&runtime, false);
                runtime.execute_query("INSERT INTO recovery_rows (id,org_id,payload) VALUES (1,'org-e','duplicate')")
                    .expect_err("UNIQUE constraint must survive recovery");
                runtime
                    .execute_query(
                        "INSERT INTO recovery_rows (id,org_id,payload) VALUES (4,'org-d','retry')",
                    )
                    .expect("new writes make progress");
                assert_rows(&runtime, true);
                runtime
                    .checkpoint()
                    .expect("recovered checkpoint makes progress");
                assert!(runtime
                    .verify_persisted_indexes("recovery_rows")
                    .expect("persisted index integrity")
                    .is_empty());
            }
            let runtime = open(path).expect("second reopen");
            assert_rows(&runtime, true);
        }
        _ => panic!("unknown child stage {stage}"),
    }
    // No graceful shutdown may hide the second failure by completing publication.
    std::process::exit(199);
}

fn run_child(path: &Path, stage: &str, fault: &str) {
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD_PATH, path)
        .env(CHILD_STAGE, stage)
        .env(CHILD_FAULT, fault)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("recovery child");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("child status").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("stop stuck recovery");
            let output = child.wait_with_output().expect("stuck recovery output");
            panic!(
                "recovery made no progress: {stage}/{fault}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("child output");
    assert_eq!(
        output.status.code(),
        Some(199),
        "{stage}/{fault}: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn interrupted_dwb_recovery_preserves_rows_indexes_and_progress() {
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        child(
            Path::new(&path),
            &std::env::var(CHILD_STAGE).expect("stage"),
            &std::env::var(CHILD_FAULT).expect("fault"),
        );
    }
    for fault in [
        "home-empty",
        "home-partial",
        "home-sync",
        "clear-empty",
        "clear-partial",
        "clear-sync",
    ] {
        let directory = tempfile::tempdir().expect("isolated recovery fixture");
        let path = directory.path().join("recovery.rdb");
        run_child(&path, "seed", fault);
        let wal_path = reddb_file::layout::unified_wal_path(&path);
        let wal = std::fs::read(&wal_path).expect("committed WAL evidence");
        let bytes = std::fs::read(&path).expect("staged catalog");
        let start =
            usize::try_from(reddb_engine::storage::pager_io::dwb_zone_offset()).expect("offset");
        let frame = &bytes[start..start + reddb_engine::storage::pager_io::DWB_ZONE_BYTES];
        let entries =
            reddb_file::decode_paged_dwb_frame(frame).expect("complete durable DWB frame");
        assert!(
            entries.iter().any(|entry| entry.page_id == 1),
            "catalog recovery must be exercised"
        );
        for _ in 0..if fault.starts_with("home-") { 2 } else { 1 } {
            run_child(&path, "recover", fault);
            assert_eq!(
                std::fs::read(&wal_path).expect("WAL after failed recovery"),
                wal,
                "failed recovery must retain its WAL evidence: {fault}"
            );
            if fault.starts_with("home-") {
                let after = std::fs::read(&path).expect("DWB after failed home write");
                assert_eq!(
                    &after[start..start + frame.len()],
                    frame,
                    "DWB must not be consumed before home pages sync: {fault}"
                );
            }
        }
        run_child(&path, "verify", fault);
    }
}
