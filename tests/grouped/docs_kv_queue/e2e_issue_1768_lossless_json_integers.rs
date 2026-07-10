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

proptest! {
    #[test]
    fn integer_extremes_round_trip_through_rql_surface(n in any::<i64>()) {
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
