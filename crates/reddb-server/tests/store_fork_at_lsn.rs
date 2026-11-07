use reddb_server::{RedDBOptions, RedDBRuntime};

#[test]
fn historical_and_future_forks_cannot_reenable_external_metadata() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
    runtime
        .execute_query("CREATE TABLE items (id INT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO items (id) VALUES (1)")
        .expect("data");
    for lsn in [0, runtime.cdc_current_lsn(), runtime.cdc_current_lsn() + 1] {
        let error = runtime
            .execute_query(&format!("FORK STORE AS experiment AT LSN {lsn}"))
            .err()
            .expect("external fork disabled");
        assert!(
            error
                .to_string()
                .contains("external manifests are disabled"),
            "{error}"
        );
    }
    assert!(!reddb_file::OperationalManifest::for_db_path(&path)
        .current_manifest_path_for_test()
        .exists());
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM items")
            .expect("preserved data")
            .result
            .len(),
        1
    );
}
