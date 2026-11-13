use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use reddb_server::serde_json;
use reddb_server::{RedDBOptions, RedDBRuntime, ReplicationConfig, StorageDeployPreset};

#[path = "support/primary_replica_file.rs"]
mod primary_replica_file;

const TIMELINE_CRASH_CHILD_ENV: &str = "REDDB_PRIMARY_REPLICA_TIMELINE_RUNTIME_CRASH_CHILD";
const TIMELINE_CRASH_DATA_PATH_ENV: &str = "REDDB_PRIMARY_REPLICA_TIMELINE_RUNTIME_CRASH_DATA_PATH";
const PRIMARY_REPLICA_CRASH_ENV: &str = "REDDB_PRIMARY_REPLICA_CRASH_AT";

#[test]
fn cold_primary_rejects_unrelated_slot_history_without_overwriting_it() {
    let directory = tempfile::tempdir().expect("foreign catalog fixture directory");
    let path = directory.path().join("database.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
            .expect("native replication profile")
    };
    let mut foreign = reddb_file::ReplicationSlotCatalog::new(reddb_file::TimelineId(99));
    foreign
        .upsert(reddb_file::ReplicationSlot::new(
            "foreign",
            foreign.timeline,
            0,
        ))
        .expect("foreign slot");
    let encoded = hex::encode(foreign.encode().expect("valid catalog checksum"));
    {
        let runtime = RedDBRuntime::with_options(options()).expect("fixture writer");
        runtime
            .execute_query("INSERT INTO preserved_items (id, name) VALUES (1, 'preserved')")
            .expect("fixture row");
        runtime
            .db()
            .store()
            .try_set_storage_metadata(
                "replication.slots",
                &serde_json::Value::String(encoded.clone()),
            )
            .expect("inject an unrelated checksummed catalog");
        runtime.flush().expect("persist fixture");
        let before = runtime
            .primary_replica_timeline_history()
            .expect("original timeline");
        let error = runtime
            .record_failover_timeline_promotion("candidate", 0)
            .expect_err("foreign slots must also refuse promotion");
        assert!(
            error.to_string().contains("outside the promotion history"),
            "{error}"
        );
        assert_eq!(
            runtime
                .primary_replica_timeline_history()
                .expect("timeline after refused promotion"),
            before
        );
    }
    let error =
        RedDBRuntime::with_options(options().with_replication(ReplicationConfig::primary()))
            .err()
            .expect("unrelated history must reject primary startup");
    assert!(
        error
            .to_string()
            .contains("outside the current timeline history"),
        "{error}"
    );
    let reader = RedDBRuntime::with_options(options().with_read_only(true))
        .expect("inspect retained authority");
    let catalog = reader
        .db()
        .store()
        .try_get_storage_metadata("replication.slots")
        .expect("read unmodified catalog")
        .expect("catalog retained");
    assert_eq!(catalog.as_text(), Some(encoded.as_str()));
    let row = reader
        .execute_query("SELECT name FROM preserved_items WHERE id = 1")
        .expect("read retained row");
    assert_eq!(row.result.len(), 1);
}

