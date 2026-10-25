//! Identical catalog semantics in native Rust and browser WASM.
use reddb_engine::catalog::{cache::CollectionContractCache, codec::*, schema::*, *};
use reddb_engine::storage::{manager::ManagerConfig, resident_store::ResidentStore};
use reddb_file::{PhysicalCollectionContract, PhysicalDeclaredColumnContract};
use reddb_types::{ColumnDef, DataType, TableDef};
use std::{collections::HashSet, sync::Arc};

pub const CASES: &[(&str, fn())] = &[
    ("catalog_contract_round_trip", contract_round_trip),
    (
        "catalog_legacy_defaults_and_corruption",
        legacy_defaults_and_corruption,
    ),
    ("catalog_schema_resolution", schema_resolution),
    (
        "catalog_stored_expression_admission",
        stored_expression_admission,
    ),
    (
        "catalog_immutable_cache_and_invalidation",
        immutable_cache_and_invalidation,
    ),
    ("catalog_snapshot_recovery", snapshot_recovery),
];

fn column(name: &str) -> PhysicalDeclaredColumnContract {
    PhysicalDeclaredColumnContract {
        name: name.into(),
        data_type: "integer".into(),
        ..Default::default()
    }
}

pub fn persisted() -> PhysicalCollectionContract {
    let mut quantity = column("quantity");
    quantity.not_null = true;
    quantity.default = Some("1".into());
    quantity.check = Some("quantity > 0".into());
    let mut final_total = column("final_total");
    final_total.generated = Some("total + 1".into());
    let mut total = column("total");
    total.generated = Some("quantity * 5".into());
    total.check = Some("total < 100".into());
    PhysicalCollectionContract {
        name: "orders".into(),
        declared_model: "table".into(),
        schema_mode: "strict".into(),
        origin: "explicit".into(),
        version: 3,
        created_at_unix_ms: 123,
        updated_at_unix_ms: 456,
        default_ttl_ms: Some(60_000),
        context_index_fields: vec!["quantity".into()],
        declared_columns: vec![quantity, final_total, total],
        timestamps_enabled: true,
        context_index_enabled: true,
        append_only: true,
        ..Default::default()
    }
}

fn contract() -> CollectionContract {
    collection_contract_from_persisted(persisted()).expect("valid contract")
}

fn contract_round_trip() {
    for model in [
        "table",
        "document",
        "graph",
        "vector",
        "kv",
        "config",
        "vault",
        "mixed",
        "hll",
        "sketch",
        "filter",
        "timeseries",
        "queue",
        "metrics",
    ] {
        let mut expected = persisted();
        expected.declared_model = model.into();
        expected.vector_dimension = Some(384);
        expected.vector_metric = Some("cosine".into());
        let native_json = reddb_file::encode_physical_collection_contract_json(&expected)
            .expect("canonical file codec");
        let decoded =
            collection_contract_from_persisted(expected.clone()).expect("runtime mapping");
        assert_eq!(collection_contract_to_persisted(&decoded), expected);
        assert_eq!(
            collection_contract_to_json(&decoded).to_string_compact(),
            native_json
        );
        let bytes = serialize_collection_contracts(&[decoded]);
        let restored = deserialize_collection_contracts(&bytes).expect("contract blob");
        assert_eq!(collection_contract_to_persisted(&restored[0]), expected);
    }
}

fn legacy_defaults_and_corruption() {
    let legacy = reddb_engine::json::from_str(
        r#"{"name":"legacy","declared_model":"document","schema_mode":"dynamic","origin":"implicit","version":1,"created_at_unix_ms":0,"updated_at_unix_ms":0}"#,
    ).expect("legacy JSON");
    let decoded = collection_contract_from_json(&legacy).expect("legacy contract");
    assert!(
        decoded.context_index_enabled,
        "legacy default stays enabled"
    );
    assert!(!decoded.timestamps_enabled);
    assert!(!decoded.append_only);
    assert!(decoded.declared_columns.is_empty());
    for bytes in [b"null".as_slice(), b"{}", b"[", &[255]] {
        assert!(deserialize_collection_contracts(bytes).is_err());
    }
    for field in ["declared_model", "schema_mode", "origin", "table_def"] {
        let mut value = collection_contract_to_json(&contract());
        value[field] = reddb_types::json::Value::String("invalid".into());
        let error = collection_contract_from_json(&value).expect_err("invalid persisted contract");
        assert!(
            is_collection_contract_decode_error(&error),
            "typed failure for {field}"
        );
    }
}

