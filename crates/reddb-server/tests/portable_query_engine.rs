use reddb_engine::query::scalar;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::{json, Value};

#[test]
fn portable_scalar_corpus_matches_the_native_runtime() {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory())
        .expect("native in-memory runtime opens");
    let cases: json::Value =
        json::from_str(include_str!("../../reddb-engine/tests/scalar-cases.json"))
            .expect("checked-in corpus is valid JSON");
    for case in cases.as_array().expect("corpus is an array") {
        let sql = case["sql"].as_str().expect("case has SQL");
        let portable = scalar::execute(sql).expect(sql);
        let native = runtime.execute_query(sql).expect(sql).result;
        assert_eq!(native.columns, portable.columns, "{sql}");
        let native_rows: Vec<Vec<json::Value>> = native
            .records
            .iter()
            .map(|record| {
                native
                    .columns
                    .iter()
                    .map(|column| record.get(column).unwrap_or(&Value::Null).to_json())
                    .collect()
            })
            .collect();
        let portable_rows: Vec<Vec<json::Value>> = portable
            .rows
            .iter()
            .map(|row| row.iter().map(Value::to_json).collect())
            .collect();
        assert_eq!(native_rows, portable_rows, "{sql}");
    }
}

#[test]
fn native_public_operator_paths_keep_the_same_types() {
    use reddb_engine::query::{engine::binding, executor, filter};
    use reddb_server::storage::query as native;

    let _: native::QueryPlan = executor::QueryPlan::new("people");
    let _: native::Filter = filter::Filter::ge("age", Value::Integer(18));
    let _: native::Binding = binding::Binding::empty();
}
