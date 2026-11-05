//! Native write-admission rules, executed identically in browser WASM.
use reddb_engine::catalog::{codec::collection_contract_from_persisted, enforcement::*, *};
use reddb_engine::storage::{entity::EntityId, metadata::MetadataValue};
use reddb_file::PhysicalDeclaredColumnContract;
use reddb_types::{ColumnDef, DataType, TableDef, Value};

pub const CASES: &[(&str, fn())] = &[
    (
        "contract_write_defaults_and_schema_modes",
        defaults_and_schema_modes,
    ),
    ("contract_write_coercion_and_nulls", coercion_and_nulls),
    ("contract_write_generated_and_checks", generated_and_checks),
    (
        "contract_write_update_plan_and_timestamps",
        update_plan_and_timestamps,
    ),
    (
        "contract_write_unique_keys_and_targets",
        unique_keys_and_targets,
    ),
    (
        "contract_write_models_and_vector_dimensions",
        models_and_vector_dimensions,
    ),
    (
        "contract_write_ttl_overrides_and_bounds",
        ttl_overrides_and_bounds,
    ),
];

pub fn contract() -> CollectionContract {
    let mut persisted = super::catalog::persisted();
    persisted.timestamps_enabled = false;
    persisted.append_only = false;
    collection_contract_from_persisted(persisted).expect("expression-bearing contract")
}

fn fields(values: &[(&str, Value)]) -> Vec<(String, Value)> {
    values
        .iter()
        .map(|(name, value)| (name.to_string(), value.clone()))
        .collect()
}

fn typed_contract(data_type: DataType) -> CollectionContract {
    let mut contract = contract();
    contract.declared_columns.clear();
    contract.table_def = Some(TableDef {
        name: contract.name.clone(),
        columns: vec![ColumnDef::new("value", data_type)
            .not_null()
            .with_variants(vec!["new".into(), "paid".into()])],
        primary_key: vec![],
        indexes: vec![],
        constraints: vec![],
        version: 1,
        created_at: 123,
        updated_at: 123,
    });
    contract
}

fn defaults_and_schema_modes() {
    let mut contract = contract();
    let normalized = CollectionContractEnforcer::new(&contract)
        .normalize_insert_at(vec![], 100)
        .expect("defaults then generated fields");
    assert_eq!(
        normalized,
        fields(&[
            ("quantity", Value::Integer(1)),
            ("total", Value::Integer(5)),
            ("final_total", Value::Integer(6)),
        ])
    );
    for mode in [
        SchemaMode::Strict,
        SchemaMode::SemiStructured,
        SchemaMode::Dynamic,
    ] {
        contract.schema_mode = mode;
        let result = CollectionContractEnforcer::new(&contract)
            .normalize_insert_at(fields(&[("extra", Value::Boolean(true))]), 100);
        if mode == SchemaMode::Strict {
            assert_eq!(result.expect_err("strict rejection").to_string(),
                "query error: collection 'orders' is strict and does not allow undeclared fields: extra");
        } else {
            assert!(result
                .expect("dynamic field")
                .contains(&("extra".into(), Value::Boolean(true))));
        }
    }
}

fn coercion_and_nulls() {
    for (data_type, input, expected) in [
        (
            DataType::Integer,
            Value::text("42"),
            Some(Value::Integer(42)),
        ),
        (DataType::Integer, Value::text("forty-two"), None),
        (DataType::Integer, Value::Null, None),
        (
            DataType::Timestamp,
            Value::text("123"),
            Some(Value::Timestamp(123)),
        ),
        (
            DataType::Duration,
            Value::text("321"),
            Some(Value::Duration(321)),
        ),
        (
            DataType::Blob,
            Value::text("bytes"),
            Some(Value::Blob(b"bytes".to_vec())),
        ),
        (DataType::Json, Value::text("{\"n\":1}"), None),
        (
            DataType::Json,
            Value::Json(b"{\"n\":1}".to_vec()),
            Some(Value::Json(b"{\"n\":1}".to_vec())),
        ),
        (DataType::Vector, Value::text("[1,2]"), None),
        (DataType::Array, Value::text("[1,2]"), None),
        (
            DataType::Enum,
            Value::text("paid"),
            Some(Value::EnumValue(1)),
        ),
        (DataType::Enum, Value::text("missing"), None),
    ] {
        let contract = typed_contract(data_type);
        let result = CollectionContractEnforcer::new(&contract)
            .normalize_insert_at(fields(&[("value", input)]), 100);
        if let Some(expected) = expected {
            assert_eq!(
                result.expect("canonical coercion"),
                fields(&[("value", expected)])
            );
        } else {
            assert!(
                matches!(result, Err(ContractError::Query(_))),
                "{data_type:?}"
            );
        }
    }
    let mut contract = typed_contract(DataType::Integer);
    contract.table_def.as_mut().expect("typed table").columns[0].nullable = true;
    assert_eq!(
        CollectionContractEnforcer::new(&contract)
            .normalize_insert_at(fields(&[("value", Value::Null)]), 1)
            .expect("nullable"),
        fields(&[("value", Value::Null)])
    );
}

