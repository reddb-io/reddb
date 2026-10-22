//! A small committed update must not reread an entire unchanged payload history.
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

#[test]
fn unrelated_wal_cache_pressure_does_not_disable_later_sparse_updates() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("pressure.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    };
    let runtime = RedDBRuntime::with_options(options()).expect("runtime");
    for query in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
        "CREATE TABLE pressure_payload (id INT, n INT, payload TEXT)",
        "CREATE UNIQUE INDEX pressure_payload_id ON pressure_payload (id)",
        "CREATE TABLE pressure_noise (id INT, n INT)",
    ] {
        runtime.execute_query(query).expect("schema");
    }
    let payload = "p".repeat(4096);
    for batch in 0..10 {
        let rows = (batch * 100..(batch + 1) * 100)
            .map(|id| format!("({id},0,'{payload}')"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO pressure_payload (id,n,payload) VALUES {rows}"
            ))
            .expect("payload history");
    }
    let noise = (0..1100)
        .map(|id| format!("({id},0)"))
        .collect::<Vec<_>>()
        .join(",");
    runtime
        .execute_query(&format!("INSERT INTO pressure_noise (id,n) VALUES {noise}"))
        .expect("unrelated rows");
    runtime.checkpoint().expect("baseline");
    runtime
        .execute_query("UPDATE pressure_noise SET n=1")
        .expect("exceed retained WAL row capacity");
    runtime
        .execute_query("UPDATE pressure_payload SET n=1256 WHERE id=2")
        .expect("acknowledged update after cache saturation");
    let pager = runtime.db().store().pager().expect("pager").clone();
    let before = pager.cache_stats();
    runtime.checkpoint().expect("checkpoint under pressure");
    let after = pager.cache_stats();
    let reads = after.hits + after.misses - before.hits - before.misses;
    assert!(
        reads < 256,
        "unrelated WAL pressure scanned payload history: {reads} reads"
    );
    drop(pager);
    drop(runtime);
    let recovered = RedDBRuntime::with_options(options()).expect("reopen under pressure");
    for (query, expected) in [
        ("SELECT n FROM pressure_payload WHERE id=2", "1256"),
        ("SELECT n FROM pressure_noise WHERE id=1099", "1"),
    ] {
        let result = recovered.execute_query(query).expect("durable update");
        assert_eq!(result.result.records.len(), 1);
        assert_eq!(
            result.result.records[0].get("n").expect("n").to_string(),
            expected
        );
    }
}

#[test]
fn sparse_checkpoint_with_pending_writer_survives_process_exit() {
    use reddb_server::runtime::mvcc::set_current_connection_id;
    const CHILD_PATH: &str = "REDDB_TEST_PENDING_SPARSE_CHECKPOINT_PATH";
    let options = |path: &std::path::Path| {
        RedDBOptions::persistent(path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    };
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        let runtime =
            RedDBRuntime::with_options(options(std::path::Path::new(&path))).expect("runtime");
        for query in [
            "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
            "CREATE TABLE crash_payload (id INT, n INT, payload TEXT)",
            "CREATE UNIQUE INDEX crash_payload_id ON crash_payload (id)",
        ] {
            runtime.execute_query(query).expect("schema");
        }
        let payload = "c".repeat(4096);
        for batch in 0..10 {
            let rows = (batch * 100..(batch + 1) * 100)
                .map(|id| format!("({id},0,'{payload}')"))
                .collect::<Vec<_>>()
                .join(",");
            runtime
                .execute_query(&format!(
                    "INSERT INTO crash_payload (id,n,payload) VALUES {rows}"
                ))
                .expect("history");
        }
        runtime.checkpoint().expect("baseline");
        set_current_connection_id(401);
        runtime.execute_query("BEGIN").expect("begin");
        runtime
            .execute_query("UPDATE crash_payload SET n=9999 WHERE id=1")
            .expect("pending");
        set_current_connection_id(402);
        runtime
            .execute_query("UPDATE crash_payload SET n=1255 WHERE id=2")
            .expect("acknowledged");
        let pager = runtime.db().store().pager().expect("pager").clone();
        let before = pager.cache_stats();
        runtime.checkpoint().expect("durable projection");
        let after = pager.cache_stats();
        assert!(after.hits + after.misses - before.hits - before.misses < 128);
        // Bypass every runtime/session destructor while the first writer is open.
        std::process::exit(73);
    }
    let directory = tempfile::tempdir().expect("crash fixture");
    let path = directory.path().join("crash.rdb");
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "sparse_checkpoint_with_pending_writer_survives_process_exit",
            "--nocapture",
        ])
        .env(CHILD_PATH, &path)
        .output()
        .expect("crash child");
    assert_eq!(
        output.status.code(),
        Some(73),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let runtime = RedDBRuntime::with_options(options(&path)).expect("recover");
    for (id, expected) in [(1, 0), (2, 1255)] {
        let read = runtime
            .execute_query(&format!("SELECT n FROM crash_payload WHERE id={id}"))
            .expect("recovered row");
        assert_eq!(read.result.records.len(), 1);
        assert_eq!(
            read.result.records[0].get("n").expect("n").to_string(),
            expected.to_string()
        );
    }
    let plan = runtime
        .execute_query("EXPLAIN SELECT * FROM crash_payload WHERE id=2")
        .expect("index");
    assert!(plan.result.records.iter().any(|record| record
        .get("op")
        .is_some_and(|value| value == &reddb_types::Value::text("index_seek"))));
}

