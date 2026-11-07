use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

#[test]
fn fork_lsn_is_an_actual_snapshot_boundary_not_a_historical_label() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(&path)
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("profile"),
    )
    .expect("runtime");
    runtime
        .execute_query("CREATE TABLE items (id INT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO items (id) VALUES (1)")
        .expect("data");
    let current = runtime.cdc_current_lsn();
    assert!(current > 0);
    for lsn in [current - 1, current + 1] {
        assert!(runtime
            .execute_query(&format!("FORK STORE AS wrong AT LSN {lsn}"))
            .is_err());
    }
    assert!(runtime.list_store_forks().expect("listing").is_empty());
    runtime
        .execute_query(&format!("FORK STORE AS exact AT LSN {current}"))
        .expect("current snapshot");
    assert_eq!(
        runtime.list_store_forks().expect("listing")[0].fork_lsn,
        current
    );
}
