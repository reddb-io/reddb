use reddb_file::OperationalManifest;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

#[test]
fn append_only_checkpoints_keep_rows_inside_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("append_only.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        runtime
            .execute_query("CREATE TABLE events (id INT, msg TEXT) APPEND ONLY")
            .unwrap();
        runtime
            .execute_query("INSERT INTO events (id, msg) VALUES (1, 'a'), (2, 'b')")
            .unwrap();
        runtime.flush().unwrap();
    }
    for expected in [2, 3] {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM events")
                .unwrap()
                .result
                .records
                .len(),
            expected
        );
        assert!(runtime.execute_query("DELETE FROM events").is_err());
        assert!(runtime
            .execute_query("UPDATE events SET msg = 'denied'")
            .is_err());
        if expected == 2 {
            runtime
                .execute_query("INSERT INTO events (id, msg) VALUES (3, 'c')")
                .unwrap();
        }
        runtime.flush().unwrap();
        let manifest = OperationalManifest::for_db_path(&path);
        assert!(!manifest.current_manifest_path_for_test().exists());
        assert!(manifest.append_only_segments_for_test().unwrap().is_empty());
    }
}

#[test]
fn append_only_retention_deletes_expired_rows_without_external_segments() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retention.rdb");
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    runtime
        .execute_query("CREATE TABLE events (id INT, msg TEXT, ts TIMESTAMP) APPEND ONLY")
        .unwrap();
    runtime
        .execute_query("INSERT INTO events (id, msg, ts) VALUES (1, 'expired', 1)")
        .unwrap();
    runtime.flush().unwrap();
    runtime
        .execute_query("ALTER COLLECTION events SET RETENTION 1 d")
        .unwrap();
    let live_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    runtime
        .execute_query(&format!(
            "INSERT INTO events (id, msg, ts) VALUES (2, 'live', {live_ts})"
        ))
        .unwrap();
    runtime.flush().expect("live row checkpoint");
    let manager = runtime.db().store().get_collection("events").unwrap();
    // A tick deliberately yields when a background checkpoint owns the
    // publication fence. Retry bounded work instead of assuming immediate entry.
    for _ in 0..100 {
        runtime.sweep_retention_tick(1000);
        if manager.count() == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(manager.count(), 1);
    assert!(runtime.execute_query("DELETE FROM events").is_err());
    let result = runtime.execute_query("SELECT rows_swept_total, segments_retired_total FROM red.retention WHERE name = 'events'").unwrap();
    let row = &result.result.records[0];
    assert_eq!(
        row.get("rows_swept_total"),
        Some(&Value::UnsignedInteger(1))
    );
    assert_eq!(
        row.get("segments_retired_total"),
        Some(&Value::UnsignedInteger(0))
    );
    runtime.flush().unwrap();
    drop(manager);
    drop(runtime);
    let cold = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    assert_eq!(
        cold.db().store().get_collection("events").unwrap().count(),
        1
    );
    assert_eq!(
        cold.execute_query("SELECT id FROM events")
            .unwrap()
            .result
            .records[0]
            .get("id"),
        Some(&Value::Integer(2))
    );
    assert!(!OperationalManifest::for_db_path(&path)
        .current_manifest_path_for_test()
        .exists());
}
