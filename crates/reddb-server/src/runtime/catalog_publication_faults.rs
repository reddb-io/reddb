use super::*;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TEST: &str = "runtime::page_integrity_faults::catalog_publication_faults::catalog_generation_publication_survives_crash_and_replays_confirmed_rows";
const CHILD: &str = "REDDB_CATALOG_PUBLICATION_PATH";

fn reference(path: &Path) -> reddb_file::page_proof::PageReference {
    let bytes = std::fs::read(path).expect("inspect persisted superblocks");
    let selected = reddb_file::select_paged_superblock(&bytes).expect("valid superblock");
    let start = selected.copy_index * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
    reddb_file::decode_database_header(
        &bytes[start..start + reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE],
    )
    .expect("decode catalog authority")
    .manifest_reference
}

fn assert_rows(runtime: &RedDBRuntime, progress: bool) {
    let point = runtime
        .execute_query("SELECT payload FROM records WHERE id = 1")
        .expect("cold HASH after catalog recovery");
    assert_eq!(point.result.records.len(), 1);
    assert_eq!(
        point.result.records[0].get("payload"),
        Some(&Value::text("committed"))
    );
    let indexed = runtime
        .execute_query(
            "SELECT id,payload FROM records WHERE payload >= 'a' AND payload < 'z' ORDER BY id",
        )
        .expect("cold BTREE after catalog recovery");
    let scan = runtime
        .execute_query("SELECT id,payload FROM records ORDER BY id")
        .expect("authoritative scan after catalog recovery");
    for result in [&indexed, &scan] {
        assert_eq!(result.result.records.len(), if progress { 3 } else { 2 });
        for (record, (id, payload)) in
            result
                .result
                .records
                .iter()
                .zip([(1, "committed"), (2, "keep"), (3, "progress")])
        {
            assert_eq!(record.get("id"), Some(&Value::Integer(id)));
            assert_eq!(record.get("payload"), Some(&Value::text(payload)));
        }
    }
    assert!(runtime
        .verify_persisted_indexes("records")
        .expect("persisted index integrity")
        .is_empty());
}

#[test]
fn catalog_generation_publication_survives_crash_and_replays_confirmed_rows() {
    if let Some(path) = std::env::var_os(CHILD) {
        let runtime = open(Path::new(&path)).expect("healthy writer before publication crash");
        runtime
            .execute_query("UPDATE records SET payload = 'committed' WHERE id = 1")
            .expect("confirmed MVCC update");
        let phase = std::env::var("REDDB_CATALOG_PUBLICATION_PHASE").expect("crash phase");
        std::env::set_var(
            if phase == "before" {
                "REDDB_TEST_EXIT_BEFORE_CATALOG_ROOT_PUBLICATION"
            } else {
                "REDDB_TEST_EXIT_AFTER_CATALOG_ROOT_PUBLICATION"
            },
            "1",
        );
        runtime
            .graceful_shutdown(false)
            .expect("injected process exit");
        panic!("catalog publication crash was not reached");
    }
    for phase in ["before", "after"] {
        let directory = tempfile::tempdir().expect("isolated publication fault");
        let path = directory.path().join("catalog-publication.rdb");
        seed(&path);
        let previous = reference(&path);
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD, &path)
            .env("REDDB_CATALOG_PUBLICATION_PHASE", phase)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("publication child");
        let deadline = Instant::now() + Duration::from_secs(60);
        while child.try_wait().expect("child status").is_none() {
            if Instant::now() >= deadline {
                child.kill().expect("stop stalled publication");
                let output = child.wait_with_output().expect("stalled child output");
                panic!(
                    "catalog publication stalled: {phase}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().expect("publication exit");
        assert_eq!(
            output.status.code(),
            Some(if phase == "before" { 197 } else { 198 }),
            "{phase}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if phase == "before" {
            assert_eq!(reference(&path), previous);
        } else {
            assert_ne!(reference(&path), previous);
        }
        for round in 0..2 {
            let started = Instant::now();
            let runtime = open(&path).expect("reopen with complete catalog authority");
            assert_rows(&runtime, round != 0);
            if round == 0 {
                runtime
                    .execute_query("INSERT INTO records (id,payload) VALUES (1,'duplicate')")
                    .expect_err("UNIQUE must survive publication recovery");
                runtime
                    .execute_query("INSERT INTO records (id,payload) VALUES (3,'progress')")
                    .expect("writer progresses after recovery");
            }
            runtime
                .graceful_shutdown(false)
                .expect("checkpoint recovered generation");
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "recovery must make bounded progress"
            );
        }
    }
}
