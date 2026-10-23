//! Inputs for metadata bytes frozen from the native writer before extraction.
use reddb_engine::storage::{
    entity::EntityId,
    metadata::{Metadata, MetadataValue as M, RefTarget},
};
use std::collections::HashMap;

pub fn metadata() -> Metadata {
    let targets = vec![
        RefTarget::TableRow {
            table: "rows".into(),
            row_id: u64::MAX,
        },
        RefTarget::Node {
            collection: "nodes".into(),
            node_id: EntityId::new(1024),
        },
        RefTarget::Edge {
            collection: "edges".into(),
            edge_id: EntityId::new(1025),
        },
        RefTarget::Vector {
            collection: "vectors".into(),
            vector_id: EntityId::new(1026),
        },
        RefTarget::Entity {
            collection: "entities".into(),
            entity_id: EntityId::new(1027),
        },
    ];
    let mut metadata = Metadata::new();
    for (key, value) in [
        ("null", M::Null),
        ("bool", M::Bool(true)),
        ("int", M::Int(i64::MIN)),
        ("float", M::Float(-0.0)),
        ("string", M::String("ação 🦀".into())),
        ("bytes", M::Bytes(vec![0, 127, 255])),
        (
            "array",
            M::Array(vec![M::Null, M::Array(vec![M::Bool(false)])]),
        ),
        (
            "object",
            M::Object(HashMap::from([
                ("z".into(), M::Int(42)),
                ("a".into(), M::String("first".into())),
            ])),
        ),
        ("timestamp", M::Timestamp(u64::MAX)),
        (
            "geo",
            M::Geo {
                lat: -23.5,
                lon: -46.625,
            },
        ),
        ("reference", M::Reference(targets[0].clone())),
        ("references", M::References(targets)),
    ] {
        metadata.set(key, value);
    }
    metadata
}