fn generated_and_checks() {
    let contract = contract();
    let enforcer = CollectionContractEnforcer::new(&contract);
    let supplied = fields(&[
        ("quantity", Value::text("3")),
        ("total", Value::Integer(999)),
        ("final_total", Value::Integer(999)),
    ]);
    let normalized = enforcer
        .normalize_insert_at(supplied, 1)
        .expect("derived values replace input");
    assert_eq!(
        normalized,
        fields(&[
            ("quantity", Value::Integer(3)),
            ("total", Value::Integer(15)),
            ("final_total", Value::Integer(16))
        ])
    );
    let mut updated = normalized;
    updated[0].1 = Value::Integer(4);
    assert_eq!(
        enforcer.normalize_update_at(updated, 2).expect("recompute"),
        fields(&[
            ("quantity", Value::Integer(4)),
            ("total", Value::Integer(20)),
            ("final_total", Value::Integer(21))
        ])
    );
    for quantity in [0, -1, 20] {
        let error = enforcer
            .normalize_insert_at(fields(&[("quantity", Value::Integer(quantity))]), 1)
            .expect_err("CHECK rejection");
        assert!(error.to_string().contains("CHECK constraint violated"));
    }
    let mut nullable = contract.clone();
    nullable.declared_columns[0].not_null = false;
    nullable.declared_columns[0].default = None;
    assert!(
        CollectionContractEnforcer::new(&nullable)
            .normalize_insert_at(vec![], 1)
            .is_ok(),
        "NULL CHECK accepts missing nullable columns"
    );
    let mut failing = contract;
    failing.declared_columns[2].generated = Some(
        reddb_rql::schema_expression::SchemaExpression::parse("quantity / 0".into())
            .expect("scalar AST"),
    );
    assert!(
        CollectionContractEnforcer::new(&failing)
            .normalize_insert_at(vec![], 1)
            .is_err(),
        "evaluation errors cannot admit a row"
    );
}

fn update_plan_and_timestamps() {
    let mut contract = typed_contract(DataType::Integer);
    contract.timestamps_enabled = true;
    let table = contract.table_def.as_mut().expect("typed table");
    table.columns[0].default = Some(b"5".to_vec());
    table.columns.extend([
        ColumnDef::new("created_at", DataType::UnsignedInteger).not_null(),
        ColumnDef::new("updated_at", DataType::UnsignedInteger).not_null(),
    ]);
    let enforcer = CollectionContractEnforcer::new(&contract);
    let inserted = enforcer
        .normalize_insert_at(vec![], 100)
        .expect("managed insert");
    assert_eq!(
        inserted,
        fields(&[
            ("value", Value::Integer(5)),
            ("created_at", Value::UnsignedInteger(100)),
            ("updated_at", Value::UnsignedInteger(100))
        ])
    );
    let updated = enforcer
        .normalize_update_at(inserted, 200)
        .expect("managed update");
    assert!(updated.contains(&("created_at".into(), Value::UnsignedInteger(100))));
    assert!(updated.contains(&("updated_at".into(), Value::UnsignedInteger(200))));
    let plan = enforcer.row_update_plan().expect("update plan");
    for name in ["created_at", "updated_at", "undeclared"] {
        assert!(normalize_row_update_assignment_with_plan(
            "orders",
            name,
            Value::Integer(1),
            Some(&plan)
        )
        .is_err());
    }
    assert_eq!(
        normalize_row_update_assignment_with_plan("orders", "value", Value::text("7"), Some(&plan))
            .expect("typed assignment"),
        Value::Integer(7)
    );
    assert!(enforcer
        .normalize_insert_at(fields(&[("created_at", Value::Integer(1))]), 100)
        .is_err());
}

