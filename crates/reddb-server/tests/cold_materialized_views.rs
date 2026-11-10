use reddb_server::{RedDBOptions, RedDBRuntime};

#[test]
fn cold_materialized_views_survive_replacement_and_drop_without_catalog_growth() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let directory = tempfile::tempdir().unwrap();
            let options = RedDBOptions::persistent(directory.path().join("views.rdb"))
                .with_memory_budget(64 * 1024 * 1024)
                .with_bounded_disk_residency(true);
            {
                let runtime = RedDBRuntime::with_options(options.clone()).unwrap();
                runtime
                    .execute_query("CREATE TABLE orders (id INT, total INT)")
                    .unwrap();
                runtime
                    .execute_query("INSERT INTO orders (id, total) VALUES (1, 100)")
                    .unwrap();
                runtime
                    .execute_query("CREATE MATERIALIZED VIEW paid_orders AS SELECT * FROM orders")
                    .unwrap();
                runtime.checkpoint().unwrap();
            }
            for cycle in 0..4 {
                let runtime = RedDBRuntime::with_options(options.clone()).unwrap();
                assert!(
                    runtime
                        .materialized_view_metadata()
                        .iter()
                        .any(|v| v.name == "paid_orders"),
                    "view missing after cold open {cycle}"
                );
                runtime
                    .execute_query(
                        "CREATE OR REPLACE MATERIALIZED VIEW paid_orders AS SELECT * FROM orders",
                    )
                    .unwrap();
                let manager = runtime
                    .db()
                    .store()
                    .get_collection("red.materialized_view.definitions")
                    .unwrap();
                let mut count = 0;
                manager
                    .try_for_each_entity(usize::MAX, |_| {
                        count += 1;
                        true
                    })
                    .unwrap();
                assert_eq!(
                    count, 1,
                    "replacement accumulated catalog rows on cycle {cycle}"
                );
                runtime
                    .execute_query("REFRESH MATERIALIZED VIEW paid_orders")
                    .unwrap();
                assert_eq!(
                    runtime
                        .execute_query("SELECT * FROM paid_orders")
                        .unwrap()
                        .result
                        .records
                        .len(),
                    1
                );
                runtime.checkpoint().unwrap();
            }
            {
                let runtime = RedDBRuntime::with_options(options.clone()).unwrap();
                runtime
                    .execute_query("DROP MATERIALIZED VIEW paid_orders")
                    .unwrap();
                runtime.checkpoint().unwrap();
            }
            let runtime = RedDBRuntime::with_options(options).unwrap();
            assert!(!runtime
                .materialized_view_metadata()
                .iter()
                .any(|v| v.name == "paid_orders"));
            let manager = runtime
                .db()
                .store()
                .get_collection("red.materialized_view.definitions")
                .unwrap();
            let mut count = 0;
            manager
                .try_for_each_entity(usize::MAX, |_| {
                    count += 1;
                    true
                })
                .unwrap();
            assert_eq!(count, 0, "drop must remove cold catalog records");
        })
        .unwrap()
        .join()
        .unwrap();
}
