use reddb_server::replication::ReplicationConfig;
use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::{RedDBOptions, RedDBRuntime};

fn logical_records(runtime: &RedDBRuntime) -> Vec<reddb_server::replication::cdc::ChangeRecord> {
    let plan = runtime.primary_replica_file_plan().expect("file plan");
    let path = plan.wal_segment_path(1);
    if !path.exists() {
        return Vec::new();
    }
    reddb_file::PrimaryReplicaWalSegment::read_from_path(path)
        .expect("logical WAL")
        .records
        .into_iter()
        .map(|record| {
            reddb_server::replication::cdc::ChangeRecord::decode(&record.payload).expect("record")
        })
        .collect()
}

#[test]
fn rollback_never_publishes_logical_changes() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(directory.path().join("primary.rdb"))
            .with_replication(ReplicationConfig::primary()),
    )
    .expect("primary");
    runtime
        .execute_query("CREATE TABLE rollback_rows (id INT)")
        .expect("schema");
    let before = runtime.primary_logical_head_lsn();
    set_current_connection_id(9001);
    runtime.execute_query("BEGIN").expect("begin");
    runtime
        .execute_query("INSERT INTO rollback_rows (id) VALUES (42)")
        .expect("insert");
    let during = runtime.primary_logical_head_lsn();
    runtime.execute_query("ROLLBACK").expect("rollback");
    clear_current_connection_id();
    assert_eq!(during, before, "uncommitted changes must remain private");
    assert_eq!(runtime.primary_logical_head_lsn(), before);
    assert!(logical_records(&runtime).is_empty());
    assert!(runtime
        .execute_query("SELECT * FROM rollback_rows")
        .expect("query")
        .result
        .records
        .is_empty());
}

#[test]
fn commit_publishes_only_surviving_savepoint_changes_in_commit_order() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("primary.rdb");
    let options = RedDBOptions::persistent(&path).with_replication(ReplicationConfig::primary());
    let primary = RedDBRuntime::with_options(options.clone()).expect("primary");
    let replica = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("replica store");
    for runtime in [&primary, &replica] {
        runtime
            .execute_query("CREATE TABLE committed_rows (id INT)")
            .expect("schema");
    }
    set_current_connection_id(9002);
    primary.execute_query("BEGIN").expect("begin");
    primary
        .execute_query("INSERT INTO committed_rows (id) VALUES (1)")
        .expect("insert");
    primary
        .execute_query("SAVEPOINT discarded")
        .expect("savepoint");
    primary
        .execute_query("INSERT INTO committed_rows (id) VALUES (2)")
        .expect("insert");
    primary
        .execute_query("ROLLBACK TO SAVEPOINT discarded")
        .expect("rollback savepoint");
    clear_current_connection_id();
    primary
        .execute_query("INSERT INTO committed_rows (id) VALUES (3)")
        .expect("autocommit");
    set_current_connection_id(9002);
    primary.execute_query("COMMIT").expect("commit");
    clear_current_connection_id();
    let records = logical_records(&primary);
    assert_eq!(records.len(), 2, "aborted savepoint must not be published");
    assert_eq!(
        records.iter().map(|record| record.lsn).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let applier = reddb_server::replication::logical::LogicalChangeApplier::new(0);
    for record in &records {
        applier
            .apply(
                replica.db().as_ref(),
                record,
                reddb_server::replication::logical::ApplyMode::Replica,
            )
            .expect("apply");
    }
    assert_eq!(
        replica
            .execute_query("SELECT * FROM committed_rows")
            .expect("query")
            .result
            .records
            .len(),
        2
    );
    assert!(replica
        .execute_query("SELECT * FROM committed_rows WHERE id = 2")
        .expect("query")
        .result
        .records
        .is_empty());
    drop(primary);
    let reopened = RedDBRuntime::with_options(options).expect("reopen");
    assert_eq!(
        logical_records(&reopened).len(),
        2,
        "restart does not duplicate projection"
    );
}

#[test]
#[cfg(debug_assertions)]
fn durable_transaction_recovers_projection_after_process_loss() {
    for point in ["before_wal", "after_wal", "after_projection"] {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("primary.rdb");
        let options = RedDBOptions::persistent(&path).with_replication(ReplicationConfig::primary());
        {
            let runtime = RedDBRuntime::with_options(options.clone()).expect("primary");
            runtime.execute_query("CREATE TABLE crash_rows (id INT)").expect("schema");
            runtime.flush().expect("checkpoint schema");
        }
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "transaction_projection_crash_child", "--nocapture"])
            .env("REDDB_TRANSACTION_REPLICATION_CRASH_PATH", &path)
            .env("REDDB_TRANSACTION_REPLICATION_CRASH_AT", point)
            .status().expect("crash child");
        assert_eq!(status.code(), Some(173), "crash at {point}");
        let recovered = RedDBRuntime::with_options(options.clone()).expect("recover");
        let expected = if point == "before_wal" { 0 } else { 2 };
        assert_eq!(recovered.execute_query("SELECT * FROM crash_rows").expect("query").result.records.len(), expected, "source after {point}");
        assert_eq!(logical_records(&recovered).len(), expected, "projection after {point}");
        drop(recovered);
        let reopened = RedDBRuntime::with_options(options).expect("reopen recovered primary");
        assert_eq!(logical_records(&reopened).len(), expected, "idempotent recovery after {point}");
    }
}

#[test]
#[cfg(debug_assertions)]
fn transaction_projection_crash_child() {
    let Ok(path) = std::env::var("REDDB_TRANSACTION_REPLICATION_CRASH_PATH") else { return; };
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(path).with_replication(ReplicationConfig::primary()),
    ).expect("primary");
    set_current_connection_id(9010);
    runtime.execute_query("BEGIN").expect("begin");
    runtime.execute_query("INSERT INTO crash_rows (id) VALUES (1), (2)").expect("insert");
    runtime.execute_query("COMMIT").expect("commit");
    panic!("expected crash point");
}
