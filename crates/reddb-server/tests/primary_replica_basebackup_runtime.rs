use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use reddb_server::{RedDBOptions, RedDBRuntime};

#[path = "support/primary_replica_file.rs"]
mod primary_replica_file;

const BASEBACKUP_CRASH_CHILD_ENV: &str = "REDDB_PRIMARY_REPLICA_BASEBACKUP_RUNTIME_CRASH_CHILD";
const BASEBACKUP_CRASH_DATA_PATH_ENV: &str = "REDDB_PRIMARY_REPLICA_BASEBACKUP_RUNTIME_DATA_PATH";
const PRIMARY_REPLICA_CRASH_ENV: &str = "REDDB_PRIMARY_REPLICA_CRASH_AT";

#[test]
fn runtime_creates_chunked_primary_replica_basebackup() {
    let data_path = primary_replica_file::temp_data_path("primary_replica_basebackup");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .execute_query("INSERT INTO backup_items (id, name) VALUES (1, 'alpha')")
        .expect("insert row");

    let manifest = runtime
        .create_primary_replica_basebackup(64)
        .expect("create basebackup")
        .expect("basebackup manifest");
    assert_eq!(manifest.timeline, reddb_file::TimelineId::initial());
    assert!(!manifest.chunks.is_empty());

    let plan = runtime
        .primary_replica_file_plan()
        .expect("primary-replica plan");
    let backup = reddb_file::BaseBackupPlan::new(
        manifest.timeline,
        manifest.start_lsn,
        manifest.checkpoint_lsn,
    );
    assert!(plan.basebackup_path(&backup).exists());
    manifest
        .verify_snapshot_parts(plan.basebackup_dir())
        .expect("basebackup chunks verify");
    let snapshot = manifest
        .read_snapshot_parts(plan.basebackup_dir())
        .expect("read basebackup snapshot");
    assert!(!snapshot.is_empty());
    let staged_root = data_path.with_extension("basebackup-staged");
    let _ = fs::remove_dir_all(&staged_root);
    if manifest.chunks.len() > 1 {
        let first = &manifest.chunks[0];
        let first_bytes = fs::read(plan.basebackup_dir().join(&first.relative_path))
            .expect("read first source basebackup chunk");
        let first_staged_path = staged_root.join(&first.relative_path);
        fs::create_dir_all(first_staged_path.parent().expect("first chunk parent"))
            .expect("create first staged parent");
        fs::write(&first_staged_path, first_bytes).expect("pre-stage first chunk");

        let second = &manifest.chunks[1];
        let second_staged_path = staged_root.join(&second.relative_path);
        fs::create_dir_all(second_staged_path.parent().expect("second chunk parent"))
            .expect("create second staged parent");
        fs::write(&second_staged_path, b"corrupt").expect("pre-stage corrupt second chunk");

        let recovered = reddb_server::replication::replica::recover_staged_basebackup_chunks(
            &manifest,
            &staged_root,
        )
        .expect("recover staged chunks");
        assert!(
            recovered.contains(&first.ordinal),
            "valid pre-staged chunk should be reused"
        );
        assert!(
            !recovered.contains(&second.ordinal),
            "corrupt pre-staged chunk should not be reused"
        );
        assert!(
            !second_staged_path.exists(),
            "corrupt pre-staged chunk should be removed for redownload"
        );
    }
    for chunk in &manifest.chunks {
        let chunk_bytes = fs::read(plan.basebackup_dir().join(&chunk.relative_path))
            .expect("read source basebackup chunk");
        let payload = reddb_wire::replication::BaseBackupChunk {
            snapshot_available: true,
            replica_id: "replica-test".to_string(),
            slot_restart_lsn: manifest.checkpoint_lsn,
            snapshot_lsn: Some(manifest.checkpoint_lsn),
            snapshot_token: None,
            snapshot_total_bytes: Some(manifest.snapshot_bytes),
            snapshot_offset: chunk.snapshot_offset,
            next_snapshot_offset: None,
            snapshot_complete: chunk.ordinal as usize + 1 == manifest.chunks.len(),
            snapshot_path: None,
            snapshot_chunk: None,
            snapshot_hex: None,
            metadata_binary: None,
            metadata_json: None,
            header_shadow: None,
            metadata_shadow: None,
            basebackup_available: true,
            basebackup_timeline: Some(manifest.timeline.0),
            basebackup_start_lsn: Some(manifest.start_lsn),
            basebackup_checkpoint_lsn: Some(manifest.checkpoint_lsn),
            basebackup_snapshot_bytes: Some(manifest.snapshot_bytes),
            basebackup_snapshot_checksum: Some(manifest.snapshot_checksum.into()),
            basebackup_manifest: Some(manifest.encode()),
            basebackup_chunks: Vec::new(),
            basebackup_chunk_ordinal: Some(chunk.ordinal),
            basebackup_chunk: Some(chunk_bytes),
        };
        let staged = reddb_server::replication::replica::stage_basebackup_snapshot_chunk(
            &payload,
            &staged_root,
        )
        .expect("stage basebackup chunk")
        .expect("basebackup payload");
        assert_eq!(staged.manifest, manifest);
        assert_eq!(staged.chunk_ordinal, Some(chunk.ordinal));
    }
    manifest
        .verify_snapshot_parts(&staged_root)
        .expect("staged chunks verify");
    let restore_path = data_path.with_extension("basebackup-restore.rdb");
    let checkpoint_lsn = runtime
        .materialize_primary_replica_basebackup_snapshot(&manifest, &staged_root, &restore_path)
        .expect("materialize basebackup snapshot");
    assert_eq!(checkpoint_lsn, manifest.checkpoint_lsn);
    let restored = RedDBRuntime::with_options(RedDBOptions::persistent(&restore_path))
        .expect("restored runtime opens");
    let result = restored
        .execute_query("SELECT id, name FROM backup_items WHERE id = 1")
        .expect("query restored basebackup");
    assert_eq!(result.result.len(), 1);

    let _ = fs::remove_file(&restore_path);
    let _ = fs::remove_dir_all(&staged_root);
    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_basebackup_retry_recovers_orphaned_parts_after_crash() {
    if std::env::var(BASEBACKUP_CRASH_CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    let data_path = primary_replica_file::temp_data_path("primary_replica_basebackup_parts_crash");
    primary_replica_file::cleanup(&data_path);

    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
            .expect("runtime boots");
        runtime
            .execute_query("INSERT INTO backup_items (id, name) VALUES (1, 'alpha')")
            .expect("insert row");
        runtime.flush().expect("flush runtime");
    }

    let child = Command::new(std::env::current_exe().expect("current test exe"))
        .arg("--exact")
        .arg("runtime_basebackup_crash_child")
        .arg("--nocapture")
        .env(BASEBACKUP_CRASH_CHILD_ENV, "1")
        .env(BASEBACKUP_CRASH_DATA_PATH_ENV, &data_path)
        .env(
            PRIMARY_REPLICA_CRASH_ENV,
            "basebackup_after_parts_dir_rename",
        )
        .status()
        .expect("run crash child");
    assert_eq!(child.code(), Some(173), "child should crash");

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime reopens");
    let plan = runtime
        .primary_replica_file_plan()
        .expect("primary-replica plan");
    assert!(
        plan.list_basebackups()
            .expect("list basebackups after crash")
            .is_empty(),
        "parts-only crash must not publish a visible basebackup"
    );

    let manifest = runtime
        .create_primary_replica_basebackup(64)
        .expect("retry basebackup after parts-only crash")
        .expect("basebackup manifest");
    manifest
        .verify_snapshot_parts(plan.basebackup_dir())
        .expect("retried basebackup chunks verify");
    let restore_path = data_path.with_extension("basebackup-retry-restore.rdb");
    runtime
        .materialize_primary_replica_basebackup_snapshot(
            &manifest,
            plan.basebackup_dir(),
            &restore_path,
        )
        .expect("materialize retried basebackup");
    let restored = RedDBRuntime::with_options(RedDBOptions::persistent(&restore_path))
        .expect("restored runtime opens");
    assert_eq!(
        restored
            .execute_query("SELECT id, name FROM backup_items WHERE id = 1")
            .expect("query restored row")
            .result
            .len(),
        1
    );

    let _ = fs::remove_file(&restore_path);
    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_basebackup_crash_child() -> ExitCode {
    if std::env::var(BASEBACKUP_CRASH_CHILD_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let data_path =
        PathBuf::from(std::env::var(BASEBACKUP_CRASH_DATA_PATH_ENV).expect("data path env"));
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    let _ = runtime.create_primary_replica_basebackup(64);
    ExitCode::from(1)
}

#[test]
fn runtime_classifies_catchup_mode_from_available_basebackups() {
    let data_path = primary_replica_file::temp_data_path("primary_replica_catchup_mode");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    assert_eq!(
        runtime
            .primary_replica_catchup_mode(1, 0)
            .expect("catchup mode")
            .expect("file plan"),
        reddb_file::ReplicaCatchupMode::Reclone,
        "without a usable basebackup, a replica behind retention must reclone"
    );

    runtime
        .execute_query("INSERT INTO catchup_items (id, name) VALUES (1, 'alpha')")
        .expect("insert row");
    let manifest = runtime
        .create_primary_replica_basebackup(64)
        .expect("create basebackup")
        .expect("basebackup manifest");
    assert_eq!(
        runtime
            .primary_replica_catchup_mode(manifest.checkpoint_lsn, 0)
            .expect("catchup mode")
            .expect("file plan"),
        reddb_file::ReplicaCatchupMode::BaseBackupThenWal,
        "a usable basebackup lets the replica rebuild from snapshot then WAL"
    );

    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_catchup_mode_uses_visible_basebackup_or_reclone_after_wal_gap() {
    let data_path = primary_replica_file::temp_data_path("primary_replica_catchup_gap");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .execute_query("INSERT INTO catchup_gap_items (id, name) VALUES (1, 'alpha')")
        .expect("insert row");
    let manifest = runtime
        .create_primary_replica_basebackup(64)
        .expect("create basebackup")
        .expect("basebackup manifest");
    let available_from_lsn = manifest.checkpoint_lsn;
    let replica_lsn = available_from_lsn.saturating_sub(1);
    assert_eq!(
        runtime
            .primary_replica_catchup_mode(available_from_lsn, replica_lsn)
            .expect("catchup mode")
            .expect("file plan"),
        reddb_file::ReplicaCatchupMode::BaseBackupThenWal,
        "replica behind retention should rebuild from visible basebackup then WAL"
    );

    let plan = runtime
        .primary_replica_file_plan()
        .expect("primary-replica plan");
    let backup = reddb_file::BaseBackupPlan::new(
        manifest.timeline,
        manifest.start_lsn,
        manifest.checkpoint_lsn,
    );
    fs::remove_file(plan.basebackup_path(&backup)).expect("remove visible basebackup manifest");
    assert_eq!(
        runtime
            .primary_replica_catchup_mode(available_from_lsn, replica_lsn)
            .expect("catchup mode without backup")
            .expect("file plan"),
        reddb_file::ReplicaCatchupMode::Reclone,
        "replica behind retention with no visible basebackup must reclone"
    );

    primary_replica_file::cleanup(&data_path);
}
