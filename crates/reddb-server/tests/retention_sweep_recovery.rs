use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn make_rows_expired(rt: &RedDBRuntime) {
    let manager = rt.db().store().get_collection("events").unwrap();
    for mut entity in manager.query_all(|_| true) {
        entity.created_at = 1;
        let row = match &mut entity.data {
            reddb_server::storage::EntityData::Row(row) => row,
            _ => panic!("expected row"),
        };
        if let Some(named) = &mut row.named {
            named.insert("created_at".into(), Value::UnsignedInteger(1));
        }
        if let Some(schema) = &row.schema {
            let index = schema.iter().position(|name| name == "created_at").unwrap();
            row.columns[index] = Value::UnsignedInteger(1);
        }
        assert_eq!(
            row.get_field("created_at"),
            Some(&Value::UnsignedInteger(1))
        );
        manager.update(entity).unwrap();
    }
}

#[test]
fn sweep_limit_does_not_leak_into_user_deletes() {
    let rt = RedDBRuntime::in_memory().unwrap();
    rt.execute_query("CREATE TABLE events (id INTEGER) WITH timestamps = true")
        .unwrap();
    rt.execute_query("INSERT INTO events (id) VALUES (1), (2), (3), (4)")
        .unwrap();
    make_rows_expired(&rt);
    rt.execute_query("INSERT INTO events (id) VALUES (5)")
        .unwrap();
    rt.execute_query("ALTER COLLECTION events SET RETENTION 1 d")
        .unwrap();
    rt.sweep_retention_tick(1);
    rt.execute_query("ALTER COLLECTION events UNSET RETENTION")
        .unwrap();
    assert_eq!(
        rt.execute_query("DELETE FROM events WHERE id < 5")
            .unwrap()
            .affected_rows,
        3
    );
    assert_eq!(
        rt.execute_query("SELECT id FROM events")
            .unwrap()
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn retention_wal_child() {
    let Some(path) = std::env::var_os("REDDB_RETENTION_SWEEP_CHILD_PATH") else {
        return;
    };
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path)).unwrap();
    rt.sweep_retention_tick(2);
    // Exit without destructors or an explicit checkpoint after the sweep.
    std::process::exit(0);
}

#[test]
fn bounded_sweep_replays_committed_deletes_after_process_exit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("unclean.rdb");
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        rt.execute_query("CREATE TABLE events (id INTEGER) WITH timestamps = true")
            .unwrap();
        rt.execute_query("INSERT INTO events (id) VALUES (1), (2), (3), (4), (5)")
            .unwrap();
        make_rows_expired(&rt);
        rt.execute_query("ALTER COLLECTION events SET RETENTION 1 s")
            .unwrap();
        rt.flush().unwrap();
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "retention_wal_child", "--nocapture"])
        .env("REDDB_RETENTION_SWEEP_CHILD_PATH", &path)
        .status()
        .unwrap();
    assert!(status.success());
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        rt.execute_query("ALTER COLLECTION events UNSET RETENTION")
            .unwrap();
        assert_eq!(
            rt.execute_query("SELECT id FROM events")
                .unwrap()
                .result
                .records
                .len(),
            3
        );
        rt.execute_query("ALTER COLLECTION events SET RETENTION 1 s")
            .unwrap();
        rt.sweep_retention_tick(2);
    }
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    rt.execute_query("ALTER COLLECTION events UNSET RETENTION")
        .unwrap();
    assert_eq!(
        rt.execute_query("SELECT id FROM events")
            .unwrap()
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn mutable_retention_ties_are_bounded_and_resume_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retention.rdb");
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        rt.execute_query("CREATE TABLE events (id INTEGER) WITH timestamps = true")
            .unwrap();
        rt.execute_query("INSERT INTO events (id) VALUES (1), (2), (3), (4), (5)")
            .unwrap();
        make_rows_expired(&rt);
        rt.flush().unwrap();
        rt.execute_query("ALTER COLLECTION events SET RETENTION 1 s")
            .unwrap();
        rt.sweep_retention_tick(2);
        rt.execute_query("ALTER COLLECTION events UNSET RETENTION")
            .unwrap();
        let rows = rt.execute_query("SELECT id FROM events").unwrap();
        assert_eq!(
            rows.result.records.len(),
            3,
            "one tick removes exactly two tied rows"
        );
        rt.execute_query("ALTER COLLECTION events SET RETENTION 1 s")
            .unwrap();
    }
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        rt.sweep_retention_tick(2);
        rt.execute_query("ALTER COLLECTION events UNSET RETENTION")
            .unwrap();
        assert_eq!(
            rt.execute_query("SELECT id FROM events")
                .unwrap()
                .result
                .records
                .len(),
            1
        );
        rt.execute_query("ALTER COLLECTION events SET RETENTION 1 s")
            .unwrap();
    }
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        rt.sweep_retention_tick(2);
        rt.execute_query("ALTER COLLECTION events UNSET RETENTION")
            .unwrap();
        assert!(rt
            .execute_query("SELECT id FROM events")
            .unwrap()
            .result
            .records
            .is_empty());
    }
}
