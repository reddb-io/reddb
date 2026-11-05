//! Physical reclamation must not recycle a root still named by the durable catalog.
use std::path::Path;
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant};

use reddb_server::api::DurabilityMode;
use reddb_server::storage::engine::PageType;
use reddb_server::storage::{EntityId, UnifiedEntity, UnifiedStore, UnifiedStoreConfig};

const CHILD_PATH: &str = "REDDB_TEST_PHYSICAL_DELETE_PATH";
const CHILD_CASE: &str = "REDDB_TEST_PHYSICAL_DELETE_CASE";

fn configuration() -> UnifiedStoreConfig {
    UnifiedStoreConfig::default()
        .with_wal()
        .with_durability_mode(DurabilityMode::WalDurableGrouped)
        .with_page_cache_slots(8)
}

fn child(path: &Path) {
    let shrink = std::env::var(CHILD_CASE).as_deref() == Ok("shrink");
    let rows = if shrink { 5000 } else { 1 };
    let store = UnifiedStore::open_with_config(path, configuration()).expect("open fixture");
    store
        .create_collection("empty_after_gc")
        .expect("collection");
    for id in 1..=rows {
        let entity = UnifiedEntity::table_row(EntityId::new(id), "empty_after_gc", id, vec![]);
        store.insert("empty_after_gc", entity).expect("seed row");
    }
    store.persist().expect("publish initial root");

    let ids: Vec<_> = (1..=if shrink { rows - 1 } else { rows })
        .map(EntityId::new)
        .collect();
    assert_eq!(
        store
            .delete_batch("empty_after_gc", &ids)
            .expect("physical reclamation"),
        ids
    );

    // An unrelated allocation and incidental pager sync are allowed before
    // the next store checkpoint. They must not overwrite an on-disk root.
    let pager = store.pager().expect("paged fixture");
    let mut page = pager.allocate_page(PageType::Overflow).expect("allocation");
    page.update_checksum();
    pager.write_page(page.page_id(), page).expect("write page");
    pager.sync().expect("incidental durable flush");
    std::fs::write(path.with_extension("ready"), b"ready").expect("crash marker");
    loop {
        thread::sleep(Duration::from_millis(20));
    }
}

struct CrashChild(Child);

impl Drop for CrashChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn physical_delete_child() {
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        child(Path::new(&path));
    }
}

#[test]
fn deleted_last_row_reopens_after_page_reuse_and_crash() {
    check_recovery("empty", 0);
}

#[test]
fn shrunken_tree_reopens_after_page_reuse_and_crash() {
    check_recovery("shrink", 1);
}

fn check_recovery(case: &str, expected_rows: usize) {
    let directory = tempfile::tempdir().expect("isolated fixture");
    let path = directory.path().join("data.rdb");
    let marker = path.with_extension("ready");
    let mut child = CrashChild(
        Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "physical_delete_child", "--nocapture"])
            .env(CHILD_PATH, &path)
            .env(CHILD_CASE, case)
            .spawn()
            .expect("start child"),
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    while !marker.exists() {
        assert!(child.0.try_wait().expect("child status").is_none());
        assert!(Instant::now() < deadline, "fixture preparation deadline");
        thread::sleep(Duration::from_millis(10));
    }
    child.0.kill().expect("forced crash");
    child.0.wait().expect("reap crashed child");

    let reopened = UnifiedStore::open_with_config(&path, configuration())
        .expect("a durable physical delete must leave a reopenable store");
    assert!(reopened.get("empty_after_gc", EntityId::new(1)).is_none());
    assert_eq!(
        reopened
            .get_collection("empty_after_gc")
            .expect("retained collection")
            .count(),
        expected_rows
    );
    if expected_rows == 1 {
        assert!(reopened
            .get("empty_after_gc", EntityId::new(5000))
            .is_some());
    }
}

#[test]
fn reclaimed_page_ledger_survives_reopen_and_stops_file_growth() {
    let directory = tempfile::tempdir().expect("isolated fixture");
    let path = directory.path().join("data.rdb");
    let mut lengths = Vec::new();
    for cycle in 0..24 {
        let store = UnifiedStore::open_with_config(&path, configuration()).expect("open fixture");
        if cycle == 0 {
            store.create_collection("rows").expect("collection");
        }
        store
            .insert(
                "rows",
                UnifiedEntity::table_row(EntityId::new(cycle + 1), "rows", cycle + 1, vec![]),
            )
            .expect("insert");
        store.persist().expect("publish row");
        store
            .delete_batch("rows", &[EntityId::new(cycle + 1)])
            .expect("delete");
        store.persist().expect("publish deferred free");
        drop(store);
        let reopened =
            UnifiedStore::open_with_config(&path, configuration()).expect("reopen ledger");
        assert_eq!(
            reopened.get_collection("rows").expect("collection").count(),
            0
        );
        reopened
            .persist()
            .expect("release previously published pages");
        drop(reopened);
        lengths.push(std::fs::metadata(&path).expect("file size").len());
    }
    assert!(
        lengths[12..].iter().all(|length| *length == lengths[12]),
        "reclamation failed to plateau: {lengths:?}"
    );
}