fn unique_keys_and_targets() {
    let mut contract = contract();
    contract.declared_columns = vec![
        reddb_engine::catalog::codec::declared_column_contract_from_persisted(
            PhysicalDeclaredColumnContract {
                name: "id".into(),
                data_type: "integer".into(),
                not_null: true,
                primary_key: true,
                ..Default::default()
            },
        )
        .expect("primary key"),
    ];
    let email = PhysicalDeclaredColumnContract {
        name: "email".into(),
        data_type: "text".into(),
        unique: true,
        ..Default::default()
    };
    contract.declared_columns.push(
        reddb_engine::catalog::codec::declared_column_contract_from_persisted(email)
            .expect("unique email"),
    );
    let enforcer = CollectionContractEnforcer::new(&contract);
    let candidate = fields(&[
        ("id", Value::Integer(1)),
        ("email", Value::text("a@example.com")),
    ]);
    let existing = [ContractRow {
        id: EntityId::new(9),
        fields: candidate.iter().cloned().collect(),
    }];
    assert!(enforcer
        .enforce_uniqueness(&candidate, &existing, None)
        .is_err());
    assert!(enforcer
        .enforce_uniqueness(&candidate, &existing, Some(EntityId::new(9)))
        .is_ok());
    assert_eq!(
        find_row_uniqueness_conflict_for_contract(
            &contract,
            "orders",
            &candidate,
            &existing,
            Some(&["EMAIL".into()]),
            None
        )
        .expect("case-insensitive target")
        .expect("email conflict")
        .entity_id,
        EntityId::new(9)
    );
    assert!(find_row_uniqueness_conflict_for_contract(
        &contract,
        "orders",
        &candidate,
        &existing,
        Some(&["missing".into()]),
        None
    )
    .is_err());
    assert!(enforcer
        .enforce_batch_uniqueness(&[candidate.clone(), candidate])
        .is_err());
    assert!(
        enforcer
            .enforce_batch_uniqueness(&[
                fields(&[("id", Value::Integer(1)), ("email", Value::Null)]),
                fields(&[("id", Value::Integer(2)), ("email", Value::Null)]),
            ])
            .is_ok(),
        "nullable UNIQUE permits multiple NULLs"
    );
    assert!(enforcer
        .enforce_batch_uniqueness(&[fields(&[("id", Value::Null)])])
        .is_err());
}

fn models_and_vector_dimensions() {
    let mut contract = contract();
    contract.declared_model = CollectionModel::Document;
    assert!(matches!(
        CollectionContractEnforcer::new(&contract).ensure_model(CollectionModel::Graph),
        Err(ContractError::InvalidOperation(_))
    ));
    contract.origin = ContractOrigin::Implicit;
    contract.schema_mode = SchemaMode::Dynamic;
    assert!(CollectionContractEnforcer::new(&contract)
        .ensure_model(CollectionModel::Graph)
        .is_ok());
    contract.vector_dimension = Some(3);
    let enforcer = CollectionContractEnforcer::new(&contract);
    assert!(enforcer.ensure_vector_dimension(3).is_ok());
    assert!(matches!(
        enforcer.ensure_vector_dimension(2),
        Err(ContractError::Query(_))
    ));
}

fn ttl_overrides_and_bounds() {
    let mut contract = contract();
    for ttl in [0, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX] {
        contract.default_ttl_ms = Some(ttl);
        let enforcer = CollectionContractEnforcer::new(&contract);
        let mut metadata = vec![];
        enforcer.default_ttl_plan().apply(&mut metadata);
        assert_eq!(metadata[0].0, "_ttl_ms");
        assert_eq!(
            metadata[0].1,
            match i64::try_from(ttl) {
                Ok(value) => MetadataValue::Int(value),
                Err(_) => MetadataValue::Timestamp(ttl),
            }
        );
        for reserved in INTERNAL_TTL_METADATA_KEYS {
            let original = vec![(reserved.into(), MetadataValue::Int(5))];
            let mut explicit = original.clone();
            enforcer.default_ttl_plan().apply(&mut explicit);
            assert_eq!(explicit, original);
        }
    }
}
