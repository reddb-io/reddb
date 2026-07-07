use reddb_file::OperationalManifest;
use reddb_server::{RedDBOptions, RedDBRuntime};

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
fn fork_store_at_lsn_records_historical_lsn_and_rejects_future_without_partial_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fork-at-lsn.rdb");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");

    exec(&rt, "CREATE TABLE fork_lsn_items (id INT, label TEXT)");
    exec(
        &rt,
        "INSERT INTO fork_lsn_items (id, label) VALUES (1, 'before')",
    );
    let historical_lsn = rt.cdc_current_lsn();
    exec(
        &rt,
        "UPDATE fork_lsn_items SET label = 'after' WHERE id = 1",
    );

    exec(
        &rt,
        &format!("FORK STORE AS historical_cut AT LSN {historical_lsn}"),
    );

    let manifest = OperationalManifest::for_db_path(&path);
    let forks = manifest.list_forks().expect("list forks");
    assert_eq!(forks.len(), 1);
    assert_eq!(forks[0].name, "historical_cut");
    assert_eq!(forks[0].fork_lsn, historical_lsn);

    let err = rt
        .execute_query("FORK STORE AS too_old AT LSN 0")
        .expect_err("LSN below retention floor must fail");
    let message = err.to_string();
    assert!(message.contains("restore from backup"), "{message}");

    let forks = manifest.list_forks().expect("list forks after old fork");
    assert_eq!(forks.len(), 1);
    assert_eq!(forks[0].name, "historical_cut");

    let future_lsn = rt.cdc_current_lsn() + 1;
    let err = rt
        .execute_query(&format!("FORK STORE AS impossible AT LSN {future_lsn}"))
        .expect_err("future LSN must fail");
    let message = err.to_string();
    assert!(message.contains("restore from backup"), "{message}");

    let forks = manifest.list_forks().expect("list forks after failed fork");
    assert_eq!(forks.len(), 1);
    assert_eq!(forks[0].name, "historical_cut");
}
