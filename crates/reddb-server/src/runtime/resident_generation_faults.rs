//! The native resident API must retain its sealed base until WAL publication.
use super::*;
use crate::storage::{EntityId, UnifiedEntity, UnifiedStore};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const CHILD: &str = "REDDB_RESIDENT_GENERATION_PATH";
const TEST: &str = "runtime::page_integrity_faults::resident_generation_faults::sealed_resident_native_base_replays_commits_after_process_death";

fn insert(store: &UnifiedStore, id: u64, payload: &str) {
    store
        .insert_auto(
            "records",
            UnifiedEntity::table_row(EntityId::new(id), "records", id, vec![Value::text(payload)]),
        )
        .expect("committed native insert");
}

fn run_child(path: &Path, phase: &str) {
    let mut child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD, path)
        .env("REDDB_RESIDENT_GENERATION_PHASE", phase)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("native child");
    let deadline = Instant::now() + Duration::from_secs(60);
    while child.try_wait().expect("child status").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("stop stalled child");
            child.wait().expect("stalled child termination");
            panic!("native {phase} stalled");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().expect("child output");
    assert!(
        output.status.success(),
        "{phase}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn verify_recovery(path: &Path) {
    for round in 0..2 {
        let store = UnifiedStore::open(path).expect("cold resident recovery");
        for (id, payload) in [(1, "original"), (3, "inserted"), (4, "progress")]
            .into_iter()
            .take(if round == 0 { 2 } else { 3 })
        {
            let entity = store
                .get("records", EntityId::new(id))
                .expect("cold point lookup");
            assert_eq!(
                entity.data.as_row().expect("row").columns,
                vec![Value::text(payload)]
            );
        }
        assert!(
            store.get("records", EntityId::new(2)).is_none(),
            "deleted row must not resurrect"
        );
        let mut rows = store
            .scan(None, |_| true)
            .into_iter()
            .filter(|(name, _)| name == "records")
            .map(|(_, row)| row.id.raw())
            .collect::<Vec<_>>();
        rows.sort_unstable();
        assert_eq!(
            rows,
            if round == 0 {
                vec![1, 3]
            } else {
                vec![1, 3, 4]
            }
        );
        if round == 0 {
            insert(&store, 4, "progress");
        }
        store.persist().expect("publish recovered generation");
    }
}

#[test]
fn sealed_resident_native_base_replays_commits_after_process_death() {
    if let Some(path) = std::env::var_os(CHILD) {
        let path = Path::new(&path);
        if std::env::var("REDDB_RESIDENT_GENERATION_PHASE").expect("phase") == "verify" {
            verify_recovery(path);
            return;
        }
        let store = UnifiedStore::open(path).expect("resident writer");
        insert(&store, 3, "inserted");
        assert!(store
            .delete("records", EntityId::new(2))
            .expect("committed delete"));
        std::process::exit(0);
    }
    let directory = tempfile::tempdir().expect("isolated native file");
    let path = directory.path().join("resident.rdb");
    {
        let store = UnifiedStore::open(&path).expect("resident store");
        insert(&store, 1, "original");
        insert(&store, 2, "deleted");
        store.persist().expect("seal resident base");
        let header = store
            .pager()
            .expect("native pager")
            .header()
            .expect("header");
        assert!(
            !header.requires_disk_residency(),
            "exercise the native resident path"
        );
        assert_ne!(
            header.required_capabilities & reddb_file::file_format::FILE_CAPABILITY_TREE_PROOFS,
            0
        );
    }
    let (root, original) = collection_root(&path, "records");
    run_child(&path, "write");
    let (after_root, after) = collection_root(&path, "records");
    assert_eq!(after_root, root, "commits must retain the published base");
    assert_eq!(
        after.as_bytes(),
        original.as_bytes(),
        "a sealed base must stay immutable"
    );
    run_child(&path, "verify");
}
