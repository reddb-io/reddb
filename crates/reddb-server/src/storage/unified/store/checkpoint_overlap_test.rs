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
fn checkpoint_keeps_the_read_source_of_an_untouched_cold_table() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("table-b.rdb");
    let db = open(&path);
    for sql in [
        "CREATE TABLE changed_table (v INTEGER)",
        "CREATE TABLE untouched_table (v INTEGER)",
        "INSERT INTO changed_table (v) VALUES (1)",
        "INSERT INTO untouched_table (v) VALUES (42)",
    ] {
        db.execute_query(sql).unwrap();
    }
    db.checkpoint().unwrap();
    let manager = db.db().store().get_collection("untouched_table").unwrap();
    let (before, overlay) = manager.current_backing_overlay().unwrap();
    assert!(overlay.is_empty());
    db.execute_query("UPDATE changed_table SET v = 2 WHERE v = 1")
        .unwrap();
    // Hold B's base read guard on an actual row until A finishes. A regression
    // reaches the timeout, then releases the reader so the threads can join.
    thread::scope(|scope| {
        let (ready, reading) = mpsc::channel();
        let (resume, released) = mpsc::channel();
        let reader = Arc::clone(&manager);
        scope.spawn(move || {
            reader
                .try_for_each_entity_unordered(1024 * 1024, |_| {
                    ready.send(()).unwrap();
                    released.recv().unwrap();
                    false
                })
                .unwrap();
        });
        reading.recv_timeout(Duration::from_secs(30)).unwrap();
        let (done, finished) = mpsc::channel();
        let checkpoint = &db;
        scope.spawn(move || done.send(checkpoint.checkpoint()).unwrap());
        let result = finished.recv_timeout(Duration::from_secs(30));
        resume.send(()).unwrap();
        result
            .expect("checkpoint waited for an unchanged table's reader")
            .unwrap();
    });
    let (after, overlay) = manager.current_backing_overlay().unwrap();
    assert!(overlay.is_empty());
    assert!(
        Arc::ptr_eq(&before, &after),
        "checkpoint replaced table B's read source"
    );
    drop((before, after, manager, db));
    let db = open(&path);
    for (table, expected) in [("untouched_table", 42), ("changed_table", 2)] {
        let rows = db
            .execute_query(&format!("SELECT v FROM {table}"))
            .unwrap()
            .result
            .records;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("v"),
            Some(&reddb_types::Value::Integer(expected))
        );
    }
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
        db.graceful_shutdown(false)
            .expect("quiesce workers before offline scrub");
        drop(db);
        let scrub = crate::storage::compact::scrub_paged(&path).expect("offline verification");
        assert!(
            scrub.trees.findings.is_empty(),
            "{mode}: {:?}",
            scrub.trees.findings
        );
        assert!(scrub.assets_verified);
        assert_eq!(scrub.unclassified_pages, 0, "{mode}: leaked pages");
    }
}

thread_local! {
    static PAUSE_CATALOG: RefCell<Option<mpsc::Sender<()>>> = const { RefCell::new(None) };
}

pub(super) fn pause_catalog_publication() {
    PAUSE_CATALOG.with_borrow_mut(|slot| {
        if let Some(ready) = slot.take() {
            ready.send(()).unwrap();
            loop {
                thread::park();
            }
        }
    });
}

fn insert_sparse_b(db: &RedDBRuntime) {
    let result = db
        .execute_query("INSERT INTO sparse_b (v) VALUES (42)")
        .unwrap();
    assert_eq!(result.affected_rows, 1);
}

#[test]
fn unrelated_insert_recovers_while_sparse_catalog_publication_is_paused() {
    sparse_catalog_recovery(
        "unrelated_insert_recovers_while_sparse_catalog_publication_is_paused",
        |_| {},
        insert_sparse_b,
    );
}

#[test]
fn warm_kind_insert_recovers_while_sparse_catalog_publication_is_paused() {
    sparse_catalog_recovery(
        "warm_kind_insert_recovers_while_sparse_catalog_publication_is_paused",
        |db| {
            assert!(db
                .db()
                .store()
                .try_get_config("collection.sparse_b.kind")
                .unwrap()
                .is_none())
        },
        insert_sparse_b,
    );
}

