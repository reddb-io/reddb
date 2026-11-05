//! Deterministic logical inputs for frozen native-codec golden bytes.
use reddb_engine::storage::entity::*;
use reddb_types::Value;
use std::collections::HashMap;

pub fn values() -> Vec<Value> {
    vec![
        Value::Null,
        Value::Boolean(true),
        Value::Integer(i64::MIN + 1),
        Value::UnsignedInteger(u64::MAX),
        Value::Float(-0.0),
        Value::text("ação 🦀"),
        Value::Blob(vec![0, 127, 255]),
        Value::Timestamp(-123456),
        Value::Duration(987654),
        Value::IpAddr("127.0.0.1".parse().expect("IPv4 fixture")),
        Value::IpAddr("2001:db8::1".parse().expect("IPv6 fixture")),
        Value::MacAddr([0, 1, 2, 3, 4, 255]),
        Value::Vector(vec![0.5, -2.0]),
        Value::Json(b"{\"n\":9007199254740993}".to_vec()),
        Value::Uuid([11; 16]),
        Value::NodeRef("node:a".into()),
        Value::EdgeRef("edge:a".into()),
        Value::VectorRef("vectors".into(), u64::MAX - 1),
        Value::RowRef("rows".into(), 42),
        Value::Color([1, 2, 3]),
        Value::Email("a@example.invalid".into()),
        Value::Url("https://example.invalid/a".into()),
        Value::Phone(5511999999999),
        Value::Semver(102030),
        Value::Cidr(0x0a000001, 24),
        Value::Date(-7),
        Value::Time(12345),
        Value::Decimal(-987654321),
        Value::EnumValue(255),
        Value::Array(vec![
            Value::Integer(7),
            Value::Array(vec![Value::Null, Value::text("nested")]),
        ]),
        Value::TimestampMs(-12345),
        Value::Ipv4(0x7f000001),
        Value::Ipv6([3; 16]),
        Value::Subnet(0x0a000001, 0xffffff00),
        Value::Port(65535),
        Value::Latitude(-123),
        Value::Longitude(456),
        Value::GeoPoint(-123, 456),
        Value::Country2(*b"BR"),
        Value::Country3(*b"BRA"),
        Value::Lang2(*b"pt"),
        Value::Lang5(*b"pt-BR"),
        Value::Currency(*b"BRL"),
        Value::ColorAlpha([1, 2, 3, 4]),
        Value::BigInt(i64::MAX),
        Value::KeyRef("settings".into(), "theme".into()),
        Value::DocRef("docs".into(), 9007199254740993),
        Value::TableRef("accounts".into()),
        Value::PageRef(u32::MAX),
        Value::Secret(vec![10, 20, 30]),
        Value::Password("fixture-hash".into()),
        Value::AssetCode("BTC".into()),
        Value::Money {
            asset_code: "BRL".into(),
            minor_units: -12345,
            scale: 2,
        },
        Value::DecimalText("12345678901234567890.000001".into()),
        Value::text("x".repeat(2047)),
        Value::text("x".repeat(2048)),
        Value::Blob(vec![7; 8192]),
    ]
}

pub fn entities() -> Vec<UnifiedEntity> {
    let properties: HashMap<String, Value> = [
        ("name".into(), Value::text("alice")),
        ("count".into(), Value::Integer(7)),
    ]
    .into_iter()
    .collect();
    let mut named = UnifiedEntity::table_row(EntityId::new(1025), "documents", 1, vec![]);
    let mut body = Vec::new();
    reddb_types::document_body_codec::encode(&[("answer", &Value::Integer(42))], &mut body)
        .expect("document fixture");
    named.data = EntityData::Row(RowData::with_names(
        vec![Value::Json(body), Value::text("fixture")],
        vec!["body".into(), "title".into()],
    ));
    let series = |id, interned| {
        UnifiedEntity::new(
            EntityId::new(id),
            EntityKind::TimeSeriesPoint(Box::new(TimeSeriesPointKind {
                series: "cpu".into(),
                metric: "load".into(),
            })),
            EntityData::TimeSeries(TimeSeriesData {
                metric: "load".into(),
                series_id: interned,
                timestamp_ns: 123456789,
                value: 1.25,
                tags: if interned.is_some() {
                    HashMap::new()
                } else {
                    HashMap::from([("host".into(), "a".into())])
                },
                fields: HashMap::from([("max".into(), Value::Integer(9))]),
            }),
        )
    };
    let mut vector = UnifiedEntity::vector(EntityId::new(1028), "vectors", vec![0.1, -0.2]);
    vector.data =
        EntityData::Vector(VectorData::new(vec![0.1, -0.2]).with_content("vector content"));
    let queue = UnifiedEntity::new(
        EntityId::new(1031),
        EntityKind::QueueMessage {
            queue: "jobs".into(),
            position: 13,
        },
        EntityData::QueueMessage(QueueMessageData {
            payload: Value::Array(vec![Value::text("job"), Value::Integer(42)]),
            priority: Some(-3),
            enqueued_at_ns: 1234567,
            attempts: 2,
            max_attempts: 7,
            acked: true,
        }),
    );
    let mut entities = vec![
        UnifiedEntity::table_row(EntityId::new(1024), "all_values", 1, values()),
        named,
        UnifiedEntity::graph_node(EntityId::new(1026), "people", "person", properties.clone()),
        UnifiedEntity::graph_edge(EntityId::new(1027), "knows", "a", "b", 1.5, properties),
        vector,
        series(1029, None),
        series(1030, Some(42)),
        queue,
    ];
    for entity in &mut entities {
        entity.created_at = 123456;
        entity.updated_at = 123499;
        entity.sequence_id = 99;
        entity.set_logical_id(EntityId::new(1000));
        entity.set_xmin(17);
        entity.set_xmax(23);
        entity.embeddings_mut().push(EmbeddingSlot {
            name: "content".into(),
            vector: vec![0.5, 1.0],
            model: "fixture".into(),
            dimension: 2,
            generated_at: 77,
        });
        let source = entity.id;
        entity.cross_refs_mut().push(CrossRef {
            source,
            target: EntityId::new(77),
            target_collection: "target".into(),
            ref_type: RefType::RowToNode,
            weight: 0.5,
            created_at: 123400,
        });
    }
    entities
}
