use std::sync::Arc;

use reddb_server::storage::backend::LocalBackend;
use reddb_server::storage::wal::PointInTimeRecovery;
use reddb_server::{RedDBOptions, RedDBRuntime};

#[test]
fn populated_single_file_backup_restores_into_an_independent_store() {
    let directory = tempfile::tempdir().expect("private synthetic test directory");
    let source = directory.path().join("source").join("data.rdb");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let remote = directory.path().join("remote");
    let snapshots = format!("{}/", remote.join("snapshots").display());
    let wal = format!("{}/", remote.join("wal").display());
    let backend = Arc::new(LocalBackend);
    let options = RedDBOptions::persistent(&source)
        .with_remote_backend(backend.clone(), remote.join("head.rdb").to_string_lossy())
        .with_metadata("red.config.backup.snapshot_prefix", &snapshots)
        .with_metadata("red.config.wal.archive.prefix", &wal)
        .with_metadata(
            "red.config.backup.head_key",
            remote.join("HEAD.json").to_string_lossy(),
        );
    let runtime = RedDBRuntime::with_options(options).expect("source runtime");
    for id in 0..25 {
        runtime
            .execute_query(&format!(
                "INSERT INTO backup_rows (id, name) VALUES ({id}, 'row-{id}')"
            ))
            .expect("synthetic row");
    }
    let expected = runtime
        .execute_query("SELECT id, name FROM backup_rows ORDER BY id")
        .expect("source readback")
        .result;
    assert_eq!(expected.len(), 25);
    let backup = runtime.trigger_backup().expect("runtime backup");
    assert!(backup.uploaded, "backup must reach the configured backend");
    let destination = directory.path().join("independent").join("data.rdb");
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let recovery = PointInTimeRecovery::new(backend, &snapshots, &wal);
    recovery
        .restore_to(0, &destination)
        .expect("independent restore");
    let restored_options = RedDBOptions::persistent(&destination);
    let restored = RedDBRuntime::with_options(restored_options).expect("restored runtime");
    let actual = restored
        .execute_query("SELECT id, name FROM backup_rows ORDER BY id")
        .expect("restored readback")
        .result;
    let rows = |result: &reddb_server::storage::query::unified::UnifiedResult| {
        result
            .records
            .iter()
            .map(|record| (record.get("id").cloned(), record.get("name").cloned()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        rows(&actual),
        rows(&expected),
        "all populated single-file rows must survive backup/restore"
    );
}

#[test]
fn operational_backup_refuses_an_unrestorable_main_file_snapshot() {
    let directory = tempfile::tempdir().expect("private synthetic test directory");
    let source = directory.path().join("source").join("data.rdb");
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    let remote = directory.path().join("remote");
    let snapshots = format!("{}/", remote.join("snapshots").display());
    let mut options = RedDBOptions::persistent(&source)
        .with_remote_backend(
            Arc::new(LocalBackend),
            remote.join("head.rdb").to_string_lossy(),
        )
        .with_metadata("red.config.backup.snapshot_prefix", &snapshots);
    options.storage_profile.packaging =
        reddb_server::storage::StoragePackaging::OperationalDirectory;
    let runtime = RedDBRuntime::with_options(options).expect("operational source runtime");
    runtime
        .execute_query("INSERT INTO backup_rows (id, name) VALUES (1, 'preserved')")
        .unwrap();
    assert!(source
        .with_extension("rdb.ops")
        .join("collections")
        .is_dir());
    let error = runtime
        .trigger_backup()
        .expect_err("incomplete backup must fail");
    assert!(error
        .to_string()
        .contains("checkpoint-consistent physical bundle"));
    assert!(
        !std::path::Path::new(&snapshots).exists(),
        "must not advertise a snapshot"
    );
    let status = runtime.backup_status();
    assert_eq!(status.total_backups, 0);
    assert!(status.last_backup.is_none());
    assert_eq!(
        runtime
            .execute_query("SELECT id, name FROM backup_rows")
            .unwrap()
            .result
            .len(),
        1
    );
}
