//! Opening a native paged store with embedded options must not take a second file lock.
use reddb_server::storage::{UnifiedStore, UnifiedStoreConfig};
use reddb_server::{RedDBOptions, RedDBRuntime};
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn embedded_options_on_a_paged_store_keep_stats_available() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("data.rdb");
    {
        let store = UnifiedStore::open_with_config(&path, UnifiedStoreConfig::default())
            .expect("native paged store");
        store.create_collection("stats_probe").expect("collection");
        store.persist().expect("initial checkpoint");
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
        .expect("open existing paged store with embedded options");
    assert!(runtime.db().is_paged());
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let pending = runtime.db().embedded_pending_wal_records();
        let stats = runtime.execute_query("SHOW STATS stats_probe");
        sender
            .send((pending, stats.is_ok()))
            .expect("send stats result");
    });
    let (pending, stats_ok) = receiver
        .recv_timeout(Duration::from_secs(3))
        .expect("stats must not wait on the pager's exclusive data-file lock");
    worker.join().expect("stats worker must close the runtime");
    assert_eq!(
        pending, None,
        "paged stores do not have an embedded WAL region"
    );
    assert!(stats_ok, "SHOW STATS must remain usable");
}

#[test]
fn native_embedded_stats_still_report_the_embedded_wal() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("data.rdb");
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("new embedded runtime");
    assert!(!runtime.db().is_paged());
    assert!(runtime.db().embedded_pending_wal_records().is_some());
    runtime
        .execute_query("CREATE TABLE stats_probe (n INT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO stats_probe (n) VALUES (1)")
        .expect("row");
    runtime
        .execute_query("SHOW STATS stats_probe")
        .expect("native embedded stats");
    runtime.checkpoint().expect("embedded checkpoint");
    assert_eq!(runtime.db().embedded_pending_wal_records(), Some(0));
}
