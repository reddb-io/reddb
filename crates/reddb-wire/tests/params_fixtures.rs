use std::collections::{BTreeMap, BTreeSet};

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

        assert!(
            by_name.insert(name, kind).is_none(),
            "duplicate fixture name {name}"
        );
        kinds.insert(kind);
        assert_valid_hex(hex, name);
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
        for param in query["params"].as_array().expect("query params") {
            let param = param.as_str().expect("query param name");
            assert!(
                by_name.contains_key(param),
                "{name}: unknown param fixture {param}"
            );
        }
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
