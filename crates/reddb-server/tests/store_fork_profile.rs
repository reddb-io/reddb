use reddb_server::{RedDBError, RedDBOptions, RedDBRuntime, StorageDeployPreset};

fn temp_data_path(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(name)
        .tempdir()
        .expect("tempdir");
    let path = dir.path().join("db.rdb");
    (dir, path)
}

#[test]
fn embedded_single_file_fork_points_to_export_path() {
    let (_dir, path) = temp_data_path("embedded-fork-guide");
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");

    let err = runtime
        .fork_store("experiment")
        .expect_err("single-file store should not fork directly");

    let RedDBError::InvalidOperation(message) = err else {
        panic!("unexpected error: {err:?}");
    };
    assert!(message.contains("single-file"), "got: {message}");
    assert!(
        message.to_ascii_lowercase().contains("export"),
        "got: {message}"
    );
    assert!(message.contains("operational-directory"), "got: {message}");
    assert!(
        message.contains("docs/engine/operational-storage-profiles.md"),
        "got: {message}"
    );
}

#[test]
fn operational_directory_fork_uses_exported_layout() {
    let (_dir, source_path) = temp_data_path("operational-fork");
    let source = RedDBRuntime::with_options(RedDBOptions::persistent(&source_path))
        .expect("single-file source runtime");
    source
        .execute_query("CREATE TABLE users (id INT)")
        .expect("create table");
    source
        .execute_query("INSERT INTO users (id) VALUES (1)")
        .expect("insert row");
    let export = source
        .create_export("fork-source")
        .expect("export single-file source");
    drop(source);

    let export_path = std::path::PathBuf::from(export.data_path);
    let options = RedDBOptions::persistent(&export_path)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
        .expect("operational storage profile");
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM users")
            .expect("exported row is readable")
            .result
            .len(),
        1
    );

    let fork = runtime.fork_store("experiment").expect("fork store");

    let manifest = reddb_file::OperationalManifest::for_db_path(&export_path);
    let forks = manifest.list_forks().expect("list forks");
    assert_eq!(forks.len(), 1);
    assert_eq!(forks[0].name, "experiment");
    assert_eq!(forks[0].fork_lsn, fork.fork_lsn);
    assert_eq!(forks[0].parent_store, manifest.store_identity());

    assert!(runtime
        .detach_fork_store("experiment")
        .expect("detach fork store"));
    assert!(
        manifest
            .list_forks()
            .expect("list forks after detach")
            .is_empty(),
        "detached fork must no longer pin parent retention"
    );
    assert!(!runtime
        .detach_fork_store("experiment")
        .expect("detach missing fork is idempotent"));
}
