use std::fs;

use reddb_server::replication::cdc::ChangeRecord;
use reddb_server::replication::logical::{ApplyMode, LogicalChangeApplier};
use reddb_server::{RedDBOptions, RedDBRuntime, ReplicationConfig};

#[path = "support/primary_replica_file.rs"]
mod primary_replica_file;

#[test]
fn runtime_promotes_ready_rebootstrap_pending_file_on_restart() {
    let data_path = primary_replica_file::temp_data_path("replica_rebootstrap_restart");
    let source_path = primary_replica_file::temp_data_path("replica_rebootstrap_source");
    primary_replica_file::cleanup(&data_path);
    primary_replica_file::cleanup(&source_path);

    {
        let old_runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("old runtime");
        old_runtime
            .execute_query("INSERT INTO old_items (id, name) VALUES (1, 'old')")
            .expect("insert old row");
        old_runtime.flush().expect("flush old runtime");
    }

    let (checkpoint_lsn, post_checkpoint_records) = {
        let source_runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&source_path).with_replication(ReplicationConfig::primary()),
        )
        .expect("source");
        source_runtime
            .execute_query("INSERT INTO new_items (id, name) VALUES (7, 'new')")
            .expect("insert new row");
        let manifest = source_runtime
            .create_primary_replica_basebackup(64)
            .expect("create source basebackup")
            .expect("source basebackup manifest");
        let plan = source_runtime
            .primary_replica_file_plan()
            .expect("source primary-replica plan");
        let pending_path = reddb_file::layout::rebootstrap_pending_path(&data_path);
        let checkpoint_lsn = source_runtime
            .materialize_primary_replica_basebackup_snapshot(
                &manifest,
                plan.basebackup_dir(),
                &pending_path,
            )
            .expect("materialize pending rebootstrap");
        reddb_file::write_rebootstrap_ready_marker(
            &data_path,
            &reddb_file::ReplicaRebootstrapReadyMarker {
                pending_path,
                checkpoint_lsn,
                timeline: manifest.timeline,
            },
        )
        .expect("write ready marker");
        source_runtime
            .execute_query("INSERT INTO new_items (id, name) VALUES (8, 'after')")
            .expect("insert post-checkpoint row");
        source_runtime.flush().expect("flush source runtime");
        let post_checkpoint_records =
            reddb_server::replication::primary::LogicalWalSpool::open(&source_path)
                .expect("open source logical wal spool")
                .read_since(checkpoint_lsn, 10)
                .expect("read post-checkpoint logical wal");
        assert!(
            !post_checkpoint_records.is_empty(),
            "source primary must retain WAL records after checkpoint_lsn={checkpoint_lsn}"
        );
        (checkpoint_lsn, post_checkpoint_records)
    };

    let promoted =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("promoted runtime");
    let result = promoted
        .execute_query("SELECT id, name FROM new_items WHERE id = 7")
        .expect("query promoted row");
    assert_eq!(result.result.len(), 1);
    assert!(promoted
        .execute_query("SELECT id, name FROM old_items WHERE id = 1")
        .is_err());
    assert!(
        reddb_file::layout::rebootstrap_previous_path(&data_path).exists(),
        "old active database should be retained as previous"
    );
    assert!(
        !reddb_file::layout::rebootstrap_ready_marker_path(&data_path).exists(),
        "ready marker should be consumed after promotion"
    );
    assert!(
        !reddb_file::layout::rebootstrap_pending_path(&data_path).exists(),
        "pending database should be promoted into active path"
    );
    let config = promoted
        .execute_query("SHOW CONFIG red.replication.last_applied_lsn")
        .expect("show last applied lsn");
    let config_value = config
        .result
        .records
        .first()
        .and_then(|record| record.get("value"))
        .map(|value| format!("{value:?}"))
        .expect("last_applied_lsn config value");
    assert!(
        config_value.contains(&checkpoint_lsn.to_string()),
        "promoted replica must resume WAL from checkpoint_lsn={checkpoint_lsn}, got {config_value}"
    );
    let applier = LogicalChangeApplier::new(checkpoint_lsn);
    let promoted_db = promoted.db();
    let mut last_applied = checkpoint_lsn;
    for (lsn, data) in post_checkpoint_records {
        let record = ChangeRecord::decode(&data).expect("decode post-checkpoint logical WAL");
        assert!(
            record.lsn > checkpoint_lsn,
            "post-checkpoint WAL record must advance beyond checkpoint"
        );
        let outcome = applier
            .apply(promoted_db.as_ref(), &record, ApplyMode::Replica)
            .expect("apply post-checkpoint WAL after rebootstrap promotion");
        if matches!(
            outcome,
            reddb_server::replication::logical::ApplyOutcome::Applied
        ) {
            last_applied = lsn;
        }
    }
    assert_eq!(
        applier.last_applied_lsn(),
        last_applied,
        "logical applier should advance from checkpoint through post-checkpoint WAL"
    );
    let post_result = promoted
        .execute_query("SELECT id, name FROM new_items WHERE id = 8")
        .expect("query post-checkpoint replicated row");
    assert_eq!(
        post_result.result.len(),
        1,
        "post-checkpoint WAL row should be visible after replay"
    );
    let _ = fs::remove_file(reddb_file::layout::rebootstrap_previous_path(&data_path));
    primary_replica_file::cleanup(&data_path);
    primary_replica_file::cleanup(&source_path);
}

