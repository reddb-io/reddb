use reddb_file::{decode_table_def, encode_table_def};
use reddb_types::{
    ColumnDef, Constraint, ConstraintType, DataType, IndexDef, IndexType, TableDef,
};
use std::collections::HashMap;

fn fixture_corpus() -> Vec<(TableDef, &'static str)> {
    let empty = TableDef {
        name: "t".into(),
        version: 1,
        created_at: 0,
        updated_at: 0,
        columns: vec![],
        primary_key: vec![],
        indexes: vec![],
        constraints: vec![],
    };

    let complex = TableDef {
        name: "embeddings".into(),
        version: 1,
        created_at: 1_700_000_000,
        updated_at: 1_700_000_500,
        columns: vec![
            ColumnDef {
                name: "id".into(),
                data_type: DataType::UnsignedInteger,
                nullable: false,
                default: None,
                vector_dim: None,
                compress: false,
                enum_variants: vec![],
                decimal_precision: 4,
                element_type: None,
                metadata: HashMap::new(),
            },
            ColumnDef {
                name: "embedding".into(),
                data_type: DataType::Vector,
                nullable: false,
                default: Some(vec![1, 2, 3]),
                vector_dim: Some(384),
                compress: true,
                enum_variants: vec!["a".into(), "b".into()],
                decimal_precision: 6,
                element_type: Some(DataType::Float),
                metadata: HashMap::from([("unit".into(), "f32".into())]),
            },
        ],
        primary_key: vec!["id".into()],
        indexes: vec![IndexDef {
            name: "idx_vec".into(),
            index_type: IndexType::Hnsw,
            unique: false,
            columns: vec!["embedding".into()],
        }],
        constraints: vec![
            Constraint {
                name: "fk".into(),
                constraint_type: ConstraintType::ForeignKey,
                columns: vec!["id".into()],
                ref_table: Some("other".into()),
                ref_columns: Some(vec!["oid".into()]),
            },
            Constraint {
                name: "nn".into(),
                constraint_type: ConstraintType::NotNull,
                columns: vec!["id".into()],
                ref_table: None,
                ref_columns: None,
            },
        ],
    };

    vec![
        (
            empty,
            "5254424c0100000001740000000000000000000000000000000000000000",
        ),
        (
            complex,
            "5254424c010000000a656d62656464696e677300f1536500000000f4f25365000000000202696402000000000004000009656d62656464696e670b00010301020301800100000102016101620601030104756e6974036633320102696401076964785f76656304000109656d62656464696e670202666b030102696401056f7468657201036f6964026e6e050102696400",
        ),
    ]
}

#[test]
fn fixture_corpus_is_byte_identical_to_legacy_codec() {
    for (table, expected_hex) in fixture_corpus() {
        assert_eq!(hex::encode(encode_table_def(&table)), expected_hex);
    }
}

#[test]
fn keystone_table_defs_round_trip() {
    for (table, _) in fixture_corpus() {
        let encoded = encode_table_def(&table);
        assert_eq!(decode_table_def(&encoded).unwrap(), table);
    }
}
