use reddb::{RedDBOptions, RedDBRuntime, Value};

#[test]
fn park_migration_catalog_commands_survive_cache_hits_and_switching_tables() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    runtime
        .execute_query("CREATE TABLE transfer_a (id INT PRIMARY KEY, value TEXT)")
        .unwrap();
    runtime
        .execute_query("CREATE TABLE transfer_b (id INT PRIMARY KEY, other TEXT)")
        .unwrap();
    for (table, column) in [
        ("transfer_a", "value"),
        ("transfer_a", "value"),
        ("transfer_b", "other"),
        ("transfer_a", "value"),
    ] {
        let described = runtime.execute_query(&format!("DESCRIBE {table}")).unwrap();
        assert!(described
            .result
            .records
            .iter()
            .any(|record| record.get("name") == Some(&Value::text(column))));
        let ddl = runtime
            .execute_query(&format!("SHOW CREATE TABLE {table}"))
            .unwrap();
        let Some(Value::Text(text)) = ddl.result.records[0].get("ddl") else {
            panic!("DDL text required")
        };
        assert!(text.contains(table) && text.contains(column), "{text}");
    }
}

#[test]
fn park_migration_implicit_index_survives_reopen_without_another_insert() {
    let directory = tempfile::tempdir().unwrap();
    let options = RedDBOptions::persistent(directory.path().join("data.rdb"));
    {
        let runtime = RedDBRuntime::with_options(options.clone()).unwrap();
        runtime
            .execute_query("CREATE TABLE transfer_data (id INT PRIMARY KEY, value TEXT)")
            .unwrap();
        runtime
            .execute_query("INSERT INTO transfer_data (id, value) VALUES (7, 'saved')")
            .unwrap();
    }
    let reopened = RedDBRuntime::with_options(options).unwrap();
    let indices = reopened
        .execute_query("SELECT * FROM red.indices WHERE collection = 'transfer_data'")
        .unwrap();
    assert!(indices
        .result
        .records
        .iter()
        .any(|record| record.get("name") == Some(&Value::text("idx_id"))));
    let rows = reopened
        .execute_query("SELECT value FROM transfer_data WHERE id = 7")
        .unwrap();
    assert_eq!(
        rows.result.records[0].get("value"),
        Some(&Value::text("saved"))
    );
}