#[test]
fn promoted_checkpoint_preserves_shared_slots_and_invalidates_divergent_acks() {
    let directory = tempfile::tempdir().expect("promotion fixture directory");
    let source_path = directory.path().join("source.rdb");
    let promoted_path = directory.path().join("promoted.rdb");
    let options = |path: &std::path::Path| {
        RedDBOptions::persistent(path)
            .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
            .expect("native replication profile")
    };
    let source = RedDBRuntime::with_options(
        options(&source_path).with_replication(ReplicationConfig::primary()),
    )
    .expect("source primary");
    source
        .execute_query("INSERT INTO promotion_items (id, name) VALUES (1, 'shared-first')")
        .expect("first committed row");
    let shared_slot_lsn = source.primary_logical_head_lsn();
    source
        .execute_query("INSERT INTO promotion_items (id, name) VALUES (2, 'shared-second')")
        .expect("second committed row");
    let manifest = source
        .create_primary_replica_basebackup(64 * 1024)
        .expect("checkpoint at the promotion boundary")
        .expect("checkpoint manifest");
    let source_plan = source
        .primary_replica_file_plan()
        .expect("source file plan");
    let fork_lsn = source
        .materialize_primary_replica_basebackup_snapshot(
            &manifest,
            source_plan.basebackup_dir(),
            &promoted_path,
        )
        .expect("materialize the future promoted replica");
    source
        .execute_query("INSERT INTO promotion_items (id, name) VALUES (3, 'old-branch-only')")
        .expect("old primary extends its divergent branch");
    let divergent_lsn = source.primary_logical_head_lsn();
    assert!(shared_slot_lsn < fork_lsn && fork_lsn < divergent_lsn);
    source.flush().expect("persist old branch");
    drop(source);

    // Restore an ancestor catalog. Both positions belong to real source
    // commits, but only one is inside this checkpoint's common ancestry.
    let mut inherited = reddb_file::ReplicationSlotCatalog::new(reddb_file::TimelineId(1));
    for (id, lsn) in [("shared", shared_slot_lsn), ("divergent", divergent_lsn)] {
        inherited
            .upsert(reddb_file::ReplicationSlot::new(
                id,
                inherited.timeline,
                lsn,
            ))
            .expect("ancestor slot fixture");
    }
    let catalog_path =
        reddb_server::replication::primary::PrimaryReplication::slot_catalog_path_for(
            &promoted_path,
        );
    let candidate = RedDBRuntime::with_options(options(&promoted_path)).expect("candidate");
    candidate
        .record_failover_timeline_promotion("candidate", fork_lsn)
        .expect("persist promoted timeline");
    candidate
        .db()
        .store()
        .try_set_storage_metadata(
            "replication.slots",
            &serde_json::Value::String(hex::encode(inherited.encode().expect("ancestor catalog"))),
        )
        .expect("restore the source catalog at the promotion boundary");
    candidate.flush().expect("persist promotion");
    drop(candidate);

    for _ in 0..2 {
        let promoted = RedDBRuntime::with_options(
            options(&promoted_path).with_replication(ReplicationConfig::primary().with_term(2)),
        )
        .expect("cold promoted primary");
        let catalog = promoted
            .primary_replica_slot_catalog()
            .expect("durable promoted catalog")
            .expect("slot catalog");
        assert_eq!(catalog.timeline, reddb_file::TimelineId(2));
        let shared = catalog
            .slots
            .iter()
            .find(|slot| slot.replica_id == "shared")
            .expect("shared slot");
        assert!(shared.active);
        assert_eq!(shared.restart_lsn, shared_slot_lsn);
        assert_eq!(shared.confirmed_lsn(), shared_slot_lsn);
        let divergent = catalog
            .slots
            .iter()
            .find(|slot| slot.replica_id == "divergent")
            .expect("divergent slot");
        assert!(!divergent.active);
        assert_eq!(
            divergent.invalidation_reason,
            Some(reddb_file::ReplicationSlotInvalidationCause::WalRemoved)
        );
        assert!(catalog
            .slots
            .iter()
            .all(|slot| slot.timeline == catalog.timeline));
        let retention = promoted
            .primary_replica_wal_retention_plan()
            .expect("retention remains available")
            .expect("retention plan");
        assert_eq!(retention.oldest_required_lsn, Some(shared_slot_lsn));
        for id in [1, 2, 3] {
            let row = promoted
                .execute_query(&format!("SELECT name FROM promotion_items WHERE id = {id}"))
                .expect("cold point read before scan");
            assert_eq!(row.result.len(), usize::from(id != 3));
        }
        promoted.flush().expect("persist imported slots");
    }
    assert!(
        !catalog_path.exists(),
        "canonical slots must remain inside the database"
    );
}