fn schema_resolution() {
    let mut contract = contract();
    let columns = resolved_contract_columns(&contract).expect("declared rules");
    assert_eq!(columns[0].data_type, DataType::Integer);
    assert!(columns[0].not_null);
    assert_eq!(columns[0].default.as_deref(), Some("1"));
    // A decoded table definition remains authoritative over declared columns.
    contract.table_def = Some(TableDef {
        name: "orders".into(),
        columns: vec![ColumnDef::new("status", DataType::Enum)
            .with_variants(vec!["new".into(), "paid".into()])
            .not_null()
            .with_default(b"'new'".to_vec())],
        primary_key: vec![],
        indexes: vec![],
        constraints: vec![],
        version: 7,
        created_at: 123,
        updated_at: 456,
    });
    // Test the table codec without expression definitions referring to other fields.
    contract.declared_columns.clear();
    let value = collection_contract_to_json(&contract);
    let restored = collection_contract_from_json(&value).expect("table definition payload");
    assert_eq!(restored.table_def, contract.table_def);
    let columns = resolved_contract_columns(&restored).expect("table rules");
    assert_eq!(columns.len(), 1);
    assert_eq!(columns[0].name, "status");
    assert_eq!(columns[0].data_type, DataType::Enum);
    assert_eq!(columns[0].enum_variants, ["new", "paid"]);
    assert_eq!(columns[0].default.as_deref(), Some("'new'"));
    assert!(columns[0].not_null);
}

fn stored_expression_admission() {
    let contract = contract();
    assert!(has_contract_expressions(&contract));
    assert_eq!(
        generated_order(&contract)
            .expect("dependency order")
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        ["total", "final_total"]
    );
    for (index, generated, check, default) in [
        (0, None, Some("quantity + 1"), None),
        (1, Some("missing + 1"), None, None),
        (2, Some("final_total + 1"), None, None),
        (1, Some("total + 1"), None, Some("1")),
        (1, Some("random()"), None, None),
    ] {
        let mut invalid = persisted();
        let column = &mut invalid.declared_columns[index];
        column.generated = generated.map(str::to_string);
        column.check = check.map(str::to_string);
        column.default = default.map(str::to_string);
        assert!(collection_contract_from_persisted(invalid).is_err());
    }
}

fn immutable_cache_and_invalidation() {
    let cache = CollectionContractCache::default();
    let original = cache.get_or_load(|| vec![contract()]);
    let mut current = contract();
    current.version = 4;
    cache.insert(current.clone());
    let updated = cache.get_or_load(|| panic!("already cached"));
    assert_eq!(original["orders"].version, 3);
    assert_eq!(updated["orders"].version, 4);
    assert_eq!(cache.remove("orders").expect("removed").version, 4);
    assert!(cache.get_or_load(|| panic!("cached removal")).is_empty());
    cache.invalidate();
    let mut loads = 0;
    let rebuilt = cache.get_or_load(|| {
        loads += 1;
        if loads == 1 {
            cache.invalidate();
        }
        vec![current.clone()]
    });
    assert_eq!(loads, 2, "cannot publish across an invalidation");
    cache.replace(Default::default());
    assert!(cache.get_or_load(|| panic!("replaced")).is_empty());
    assert_eq!(rebuilt["orders"].version, 4);
    assert!(!Arc::ptr_eq(&original, &updated));
    for remove in [false, true] {
        cache.invalidate();
        let published = cache.get_or_load(|| {
            if remove {
                cache.remove("orders");
            } else {
                cache.insert(current.clone());
            }
            vec![contract()]
        });
        assert_eq!(
            published.get("orders").map(|value| value.version),
            if remove { None } else { Some(4) }
        );
    }
}

pub fn snapshot_fixture() -> Vec<u8> {
    let store = ResidentStore::new();
    store.get_or_create_collection("orders", &ManagerConfig::default());
    store.set_aux_metadata(serialize_collection_contracts(&[contract()]));
    store.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {})
}

pub fn verify_snapshot(bytes: &[u8]) -> u32 {
    let (store, _) = ResidentStore::from_snapshot(bytes, &ManagerConfig::default(), true, 1000)
        .expect("canonical snapshot recovery");
    assert!(store.get_collection("orders").is_some());
    let contracts =
        deserialize_collection_contracts(&store.aux_metadata()).expect("recovered catalog");
    assert_eq!(contracts.len(), 1);
    assert_eq!(collection_contract_to_persisted(&contracts[0]), persisted());
    validate_contract_expressions(&contracts[0]).expect("schema survives restart");
    1
}

fn snapshot_recovery() {
    assert_eq!(verify_snapshot(&snapshot_fixture()), 1);
}
