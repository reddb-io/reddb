//! Checkpoint publication must fence row mutations as well as commit visibility.
use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

const CHILD_PATH: &str = "REDDB_TEST_CONCURRENT_CHECKPOINT_PATH";

fn open(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(
        RedDBOptions::persistent(path).with_memory_budget(2 * 1024 * 1024 * 1024),
    )
    .expect("open runtime")
}

fn sql(runtime: &RedDBRuntime, query: &str) {
    runtime.execute_query(query).expect("fixture SQL");
}

fn checkpoint_child(path: &Path, explicit: bool, insert_batch: bool) {
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile"),
    )
    .expect("paged runtime");
    runtime.checkpoint().expect("initialize file");
    drop(runtime);
    let runtime = Arc::new(open(path));
    sql(
        &runtime,
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
    );
    let payload = "x".repeat(131072);
    for table in 0..8 {
        sql(
            &runtime,
            &format!("CREATE TABLE pad_{table} (id INTEGER,n INTEGER,payload TEXT)"),
        );
        for id in 0..16 {
            sql(
                &runtime,
                &format!("INSERT INTO pad_{table} (id,n,payload) VALUES ({id},0,'{payload}')"),
            );
        }
    }
    for table in 0..16 {
        sql(
            &runtime,
            &format!("CREATE TABLE target_{table} (id INTEGER,n INTEGER)"),
        );
        sql(
            &runtime,
            &format!("INSERT INTO target_{table} (id,n) VALUES (1,0)"),
        );
    }
    runtime.checkpoint().expect("seed checkpoint");
    for table in 0..8 {
        sql(&runtime, &format!("UPDATE pad_{table} SET n=1"));
        // Exercise full checkpoint serialization rather than the sparse WAL
        // projection, whose work can finish without any page allocation.
        let store = runtime.db().store();
        let manager = store
            .get_collection(&format!("pad_{table}"))
            .expect("pad collection");
        let mut entity = manager
            .query_all(|_| true)
            .into_iter()
            .next()
            .expect("pad row");
        entity.updated_at += 1;
        manager
            .update(entity)
            .expect("unlogged pad edit forces replacement");
    }
    let pager = Arc::clone(runtime.db().store().pager().expect("paged store"));
    let pages = pager.page_count().expect("page count");
    let free = pager.free_page_count().expect("free page count");
    let finished = Arc::new(AtomicBool::new(false));
    let checkpoint = {
        let runtime = Arc::clone(&runtime);
        let finished = Arc::clone(&finished);
        thread::spawn(move || {
            runtime.checkpoint().expect("concurrent checkpoint");
            finished.store(true, Ordering::SeqCst);
        })
    };
    // Allocation precedes the final flush, including reuse from the free list.
    // File length alone misses reused pages and changes too late to establish
    // that a transaction begins during checkpoint serialization.
    let deadline = Instant::now() + Duration::from_secs(30);
    while pager.page_count().expect("checkpoint progress") <= pages
        && pager
            .free_page_count()
            .expect("checkpoint free-page progress")
            == free
    {
        assert!(
            !finished.load(Ordering::SeqCst),
            "checkpoint finished before allocation window"
        );
        assert!(Instant::now() < deadline, "checkpoint allocation deadline");
        thread::yield_now();
    }
    set_current_connection_id(42);
    if explicit {
        sql(&runtime, "BEGIN");
    }
    for table in 0..16 {
        sql(
            &runtime,
            &if insert_batch {
                format!("INSERT INTO target_{table} (id,n) VALUES (2,9),(3,10)")
            } else {
                format!("UPDATE target_{table} SET n=9 WHERE id=1")
            },
        );
    }
    clear_current_connection_id();
    checkpoint.join().expect("checkpoint worker");
    std::fs::write(path.with_extension("ready"), b"ready").expect("crash marker");
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

struct CrashChild(Child);
impl Drop for CrashChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn crash_case(name: &str, explicit: bool, insert_batch: bool) {
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        checkpoint_child(Path::new(&path), explicit, insert_batch);
        unreachable!();
    }
    let directory = tempfile::tempdir().expect("isolated fixture");
    let path = directory.path().join("data.rdb");
    let mut child = CrashChild(
        Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", name, "--nocapture"])
            .env(CHILD_PATH, &path)
            .spawn()
            .expect("checkpoint child"),
    );
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path.with_extension("ready").exists() {
        assert!(
            child.0.try_wait().expect("child status").is_none(),
            "child exited early"
        );
        assert!(Instant::now() < deadline, "child deadline");
        thread::sleep(Duration::from_millis(10));
    }
    child.0.kill().expect("force process termination");
    assert!(!child.0.wait().expect("wait for child").success());
    let restored = open(&path);
    let expected = if explicit { "0" } else { "9" };
    for table in 0..16 {
        if insert_batch {
            let result = restored
                .execute_query(&format!("SELECT id,n FROM target_{table} ORDER BY id"))
                .expect("recovered inserted rows");
            let expected = if explicit {
                vec![(1, 0)]
            } else {
                vec![(1, 0), (2, 9), (3, 10)]
            };
            assert_eq!(
                result.result.records.len(),
                expected.len(),
                "target_{table}"
            );
            for (row, (id, n)) in result.result.records.iter().zip(expected) {
                assert_eq!(row.get("id").expect("id").to_string(), id.to_string());
                assert_eq!(row.get("n").expect("n").to_string(), n.to_string());
            }
            continue;
        }
        let result = restored
            .execute_query(&format!("SELECT n FROM target_{table} WHERE id=1"))
            .expect("recovered row");
        assert_eq!(result.result.records.len(), 1);
        assert_eq!(
            result.result.records[0].get("n").expect("n").to_string(),
            expected,
            "target_{table}"
        );
    }
}

#[test]
fn transactions_started_during_checkpoint_do_not_become_committed_after_crash() {
    crash_case(
        "transactions_started_during_checkpoint_do_not_become_committed_after_crash",
        true,
        false,
    );
}

#[test]
fn autocommit_updates_started_during_checkpoint_survive_crash() {
    crash_case(
        "autocommit_updates_started_during_checkpoint_survive_crash",
        false,
        false,
    );
}

#[test]
fn unfinished_bulk_inserts_started_during_checkpoint_do_not_survive_crash() {
    crash_case(
        "unfinished_bulk_inserts_started_during_checkpoint_do_not_survive_crash",
        true,
        true,
    );
}

#[test]
fn autocommit_bulk_inserts_started_during_checkpoint_survive_crash() {
    crash_case(
        "autocommit_bulk_inserts_started_during_checkpoint_survive_crash",
        false,
        true,
    );
}