#[test]
fn runtime_persists_timeline_history_for_failover_promotion() {
    let data_path = primary_replica_file::temp_data_path("primary_replica_timeline_history");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let promoted = runtime
        .record_failover_timeline_promotion("replica-a", 42)
        .expect("record promotion timeline");

    assert_eq!(promoted.current(), Some(reddb_file::TimelineId(2)));
    assert_eq!(promoted.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));

    let path = runtime
        .primary_replica_timeline_history_path()
        .expect("timeline history path");
    assert!(
        !path.exists(),
        "timeline metadata stays inside the database"
    );
    let from_disk = runtime
        .primary_replica_timeline_history()
        .expect("read timeline")
        .expect("history");
    assert_eq!(from_disk.current(), Some(reddb_file::TimelineId(2)));
    assert_eq!(from_disk.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));
    assert_eq!(from_disk.entries[1].reason, "promote replica-a");

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_appends_timeline_history_for_second_failover_promotion() {
    let data_path =
        primary_replica_file::temp_data_path("primary_replica_timeline_second_promotion");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .record_failover_timeline_promotion("replica-a", 42)
        .expect("first promotion timeline");
    let promoted = runtime
        .record_failover_timeline_promotion("replica-b", 77)
        .expect("second promotion timeline");

    assert_eq!(promoted.current(), Some(reddb_file::TimelineId(3)));
    assert_eq!(promoted.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));
    assert_eq!(promoted.ancestor_lsn(reddb_file::TimelineId(3)), Some(77));
    let chain = promoted
        .descendant_chain_from(reddb_file::TimelineId(1))
        .expect("chain from initial timeline");
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].timeline, reddb_file::TimelineId(2));
    assert_eq!(chain[1].timeline, reddb_file::TimelineId(3));

    let path = runtime
        .primary_replica_timeline_history_path()
        .expect("timeline history path");
    assert!(
        !path.exists(),
        "timeline metadata stays inside the database"
    );
    let from_disk = runtime
        .primary_replica_timeline_history()
        .expect("read timeline")
        .expect("history");
    assert_eq!(from_disk.current(), Some(reddb_file::TimelineId(3)));
    assert_eq!(from_disk.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));
    assert_eq!(from_disk.ancestor_lsn(reddb_file::TimelineId(3)), Some(77));
    assert_eq!(from_disk.entries[1].reason, "promote replica-a");
    assert_eq!(from_disk.entries[2].reason, "promote replica-b");

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_timeline_promotion_survives_atomic_crash_points() {
    if std::env::var(TIMELINE_CRASH_CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in [
        "timeline_before_wal",
        "timeline_after_wal",
        "timeline_after_sync",
    ] {
        let data_path = primary_replica_file::temp_data_path(&format!(
            "primary_replica_timeline_crash_{point}"
        ));
        primary_replica_file::cleanup(&data_path);

        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
            .expect("runtime boots");
        runtime
            .record_failover_timeline_promotion("replica-a", 42)
            .expect("initial promotion timeline");
        let initial = runtime
            .primary_replica_timeline_history()
            .expect("read timeline")
            .expect("history");
        // The child is the next writer; an embedded store admits one at a time.
        drop(runtime);

        let child = Command::new(std::env::current_exe().expect("current test exe"))
            .arg("--exact")
            .arg("runtime_timeline_promotion_crash_child")
            .arg("--nocapture")
            .env(TIMELINE_CRASH_CHILD_ENV, "1")
            .env(TIMELINE_CRASH_DATA_PATH_ENV, &data_path)
            .env(PRIMARY_REPLICA_CRASH_ENV, point)
            .status()
            .expect("run crash child");
        assert_eq!(
            child.code(),
            Some(173),
            "child should crash at {point}, status={child:?}"
        );

        let recovered =
            RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("recover WAL");
        let history = recovered
            .primary_replica_timeline_history()
            .expect("read timeline")
            .expect("history");
        if point == "timeline_after_sync" {
            assert_eq!(history.current(), Some(reddb_file::TimelineId(3)));
        }
        assert!(
            history.current() == Some(reddb_file::TimelineId(2))
                || history.current() == Some(reddb_file::TimelineId(3)),
            "timeline must be old or new after {point}, got {:?}",
            history.current()
        );
        if history.current() == Some(reddb_file::TimelineId(2)) {
            assert_eq!(history, initial);
        } else {
            assert_eq!(history.ancestor_lsn(reddb_file::TimelineId(3)), Some(77));
            assert_eq!(history.entries[2].reason, "promote replica-b");
        }

        drop(recovered);
        primary_replica_file::cleanup(&data_path);
    }
}

#[test]
fn runtime_timeline_promotion_crash_child() -> ExitCode {
    if std::env::var(TIMELINE_CRASH_CHILD_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let data_path =
        PathBuf::from(std::env::var(TIMELINE_CRASH_DATA_PATH_ENV).expect("data path env"));
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let _ = runtime.record_failover_timeline_promotion("replica-b", 77);
    ExitCode::from(1)
}

#[test]
fn runtime_persists_rejoin_plan_from_timeline_history_chain() {
    let data_path = primary_replica_file::temp_data_path("primary_replica_rejoin_plan");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .record_failover_timeline_promotion("replica-a", 42)
        .expect("first promotion timeline");
    runtime
        .record_failover_timeline_promotion("replica-b", 77)
        .expect("second promotion timeline");

    let decision = runtime
        .persist_primary_replica_rejoin_plan(reddb_file::TimelineId(1), 60, 40)
        .expect("persist rejoin plan")
        .expect("rejoin decision");
    assert_eq!(
        decision,
        reddb_file::RejoinDecision::Rewind {
            target_timeline: reddb_file::TimelineId(3),
            rewind_to_lsn: 42,
        }
    );
    assert!(
        primary_replica_file::show_config_value(&runtime, "replication.state")
            .contains("rejoin_rewind_required")
    );
    assert!(
        primary_replica_file::show_config_value(&runtime, "replication.rejoin_node_timeline")
            .contains("1")
    );
    assert!(primary_replica_file::show_config_value(
        &runtime,
        "replication.rejoin_target_timeline"
    )
    .contains("3"));
    assert!(
        primary_replica_file::show_config_value(&runtime, "replication.rejoin_rewind_to_lsn")
            .contains("42")
    );
    assert!(primary_replica_file::show_config_value(
        &runtime,
        "replication.rejoin_rewind_confirmed_lsn"
    )
    .contains("0"));
    assert!(primary_replica_file::show_config_value(
        &runtime,
        "replication.rejoin_rewind_confirmed_timeline"
    )
    .contains("0"));

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_failover_promotion_fails_closed_on_corrupt_timeline_history() {
    let data_path = primary_replica_file::temp_data_path("primary_replica_timeline_corrupt");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let path = runtime
        .primary_replica_timeline_history_path()
        .expect("timeline history path");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create timeline parent");
    }
    fs::write(&path, b"corrupt timeline history").expect("write corrupt timeline");

    let err = match runtime.record_failover_timeline_promotion("replica-a", 42) {
        Ok(_) => panic!("corrupt timeline history must fail closed"),
        Err(err) => err,
    };
    let message = err.to_string();
    assert!(
        message.contains("timeline") || message.contains("checksum") || message.contains("invalid"),
        "error should identify timeline history corruption, got {message}"
    );
    assert_eq!(
        fs::read(&path).expect("read corrupt timeline"),
        b"corrupt timeline history",
        "failed promotion must not replace corrupt timeline history"
    );

    primary_replica_file::cleanup(&data_path);
}