#[test]
fn runtime_consumes_leftover_rebootstrap_marker_after_completed_rename_crash() {
    let data_path = primary_replica_file::temp_data_path("replica_rebootstrap_marker_after_rename");
    let source_path =
        primary_replica_file::temp_data_path("replica_rebootstrap_marker_after_rename_source");
    primary_replica_file::cleanup(&data_path);
    primary_replica_file::cleanup(&source_path);

    {
        let old_runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("old runtime");
        old_runtime
            .execute_query("INSERT INTO old_items (id, name) VALUES (1, 'old')")
            .expect("insert old row");
        old_runtime.flush().expect("flush old runtime");
    }

    let pending_path = reddb_file::layout::rebootstrap_pending_path(&data_path);
    let checkpoint_lsn = {
        let source_runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&source_path).with_replication(ReplicationConfig::primary()),
        )
        .expect("source runtime");
        source_runtime
            .execute_query("INSERT INTO promoted_items (id, name) VALUES (7, 'promoted')")
            .expect("insert promoted row");
        let manifest = source_runtime
            .create_primary_replica_basebackup(64)
            .expect("create source basebackup")
            .expect("source basebackup manifest");
        let plan = source_runtime
            .primary_replica_file_plan()
            .expect("source primary-replica plan");
        let checkpoint_lsn = source_runtime
            .materialize_primary_replica_basebackup_snapshot(
                &manifest,
                plan.basebackup_dir(),
                &pending_path,
            )
            .expect("materialize pending rebootstrap");
        reddb_file::write_rebootstrap_ready_marker(
            &data_path,
            &reddb_file::ReplicaRebootstrapReadyMarker {
                pending_path: pending_path.clone(),
                checkpoint_lsn,
                timeline: manifest.timeline,
            },
        )
        .expect("write ready marker");
        checkpoint_lsn
    };

    let previous_path = reddb_file::layout::rebootstrap_previous_path(&data_path);
    let _ = fs::remove_file(&previous_path);
    fs::rename(&data_path, &previous_path).expect("simulate old data renamed to previous");
    fs::rename(&pending_path, &data_path).expect("simulate pending promoted to active");
    assert!(
        reddb_file::layout::rebootstrap_ready_marker_path(&data_path).exists(),
        "crash simulation leaves ready marker behind"
    );

    let reopened =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("reopen runtime");
    let result = reopened
        .execute_query("SELECT id, name FROM promoted_items WHERE id = 7")
        .expect("query promoted row");
    assert_eq!(result.result.len(), 1);
    let config_value =
        primary_replica_file::show_config_value(&reopened, "red.replication.last_applied_lsn");
    assert!(
        config_value.contains(&checkpoint_lsn.to_string()),
        "active database must prove it is the checkpointed rebootstrap snapshot, got {config_value}"
    );
    assert!(
        !reddb_file::layout::rebootstrap_ready_marker_path(&data_path).exists(),
        "leftover ready marker should be consumed only after active snapshot matches checkpoint"
    );
    assert!(
        previous_path.exists(),
        "previous database should remain for operator recovery"
    );

    let _ = fs::remove_file(previous_path);
    primary_replica_file::cleanup(&data_path);
    primary_replica_file::cleanup(&source_path);
}

