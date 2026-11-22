use super::UnifiedStore;
use crate::{api::DurabilityMode, RedDBOptions, RedDBRuntime};
use std::{cell::RefCell, path::Path, sync::mpsc, sync::Arc, thread, time::Duration};

thread_local! {
    static PAUSE: RefCell<Option<(u32, mpsc::Sender<()>)>> = const { RefCell::new(None) };
}

pub(super) fn after_primary_batch(records: &[(Vec<u8>, Vec<u8>)]) {
    PAUSE.with_borrow(|pause| {
        if let (Some((version, ready)), Some((_, bytes))) = (pause, records.first()) {
            let (row, _) = UnifiedStore::deserialize_entity_record(bytes, *version).unwrap();
            if row.kind.collection() == "checkpoint_a" {
                ready.send(()).unwrap();
                // Only the child process arms this hook. Its parent checks crash recovery.
                loop {
                    thread::park();
                }
            }
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
        let values = (0..1536).map(|i| format!("({i})")).collect::<Vec<_>>();
        for chunk in values.chunks(128) {
            db.execute_query(&format!(
                "INSERT INTO checkpoint_a (v) VALUES {}",
                chunk.join(",")
            ))
            .unwrap();
        }
        let (ready, paused) = mpsc::channel();
        let checkpoint = Arc::clone(&db);
        thread::spawn(move || {
            let version = checkpoint.db().store().format_version();
            PAUSE.with_borrow_mut(|slot| *slot = Some((version, ready)));
            checkpoint.checkpoint().unwrap();
        });
        paused
            .recv_timeout(Duration::from_secs(60))
            .expect("checkpoint did not write table A");
        let (committed, ack) = mpsc::channel();
        thread::spawn(move || {
            let result = db
                .execute_query("INSERT INTO checkpoint_b (v) VALUES (42)")
                .unwrap();
            assert_eq!(result.affected_rows, 1);
            committed.send(()).unwrap();
        });
        match ack.recv_timeout(Duration::from_secs(5)) {
            Ok(()) => std::process::exit(ACKNOWLEDGED),
            Err(mpsc::RecvTimeoutError::Timeout) => std::process::exit(BLOCKED),
            Err(error) => panic!("writer failed: {error}"),
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent.rdb");
    {
        let db = open(&path);
        db.execute_query("CREATE TABLE checkpoint_a (v INTEGER)")
            .unwrap();
        db.execute_query("CREATE TABLE checkpoint_b (v INTEGER)")
            .unwrap();
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
    assert_eq!(
        db.execute_query("SELECT v FROM checkpoint_a")
            .unwrap()
            .result
            .records
            .len(),
        1536
    );
}
