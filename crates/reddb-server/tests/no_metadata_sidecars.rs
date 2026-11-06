//! Database metadata belongs to the database's own recovery boundary.
use reddb_server::api::DurabilityMode;
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use std::path::Path;

fn options(path: &Path, bounded: bool) -> RedDBOptions {
    let options = RedDBOptions::persistent(path);
    let options = if bounded {
        options
    } else {
        options
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("native profile")
    };
    options
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(bounded)
        .with_durability_mode(DurabilityMode::WalDurableGrouped)
}

fn assert_database_files_only(root: &Path) {
    for entry in std::fs::read_dir(root).expect("database directory") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if path.is_dir() {
            assert_database_files_only(&path);
        } else {
            let name = entry.file_name();
            let name = name.to_str().expect("file name");
            assert!(
                name == "data.rdb" || name == "data.rdb-uwal",
                "unexpected database sidecar: {}",
                path.display()
            );
        }
    }
}

#[test]
fn metadata_stays_inside_database_across_checkpoints_and_cold_boot() {
    for bounded in [true, false] {
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("data.rdb");
        {
            let runtime = RedDBRuntime::with_options(options(&path, bounded)).expect("create");
            runtime
                .execute_query("CREATE DOCUMENT documents")
                .expect("contract");
            runtime
                .execute_query("INSERT INTO documents DOCUMENT VALUES ({\"name\":\"kept\"})")
                .expect("data");
            runtime
                .execute_query("CREATE INDEX names ON documents (name) USING HASH")
                .expect("index");
            runtime.checkpoint().expect("checkpoint");
            assert_database_files_only(directory.path());
        }
        for read_only in [false, true, false] {
            let runtime =
                RedDBRuntime::with_options(options(&path, bounded).with_read_only(read_only))
                    .expect("cold boot");
            assert_eq!(
                runtime
                    .db()
                    .collection_contract("documents")
                    .expect("contract")
                    .declared_model,
                reddb_server::catalog::CollectionModel::Document
            );
            assert_eq!(
                runtime
                    .execute_query("SELECT name FROM documents WHERE name = 'kept'")
                    .expect("indexed data")
                    .result
                    .records
                    .len(),
                1
            );
            runtime.checkpoint().expect("checkpoint or read-only no-op");
            assert_database_files_only(directory.path());
        }
    }
}

#[test]
fn acknowledged_metadata_survives_process_exit_without_checkpoint() {
    const CHILD: &str = "REDDB_NO_SIDECAR_CRASH_CHILD";
    if let Some(path) = std::env::var_os(CHILD) {
        let runtime = RedDBRuntime::with_options(options(Path::new(&path), true)).expect("child");
        runtime
            .execute_query("CREATE DOCUMENT durable_docs")
            .expect("acknowledged DDL");
        runtime
            .execute_query("INSERT INTO durable_docs DOCUMENT VALUES ({\"id\":7})")
            .expect("acknowledged DML");
        // Deliberately bypass Drop and all shutdown checkpoints.
        std::process::exit(0);
    }
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "acknowledged_metadata_survives_process_exit_without_checkpoint",
        ])
        .env(CHILD, &path)
        .status()
        .expect("child process");
    assert!(status.success());
    assert_database_files_only(directory.path());
    for read_only in [true, false, true] {
        let runtime = RedDBRuntime::with_options(options(&path, true).with_read_only(read_only))
            .expect("WAL recovery");
        assert_eq!(
            runtime
                .db()
                .collection_contract("durable_docs")
                .expect("recovered contract")
                .declared_model,
            reddb_server::catalog::CollectionModel::Document
        );
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM durable_docs")
                .expect("recovered data")
                .result
                .records
                .len(),
            1
        );
        runtime.checkpoint().expect("checkpoint");
    }
    assert_database_files_only(directory.path());
}

