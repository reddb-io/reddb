use proptest::prelude::*;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn select_one(rt: &RedDBRuntime, sql: &str, alias: &str) -> Value {
    let page = rt.execute_query(sql).expect("SELECT should succeed");
    let record = page
        .result
        .records
        .first()
        .unwrap_or_else(|| panic!("no records for `{sql}`"));
    record
        .get(alias)
        .unwrap_or_else(|| panic!("no `{alias}` field in {record:?}"))
        .clone()
}

fn select_texts(rt: &RedDBRuntime, sql: &str, alias: &str) -> Vec<String> {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .result
        .records
        .iter()
        .map(|record| match record.get(alias) {
            Some(Value::Text(value)) => value.to_string(),
            other => panic!("expected text field `{alias}`, got {other:?} in {record:?}"),
        })
        .collect()
}

fn explain_ops(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(&format!("EXPLAIN {sql}"))
        .unwrap_or_else(|err| panic!("EXPLAIN {sql}: {err:?}"))
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("op") {
            Some(Value::Text(value)) => Some(value.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn large_integer_round_trips_exactly_through_document_body() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue1768_big")
        .expect("CREATE DOCUMENT");

    let big = 9_007_199_254_740_993_i64;
    let max = i64::MAX;
    rt.execute_query(&format!(
        "INSERT INTO issue1768_big DOCUMENT VALUES ({{\"big\":{big},\"edge\":{max}}})"
    ))
    .expect("INSERT DOCUMENT");

    assert_eq!(
        select_one(
            &rt,
            "SELECT body.big AS big_val FROM issue1768_big",
            "big_val",
        ),
        Value::Integer(big)
    );
    assert_eq!(
        select_one(
            &rt,
            "SELECT body.edge AS edge_val FROM issue1768_big",
            "edge_val",
        ),
        Value::Integer(max)
    );
}

#[test]
fn genuine_float_keeps_float_behaviour() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue1768_float")
        .expect("CREATE DOCUMENT");
    rt.execute_query("INSERT INTO issue1768_float DOCUMENT VALUES ({\"ratio\":3.5,\"exp\":5e-1})")
        .expect("INSERT DOCUMENT");

    assert_eq!(
        select_one(
            &rt,
            "SELECT body.ratio AS ratio_val FROM issue1768_float",
            "ratio_val",
        ),
        Value::Float(3.5)
    );
    assert_eq!(
        select_one(
            &rt,
            "SELECT body.exp AS exp_val FROM issue1768_float",
            "exp_val",
        ),
        Value::Float(0.5)
    );
}

#[test]
fn cross_model_numeric_comparison_and_index_order_are_equivalent() {
    let rt = runtime();
    rt.execute_query("CREATE TABLE issue1782_rows (name TEXT, n INT)")
        .expect("CREATE TABLE");
    rt.execute_query("CREATE INDEX idx_issue1782_rows_n ON issue1782_rows (n)")
        .expect("CREATE row index");
    rt.execute_query(
        "INSERT INTO issue1782_rows (name, n) VALUES ('low', 9), ('same', 10), ('high', 11)",
    )
    .expect("INSERT rows");

    rt.execute_query("CREATE KV issue1782_kv")
        .expect("CREATE KV");
    rt.execute_query("CREATE INDEX idx_issue1782_kv_value ON issue1782_kv (value)")
        .expect("CREATE kv index");
    rt.execute_query("INSERT INTO issue1782_kv KV (key, value) VALUES ('low', 9)")
        .expect("INSERT low kv");
    rt.execute_query("INSERT INTO issue1782_kv KV (key, value) VALUES ('same', 10)")
        .expect("INSERT same kv");
    rt.execute_query("INSERT INTO issue1782_kv KV (key, value) VALUES ('high', 11)")
        .expect("INSERT high kv");

    rt.execute_query("CREATE DOCUMENT issue1782_docs")
        .expect("CREATE DOCUMENT");
    rt.execute_query("CREATE INDEX idx_issue1782_docs_n ON issue1782_docs (body.n)")
        .expect("CREATE document index");
    rt.execute_query(r#"INSERT INTO issue1782_docs DOCUMENT VALUES ({"name":"low","n":9})"#)
        .expect("INSERT low doc");
    rt.execute_query(r#"INSERT INTO issue1782_docs DOCUMENT VALUES ({"name":"same","n":10})"#)
        .expect("INSERT same doc");
    rt.execute_query(r#"INSERT INTO issue1782_docs DOCUMENT VALUES ({"name":"high","n":11})"#)
        .expect("INSERT high doc");

    assert_eq!(
        select_texts(
            &rt,
            "SELECT name FROM issue1782_rows WHERE n = 10.0",
            "name",
        ),
        vec!["same"],
    );
    assert_eq!(
        select_texts(
            &rt,
            "SELECT key FROM issue1782_kv WHERE value = 10.0",
            "key",
        ),
        vec!["same"],
    );
    assert_eq!(
        select_texts(
            &rt,
            "SELECT body.name AS name FROM issue1782_docs WHERE body.n = 10.0",
            "name",
        ),
        vec!["same"],
    );

    assert_eq!(
        select_texts(
            &rt,
            "SELECT name FROM issue1782_rows WHERE n BETWEEN 9.5 AND 11 ORDER BY n",
            "name",
        ),
        vec!["same", "high"],
    );
    assert_eq!(
        select_texts(
            &rt,
            "SELECT key FROM issue1782_kv WHERE value BETWEEN 9.5 AND 11 ORDER BY value",
            "key",
        ),
        vec!["same", "high"],
    );
    assert_eq!(
        select_texts(
            &rt,
            "SELECT key FROM issue1782_kv WHERE value BETWEEN 10 AND 11 ORDER BY value DESC",
            "key",
        ),
        vec!["high", "same"],
    );
    assert_eq!(
        select_texts(
            &rt,
            "SELECT body.name AS name FROM issue1782_docs WHERE body.n BETWEEN 9.5 AND 11 ORDER BY body.n",
            "name",
        ),
        vec!["same", "high"],
    );

    for sql in [
        "SELECT name FROM issue1782_rows WHERE n = 10.0",
        "SELECT key FROM issue1782_kv WHERE value = 10.0",
        "SELECT body.name AS name FROM issue1782_docs WHERE body.n = 10.0",
    ] {
        let ops = explain_ops(&rt, sql);
        assert!(
            ops.contains("index_seek"),
            "numeric equality should plan through an index for `{sql}`; ops={ops}"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    #[test]
    fn integer_extremes_round_trip_through_rql_surface(n in prop_oneof![
        Just(i64::MIN),
        Just(i64::MIN + 1),
        Just(-9_007_199_254_740_993_i64),
        Just(9_007_199_254_740_993_i64),
        Just(i64::MAX - 1),
        Just(i64::MAX),
        any::<i64>(),
    ]) {
        let rt = runtime();
        rt.execute_query("CREATE DOCUMENT issue1768_prop")
            .expect("CREATE DOCUMENT");
        rt.execute_query(&format!(
            "INSERT INTO issue1768_prop DOCUMENT VALUES ({{\"n\":{n}}})"
        ))
        .expect("INSERT DOCUMENT");

        let got = select_one(
            &rt,
            "SELECT body.n AS n_val FROM issue1768_prop",
            "n_val",
        );
        prop_assert_eq!(got, Value::Integer(n));
    }
}
