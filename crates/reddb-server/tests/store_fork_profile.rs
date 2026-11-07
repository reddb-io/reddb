use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use std::path::Path;

fn options(path: &Path, preset: StorageDeployPreset) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_memory_budget(64 * 1024 * 1024)
        .with_storage_profile(preset.selection())
        .expect("profile")
}

fn values(runtime: &RedDBRuntime) -> Vec<String> {
    runtime
        .execute_query("SELECT value FROM items ORDER BY id")
        .expect("rows")
        .result
        .records
        .iter()
        .map(|row| match row.get("value").expect("value") {
            reddb_types::Value::Text(text) => text.to_string(),
            value => panic!("expected text, got {value:?}"),
        })
        .collect()
}

fn seed(runtime: &RedDBRuntime) {
    runtime
        .execute_query("CREATE TABLE items (id INT PRIMARY KEY, value TEXT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO items (id,value) VALUES (1,'original')")
        .expect("row");
}

fn no_metadata_sidecars(root: &Path) {
    for entry in std::fs::read_dir(root).expect("directory") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            no_metadata_sidecars(&path);
            continue;
        }
        let name = path.file_name().expect("name").to_string_lossy();
        assert!(
            name.ends_with(".rdb")
                || name.ends_with(".wal")
                || name.ends_with("-wal")
                || name.ends_with("-uwal")
                || name.ends_with(".audit.log")
                || name.ends_with(".writer.lock"),
            "unexpected metadata artifact: {}",
            path.display()
        );
    }
}

#[test]
fn independent_fork_preserves_rows_contracts_and_isolation_after_cold_boot() {
    for preset in [
        StorageDeployPreset::Embedded,
        StorageDeployPreset::Serverless,
        StorageDeployPreset::PrimaryReplicaProductionHa,
    ] {
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("data.rdb");
        let parent = RedDBRuntime::with_options(options(&path, preset)).expect("parent");
        seed(&parent);
        let fork = parent.fork_store("experiment").expect("fork");
        assert_eq!(fork.shared_by_reference, 0);
        assert_eq!(fork.collections_total, fork.hydrated);
        let fork_path = parent
            .fork_store_path("experiment")
            .expect("path")
            .expect("fork");
        parent
            .execute_query("UPDATE items SET value='parent' WHERE id=1")
            .expect("parent update");
        let child = RedDBRuntime::with_options(options(&fork_path, preset)).expect("open fork");
        assert_eq!(values(&child), ["original"]);
        assert!(
            child
                .execute_query("INSERT INTO items (id,value) VALUES (1,'duplicate')")
                .is_err(),
            "primary key contract survives fork"
        );
        child
            .execute_query("UPDATE items SET value='fork' WHERE id=1")
            .expect("fork update");
        assert_eq!(values(&parent), ["parent"]);
        child.checkpoint().expect("fork checkpoint");
        parent.checkpoint().expect("parent checkpoint");
        drop(child);
        drop(parent);
        for _ in 0..3 {
            let parent =
                RedDBRuntime::with_options(options(&path, preset)).expect("parent cold boot");
            let child =
                RedDBRuntime::with_options(options(&fork_path, preset)).expect("fork cold boot");
            assert_eq!(values(&parent), ["parent"]);
            assert_eq!(values(&child), ["fork"]);
            assert_eq!(parent.list_store_forks().expect("listing").len(), 1);
            assert!(child
                .list_store_forks()
                .expect("no inherited siblings")
                .is_empty());
            no_metadata_sidecars(directory.path());
        }
    }
}

#[test]
fn fork_drop_detach_and_promotion_preserve_the_right_database() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let preset = StorageDeployPreset::Serverless;
    let parent = RedDBRuntime::with_options(options(&path, preset)).expect("parent");
    seed(&parent);
    parent
        .execute_query("FORK STORE AS discard")
        .expect("SQL create");
    let discarded = parent
        .fork_store_path("discard")
        .expect("path")
        .expect("fork");
    parent.execute_query("DROP FORK discard").expect("SQL drop");
    assert!(!discarded.exists());
    parent
        .execute_query("DROP FORK IF EXISTS discard")
        .expect("idempotent drop");
    parent.fork_store("keep").expect("fork");
    let detached = parent.fork_store_path("keep").expect("path").expect("fork");
    assert!(parent.detach_fork_store("keep").expect("detach"));
    assert!(!parent
        .drop_fork_store("keep")
        .expect("detached data is not parent-owned"));
    parent.fork_store("replacement").expect("fork");
    let promoted = parent
        .fork_store_path("replacement")
        .expect("path")
        .expect("fork");
    {
        let child = RedDBRuntime::with_options(options(&promoted, preset)).expect("candidate");
        child
            .execute_query("UPDATE items SET value='replacement' WHERE id=1")
            .expect("diverge");
        child.checkpoint().expect("durable candidate");
    }
    parent
        .execute_query("PROMOTE FORK replacement")
        .expect("promote");
    assert!(parent.list_store_forks().expect("listing").is_empty());
    assert_eq!(
        parent
            .promote_fork_store("replacement")
            .expect("idempotent retry"),
        promoted
    );
    drop(parent);
    for (path, expected) in [
        (&path, "original"),
        (&detached, "original"),
        (&promoted, "replacement"),
    ] {
        let runtime = RedDBRuntime::with_options(options(path, preset)).expect("cold boot");
        assert_eq!(values(&runtime), [expected]);
    }
    no_metadata_sidecars(directory.path());
}

