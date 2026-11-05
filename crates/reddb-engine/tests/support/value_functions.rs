//! Native resolved-value scalar dispatch and bounds, shared with browser WASM.
use reddb_engine::query::runtime::value_functions::{
    evaluate_value_function_with_host, ValueFunctionHost,
};
use reddb_types::Value;
use std::cell::{Cell, RefCell};

pub const CASES: &[(&str, fn())] = &[
    ("value_functions_text_and_missing", text_and_missing),
    ("value_functions_clock_money_and_geo", clock_money_and_geo),
    (
        "value_functions_json_and_binary_documents",
        json_and_binary_documents,
    ),
    ("value_functions_json_mutation", json_mutation),
    (
        "value_functions_depth_and_wide_indices",
        depth_and_wide_indices,
    ),
    ("value_functions_host_routing", host_routing),
];

#[derive(Default)]
struct Host {
    clock_reads: Cell<usize>,
    fallbacks: RefCell<Vec<(String, Vec<Value>)>>,
}
impl ValueFunctionHost for Host {
    fn current_unix_ms(&self) -> i64 {
        self.clock_reads.set(self.clock_reads.get() + 1);
        86_400_001
    }
    fn fallback_function(&self, name: &str, args: &[Value]) -> Option<Value> {
        self.fallbacks
            .borrow_mut()
            .push((name.into(), args.to_vec()));
        Some(Value::text("host fallback"))
    }
}
fn call(host: &Host, name: &str, args: &[Value]) -> Option<Value> {
    evaluate_value_function_with_host(host, name, args)
}
fn json(text: &str) -> Value {
    Value::Json(text.as_bytes().to_vec())
}
fn json_output(value: Option<Value>) -> reddb_engine::json::Value {
    let Some(Value::Json(bytes)) = value else {
        panic!("expected a JSON result")
    };
    reddb_engine::json::from_slice(&bytes).expect("valid JSON output")
}

fn text_and_missing() {
    let host = Host::default();
    for (name, args, expected) in [
        (
            "LENGTH",
            vec![Value::text("aé🦀z")],
            Some(Value::Integer(4)),
        ),
        (
            "OCTET_LENGTH",
            vec![Value::text("aé🦀z")],
            Some(Value::Integer(8)),
        ),
        (
            "BIT_LENGTH",
            vec![Value::text("aé🦀z")],
            Some(Value::Integer(64)),
        ),
        ("LOWER", vec![Value::text("AbC")], Some(Value::text("abc"))),
        ("UPPER", vec![Value::Integer(3)], Some(Value::Integer(3))),
        (
            "SUBSTRING",
            vec![
                Value::text("aé🦀z"),
                Value::Integer(2),
                Value::Integer(4_294_967_296),
            ],
            Some(Value::text("é🦀z")),
        ),
        (
            "SUBSTRING",
            vec![Value::text("é42z"), Value::text("é([0-9]+)z")],
            Some(Value::text("42")),
        ),
        (
            "RIGHT",
            vec![Value::text("aé🦀z"), Value::Integer(i64::MIN)],
            Some(Value::text("")),
        ),
        (
            "TRIM",
            vec![Value::text("xxabcxx"), Value::text("x")],
            Some(Value::text("abc")),
        ),
        (
            "CONCAT",
            vec![Value::Null, Value::text("a"), Value::text("b")],
            Some(Value::text("ab")),
        ),
        (
            "CONCAT_WS",
            vec![
                Value::text(":"),
                Value::Null,
                Value::text("a"),
                Value::text("b"),
            ],
            Some(Value::text("'a':'b'")),
        ),
        (
            "COALESCE",
            vec![Value::Null, Value::Integer(3)],
            Some(Value::Integer(3)),
        ),
        ("COALESCE", vec![], Some(Value::Null)),
        ("LENGTH", vec![Value::Null], Some(Value::Null)),
        ("LENGTH", vec![], None),
        ("ABS", vec![Value::Integer(i64::MIN)], None),
        ("ABS", vec![Value::Integer(-3)], Some(Value::Integer(3))),
        ("ROUND", vec![Value::Float(1.6)], Some(Value::Float(2.0))),
    ] {
        assert_eq!(call(&host, name, &args), expected, "{name}");
    }
    assert!(
        host.fallbacks.borrow().is_empty(),
        "known failures must not fall through to native effects"
    );
    assert_eq!(host.clock_reads.get(), 0);
}