#[test]
fn runtime_fails_closed_on_ready_rebootstrap_marker_with_missing_pending_file() {
    let data_path = primary_replica_file::temp_data_path("replica_rebootstrap_missing_pending");
    primary_replica_file::cleanup(&data_path);

    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime");
        runtime
            .execute_query("INSERT INTO stable_items (id, name) VALUES (1, 'stable')")
            .expect("insert stable row");
        runtime.flush().expect("flush runtime");
    }

    let pending_path = reddb_file::layout::rebootstrap_pending_path(&data_path);
    let _ = fs::remove_file(&pending_path);
    reddb_file::write_rebootstrap_ready_marker(
        &data_path,
        &reddb_file::ReplicaRebootstrapReadyMarker {
            pending_path: pending_path.clone(),
            checkpoint_lsn: 123_456,
            timeline: reddb_file::TimelineId::initial(),
        },
    )
    .expect("write ready marker");

    let err = match RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)) {
        Ok(_) => panic!("missing pending rebootstrap must fail closed"),
        Err(err) => err,
    };
    let message = err.to_string();
    assert!(
        message.contains("pending database is missing"),
        "error should identify missing pending rebootstrap, got {message}"
    );
    assert!(data_path.exists(), "active database must remain in place");
    assert!(
        reddb_file::layout::rebootstrap_ready_marker_path(&data_path).exists(),
        "failed closed path must preserve ready marker for operator recovery"
    );
    assert!(
        !reddb_file::layout::rebootstrap_previous_path(&data_path).exists(),
        "active database must not be renamed when pending file is missing"
    );

    let _ = fs::remove_file(reddb_file::layout::rebootstrap_ready_marker_path(
        &data_path,
    ));
    let reopened =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("reopen active db");
    let result = reopened
        .execute_query("SELECT id, name FROM stable_items WHERE id = 1")
        .expect("query stable row after failed closed rebootstrap");
    assert_eq!(result.result.len(), 1);

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_ignores_incomplete_rebootstrap_staging_without_ready_marker() {
    let data_path = primary_replica_file::temp_data_path("replica_rebootstrap_incomplete_staging");
    primary_replica_file::cleanup(&data_path);

    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime");
        runtime
            .execute_query("INSERT INTO stable_items (id, name) VALUES (1, 'stable')")
            .expect("insert stable row");
        runtime.flush().expect("flush runtime");
    }

    let staging_root = reddb_file::layout::rebootstrap_staging_root(&data_path);
    fs::create_dir_all(&staging_root).expect("create staging root");
    fs::write(staging_root.join("partial.chunk"), b"incomplete").expect("write partial chunk");

    let reopened =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("reopen runtime");
    let result = reopened
        .execute_query("SELECT id, name FROM stable_items WHERE id = 1")
        .expect("query stable row after incomplete staging");
    assert_eq!(
        result.result.len(),
        1,
        "incomplete basebackup staging without a ready marker must not replace active data"
    );
    assert!(
        !reddb_file::layout::rebootstrap_previous_path(&data_path).exists(),
        "active database should not be renamed when ready marker is absent"
    );

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_fails_closed_on_corrupt_ready_rebootstrap_pending_file() {
    let data_path = primary_replica_file::temp_data_path("replica_rebootstrap_corrupt_pending");
    primary_replica_file::cleanup(&data_path);

    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime");
        runtime
            .execute_query("INSERT INTO stable_items (id, name) VALUES (1, 'stable')")
            .expect("insert stable row");
        runtime.flush().expect("flush runtime");
    }

    let pending_path = reddb_file::layout::rebootstrap_pending_path(&data_path);
    fs::write(&pending_path, b"not an embedded rdb").expect("write corrupt pending rdb");
    reddb_file::write_rebootstrap_ready_marker(
        &data_path,
        &reddb_file::ReplicaRebootstrapReadyMarker {
            pending_path: pending_path.clone(),
            checkpoint_lsn: 9,
            timeline: reddb_file::TimelineId::initial(),
        },
    )
    .expect("write ready marker");

    let err = match RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)) {
        Ok(_) => panic!("corrupt pending rebootstrap must fail closed"),
        Err(err) => err,
    };
    let message = err.to_string();
    assert!(
        message.contains("pending replica rebootstrap"),
        "error should identify pending rebootstrap validation/open failure, got {message}"
    );
    assert!(
        data_path.exists(),
        "active database must remain in place after corrupt pending rebootstrap"
    );
    assert!(
        !reddb_file::layout::rebootstrap_previous_path(&data_path).exists(),
        "active database must not be renamed before pending validation succeeds"
    );

    let _ = fs::remove_file(reddb_file::layout::rebootstrap_ready_marker_path(
        &data_path,
    ));
    let _ = fs::remove_file(&pending_path);
    let reopened =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("reopen active db");
    let result = reopened
        .execute_query("SELECT id, name FROM stable_items WHERE id = 1")
        .expect("query stable row after failed closed rebootstrap");
    assert_eq!(
        result.result.len(),
        1,
        "failed rebootstrap must not corrupt active database"
    );

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_fails_closed_on_corrupt_ready_rebootstrap_marker() {
    let data_path = primary_replica_file::temp_data_path("replica_rebootstrap_corrupt_marker");
    let source_path =
        primary_replica_file::temp_data_path("replica_rebootstrap_corrupt_marker_source");
    primary_replica_file::cleanup(&data_path);
    primary_replica_file::cleanup(&source_path);

    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime");
        runtime
            .execute_query("INSERT INTO stable_items (id, name) VALUES (1, 'stable')")
            .expect("insert stable row");
        runtime.flush().expect("flush runtime");
    }

    let pending_path = reddb_file::layout::rebootstrap_pending_path(&data_path);
    {
        let source_runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&source_path).with_replication(ReplicationConfig::primary()),
        )
        .expect("source runtime");
        source_runtime
            .execute_query("INSERT INTO fresh_items (id, name) VALUES (7, 'fresh')")
            .expect("insert fresh row");
        let manifest = source_runtime
            .create_primary_replica_basebackup(64)
            .expect("create source basebackup")
            .expect("source basebackup manifest");
        let plan = source_runtime
            .primary_replica_file_plan()
            .expect("source primary-replica plan");
        source_runtime
            .materialize_primary_replica_basebackup_snapshot(
                &manifest,
                plan.basebackup_dir(),
                &pending_path,
            )
            .expect("materialize valid pending rebootstrap");
    }
    fs::write(
        reddb_file::layout::rebootstrap_ready_marker_path(&data_path),
        b"not json",
    )
    .expect("write corrupt ready marker");

    let err = match RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)) {
        Ok(_) => panic!("corrupt rebootstrap marker must fail closed"),
        Err(err) => err,
    };
    let message = err.to_string();
    assert!(
        message.contains("pending replica rebootstrap marker"),
        "error should identify ready marker corruption, got {message}"
    );
    assert!(
        data_path.exists(),
        "active database must remain in place after corrupt ready marker"
    );
    assert!(
        pending_path.exists(),
        "pending database must not be promoted when marker is corrupt"
    );
    assert!(
        !reddb_file::layout::rebootstrap_previous_path(&data_path).exists(),
        "active database must not be renamed before marker validation succeeds"
    );

    let _ = fs::remove_file(reddb_file::layout::rebootstrap_ready_marker_path(
        &data_path,
    ));
    let _ = fs::remove_file(&pending_path);
    let reopened =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("reopen active db");
    let result = reopened
        .execute_query("SELECT id, name FROM stable_items WHERE id = 1")
        .expect("query stable row after corrupt marker");
    assert_eq!(result.result.len(), 1);

    primary_replica_file::cleanup(&data_path);
    primary_replica_file::cleanup(&source_path);
}
