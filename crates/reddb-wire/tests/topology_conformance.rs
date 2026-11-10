use reddb_wire::routing::{topology_from_json, topology_json, TOPOLOGY_TRANSPORT_METADATA_KEY};
use reddb_wire::topology::{decode_topology, encode_topology};

#[test]
fn shared_topology_contract_fixtures() {
    let manifest: serde_json::Value = serde_json::from_str(include_str!(
        "../../../testdata/conformance/topology/manifest.json"
    ))
    .expect("valid shared topology fixtures");
    assert_eq!(manifest["metadata_key"], TOPOLOGY_TRANSPORT_METADATA_KEY);
    for case in manifest["cases"].as_array().expect("fixture cases") {
        let hex = case["hex"].as_str().expect("fixture hex");
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|offset| u8::from_str_radix(&hex[offset..offset + 2], 16).expect("hex byte"))
            .collect();
        let decoded = decode_topology(&bytes);
        if case["error"] == true {
            assert!(decoded.is_err(), "{}", case["name"]);
            continue;
        }
        let decoded = decoded.expect("valid topology");
        assert_eq!(
            decoded.as_ref().map(topology_json),
            case.get("expected").filter(|v| !v.is_null()).cloned(),
            "{}",
            case["name"]
        );
        if case["canonical"] == true {
            let topology = topology_from_json(&case["expected"]).expect("canonical JSON");
            assert_eq!(encode_topology(&topology), bytes, "{}", case["name"]);
        }
    }
}
