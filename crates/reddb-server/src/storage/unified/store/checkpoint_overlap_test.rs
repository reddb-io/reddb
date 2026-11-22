use super::UnifiedStore;
use crate::{api::DurabilityMode, RedDBOptions, RedDBRuntime};
use std::{cell::RefCell, path::Path, sync::mpsc, sync::Arc, thread, time::Duration};

thread_local! {
    static PAUSE: RefCell<Option<(u32, mpsc::Sender<()>, mpsc::Receiver<()>)>> = const { RefCell::new(None) };
}

pub(super) fn after_primary_batch(records: &[(Vec<u8>, Vec<u8>)]) {
    PAUSE.with_borrow_mut(|slot| {
        let Some((version, _, _)) = slot.as_ref() else {
            return;
        };
        let Some((_, bytes)) = records.first() else {
            return;
        };
        let (row, _) = UnifiedStore::deserialize_entity_record(bytes, *version).unwrap();
        if row.kind.collection() == "checkpoint_a" {
            let (_, ready, resume) = slot.take().unwrap();
            ready.send(()).unwrap();
            resume
                .recv_timeout(Duration::from_secs(60))
                .expect("resume checkpoint");
        }
    });
}

fn open(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(
        RedDBOptions::persistent(path.to_owned())
            .with_bounded_disk_residency(true)
            .with_memory_budget(64 * 1024 * 1024)
            .with_durability_mode(DurabilityMode::Strict),
    )
    .unwrap()
}

#[test]
fn unrelated_insert_commits_and_recovers_during_primary_checkpoint() {
    const CHILD: &str = "REDDB_TEST_PAUSED_PRIMARY_DATABASE";
    const ACKNOWLEDGED: i32 = 72;
    const BLOCKED: i32 = 73;
    if let Some(path) = std::env::var_os(CHILD) {
        let db = Arc::new(open(Path::new(&path)));
        let mode = std::env::var("REDDB_TEST_PAUSED_PRIMARY_MODE").unwrap();
        let values = (0..1536).map(|i| format!("({i})")).collect::<Vec<_>>();
        for chunk in values.chunks(128) {
            db.execute_query(&format!(
                "INSERT INTO checkpoint_a (v) VALUES {}",
                chunk.join(",")
            ))
            .unwrap();
        }
        let (ready, paused) = mpsc::channel();
        let (resume, can_resume) = mpsc::channel();
        let checkpoint = Arc::clone(&db);
        let builder = thread::spawn(move || {
            let version = checkpoint.db().store().format_version();
            PAUSE.with_borrow_mut(|slot| *slot = Some((version, ready, can_resume)));
            checkpoint.checkpoint().unwrap();
        });
        paused
            .recv_timeout(Duration::from_secs(60))
            .expect("checkpoint did not write table A");
        let (committed, ack) = mpsc::channel();
        let mutate_a = mode == "changed";
        thread::spawn(move || {
            let result = db
                .execute_query("INSERT INTO checkpoint_b (v) VALUES (42)")
                .unwrap();
            assert_eq!(result.affected_rows, 1);
            if mutate_a {
                assert_eq!(
                    db.execute_query("UPDATE checkpoint_a SET v = -2 WHERE v = -1")
                        .unwrap()
                        .affected_rows,
                    1
                );
            }
            committed.send(()).unwrap();
        });
        match ack.recv_timeout(Duration::from_secs(5)) {
            Ok(()) => {
                if mode != "interrupted" {
                    resume.send(()).unwrap();
                    builder.join().unwrap();
                }
                std::process::exit(ACKNOWLEDGED);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => std::process::exit(BLOCKED),
            Err(error) => panic!("writer failed: {error}"),
        }
    }
    for mode in ["interrupted", "completed", "changed"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("concurrent.rdb");
        {
            let db = open(&path);
            db.execute_query("CREATE TABLE checkpoint_a (v INTEGER)")
                .unwrap();
            db.execute_query("CREATE TABLE checkpoint_b (v INTEGER)")
                .unwrap();
            if mode != "interrupted" {
                db.execute_query("INSERT INTO checkpoint_a (v) VALUES (-1)")
                    .unwrap();
            }
            db.checkpoint().unwrap();
        }
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!(
                    "{}::unrelated_insert_commits_and_recovers_during_primary_checkpoint",
                    module_path!().split_once("::").unwrap().1
                ),
                "--nocapture",
            ])
            .env(CHILD, &path)
            .env("REDDB_TEST_PAUSED_PRIMARY_MODE", mode)
            .output()
            .unwrap();
        assert_ne!(
            child.status.code(),
            Some(BLOCKED),
            "table B waited for the paused checkpoint on A"
        );
        assert_eq!(
            child.status.code(),
            Some(ACKNOWLEDGED),
            "{}",
            String::from_utf8_lossy(&child.stderr)
        );
        let db = open(&path);
        let rows = db
            .execute_query("SELECT v FROM checkpoint_b")
            .unwrap()
            .result
            .records;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("v"), Some(&reddb_types::Value::Integer(42)));
        let mut expected: Vec<i64> = (0..1536).collect();
        if mode != "interrupted" {
            expected.insert(0, if mode == "changed" { -2 } else { -1 });
        }
        let rows = db
            .execute_query("SELECT v FROM checkpoint_a ORDER BY v LIMIT 2000")
            .unwrap()
            .result
            .records;
        let actual: Vec<_> = rows
            .iter()
            .map(|row| row.get("v").unwrap().clone())
            .collect();
        assert_eq!(
            actual,
            expected
                .into_iter()
                .map(reddb_types::Value::Integer)
                .collect::<Vec<_>>(),
            "{mode}"
        );
        db.checkpoint().unwrap();
        drop(db);
        let scrub = crate::storage::compact::scrub_paged(&path).expect("offline verification");
        assert!(
            scrub.trees.findings.is_empty(),
            "{mode}: {:?}",
            scrub.trees.findings
        );
        assert!(scrub.assets_verified);
    }
}