fn clock_money_and_geo() {
    let host = Host::default();
    for (name, expected) in [
        ("NOW", Value::TimestampMs(86_400_001)),
        ("CURRENT_TIMESTAMP", Value::TimestampMs(86_400_001)),
        ("CURRENT_DATE", Value::Date(1)),
        ("CURRENT_TIME", Value::Time(1)),
    ] {
        assert_eq!(call(&host, name, &[]), Some(expected));
    }
    assert_eq!(host.clock_reads.get(), 4);
    assert_eq!(
        call(
            &host,
            "TIME_BUCKET",
            &[Value::text("5ms"), Value::TimestampMs(17)]
        ),
        Some(Value::UnsignedInteger(15_000_000))
    );
    assert_eq!(
        call(
            &host,
            "TIME_BUCKET",
            &[Value::text("5ms"), Value::TimestampMs(i64::MAX)]
        ),
        None
    );
    let money = call(
        &host,
        "MONEY",
        &[Value::AssetCode("BTC".into()), Value::text("0.125")],
    )
    .expect("money");
    assert_eq!(
        money,
        Value::Money {
            asset_code: "BTC".into(),
            minor_units: 125,
            scale: 3
        }
    );
    assert_eq!(
        call(&host, "MONEY_MINOR", std::slice::from_ref(&money)),
        Some(Value::BigInt(125))
    );
    for name in ["GEO_DISTANCE", "HAVERSINE", "VINCENTY"] {
        assert_eq!(
            call(&host, name, &[Value::GeoPoint(0, 0), Value::GeoPoint(0, 0)]),
            Some(Value::Float(0.0))
        );
    }
    let polygon = call(
        &host,
        "POLYGON",
        &[
            Value::Integer(0),
            Value::Integer(0),
            Value::Integer(0),
            Value::Integer(2),
            Value::Integer(2),
            Value::Integer(0),
        ],
    )
    .expect("polygon");
    assert_eq!(
        call(
            &host,
            "GEO_WITHIN",
            &[Value::GeoPoint(500_000, 500_000), polygon]
        ),
        Some(Value::Boolean(true))
    );
    assert!(host.fallbacks.borrow().is_empty());
}

