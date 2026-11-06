//! Reproductions from Hunter and Red Router. Only disposable stores are opened.
use reddb_server::{RedDBOptions, RedDBRuntime};

#[test]
fn historical_empty_system_aliases_survive_runtime_cold_boot() {
    use reddb_server::storage::{EntityId, UnifiedEntity};
    use reddb_types::Value;
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("historical.rdb");
    let mut bytes = reddb_file::encode_native_store_header(12);
    reddb_file::encode_native_dump_count(&mut bytes, 3);
    for name in ["red_config", "red.config"] {
        reddb_file::encode_native_dump_collection_header(&mut bytes, name, 0);
    }
    reddb_file::encode_native_dump_collection_header(&mut bytes, "fixture_owner", 1);
    let row = UnifiedEntity::table_row(
        EntityId::new(41),
        "fixture_owner",
        1,
        vec![Value::Integer(7)],
    );
    reddb_file::encode_native_dump_entity_record(
        &mut bytes,
        &reddb_engine::storage::metadata_codec::serialize_entity_record(&row, None, 12),
    );
    reddb_file::encode_native_dump_count(&mut bytes, 0);
    reddb_file::encode_native_dump_entity_record(&mut bytes, &[]);
    reddb_file::append_native_store_crc32_footer(&mut bytes);
    std::fs::write(&path, &bytes).expect("historical fixture");
    for _ in 0..3 {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("cold boot");
        let store = runtime.db().store();
        let owner = store
            .get_collection("fixture_owner")
            .expect("preserved collection");
        let rows = owner.query_all(|_| true);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, row.id);
        assert!(store
            .list_collections()
            .iter()
            .all(|name| name != "red_config"));
        assert!(store.get_collection("red.config").is_some());
        runtime.flush().expect("checkpoint migrated data");
    }
}

#[test]
fn read_only_open_query_and_drop_preserve_database_bytes() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("readonly.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("create");
        runtime
            .execute_query("CREATE TABLE owner (id INT PRIMARY KEY, payload TEXT)")
            .expect("schema");
        runtime
            .execute_query("INSERT INTO owner (id, payload) VALUES (1, 'preserved')")
            .expect("seed");
        runtime.flush().expect("checkpoint");
    }
    let before = std::fs::read(&path).expect("cold bytes");
    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path).with_read_only(true))
                .expect("read-only open");
        let result = runtime
            .execute_query("SELECT payload FROM owner WHERE id = 1")
            .expect("read");
        assert_eq!(result.result.records.len(), 1);
        let opened = std::fs::read(&path).expect("bytes after read");
        assert!(
            opened == before,
            "read-only open/query changed the database bytes ({} -> {})",
            before.len(),
            opened.len()
        );
    }
    assert!(
        std::fs::read(&path).expect("bytes after drop") == before,
        "read-only drop changed the database bytes"
    );
}

#[test]
fn replica_replay_updates_populated_secondary_index_before_advancing_lsn() {
    use reddb_server::replication::cdc::{change_record_from_entity, ChangeOperation};
    use reddb_server::replication::logical::{ApplyMode, LogicalChangeApplier};

    let primary = RedDBRuntime::in_memory().expect("primary fixture");
    let replica = RedDBRuntime::in_memory().expect("replica fixture");
    for runtime in [&primary, &replica] {
        runtime
            .execute_query("CREATE TABLE replicated (id INT, status TEXT)")
            .expect("schema");
    }
    primary
        .execute_query("INSERT INTO replicated (id, status) VALUES (1, 'ready'), (2, 'ready')")
        .expect("source rows");
    let mut entities = primary
        .db()
        .store()
        .get_collection("replicated")
        .expect("source collection")
        .query_all(|_| true);
    entities.sort_by_key(|entity| entity.id.raw());
    assert_eq!(entities.len(), 2);
    let applier = LogicalChangeApplier::new(0);
    let record = |index: usize| {
        change_record_from_entity(
            index as u64 + 1,
            1,
            ChangeOperation::Insert,
            "replicated",
            "table",
            &entities[index],
            12,
            None,
        )
    };
    applier
        .apply(&replica.db(), &record(0), ApplyMode::Replica)
        .expect("first replay");
    replica
        .execute_query("CREATE INDEX replica_status ON replicated (status) USING HASH")
        .expect("populate index");
    let index = replica
        .index_store_ref()
        .find_index_for_column("replicated", "status")
        .expect("registered index");
    assert_eq!(replica.index_store_ref().entries_indexed(&index), Some(1));
    applier
        .apply(&replica.db(), &record(1), ApplyMode::Replica)
        .expect("second replay");
    assert_eq!(applier.last_applied_lsn(), 2);
    let all = replica
        .execute_query("SELECT id FROM replicated")
        .expect("full scan");
    assert_eq!(
        all.result.records.len(),
        2,
        "storage must contain both replayed rows"
    );
    let filtered = replica
        .execute_query("SELECT id FROM replicated WHERE status = 'ready'")
        .expect("indexed query");
    assert_eq!(
        (
            replica.index_store_ref().entries_indexed(&index),
            filtered.result.records.len()
        ),
        (Some(2), 2),
        "applied frontier advanced but the secondary index and indexed SQL omit a replayed row"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn windows_memory_probe_detects_host_ram() {
    use reddb_server::storage::{memory_budget, DeployProfile};
    let inputs = memory_budget::host_inputs(DeployProfile::Embedded, None);
    assert!(
        inputs.physical_ram_bytes > 0,
        "Windows must detect RAM instead of silently selecting the 64 MiB fallback"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn cold_boot_fits_one_mib_windows_stack() {
    const CHILD_PATH: &str = "REDDB_CONSUMER_STACK_CHILD_PATH";
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        // A stack overflow aborts only this subprocess, not the test runner.
        std::thread::Builder::new()
            .stack_size(1024 * 1024)
            .spawn(move || {
                let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(path))
                    .expect("cold boot on 1 MiB stack");
                let rows = runtime
                    .execute_query("SELECT id FROM stack_fixture")
                    .expect("read restored data");
                assert_eq!(rows.result.records.len(), 1);
            })
            .expect("bounded-stack worker")
            .join()
            .expect("worker completed");
        return;
    }
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("stack.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("create");
        runtime
            .execute_query("CREATE TABLE stack_fixture (id INT)")
            .expect("schema");
        runtime
            .execute_query("INSERT INTO stack_fixture (id) VALUES (1)")
            .expect("seed");
        runtime.flush().expect("checkpoint");
    }
    let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "cold_boot_fits_one_mib_windows_stack",
            "--nocapture",
        ])
        .env(CHILD_PATH, &path)
        .status()
        .expect("child boot process");
    assert!(
        status.success(),
        "cold boot failed on a 1 MiB Windows stack: {status}"
    );
}
