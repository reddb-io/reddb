#[path = "../../support/mod.rs"]
mod support;

use std::path::Path;
use std::sync::{Arc, Mutex};

use reddb::RedDBRuntime;
use support::{temp_db, TestDb};

fn create_db(path: &Path) {
    let rt = RedDBRuntime::with_options(reddb::RedDBOptions::persistent(path)).expect("open db");
    rt.execute_query("CREATE TABLE cleanup_harness (id INT)")
        .expect("create table");
    rt.execute_query("INSERT INTO cleanup_harness (id) VALUES (1)")
        .expect("insert row");
    drop(rt);
}

#[test]
fn temp_db_returns_unique_isolated_paths() {
    let (first_dir, first_path) = temp_db();
    let (second_dir, second_path) = temp_db();

    assert_ne!(first_dir.path(), second_dir.path());
    assert_ne!(first_path, second_path);
    assert!(first_path.starts_with(first_dir.path()));
    assert!(second_path.starts_with(second_dir.path()));
}

#[test]
fn test_db_guard_removes_dir_on_success() {
    let dir = {
        let db = TestDb::new();
        let dir = db.dir().to_path_buf();
        create_db(db.path());
        assert!(dir.exists(), "test DB dir should exist before guard drop");
        assert!(
            std::fs::read_dir(&dir)
                .expect("read test DB dir")
                .next()
                .is_some(),
            "DB creation should leave artifacts before guard drop"
        );
        dir
    };

    assert!(
        !dir.exists(),
        "TempDir-backed test DB guard should remove its directory on drop"
    );
}

#[test]
fn test_db_guard_removes_dir_during_panic_unwind() {
    let created_dir = Arc::new(Mutex::new(None));
    let created_dir_for_panic = Arc::clone(&created_dir);

    let result = std::panic::catch_unwind(move || {
        let db = TestDb::new();
        let dir = db.dir().to_path_buf();
        *created_dir_for_panic.lock().expect("record created dir") = Some(dir.clone());

        create_db(db.path());
        assert!(dir.exists(), "test DB dir should exist before panic");
        panic!("exercise TestDb cleanup during unwind");
    });

    assert!(result.is_err(), "test closure should panic");
    let dir = created_dir
        .lock()
        .expect("created dir lock")
        .clone()
        .expect("created dir recorded");
    assert!(
        !dir.exists(),
        "TempDir-backed test DB guard should remove its directory during panic unwind"
    );
}