fn sparse_b_row() -> super::UnifiedEntity {
    super::UnifiedEntity::table_row(
        super::EntityId::new(4_000_000),
        "sparse_b",
        1,
        vec![reddb_types::Value::Integer(42)],
    )
}

#[test]
fn unrelated_store_insert_recovers_while_sparse_catalog_publication_is_paused() {
    sparse_catalog_recovery(
        "unrelated_store_insert_recovers_while_sparse_catalog_publication_is_paused",
        |_| {},
        |db| {
            let row = sparse_b_row();
            let id = row.id;
            assert_eq!(db.db().store().insert_auto("sparse_b", row).unwrap(), id);
        },
    );
}

#[test]
fn unrelated_wal_commit_recovers_while_sparse_catalog_publication_is_paused() {
    sparse_catalog_recovery(
        "unrelated_wal_commit_recovers_while_sparse_catalog_publication_is_paused",
        |_| {},
        |db| {
            use super::StoreWalAction;
            let store = db.db().store();
            // Exercise the actual strict WAL barrier independently of SQL admission.
            // Only recovery can make this row visible to the subsequent SQL SELECT.
            let row = sparse_b_row();
            let action =
                StoreWalAction::upsert_entity("sparse_b", &row, None, store.format_version());
            store
                .commit
                .as_ref()
                .unwrap()
                .append_actions(&[action])
                .unwrap();
        },
    );
}

fn sparse_catalog_recovery(test_name: &str, prepare: fn(&RedDBRuntime), write: fn(&RedDBRuntime)) {
    const CHILD: &str = "REDDB_TEST_SPARSE_CATALOG_DATABASE";
    if let Some(path) = std::env::var_os(CHILD) {
        let db = Arc::new(open(Path::new(&path)));
        for i in 0..8 {
            let updated = db
                .execute_query(&format!("UPDATE sparse_a SET v = v + 100 WHERE v = {i}"))
                .unwrap();
            assert_eq!(updated.affected_rows, 1);
        }
        prepare(&db);
        let (ready, paused) = mpsc::channel();
        let checkpoint = Arc::clone(&db);
        thread::spawn(move || {
            PAUSE_CATALOG.with_borrow_mut(|slot| *slot = Some(ready));
            checkpoint.checkpoint().unwrap();
        });
        paused
            .recv_timeout(Duration::from_secs(60))
            .expect("catalog never reached publication");
        let (committed, ack) = mpsc::channel();
        thread::Builder::new()
            .name("sparse-catalog-writer".into())
            .spawn(move || {
                write(&db);
                committed.send(()).unwrap();
            })
            .unwrap();
        match ack.recv_timeout(Duration::from_secs(5)) {
            Ok(()) => std::process::exit(72),
            Err(mpsc::RecvTimeoutError::Timeout) => std::process::exit(73),
            Err(error) => panic!("writer failed: {error}"),
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sparse.rdb");
    {
        let db = open(&path);
        db.execute_query("CREATE TABLE sparse_a (v INTEGER)")
            .unwrap();
        db.execute_query("CREATE TABLE sparse_b (v INTEGER)")
            .unwrap();
        db.execute_query("INSERT INTO sparse_a (v) VALUES (0),(1),(2),(3),(4),(5),(6),(7)")
            .unwrap();
        db.checkpoint().unwrap();
    }
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!(
                "{}::{test_name}",
                module_path!().split_once("::").unwrap().1
            ),
            "--nocapture",
        ])
        .env(CHILD, &path)
        .output()
        .unwrap();
    assert_ne!(
        child.status.code(),
        Some(73),
        "{test_name}: the writer was blocked at catalog publication\n{}",
        String::from_utf8_lossy(&child.stderr)
    );
    assert_eq!(
        child.status.code(),
        Some(72),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    let db = open(&path);
    assert_eq!(
        db.execute_query("SELECT v FROM sparse_b")
            .unwrap()
            .result
            .records[0]
            .get("v"),
        Some(&reddb_types::Value::Integer(42))
    );
    let rows = db
        .execute_query("SELECT v FROM sparse_a ORDER BY v")
        .unwrap()
        .result
        .records;
    let actual: Vec<_> = rows
        .iter()
        .map(|row| row.get("v").unwrap().clone())
        .collect();
    assert_eq!(
        actual,
        (100..108)
            .map(reddb_types::Value::Integer)
            .collect::<Vec<_>>()
    );
}