#[test]
fn sparse_checkpoint_tracks_pending_commit_and_rollback_without_scanning_history() {
    use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};

    struct ClearConnection;
    impl Drop for ClearConnection {
        fn drop(&mut self) {
            clear_current_connection_id();
        }
    }
    let _connection = ClearConnection;
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("pending.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    };
    let runtime = RedDBRuntime::with_options(options()).expect("runtime");
    for query in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
        "CREATE TABLE pending_payload (id INT, n INT, payload TEXT)",
        "CREATE UNIQUE INDEX pending_payload_id ON pending_payload (id)",
    ] {
        runtime.execute_query(query).expect("schema");
    }
    let payload = "p".repeat(4096);
    for batch in 0..10 {
        let rows = (batch * 100..(batch + 1) * 100)
            .map(|id| format!("({id}, 0, '{payload}')"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO pending_payload (id,n,payload) VALUES {rows}"
            ))
            .expect("history");
    }
    runtime.checkpoint().expect("baseline");
    drop(runtime);
    let runtime = RedDBRuntime::with_options(options()).expect("cold reopen");
    runtime
        .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
        .expect("isolate checkpoint");
    let bounded_checkpoint = |phase: &str| {
        let pager = runtime.db().store().pager().expect("pager").clone();
        let before = pager.cache_stats();
        runtime.checkpoint().expect(phase);
        let after = pager.cache_stats();
        let reads = after.hits + after.misses - before.hits - before.misses;
        assert!(
            reads < 128,
            "{phase} scanned unchanged history: {reads} page reads"
        );
    };

    set_current_connection_id(101);
    runtime.execute_query("BEGIN").expect("pending transaction");
    runtime
        .execute_query("UPDATE pending_payload SET n=9 WHERE id=1")
        .expect("pending row");
    set_current_connection_id(202);
    runtime
        .execute_query("UPDATE pending_payload SET n=2 WHERE id=2")
        .expect("acknowledged row");
    bounded_checkpoint("committed row beside pending row");
    set_current_connection_id(101);
    runtime.execute_query("ROLLBACK").expect("rollback");
    set_current_connection_id(202);
    bounded_checkpoint("rollback visibility");

    set_current_connection_id(303);
    runtime.execute_query("BEGIN").expect("later commit");
    runtime
        .execute_query("UPDATE pending_payload SET n=3 WHERE id=3")
        .expect("later row");
    set_current_connection_id(202);
    bounded_checkpoint("second pending row");
    set_current_connection_id(303);
    runtime
        .execute_query("COMMIT")
        .expect("commit after checkpoint");
    set_current_connection_id(202);
    bounded_checkpoint("committed visibility without a new row mutation");
    drop(runtime);
    clear_current_connection_id();

    let recovered = RedDBRuntime::with_options(options()).expect("reopen");
    for (id, expected) in [(1, 0), (2, 2), (3, 3)] {
        let read = recovered
            .execute_query(&format!("SELECT n FROM pending_payload WHERE id={id}"))
            .expect("durable row");
        assert_eq!(read.result.records.len(), 1);
        assert_eq!(
            read.result.records[0].get("n").expect("n").to_string(),
            expected.to_string()
        );
    }
    let plan = recovered
        .execute_query("EXPLAIN SELECT * FROM pending_payload WHERE id=2")
        .expect("recovered index");
    assert!(plan.result.records.iter().any(|record| record
        .get("op")
        .is_some_and(|value| value == &reddb_types::Value::text("index_seek"))));
}