#[test]
fn forks_reject_read_only_transactions_and_busy_destinations() {
    assert!(RedDBRuntime::in_memory()
        .expect("ephemeral runtime")
        .fork_store("ephemeral")
        .is_err());
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let preset = StorageDeployPreset::Serverless;
    let runtime = RedDBRuntime::with_options(options(&path, preset)).expect("parent");
    seed(&runtime);
    runtime.execute_query("BEGIN").expect("begin");
    assert!(runtime.fork_store("transaction").is_err());
    runtime.execute_query("ROLLBACK").expect("rollback");
    runtime.fork_store("busy").expect("fork");
    let child_path = runtime
        .fork_store_path("busy")
        .expect("path")
        .expect("fork");
    let child = RedDBRuntime::with_options(options(&child_path, preset)).expect("child");
    assert!(runtime.promote_fork_store("busy").is_err());
    assert!(runtime.drop_fork_store("busy").is_err());
    assert_eq!(values(&child), ["original"]);
    drop(child);
    assert!(runtime.drop_fork_store("busy").expect("drop once closed"));
    runtime.checkpoint().expect("checkpoint");
    drop(runtime);
    let readonly =
        RedDBRuntime::with_options(options(&path, preset).with_read_only(true)).expect("readonly");
    assert!(readonly.fork_store("denied").is_err());
    assert!(readonly.detach_fork_store("denied").is_err());
    assert!(readonly.drop_fork_store("denied").is_err());
    no_metadata_sidecars(directory.path());
}

#[test]
fn bounded_fork_excludes_another_connections_uncommitted_versions() {
    use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let open = |path: &Path| {
        RedDBRuntime::with_options(
            RedDBOptions::persistent(path)
                .with_memory_budget(64 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .expect("bounded runtime")
    };
    let parent = open(&path);
    seed(&parent);
    parent.checkpoint().expect("committed baseline");
    set_current_connection_id(42);
    parent
        .execute_query("BEGIN")
        .expect("begin another connection");
    parent
        .execute_query("UPDATE items SET value='pending' WHERE id=1")
        .expect("uncommitted update");
    clear_current_connection_id();
    parent
        .fork_store("committed")
        .expect("fork committed checkpoint");
    let child_path = parent
        .fork_store_path("committed")
        .expect("path")
        .expect("fork");
    set_current_connection_id(42);
    parent.execute_query("COMMIT").expect("later commit");
    clear_current_connection_id();
    assert_eq!(values(&parent), ["pending"]);
    parent.checkpoint().expect("parent checkpoint");
    drop(parent);
    for _ in 0..3 {
        let child = open(&child_path);
        assert!(child
            .db()
            .store()
            .get_collection("items")
            .expect("table")
            .has_backing());
        assert_eq!(values(&child), ["original"]);
        child.checkpoint().expect("fork checkpoint");
    }
    no_metadata_sidecars(directory.path());
}

#[test]
fn drop_refuses_a_replaced_fork_file() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let foreign_path = directory.path().join("foreign.rdb");
    let preset = StorageDeployPreset::Serverless;
    let parent = RedDBRuntime::with_options(options(&path, preset)).expect("parent");
    seed(&parent);
    parent.fork_store("owned").expect("fork");
    let child_path = parent
        .fork_store_path("owned")
        .expect("path")
        .expect("fork");
    {
        let foreign =
            RedDBRuntime::with_options(options(&foreign_path, preset)).expect("foreign database");
        seed(&foreign);
        foreign.checkpoint().expect("foreign checkpoint");
    }
    std::fs::copy(&foreign_path, &child_path).expect("simulate unrelated file replacement");
    let before = std::fs::read(&child_path).expect("foreign bytes");
    assert!(parent.drop_fork_store("owned").is_err());
    assert_eq!(std::fs::read(&child_path).expect("preserved"), before);
}

#[test]
fn replacement_primary_does_not_inherit_the_parents_replication_slots() {
    use reddb_server::replication::ReplicationConfig;
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let preset = StorageDeployPreset::Serverless;
    let parent = RedDBRuntime::with_options(
        options(&path, preset).with_replication(ReplicationConfig::primary()),
    )
    .expect("primary");
    seed(&parent);
    parent
        .ack_primary_replica_lsn_and_prune("parent-replica", 10, 8, 0, 0)
        .expect("slot");
    parent.fork_store("replacement").expect("fork");
    let child_path = parent
        .promote_fork_store("replacement")
        .expect("replacement");
    let replacement = RedDBRuntime::with_options(
        options(&child_path, preset).with_replication(ReplicationConfig::primary()),
    )
    .expect("replacement primary");
    assert!(replacement
        .primary_replica_slot_catalog()
        .expect("catalog")
        .expect("primary catalog")
        .slots
        .is_empty());
    assert_eq!(
        parent
            .primary_replica_slot_catalog()
            .expect("parent catalog")
            .expect("catalog")
            .slots
            .len(),
        1
    );
    assert_eq!(values(&replacement), ["original"]);
}
