use reddb_server::{RedDBOptions, RedDBRuntime};

#[test]
fn empty_sql_tables_survive_cold_reopen() {
    for checkpoint_before_close in [false, true] {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let path = directory.path().join("empty-schema.rdb");
        let options = RedDBOptions::persistent(&path);

        {
            let runtime = RedDBRuntime::with_options(options.clone()).expect("initial runtime");
            runtime
                .execute_query("CREATE TABLE empty_a (id TEXT, payload TEXT)")
                .expect("first empty table should be created");
            runtime
                .execute_query("CREATE TABLE empty_b (id INT PRIMARY KEY)")
                .expect("second empty table should be created");
            if checkpoint_before_close {
                runtime.checkpoint().expect("explicit checkpoint");
            }
        }

        let reopened = RedDBRuntime::with_options(options).expect("cold reopen");
        for table in ["empty_a", "empty_b"] {
            let rows = reopened
                .execute_query(&format!("SELECT * FROM {table}"))
                .unwrap_or_else(|error| {
                    panic!(
                        "{table} should survive reopen (checkpoint={checkpoint_before_close}): {error}"
                    )
                });
            assert!(rows.result.records.is_empty(), "{table} should stay empty");
        }
    }
}