#[test]
fn one_committed_update_checkpoints_a_bounded_set_of_pages() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("data.rdb");
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("paged profile");
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    runtime
        .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
        .expect("isolate checkpoint work");
    runtime
        .execute_query("CREATE TABLE checkpoint_payload (id INT, n INT, payload TEXT)")
        .expect("table");
    let payload = "x".repeat(4096);
    for batch in 0..10 {
        let rows = (batch * 100..(batch + 1) * 100)
            .map(|id| format!("({id}, 0, '{payload}')"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO checkpoint_payload (id, n, payload) VALUES {rows}"
            ))
            .expect("seed payloads");
    }
    runtime.checkpoint().expect("initial checkpoint");
    runtime
        .execute_query("UPDATE checkpoint_payload SET n = 1 WHERE id = 1")
        .expect("single committed update");
    let pager = runtime.db().store().pager().expect("pager").clone();
    let before = pager.cache_stats();
    runtime.checkpoint().expect("updated checkpoint");
    let after = pager.cache_stats();
    let reads = after.hits + after.misses - before.hits - before.misses;
    assert!(
        reads < 128,
        "one updated row caused {reads} page reads across unchanged payloads"
    );
    eprintln!("single committed update checkpoint page reads: {reads}");
    drop(pager);
    drop(runtime);
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("paged profile");
    let recovered = RedDBRuntime::with_options(options).expect("reopen sparse checkpoint");
    let read = recovered
        .execute_query("SELECT n, payload FROM checkpoint_payload WHERE id = 1")
        .expect("recovered row");
    assert_eq!(read.result.records.len(), 1);
    assert_eq!(read.result.records[0].get("n").expect("n").to_string(), "1");
    assert_eq!(
        read.result.records[0].get("payload").expect("payload"),
        &reddb_types::Value::text(payload)
    );
}

#[test]
fn open_and_rolled_back_changes_stay_out_of_the_durable_projection() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("data.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    };
    let runtime = RedDBRuntime::with_options(options()).expect("runtime");
    for query in [
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
        "CREATE TABLE checkpoint_payload (id INT, n INT, payload TEXT)",
        "INSERT INTO checkpoint_payload (id, n, payload) VALUES (1, 0, 'original')",
    ] {
        runtime.execute_query(query).expect("seed");
    }
    runtime.checkpoint().expect("initial checkpoint");
    runtime.execute_query("BEGIN").expect("begin");
    runtime
        .execute_query("UPDATE checkpoint_payload SET n = 1 WHERE id = 1")
        .expect("open update");
    runtime.checkpoint().expect("checkpoint with open update");
    runtime.execute_query("ROLLBACK").expect("rollback");
    runtime.checkpoint().expect("checkpoint after rollback");
    let read = runtime
        .execute_query("SELECT n FROM checkpoint_payload WHERE id = 1")
        .expect("read rollback");
    assert_eq!(read.result.records[0].get("n").expect("n").to_string(), "0");
    runtime.execute_query("BEGIN").expect("second begin");
    runtime
        .execute_query("UPDATE checkpoint_payload SET n = 2 WHERE id = 1")
        .expect("committed update");
    runtime.execute_query("COMMIT").expect("commit");
    runtime.checkpoint().expect("committed checkpoint");
    drop(runtime);
    let recovered = RedDBRuntime::with_options(options()).expect("reopen");
    let read = recovered
        .execute_query("SELECT n FROM checkpoint_payload WHERE id = 1")
        .expect("read recovered commit");
    assert_eq!(read.result.records.len(), 1);
    assert_eq!(read.result.records[0].get("n").expect("n").to_string(), "2");
}

#[cfg(debug_assertions)]
fn table_root(runtime: &RedDBRuntime) -> u32 {
    use reddb_server::storage::engine::HEADER_SIZE;
    let store = runtime.db().store();
    let page = store
        .pager()
        .expect("pager")
        .read_manifest_page()
        .expect("catalog");
    let content = &page.as_bytes()[HEADER_SIZE..];
    let header = reddb_file::decode_native_paged_metadata_header(content)
        .expect("metadata")
        .expect("direct metadata");
    let mut position = reddb_file::METADATA_HEADER_BYTES;
    for _ in 0..header.collection_count {
        let root = reddb_file::decode_native_paged_collection_root(content, &mut position)
            .expect("collection root");
        if root.collection == "checkpoint_payload" {
            return root.root_page;
        }
    }
    panic!("missing test table root");
}

