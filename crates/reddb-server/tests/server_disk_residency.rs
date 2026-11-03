//! Exercise the public factory with the library compiled without cfg(test).
use reddb_server::storage::memory_budget::resolve_for_boot;
use reddb_server::storage::memory_pools::{BudgetShares, MemoryPool};
use reddb_server::storage::{DeployProfile, RedDB, StoragePackaging};
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

#[test]
fn explicit_residency_preserves_sql_indexes_counts_and_shared_budget_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.rdb");
    let ordinary = RedDBOptions::persistent(&path).with_memory_budget(64 * 1024 * 1024);
    assert!(!ordinary.bounded_disk_residency);
    let options = ordinary.clone().with_bounded_disk_residency(true);
    let shares = BudgetShares::resolve(
        resolve_for_boot(DeployProfile::Embedded, options.memory_budget_bytes).unwrap(),
        DeployProfile::Embedded,
    );
    {
        let rt = RedDBRuntime::with_options(options.clone()).unwrap();
        assert_eq!(
            rt.db().store().config().resident_memory_bytes_max,
            Some(shares.share_bytes(MemoryPool::SegmentArena))
        );
        rt.execute_query("CREATE TABLE test_rows (id INT, a INT, b INT)")
            .unwrap();
        rt.execute_query("CREATE INDEX rows_a ON test_rows (a) USING HASH")
            .unwrap();
        rt.execute_query("CREATE INDEX rows_b ON test_rows (b) USING BTREE")
            .unwrap();
        for id in 0..64 {
            rt.execute_query(&format!(
                "INSERT INTO test_rows (id, a, b) VALUES ({id}, {}, {id})",
                id % 4
            ))
            .unwrap();
        }
        rt.flush().unwrap();
    }
    for round in 0..2 {
        let rt = RedDBRuntime::with_options(options.clone()).unwrap();
        assert_eq!(
            rt.db().store().config().resident_memory_bytes_max,
            Some(shares.share_bytes(MemoryPool::SegmentArena))
        );
        let result = rt.execute_query("SELECT id FROM test_rows WHERE a = 3 AND b BETWEEN 0 AND 63 ORDER BY id LIMIT 3 OFFSET 1").unwrap();
        let ids: Vec<_> = result
            .result
            .records
            .iter()
            .map(|row| row.get("id").cloned().unwrap())
            .collect();
        assert_eq!(
            ids,
            vec![Value::Integer(7), Value::Integer(11), Value::Integer(15)]
        );
        let result = rt
            .execute_query("SELECT COUNT(*) AS n FROM test_rows")
            .unwrap();
        assert_eq!(
            result.result.records[0].get("n").unwrap().display_string(),
            if round == 0 { "64" } else { "63" }
        );
        if round == 0 {
            rt.execute_query("DELETE FROM test_rows WHERE id = 63")
                .unwrap();
            rt.execute_query("UPDATE test_rows SET b = 999 WHERE id = 0")
                .unwrap();
        } else {
            let result = rt
                .execute_query("SELECT id FROM test_rows WHERE b = 999")
                .unwrap();
            assert_eq!(result.result.records[0].get("id"), Some(&Value::Integer(0)));
        }
        rt.flush().unwrap();
    }
    let error = match RedDB::open_with_options(&ordinary) {
        Ok(_) => panic!("resident writer must reject prepared disk-residency format"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("disk-resident"), "{error}");
}

#[test]
fn unsupported_residency_configurations_fail_before_touching_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.rdb");
    let base = RedDBOptions::persistent(&path).with_bounded_disk_residency(true);
    let mut primary = base.clone();
    primary.replication = reddb_server::replication::ReplicationConfig::primary();
    let mut directory = base.clone();
    directory.storage_profile.packaging = StoragePackaging::OperationalDirectory;
    let mut serverless = base.clone();
    serverless.storage_profile.deploy_profile = DeployProfile::Serverless;
    let mut remote = base.clone();
    remote.remote_backend = Some(std::sync::Arc::new(
        reddb_server::storage::backend::LocalBackend,
    ));
    let mut remote_atomic = base.clone();
    remote_atomic.remote_backend_atomic = Some(std::sync::Arc::new(
        reddb_server::storage::backend::LocalBackend,
    ));
    for options in [primary, directory, serverless, remote, remote_atomic] {
        let error = match RedDB::open_with_options(&options) {
            Ok(_) => panic!("unsupported residency configuration was accepted"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("requires standalone single-file local storage"),
            "{error}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}

#[test]
fn explicit_residency_does_not_convert_legacy_snapshots_or_create_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.rdb");
    let mut options = RedDBOptions::persistent(&path)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    options.create_if_missing = false;
    assert!(RedDB::open_with_options(&options).is_err());
    assert!(!path.exists());

    let ordinary = options.clone().with_bounded_disk_residency(false);
    let mut ordinary = ordinary;
    ordinary.create_if_missing = true;
    {
        let rt = RedDBRuntime::with_options(ordinary.clone()).unwrap();
        rt.execute_query("CREATE TABLE preserved (id INT)").unwrap();
        rt.execute_query("INSERT INTO preserved (id) VALUES (42)")
            .unwrap();
        rt.flush().unwrap();
    }
    let before = std::fs::read(&path).unwrap();
    let error = match RedDB::open_with_options(&options) {
        Ok(_) => panic!("legacy snapshot conversion must be explicit"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("native paged database"), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let rt = RedDBRuntime::with_options(ordinary).unwrap();
    let result = rt.execute_query("SELECT id FROM preserved").unwrap();
    assert_eq!(
        result.result.records[0].get("id"),
        Some(&Value::Integer(42))
    );
}

#[test]
fn explicit_residency_prepares_existing_native_rows_and_indexes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("native.rdb");
    let resident = RedDBOptions::persistent(&path)
        .with_memory_budget(64 * 1024 * 1024)
        .with_storage_profile(reddb_server::StorageDeployPreset::PrimaryReplicaDev.selection())
        .unwrap();
    {
        let rt = RedDBRuntime::with_options(resident).unwrap();
        assert_eq!(rt.db().store().config().resident_memory_bytes_max, None);
        rt.execute_query("CREATE TABLE legacy_rows (id INT, value TEXT)")
            .unwrap();
        rt.execute_query("CREATE INDEX legacy_value ON legacy_rows (value) USING HASH")
            .unwrap();
        rt.execute_query("INSERT INTO legacy_rows (id, value) VALUES (42, 'preserved')")
            .unwrap();
        rt.flush().unwrap();
    }
    let bounded = RedDBOptions::persistent(&path)
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(true);
    for _ in 0..2 {
        let rt = RedDBRuntime::with_options(bounded.clone()).unwrap();
        let result = rt
            .execute_query("SELECT id FROM legacy_rows WHERE value = 'preserved'")
            .unwrap();
        assert_eq!(result.result.records.len(), 1);
        assert_eq!(
            result.result.records[0].get("id"),
            Some(&Value::Integer(42))
        );
        rt.flush().unwrap();
    }
}
