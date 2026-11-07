//! Replication control state must recover with the database, without sidecars.
use reddb_server::api::DurabilityMode;
use reddb_server::replication::{primary::PrimaryReplication, ReplicationConfig};
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use std::path::Path;

fn options(path: &Path, mode: &str) -> RedDBOptions {
    let durability = match mode {
        "strict" => DurabilityMode::Strict,
        "grouped" => DurabilityMode::WalDurableGrouped,
        "async" => DurabilityMode::Async,
        _ => panic!("unknown fixture durability"),
    };
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("native profile")
        .with_memory_budget(64 * 1024 * 1024)
        .with_durability_mode(durability)
        .with_replication(ReplicationConfig::primary())
}

fn assert_no_control_sidecars(runtime: &RedDBRuntime, path: &Path) {
    assert!(!PrimaryReplication::slot_path_for(path).exists());
    assert!(!PrimaryReplication::slot_catalog_path_for(path).exists());
    assert!(!runtime
        .primary_replica_timeline_history_path()
        .expect("path")
        .exists());
    assert!(!runtime
        .replica_relay_manifest_path("replica-a")
        .expect("path")
        .exists());
    assert!(!reddb_file::physical_metadata_binary_path(path).exists());
}

#[test]
fn control_records_survive_process_exit_and_repeated_cold_boot_in_every_durability_mode() {
    const CHILD: &str = "REDDB_INTERNAL_REPLICATION_CHILD";
    if let Some(path) = std::env::var_os(CHILD) {
        let mode = std::env::var("REDDB_INTERNAL_REPLICATION_MODE").expect("mode");
        let runtime = RedDBRuntime::with_options(options(Path::new(&path), &mode)).expect("child");
        runtime
            .ack_primary_replica_lsn_and_prune("replica-a", 42, 40, 0, 0)
            .expect("persist ACK");
        runtime
            .record_failover_timeline_promotion("replica-a", 40)
            .expect("promote");
        runtime
            .record_replica_relay_batch("replica-a", &[(1, b"record".to_vec())], 1)
            .expect("persist relay");
        assert_no_control_sidecars(&runtime, Path::new(&path));
        // Deliberately bypass shutdown, destructors and checkpoint.
        std::process::exit(0);
    }
    for mode in ["strict", "grouped", "async"] {
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("data.rdb");
        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", "control_records_survive_process_exit_and_repeated_cold_boot_in_every_durability_mode", "--nocapture"])
            .env(CHILD, &path)
            .env("REDDB_INTERNAL_REPLICATION_MODE", mode)
            .status().expect("child process");
        assert!(status.success(), "child failed in {mode}: {status}");
        for _ in 0..3 {
            let runtime = RedDBRuntime::with_options(options(&path, mode)).expect("cold boot");
            let catalog = runtime
                .primary_replica_slot_catalog()
                .expect("slots")
                .expect("catalog");
            let slot = catalog
                .slots
                .iter()
                .find(|slot| slot.replica_id == "replica-a")
                .expect("slot");
            assert_eq!(slot.restart_lsn, 40);
            assert_eq!(slot.confirmed_flush_lsn, 40);
            let history = runtime
                .primary_replica_timeline_history()
                .expect("timeline")
                .expect("history");
            assert_eq!(history.current(), Some(reddb_file::TimelineId(2)));
            assert_eq!(history.ancestor_lsn(reddb_file::TimelineId(2)), Some(40));
            let relay = runtime
                .replica_relay_manifest("replica-a")
                .expect("relay")
                .expect("manifest");
            assert_eq!(relay.flushed_lsn, 1);
            assert_eq!(relay.applied_lsn, 1);
            runtime.checkpoint().expect("checkpoint");
            assert_no_control_sidecars(&runtime, &path);
        }
    }
}

#[test]
fn legacy_slot_catalog_is_imported_before_retiring_its_files() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    {
        let legacy = PrimaryReplication::new(Some(&path));
        legacy.register_replica("replica-a".into());
        legacy.ack_replica_lsn("replica-a", 30, 20);
    }
    assert!(PrimaryReplication::slot_path_for(&path).exists());
    assert!(PrimaryReplication::slot_catalog_path_for(&path).exists());
    for _ in 0..2 {
        let runtime =
            RedDBRuntime::with_options(options(&path, "async")).expect("import/cold boot");
        let catalog = runtime
            .primary_replica_slot_catalog()
            .expect("slots")
            .expect("catalog");
        assert_eq!(catalog.slots[0].restart_lsn, 20);
        assert_eq!(catalog.slots[0].confirmed_write_lsn, 30);
        assert_no_control_sidecars(&runtime, &path);
        runtime.checkpoint().expect("checkpoint imported slots");
    }
}