#[test]
fn legacy_metadata_is_imported_before_its_sidecars_are_removed() {
    use reddb_server::physical::PhysicalMetadataFile;
    use reddb_server::storage::UnifiedStore;
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let legacy = {
        let runtime = RedDBRuntime::with_options(options(&path, true)).expect("create");
        runtime
            .execute_query("CREATE DOCUMENT legacy_docs")
            .expect("contract");
        runtime
            .execute_query("INSERT INTO legacy_docs DOCUMENT VALUES ({\"id\":8})")
            .expect("data");
        runtime.checkpoint().expect("checkpoint");
        runtime.db().physical_metadata().expect("metadata")
    };
    // Build the pre-migration representation: native data, external contracts.
    {
        let store = UnifiedStore::open_with_config(
            &path,
            reddb_server::storage::UnifiedStoreConfig {
                resident_memory_bytes_max: Some(64 * 1024 * 1024),
                ..Default::default()
            },
        )
        .expect("fixture editor");
        let manager = store
            .get_collection("red.storage.metadata")
            .expect("config");
        for entity in manager.query_all(|entity| {
            entity.data.as_row().and_then(|row| row.get_field("key"))
                == Some(&reddb_types::Value::text("physical"))
        }) {
            store
                .delete("red.storage.metadata", entity.id)
                .expect("remove new representation");
        }
        store.persist().expect("legacy fixture checkpoint");
    }
    let binary = legacy.save_for_data_path(&path).expect("legacy binary");
    let json = PhysicalMetadataFile::metadata_path_for(&path);
    legacy.save_to_path(&json).expect("legacy JSON");
    let original_binary = std::fs::read(&binary).expect("binary");
    {
        let reader = RedDBRuntime::with_options(options(&path, true).with_read_only(true))
            .expect("legacy read-only");
        assert!(reader.db().collection_contract("legacy_docs").is_some());
        assert_eq!(
            std::fs::read(&binary).expect("preserved binary"),
            original_binary
        );
        assert!(json.exists());
    }
    {
        let runtime = RedDBRuntime::with_options(options(&path, true)).expect("import");
        assert_eq!(
            runtime
                .db()
                .collection_contract("legacy_docs")
                .expect("imported contract")
                .declared_model,
            reddb_server::catalog::CollectionModel::Document
        );
        runtime.checkpoint().expect("durable import");
        assert_database_files_only(directory.path());
    }
    let reader = RedDBRuntime::with_options(options(&path, true).with_read_only(true))
        .expect("cold boot without sidecars");
    assert_eq!(
        reader
            .execute_query("SELECT id FROM legacy_docs")
            .expect("preserved data")
            .result
            .records
            .len(),
        1
    );
    assert_eq!(
        reader
            .db()
            .collection_contract("legacy_docs")
            .expect("preserved contract")
            .declared_model,
        reddb_server::catalog::CollectionModel::Document
    );
}

#[test]
fn corrupt_internal_metadata_cannot_fall_back_to_a_stale_sidecar() {
    use reddb_server::storage::UnifiedStore;
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let metadata = {
        let runtime = RedDBRuntime::with_options(options(&path, true)).expect("create");
        runtime.checkpoint().expect("checkpoint");
        runtime.db().physical_metadata().expect("metadata")
    };
    metadata.save_for_data_path(&path).expect("stale sidecar");
    {
        let store = UnifiedStore::open_with_config(
            &path,
            reddb_server::storage::UnifiedStoreConfig {
                resident_memory_bytes_max: Some(64 * 1024 * 1024),
                ..Default::default()
            },
        )
        .expect("fixture editor");
        store
            .try_set_storage_metadata(
                "physical",
                &reddb_server::serde_json::Value::String("corrupt".into()),
            )
            .expect("corrupt record");
        store.persist().expect("checkpoint damage");
    }
    let error = RedDBRuntime::with_options(options(&path, true).with_read_only(true))
        .err()
        .expect("fail closed");
    assert!(
        error
            .to_string()
            .contains("invalid internal physical metadata record"),
        "{error}"
    );
}
