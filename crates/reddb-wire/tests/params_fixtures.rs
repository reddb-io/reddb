use std::collections::{BTreeMap, BTreeSet};

use prost::Message;
use reddb_grpc_proto::query_value::Kind;
use reddb_grpc_proto::{QueryNull, QueryRequest, QueryValue, QueryVector};
use serde_json::Value;

#[test]
fn params_manifest_is_complete_and_well_formed() {
    let manifest: Value =
        serde_json::from_str(include_str!("fixtures/params/manifest.json")).expect("manifest json");

    assert_eq!(manifest["version"], 1);
    assert_eq!(manifest["layout"], "redwire-query-with-params-v1");

    let values = manifest["values"].as_array().expect("values array");
    let mut by_name = BTreeMap::new();
    let mut kinds = BTreeSet::new();

    for value in values {
        let name = value["name"].as_str().expect("value name");
        let kind = value["kind"].as_str().expect("value kind");
        let hex = value["redwire_hex"].as_str().expect("value redwire_hex");
        let grpc_hex = value["grpc_hex"].as_str().expect("value grpc_hex");

        assert!(
            by_name.insert(name, kind).is_none(),
            "duplicate fixture name {name}"
        );
        kinds.insert(kind);
        assert_valid_hex(hex, name);
        assert_valid_hex(grpc_hex, name);
        assert_eq!(grpc_hex, encode_grpc_value_hex(name), "{name}: grpc_hex");
    }

    for required in [
        "null",
        "bool",
        "int",
        "float",
        "text",
        "bytes",
        "json",
        "timestamp",
        "uuid",
        "vector",
    ] {
        assert!(kinds.contains(required), "missing kind {required}");
    }

    for required in [
        "int_min",
        "int_max",
        "float_nan",
        "float_pos_inf",
        "float_neg_inf",
        "float_subnormal_min",
        "bytes_empty",
        "bytes_deadbeef",
        "json_nested",
        "timestamp_max",
        "uuid_001122",
        "vector_empty",
        "vector_three",
    ] {
        assert!(by_name.contains_key(required), "missing fixture {required}");
    }

    let queries = manifest["queries"].as_array().expect("queries array");
    assert!(!queries.is_empty(), "query fixtures must not be empty");
    for query in queries {
        let name = query["name"].as_str().expect("query name");
        assert!(query["sql"].as_str().is_some(), "{name}: missing sql");
        assert_valid_hex(
            query["redwire_hex"].as_str().expect("query redwire_hex"),
            name,
        );
        assert_valid_hex(
            query["grpc_request_hex"]
                .as_str()
                .expect("query grpc_request_hex"),
            name,
        );
        for param in query["params"].as_array().expect("query params") {
            let param = param.as_str().expect("query param name");
            assert!(
                by_name.contains_key(param),
                "{name}: unknown param fixture {param}"
            );
        }
        assert_eq!(
            query["grpc_request_hex"].as_str().unwrap(),
            encode_grpc_query_hex(query),
            "{name}: grpc_request_hex"
        );
    }
}

fn encode_grpc_value_hex(name: &str) -> String {
    to_hex(&grpc_value(name).encode_to_vec())
}

fn encode_grpc_query_hex(query: &Value) -> String {
    let params = query["params"]
        .as_array()
        .expect("query params")
        .iter()
        .map(|param| grpc_value(param.as_str().expect("query param name")))
        .collect();
    let request = QueryRequest {
        query: query["sql"].as_str().expect("query sql").to_string(),
        entity_types: Vec::new(),
        capabilities: Vec::new(),
        params,
    };
    to_hex(&request.encode_to_vec())
}

fn grpc_value(name: &str) -> QueryValue {
    let kind = match name {
        "null" => Kind::NullValue(QueryNull {}),
        "bool_true" => Kind::BoolValue(true),
        "bool_false" => Kind::BoolValue(false),
        "int_min" => Kind::IntValue(i64::MIN),
        "int_max" => Kind::IntValue(i64::MAX),
        "int_42" => Kind::IntValue(42),
        "float_nan" => Kind::FloatValue(f64::from_bits(0x7ff8000000000000)),
        "float_pos_inf" => Kind::FloatValue(f64::INFINITY),
        "float_neg_inf" => Kind::FloatValue(f64::NEG_INFINITY),
        "float_subnormal_min" => Kind::FloatValue(f64::from_bits(1)),
        "text_unicode" => Kind::TextValue("h\u{e9}llo".to_string()),
        "text_x" => Kind::TextValue("x".to_string()),
        "bytes_empty" => Kind::BytesValue(Vec::new()),
        "bytes_deadbeef" => Kind::BytesValue(vec![0xde, 0xad, 0xbe, 0xef]),
        "json_nested" => Kind::JsonValue(r#"{"a":null,"z":[1,{"deep":[true,false]}]}"#.to_string()),
        "timestamp_zero" => Kind::TimestampValue(0),
        "timestamp_max" => Kind::TimestampValue(i64::MAX),
        "uuid_001122" => Kind::UuidValue(vec![
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ]),
        "vector_empty" => Kind::VectorValue(QueryVector { values: Vec::new() }),
        "vector_three" => Kind::VectorValue(QueryVector {
            values: vec![1.0, 2.0, -0.5],
        }),
        other => panic!("unknown fixture {other}"),
    };
    QueryValue { kind: Some(kind) }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn assert_valid_hex(hex: &str, label: &str) {
    assert!(!hex.is_empty(), "{label}: empty hex");
    assert_eq!(hex.len() % 2, 0, "{label}: odd hex length");
    assert!(
        hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "{label}: non-hex byte in {hex}"
    );
}
