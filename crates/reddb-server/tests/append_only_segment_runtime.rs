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
        .execute_query("CREATE TABLE events (id INT, msg TEXT) WITH timestamps = true APPEND ONLY")
        .unwrap();
    runtime
        .execute_query("INSERT INTO events (id, msg) VALUES (1, 'expired')")
        .unwrap();
    let manager = runtime.db().store().get_collection("events").unwrap();
    let mut entity = manager.query_all(|_| true).remove(0);
    entity.created_at = 1;
    if let reddb_server::storage::EntityData::Row(row) = &mut entity.data {
        if let Some(named) = &mut row.named {
            named.insert("created_at".into(), Value::UnsignedInteger(1));
        }
        if let Some(schema) = &row.schema {
            let index = schema.iter().position(|name| name == "created_at").unwrap();
            row.columns[index] = Value::UnsignedInteger(1);
        }
    }
    manager.update(entity).unwrap();
    runtime.flush().unwrap();
    runtime
        .execute_query("ALTER COLLECTION events SET RETENTION 1 d")
        .unwrap();
    runtime
        .execute_query("INSERT INTO events (id, msg) VALUES (2, 'live')")
        .unwrap();
    runtime.sweep_retention_tick(1000);
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
