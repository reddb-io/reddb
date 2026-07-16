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

fn decimal_text() -> impl Strategy<Value = String> {
    (
        1u8..10,
        proptest::collection::vec(0u8..10, 19..40),
        proptest::collection::vec(0u8..10, 18..40),
    )
        .prop_map(|(first, rest, frac)| {
            let mut out = String::new();
            out.push(char::from(b'0' + first));
            for digit in rest {
                out.push(char::from(b'0' + digit));
            }
            out.push('.');
            for digit in frac {
                out.push(char::from(b'0' + digit));
            }
            out
        })
}

#[test]
fn high_precision_decimal_round_trips_exactly_through_document_body() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue1779_decimal")
        .expect("CREATE DOCUMENT");

    let decimal = "3.1415926535897932384626433832795028841971";
    rt.execute_query(&format!(
        "INSERT INTO issue1779_decimal DOCUMENT VALUES ({{\"n\":{decimal}}})"
    ))
    .expect("INSERT DOCUMENT");

    assert_eq!(
        select_one(
            &rt,
            "SELECT body.n AS n_val FROM issue1779_decimal",
            "n_val",
        ),
        Value::DecimalText(decimal.to_string())
    );
}

#[test]
fn decimal_text_values_sort_by_numeric_order() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT issue1779_order")
        .expect("CREATE DOCUMENT");

    rt.execute_query(
        "INSERT INTO issue1779_order DOCUMENT VALUES \
         ({\"n\":3.14159265358979323847}),\
         ({\"n\":3.14159265358979323846}),\
         ({\"n\":18446744073709551617}),\
         ({\"n\":18446744073709551616})",
    )
    .expect("INSERT DOCUMENT");

    let page = rt
        .execute_query("SELECT body.n AS n_val FROM issue1779_order ORDER BY body.n")
        .expect("SELECT should succeed");
    let got: Vec<Value> = page
        .result
        .records
        .iter()
        .map(|record| record.get("n_val").expect("n_val").clone())
        .collect();

    assert_eq!(
        got,
        vec![
            Value::DecimalText("3.14159265358979323846".to_string()),
            Value::DecimalText("3.14159265358979323847".to_string()),
            Value::DecimalText("18446744073709551616".to_string()),
            Value::DecimalText("18446744073709551617".to_string()),
        ]
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        ..ProptestConfig::default()
    })]

    #[test]
    fn generated_beyond_native_decimals_round_trip(decimal in decimal_text()) {
        let rt = runtime();
        rt.execute_query("CREATE DOCUMENT issue1779_prop")
            .expect("CREATE DOCUMENT");
        rt.execute_query(&format!(
            "INSERT INTO issue1779_prop DOCUMENT VALUES ({{\"n\":{decimal}}})"
        ))
        .expect("INSERT DOCUMENT");

        let got = select_one(
            &rt,
            "SELECT body.n AS n_val FROM issue1779_prop",
            "n_val",
        );
        prop_assert_eq!(got, Value::DecimalText(decimal));
    }
}
