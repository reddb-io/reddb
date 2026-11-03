use reddb_server::storage::{EntityId, UnifiedEntity};
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

#[test]
fn legacy_table_catalog_preserves_model_schema_and_count_after_cold_reopen() {
    let directory = tempfile::tempdir().expect("private catalog fixture");
    let options = RedDBOptions::persistent(directory.path().join("catalog.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("ordinary runtime");
        let store = runtime.db().store();
        store
            .create_collection("legacy_catalog_rows")
            .expect("legacy collection");
        for id in 1..=3 {
            store
                .insert(
                    "legacy_catalog_rows",
                    UnifiedEntity::table_row(
                        EntityId::new(1_000_000 + id),
                        "legacy_catalog_rows",
                        id,
                        vec![Value::Integer(id as i64), Value::text("persisted")],
                    ),
                )
                .expect("legacy row without a declared contract");
        }
        assert_legacy_table_catalog(&runtime);
        runtime.checkpoint().expect("persist ordinary fixture");
    }
    for _ in 0..2 {
        let runtime = RedDBRuntime::with_options(options.clone().with_bounded_disk_residency(true))
            .expect("disk-resident reopen");
        assert_legacy_table_catalog(&runtime);
        runtime.checkpoint().expect("checkpoint cold catalog");
    }
}

fn assert_legacy_table_catalog(runtime: &RedDBRuntime) {
    let result = runtime
        .execute_query(
            "SELECT model, schema_mode, entities FROM red.collections WHERE name = 'legacy_catalog_rows'",
        )
        .expect("authoritative catalog query");
    assert_eq!(result.result.records.len(), 1);
    let descriptor = &result.result.records[0];
    assert_eq!(descriptor.get("model"), Some(&Value::text("table")));
    assert_eq!(descriptor.get("schema_mode"), Some(&Value::text("strict")));
    assert_eq!(descriptor.get("entities"), Some(&Value::UnsignedInteger(3)));
    let tables = runtime
        .execute_query("SELECT name FROM red.tables WHERE name = 'legacy_catalog_rows'")
        .expect("typed table catalog must include cold legacy tables");
    assert_eq!(tables.result.records.len(), 1);
    let count = runtime
        .execute_query("SELECT COUNT(*) AS total FROM legacy_catalog_rows")
        .expect("cold data remains present");
    assert_eq!(
        count.result.records[0]
            .get("total")
            .expect("count result")
            .display_string(),
        "3"
    );
}

fn cold_fixture() -> (tempfile::TempDir, RedDBRuntime) {
    let directory = tempfile::tempdir().expect("private catalog fixture");
    let options = RedDBOptions::persistent(directory.path().join("catalog.rdb"))
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    {
        let runtime = RedDBRuntime::with_options(options.clone()).expect("seed runtime");
        let store = runtime.db().store();
        store
            .create_collection("legacy_catalog_rows")
            .expect("collection");
        for id in 1..=3 {
            store
                .insert(
                    "legacy_catalog_rows",
                    UnifiedEntity::table_row(
                        EntityId::new(1_000_000 + id),
                        "legacy_catalog_rows",
                        id,
                        vec![Value::Integer(id as i64)],
                    ),
                )
                .expect("seed legacy row");
        }
        runtime.checkpoint().expect("persist fixture");
    }
    let runtime = RedDBRuntime::with_options(options).expect("cold reopen");
    (directory, runtime)
}

#[test]
fn cold_catalog_merges_replacements_deletions_and_resident_inserts_once() {
    let (_directory, runtime) = cold_fixture();
    let store = runtime.db().store();
    let manager = store
        .get_collection("legacy_catalog_rows")
        .expect("collection");
    manager
        .update(UnifiedEntity::vector(
            EntityId::new(1_000_001),
            "legacy_catalog_rows",
            vec![1.0, 0.0],
        ))
        .expect("replace a cold table row with a resident vector");
    store
        .delete("legacy_catalog_rows", EntityId::new(1_000_002))
        .expect("delete cold row");
    store
        .insert(
            "legacy_catalog_rows",
            UnifiedEntity::table_row(
                EntityId::new(1_000_004),
                "legacy_catalog_rows",
                4,
                vec![Value::Integer(4)],
            ),
        )
        .expect("resident insert");
    for checkpoint in [false, true] {
        if checkpoint {
            runtime.checkpoint().expect("publish the merged generation");
        }
        let catalog = runtime.try_catalog().expect("authoritative catalog");
        let descriptor = catalog
            .collections
            .iter()
            .find(|collection| collection.name == "legacy_catalog_rows")
            .expect("legacy collection");
        assert_eq!(
            descriptor.model,
            reddb_server::catalog::CollectionModel::Mixed
        );
        assert_eq!(
            descriptor.entities, 3,
            "shadowed and deleted base rows are excluded"
        );
    }
}

#[test]
fn cold_catalog_propagates_corruption_instead_of_reporting_an_empty_collection() {
    use reddb_engine::storage::page::{Page, PageType};
    let (_directory, runtime) = cold_fixture();
    let store = runtime.db().store();
    let manager = store
        .get_collection("legacy_catalog_rows")
        .expect("collection");
    let root = manager.backing_root_page().expect("cold root");
    let pager = store.pager().expect("native pager");
    let original = pager.read_page(root).expect("original page");
    pager
        .write_page(root, Page::new(PageType::Overflow, root))
        .expect("inject corruption into the disposable fixture");
    let results: Vec<_> = [
        "red.collections",
        "red.tables",
        "red.columns",
        "red.indices",
    ]
    .into_iter()
    .map(|view| {
        (
            view,
            runtime.execute_query(&format!("SELECT * FROM {view}")),
        )
    })
    .collect();
    pager
        .write_page(root, original)
        .expect("restore fixture page");
    for (view, result) in results {
        let error = result.expect_err("storage errors must not become a successful empty catalog");
        assert!(
            error.to_string().to_lowercase().contains("page"),
            "{view}: {error}"
        );
    }
    assert_legacy_table_catalog(&runtime);
}

#[test]
fn cold_catalog_shares_memory_admission_with_other_queries() {
    use reddb_server::storage::memory_pools::MemoryPool;
    let (_directory, runtime) = cold_fixture();
    let accounting = runtime.memory_accounting();
    let occupied = runtime.memory_budget().resolved_bytes;
    accounting.charge(MemoryPool::QueryBuffers, occupied);
    let result = runtime.execute_query("SELECT * FROM red.collections");
    assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), occupied);
    accounting.release(MemoryPool::QueryBuffers, occupied);
    let error = result.expect_err("catalog cannot bypass shared query admission");
    assert!(
        matches!(
            error,
            reddb_server::RedDBError::MaterializationLimitExceeded { .. }
        ),
        "{error}"
    );
    assert_legacy_table_catalog(&runtime);
    assert_eq!(accounting.used_bytes(MemoryPool::QueryBuffers), 0);
}