#[cfg(debug_assertions)]
#[test]
fn sparse_root_crash_worker() {
    use reddb_server::storage::engine::PageType;
    let Some(path) = std::env::var_os("REDDB_SPARSE_ROOT_CRASH_PATH") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("paged profile");
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    runtime
        .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
        .expect("pause GC");
    runtime
        .execute_query("CREATE TABLE checkpoint_payload (id INT, n INT, payload TEXT)")
        .expect("table");
    let mut count = 0;
    for id in 1..500 {
        runtime
            .execute_query(&format!(
                "INSERT INTO checkpoint_payload (id, n, payload) VALUES ({id}, 0, 'retained-row')"
            ))
            .expect("baseline row");
        runtime.checkpoint().expect("baseline checkpoint");
        let store = runtime.db().store();
        let page = store
            .pager()
            .expect("pager")
            .read_page(table_root(&runtime))
            .expect("root page");
        assert_eq!(
            page.page_type().expect("page type"),
            PageType::BTreeLeaf,
            "seed crossed leaf capacity"
        );
        if page.header().expect("page header").free_space() < 512 {
            count = id;
            break;
        }
    }
    assert!(count > 1, "no near-full leaf found");
    std::fs::write(path.with_extension("rows"), count.to_string()).expect("row count");
    for revision in 1..50 {
        runtime
            .execute_query(&format!(
                "UPDATE checkpoint_payload SET n = {revision} WHERE id = 1"
            ))
            .expect("acknowledged update");
        std::fs::write(path.with_extension("expected"), revision.to_string())
            .expect("acknowledged value");
        runtime.checkpoint().expect("checkpoint before root split");
    }
    panic!("sparse projection did not reach the root-change crash point");
}

#[cfg(debug_assertions)]
#[test]
fn interrupted_sparse_root_publication_preserves_all_acknowledged_rows() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("data.rdb");
    let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "sparse_root_crash_worker", "--nocapture"])
        .env("REDDB_SPARSE_ROOT_CRASH_PATH", &path)
        .env("REDDB_TEST_CHECKPOINT_EXIT_AFTER_SPARSE_ROOT_CHANGE", "1")
        .status()
        .expect("crash worker");
    assert_eq!(
        status.code(),
        Some(176),
        "root-change crash point was not reached"
    );
    let expected =
        std::fs::read_to_string(path.with_extension("expected")).expect("expected value");
    let count = std::fs::read_to_string(path.with_extension("rows")).expect("baseline row count");
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("paged profile");
    let recovered =
        RedDBRuntime::with_options(options).expect("recover interrupted root publication");
    let value = recovered
        .execute_query("SELECT n FROM checkpoint_payload WHERE id = 1")
        .expect("recovered value");
    assert_eq!(
        value.result.records[0].get("n").expect("n").to_string(),
        expected
    );
    let rows = recovered
        .execute_query("SELECT COUNT(*) AS total FROM checkpoint_payload")
        .expect("all acknowledged rows");
    assert_eq!(
        rows.result.records[0]
            .get("total")
            .expect("count")
            .to_string(),
        count
    );
}

#[test]
fn exhausted_tracking_falls_back_then_resumes_after_publication() {
    let directory = tempfile::tempdir().expect("temporary database");
    let path = directory.path().join("data.rdb");
    let options = RedDBOptions::persistent(&path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("paged profile");
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    runtime
        .execute_query("SET CONFIG runtime.mvcc.autovacuum.enabled = false")
        .expect("isolate checkpoint work");
    runtime
        .execute_query("CREATE TABLE checkpoint_payload (id INT, n INT, payload TEXT)")
        .expect("table");
    let payload = "x".repeat(4096);
    for batch in 0..10 {
        let rows = (batch * 100..(batch + 1) * 100)
            .map(|id| format!("({id}, 0, '{payload}')"))
            .collect::<Vec<_>>()
            .join(",");
        runtime
            .execute_query(&format!(
                "INSERT INTO checkpoint_payload (id, n, payload) VALUES {rows}"
            ))
            .expect("seed payloads");
    }
    runtime.checkpoint().expect("initial checkpoint");
    for revision in 1..=260 {
        runtime
            .execute_query(&format!(
                "UPDATE checkpoint_payload SET n = {revision} WHERE id = 1"
            ))
            .expect("acknowledged update");
    }
    runtime
        .checkpoint()
        .expect("conservative checkpoint after tracking exhaustion");
    runtime
        .execute_query("UPDATE checkpoint_payload SET n = 261 WHERE id = 1")
        .expect("update after fallback");
    let pager = runtime.db().store().pager().expect("pager").clone();
    let before = pager.cache_stats();
    runtime.checkpoint().expect("sparse tracking resumed");
    let after = pager.cache_stats();
    let reads = after.hits + after.misses - before.hits - before.misses;
    assert!(
        reads < 128,
        "tracking did not resume after fallback: {reads} page reads"
    );
    let read = runtime
        .execute_query("SELECT n FROM checkpoint_payload WHERE id = 1")
        .expect("last acknowledged value");
    assert_eq!(
        read.result.records[0].get("n").expect("n").to_string(),
        "261"
    );
}