fn json_and_binary_documents() {
    let host = Host::default();
    let score = Value::Integer(9_007_199_254_740_993);
    let mut bytes = vec![];
    reddb_types::document_body_codec::encode(
        &[("score", &score), ("name", &Value::text("red"))],
        &mut bytes,
    )
    .expect("canonical binary document");
    let document = Value::Json(bytes);
    assert_eq!(
        call(
            &host,
            "JSON_EXTRACT_TEXT",
            &[document.clone(), Value::text("$.score")]
        ),
        Some(Value::text("9007199254740993"))
    );
    assert_eq!(
        call(
            &host,
            "JSON_EXTRACT",
            &[document.clone(), Value::text("$.name")]
        ),
        Some(Value::text("\"red\""))
    );
    assert_eq!(
        call(&host, "JSON_EXTRACT", &[document, Value::text("$.missing")]),
        Some(Value::Null)
    );
    assert_eq!(
        call(&host, "JSON_ARRAY_LENGTH", &[json("[1,2,3]")]),
        Some(Value::Integer(3))
    );
    assert_eq!(
        call(&host, "JSON_TYPEOF", &[json("{\"a\":1}")]),
        Some(Value::text("object"))
    );
    assert_eq!(
        call(&host, "JSON_PARSE", &[Value::text("bad")]),
        Some(Value::Null)
    );
    assert_eq!(
        call(&host, "JSON_VALID", &[Value::text("{\"a\":1}")]),
        Some(Value::Boolean(true))
    );
    for (input, needle, expected) in [
        (json(r#"{"items":[{"label":"person"}]}"#), "person", true),
        (json(r#"{"person":0}"#), "person", false),
        (json(r#"{"label":"person"}"#), "per", false),
        (Value::text("person"), "per", true),
    ] {
        assert_eq!(
            call(&host, "CONTAINS", &[input, Value::text(needle)]),
            Some(Value::Boolean(expected))
        );
    }
    assert!(host.fallbacks.borrow().is_empty());
}

fn json_mutation() {
    let host = Host::default();
    let input = json("{}");
    let output = json_output(call(
        &host,
        "JSON_SET",
        &[
            input.clone(),
            Value::text("$.a[2].name"),
            Value::text("red"),
        ],
    ));
    assert_eq!(output["a"].as_array().expect("array").len(), 3);
    let slots = output["a"].as_array().expect("array slots");
    assert!(matches!(slots[0], reddb_engine::json::Value::Null));
    assert_eq!(slots[2]["name"].as_str(), Some("red"));
    assert_eq!(input, json("{}"), "JSON_SET mutates only its owned result");
    assert_eq!(
        json_output(call(
            &host,
            "JSON_SET",
            &[input, Value::text("$"), Value::text("root")]
        ))
        .as_str(),
        Some("root")
    );
    let array = json_output(call(
        &host,
        "JSON_ARRAY",
        &[Value::Integer(1), Value::text("two"), Value::Boolean(true)],
    ));
    let items = array.as_array().expect("array constructor output");
    assert_eq!(items[0].as_i64(), Some(1));
    assert_eq!(items[1].as_str(), Some("two"));
    let object = json_output(call(
        &host,
        "JSON_OBJECT",
        &[Value::text("value"), Value::Integer(7)],
    ));
    assert_eq!(object["value"].as_i64(), Some(7));
    assert_eq!(
        call(&host, "JSON_OBJECT", &[Value::text("unpaired")]),
        Some(Value::Null)
    );
    assert_eq!(
        call(
            &host,
            "JSON_SET",
            &[json("{}"), Value::text("$.a["), Value::Integer(1)]
        ),
        None
    );
}

fn depth_and_wide_indices() {
    let host = Host::default();
    for index in ["4294967296", "18446744073709551615"] {
        let path = Value::text(format!("$[{index}]"));
        assert_eq!(
            call(&host, "JSON_EXTRACT", &[json("[]"), path.clone()]),
            Some(Value::Null),
            "wide valid read is a missing path on every target"
        );
        assert_eq!(
            call(&host, "JSON_SET", &[json("[]"), path, Value::Integer(1)]),
            Some(Value::Null),
            "wide growth is rejected before allocation on every target"
        );
    }
    let path = format!(
        "${}",
        ".a".repeat(reddb_rql::limits::JSON_LITERAL_MAX_DEPTH as usize)
    );
    assert!(matches!(
        call(
            &host,
            "JSON_SET",
            &[json("{}"), Value::text(path.as_str()), Value::Integer(1)]
        ),
        Some(Value::Json(_))
    ));
    assert_eq!(
        call(
            &host,
            "JSON_SET",
            &[
                json("{}"),
                Value::text(format!("{path}.a")),
                Value::Integer(1)
            ]
        ),
        None
    );
    let mut nested = Value::text("needle");
    for _ in 0..reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        nested = Value::Array(vec![nested]);
    }
    assert_eq!(
        call(&host, "CONTAINS", &[nested.clone(), Value::text("needle")]),
        Some(Value::Boolean(true))
    );
    assert_eq!(
        call(
            &host,
            "CONTAINS",
            &[Value::Array(vec![nested]), Value::text("needle")]
        ),
        Some(Value::Boolean(false))
    );
}

fn host_routing() {
    let host = Host::default();
    let args = [Value::Integer(42)];
    for name in [
        "CURRENT_TENANT",
        "CURRENT_USER",
        "CURRENT_ROLE",
        "PG_TRY_ADVISORY_LOCK",
        "H3_INDEX",
        "VERIFY_PASSWORD",
        "unknown",
        "coalesce",
    ] {
        assert_eq!(call(&host, name, &args), Some(Value::text("host fallback")));
    }
    assert_eq!(host.fallbacks.borrow().len(), 8);
    assert!(host
        .fallbacks
        .borrow()
        .iter()
        .all(|(_, forwarded)| forwarded == &args));
    assert_eq!(host.clock_reads.get(), 0);
}
