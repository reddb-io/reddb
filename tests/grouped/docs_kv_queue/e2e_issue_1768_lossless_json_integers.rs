// Regression coverage for issue #1768 — Lossless JSON numbers: exact integers
// survive the document body through the public RQL surface (INSERT → SELECT).
//
// A large integer beyond f64's exact range (±2^53) must round-trip byte-for-
// byte; a genuine float must keep its float behaviour. The in-house JSON value
// type carries an exact-integer representation, and the document body codec
// stores it losslessly (i64), so the pre-#1768 f64 snap no longer happens.

use proptest::prelude::*;
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

/// Project a single aliased body field from the first (only) record.
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

// Acceptance: a large integer beyond f64's exact range written into a document
// body comes back exactly as written via INSERT → SELECT.
#[test]
fn large_integer_round_trips_exactly_through_document_body() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue1768_big")
        .expect("CREATE DOCUMENT");

    // 2^53 + 1 = 9007199254740993 is NOT exactly representable as f64 (it snaps
    // to 9007199254740992). i64::MAX exercises the far edge.
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
        Value::Integer(big),
        "large integer must survive the document body exactly"
    );
    assert_eq!(
        select_one(
            &rt,
            "SELECT body.edge AS edge_val FROM issue1768_big",
            "edge_val",
        ),
        Value::Integer(max),
        "i64::MAX must survive the document body exactly"
    );
}

// Acceptance: genuine floats keep their current behavior.
#[test]
fn genuine_float_keeps_float_behaviour() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue1768_float")
        .expect("CREATE DOCUMENT");
    rt.execute_query(
        "INSERT INTO issue1768_float DOCUMENT VALUES ({\"ratio\":3.5,\"exp\":1.5e3})",
    )
    .expect("INSERT DOCUMENT");

    assert_eq!(
        select_one(
            &rt,
            "SELECT body.ratio AS ratio_val FROM issue1768_float",
            "ratio_val",
        ),
        Value::Float(3.5),
        "a fractional literal must stay a float"
    );
    match select_one(
        &rt,
        "SELECT body.exp AS exp_val FROM issue1768_float",
        "exp_val",
    ) {
        Value::Float(n) => assert_eq!(n, 1500.0),
        other => panic!("exponent literal must stay a float, got {other:?}"),
    }
}

proptest! {
    // Acceptance: generated integer extremes round-trip exactly through the
    // document body via the RQL surface.
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
