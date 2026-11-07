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

fn assert_database_state_files_only(root: &Path) {
    for entry in std::fs::read_dir(root).expect("database directory") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if path.is_dir() {
            assert_database_state_files_only(&path);
        } else {
            let name = entry.file_name();
            let name = name.to_str().expect("file name");
            // Security audit output is diagnostic evidence, not a recovery
            // sidecar. Preserve the independent security evidence sink.
            assert!(
                name == "data.rdb"
                    || Some(entry.file_name().as_os_str())
                        == reddb_file::unified_wal_path(&root.join("data.rdb")).file_name()
                    || name == ".audit.log",
                "unexpected database sidecar: {}",
                path.display()
            );
        }
    }
}

#[test]
fn queue_dedup_and_capacity_recover_after_acknowledged_process_exit() {
    const CHILD: &str = "REDDB_QUEUE_INDEX_CRASH_CHILD";
    let queue_options = |path: &Path, bounded: bool, grouped: bool| {
        options(path, bounded).with_durability_mode(if grouped {
            DurabilityMode::WalDurableGrouped
        } else {
            DurabilityMode::Strict
        })
    };
    if let Some(path) = std::env::var_os(CHILD) {
        let runtime = RedDBRuntime::with_options(queue_options(
            Path::new(&path),
            std::env::var("REDDB_QUEUE_INDEX_BOUNDED").expect("bounded") == "true",
            std::env::var("REDDB_QUEUE_INDEX_GROUPED").expect("grouped") == "true",
        ))
        .expect("child runtime");
        let delivery = runtime
            .execute_query("QUEUE READ jobs GROUP workers CONSUMER worker COUNT 1")
            .expect("deliver");
        let message_id = match delivery.result.records[0].get("message_id") {
            Some(reddb_types::Value::Text(value)) => value.to_string(),
            other => panic!("delivery id: {other:?}"),
        };
        runtime
            .execute_query(&format!("QUEUE ACK jobs GROUP workers '{message_id}'"))
            .expect("acknowledged ACK");
        runtime
            .execute_query("QUEUE PUSH jobs 'kept' DEDUP 'surviving-key'")
            .expect("ACK releases capacity");
        // Do not run destructors or a shutdown checkpoint: recover the WAL
        // overlay over previously published capacity/dedup index roots.
        std::process::exit(0);
    }
    for bounded in [false, true] {
        for grouped in [false, true] {
            let directory = tempfile::tempdir().expect("fixture");
            let path = directory.path().join("data.rdb");
            {
                let runtime = RedDBRuntime::with_options(queue_options(&path, bounded, grouped))
                    .expect("seed");
                runtime
                    .execute_query("CREATE QUEUE jobs MAX_SIZE 1 DEDUP_WINDOW 3600 s")
                    .expect("queue");
                runtime
                    .execute_query("QUEUE GROUP CREATE jobs workers")
                    .expect("group");
                runtime
                    .execute_query("QUEUE PUSH jobs 'old' DEDUP 'old-key'")
                    .expect("seed message");
                runtime.checkpoint().expect("publish base indices");
            }
            let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "queue_dedup_and_capacity_recover_after_acknowledged_process_exit",
                ])
                .env(CHILD, &path)
                .env("REDDB_QUEUE_INDEX_BOUNDED", bounded.to_string())
                .env("REDDB_QUEUE_INDEX_GROUPED", grouped.to_string())
                .status()
                .expect("child process");
            assert!(status.success(), "bounded={bounded}, grouped={grouped}");
            for _ in 0..3 {
                let runtime = RedDBRuntime::with_options(queue_options(&path, bounded, grouped))
                    .expect("recover");
                let peek = runtime.execute_query("QUEUE PEEK jobs 10").expect("peek");
                assert_eq!(peek.result.records.len(), 1, "ACK must survive recovery");
                assert_eq!(
                    peek.result.records[0].get("payload"),
                    Some(&reddb_types::Value::Text("kept".into()))
                );
                let retry = runtime
                    .execute_query("QUEUE PUSH jobs 'duplicate' DEDUP 'surviving-key'")
                    .expect("duplicate succeeds even at capacity");
                assert_eq!(
                    retry.result.records[0].get("message_id"),
                    peek.result.records[0].get("message_id")
                );
                assert!(
                    runtime.execute_query("QUEUE PUSH jobs 'overflow'").is_err(),
                    "capacity remains enforced"
                );
                assert!(runtime
                    .execute_query("QUEUE PENDING jobs GROUP workers")
                    .expect("pending")
                    .result
                    .records
                    .is_empty());
                runtime.checkpoint().expect("checkpoint recovered overlay");
                assert_database_state_files_only(directory.path());
            }
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
            assert!(
                runtime
                    .db()
                    .native_vector_artifact_pages()
                    .unwrap_or_default()
                    .iter()
                    .all(|artifact| artifact.collection != "red.storage.metadata"),
                "physical control documents must not become their own derived search artifacts"
            );
            assert_database_state_files_only(directory.path());
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
            assert_database_state_files_only(directory.path());
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
    assert_database_state_files_only(directory.path());
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
    assert_database_state_files_only(directory.path());
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
        assert_database_state_files_only(directory.path());
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

#[test]
fn embedded_options_reopening_native_store_do_not_create_an_auxiliary_lock() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    {
        let runtime = RedDBRuntime::with_options(options(&path, false)).expect("native store");
        runtime
            .execute_query("CREATE TABLE native_rows (id INT)")
            .expect("table");
        runtime
            .execute_query("INSERT INTO native_rows (id) VALUES (7)")
            .expect("row");
        runtime.checkpoint().expect("checkpoint");
    }
    for read_only in [false, true, false] {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&path)
                .with_memory_budget(64 * 1024 * 1024)
                .with_bounded_disk_residency(false)
                .with_read_only(read_only),
        )
        .expect("reopen native store under embedded options");
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM native_rows")
                .expect("row")
                .result
                .records
                .len(),
            1
        );
        runtime.checkpoint().expect("checkpoint");
        assert_database_state_files_only(directory.path());
    }
}
