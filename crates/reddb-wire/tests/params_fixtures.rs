use std::collections::{BTreeMap, BTreeSet};

use prost::Message;
use reddb_grpc_proto::query_value::Kind;
use reddb_grpc_proto::{QueryNull, QueryRequest, QueryValue, QueryVector};
use reddb_wire::query_with_params::{
    decode_query_with_params, decode_value as decode_redwire_value, encode_query_with_params,
    encode_value as encode_redwire_value, ParamValue as RedWireParamValue,
};
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
        let mut encoded = Vec::new();
        encode_redwire_value(&redwire_value(name), &mut encoded).expect("redwire value");
        assert_eq!(hex, to_hex(&encoded), "{name}: redwire_hex");
        let mut pos = 0;
        let decoded = decode_redwire_value(&from_hex(hex), &mut pos).expect("decode value");
        assert_redwire_value_eq(&redwire_value(name), &decoded, name);
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
        "bytes_256",
        "json_nested",
        "timestamp_max",
        "uuid_001122",
        "vector_empty",
        "vector_three",
        "vector_128",
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
        assert_eq!(
            query["redwire_hex"].as_str().unwrap(),
            encode_redwire_query_hex(query),
            "{name}: redwire_hex"
        );
        let (decoded_sql, decoded_params) = decode_query_with_params(&from_hex(
            query["redwire_hex"].as_str().expect("query redwire_hex"),
        ))
        .expect("decode redwire query");
        assert_eq!(query["sql"].as_str().expect("query sql"), decoded_sql);
        let expected_params = query["params"]
            .as_array()
            .expect("query params")
            .iter()
            .map(|param| redwire_value(param.as_str().expect("query param name")))
            .collect::<Vec<_>>();
        assert_eq!(expected_params, decoded_params, "{name}: redwire decode");
    }
}

fn encode_redwire_query_hex(query: &Value) -> String {
    let params = query["params"]
        .as_array()
        .expect("query params")
        .iter()
        .map(|param| redwire_value(param.as_str().expect("query param name")))
        .collect::<Vec<_>>();
    to_hex(
        &encode_query_with_params(query["sql"].as_str().expect("query sql"), &params)
            .expect("redwire query"),
    )
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
        "bytes_256" => Kind::BytesValue((0..=255).map(|value| value as u8).collect()),
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
        "vector_128" => Kind::VectorValue(QueryVector {
            values: (0..128).map(|value| value as f32).collect(),
        }),
        other => panic!("unknown fixture {other}"),
    };
    QueryValue { kind: Some(kind) }
}

fn redwire_value(name: &str) -> RedWireParamValue {
    match name {
        "null" => RedWireParamValue::Null,
        "bool_true" => RedWireParamValue::Bool(true),
        "bool_false" => RedWireParamValue::Bool(false),
        "int_min" => RedWireParamValue::Int(i64::MIN),
        "int_max" => RedWireParamValue::Int(i64::MAX),
        "int_42" => RedWireParamValue::Int(42),
        "float_nan" => RedWireParamValue::Float(f64::from_bits(0x7ff8000000000000)),
        "float_pos_inf" => RedWireParamValue::Float(f64::INFINITY),
        "float_neg_inf" => RedWireParamValue::Float(f64::NEG_INFINITY),
        "float_subnormal_min" => RedWireParamValue::Float(f64::from_bits(1)),
        "text_unicode" => RedWireParamValue::Text("h\u{e9}llo".to_string()),
        "text_x" => RedWireParamValue::Text("x".to_string()),
        "bytes_empty" => RedWireParamValue::Bytes(Vec::new()),
        "bytes_deadbeef" => RedWireParamValue::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
        "bytes_256" => RedWireParamValue::Bytes((0..=255).map(|value| value as u8).collect()),
        "json_nested" => RedWireParamValue::Json(
            r#"{"a":null,"z":[1,{"deep":[true,false]}]}"#.as_bytes().to_vec(),
        ),
        "timestamp_zero" => RedWireParamValue::Timestamp(0),
        "timestamp_max" => RedWireParamValue::Timestamp(i64::MAX),
        "uuid_001122" => RedWireParamValue::Uuid([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ]),
        "vector_empty" => RedWireParamValue::Vector(Vec::new()),
        "vector_three" => RedWireParamValue::Vector(vec![1.0, 2.0, -0.5]),
        "vector_128" => RedWireParamValue::Vector((0..128).map(|value| value as f32).collect()),
        other => panic!("unknown fixture {other}"),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0, "odd hex length");
    (0..hex.len())
        .step_by(2)
        .map(|idx| u8::from_str_radix(&hex[idx..idx + 2], 16).expect("hex byte"))
        .collect()
}

fn assert_redwire_value_eq(expected: &RedWireParamValue, actual: &RedWireParamValue, name: &str) {
    match (expected, actual) {
        (RedWireParamValue::Float(a), RedWireParamValue::Float(b)) if a.is_nan() && b.is_nan() => {}
        _ => assert_eq!(expected, actual, "{name}: redwire decode"),
    }
}

fn assert_valid_hex(hex: &str, label: &str) {
    assert!(!hex.is_empty(), "{label}: empty hex");
    assert_eq!(hex.len() % 2, 0, "{label}: odd hex length");
    assert!(
        hex.bytes().all(|b| b.is_ascii_hexdigit()),
        "{label}: non-hex byte in {hex}"
    );
}
