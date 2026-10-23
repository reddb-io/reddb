//! Identical native/WASM assertions over the original native record oracle.
use crate::record_corpus;
use reddb_engine::query::{records::*, unified::*};
use reddb_engine::storage::entity::*;
use reddb_types::{json, Value};
use std::sync::Arc;

pub const CASES: &[(&str, fn())] = &[
    ("record_native_materialization_goldens", native_goldens),
    ("record_schema_and_graph_representation", representation),
    ("record_projection_and_document_body", projection),
    ("record_typed_timeseries_and_identity", tags_and_identity),
];

fn fields(record: &UnifiedRecord) -> json::Value {
    let object = json::Value::Object(
        record
            .iter_fields()
            .map(|(key, value)| (key.to_string(), value.to_json()))
            .collect(),
    );
    // The oracle passed through the canonical JSON writer and reader. Apply
    // that same normalization to floats/decimal JSON representations here.
    json::from_slice(&json::to_vec(&object).expect("record JSON")).expect("record JSON parsed")
}

fn modes(entity: &UnifiedEntity) -> [(&'static str, Option<UnifiedRecord>); 8] {
    let columns = record_corpus::columns();
    [
        (
            "full_owned",
            runtime_table_record_from_entity(entity.clone()),
        ),
        (
            "full_borrowed",
            runtime_table_record_from_entity_ref(entity),
        ),
        ("lean_owned", runtime_table_record_lean(entity.clone())),
        ("lean_borrowed", runtime_table_record_lean_ref(entity)),
        (
            "projected_owned",
            runtime_table_record_from_entity_projected(entity.clone(), &columns),
        ),
        (
            "projected_borrowed",
            runtime_table_record_from_entity_ref_projected(entity, &columns),
        ),
        (
            "universal_owned",
            runtime_any_record_from_entity(entity.clone()),
        ),
        (
            "universal_borrowed",
            runtime_any_record_from_entity_ref(entity),
        ),
    ]
}

fn native_goldens() {
    let golden: json::Value = json::from_str(include_str!("../fixtures/native-records.json"))
        .expect("original native records");
    let golden = golden.as_array().expect("record corpus array");
    let entities = record_corpus::entities();
    assert_eq!(golden.len(), entities.len() * 8);
    for (index, entity) in entities.iter().enumerate() {
        for ((mode, record), expected) in modes(entity)
            .into_iter()
            .zip(&golden[index * 8..index * 8 + 8])
        {
            assert_eq!(expected["mode"].as_str(), Some(mode));
            assert_eq!(expected["entity"].as_u64(), Some(index as u64));
            assert_eq!(
                record.as_ref().map(fields).unwrap_or(json::Value::Null),
                expected["fields"],
                "entity {index}, mode {mode}"
            );
        }
    }
}

fn representation() {
    let schema = Arc::new(vec![Arc::from("n"), Arc::from("n")]);
    let mut record = UnifiedRecord::with_schema(schema, vec![Value::Integer(1), Value::Integer(2)]);
    assert_eq!(record.get("n"), Some(&Value::Integer(2)));
    record.set("n", Value::Integer(3));
    record.set("extra", Value::UnsignedInteger(u64::MAX));
    record.set_arc_if_absent(Arc::from("n"), Value::Null);
    assert_eq!(record.get("n"), Some(&Value::Integer(3)));
    assert_eq!(record.get("extra"), Some(&Value::UnsignedInteger(u64::MAX)));
    assert!(record.columnar_schema().is_some());
    let secret = Value::Secret(vec![1, 2, 3]);
    record.set("secret", secret.clone());
    assert_eq!(
        record.get("secret"),
        Some(&secret),
        "materialization supplies representation, not secret policy"
    );
    let node = MatchedNode {
        id: "node:a".into(),
        label: "Alice".into(),
        node_label: "person".into(),
        properties: Default::default(),
    };
    record.set_node("n", node);
    assert_eq!(
        record.get_node("n").expect("matched node").node_label,
        "person"
    );
    let edge = MatchedEdge::from_tuple("a", "knows", "b", 0.5);
    record.set_edge("e", edge.clone());
    let path = GraphPath::start("a").extend(edge, "b");
    assert_eq!(path.len(), 1);
    assert_eq!(path.total_weight, 0.5);
    let mut result = UnifiedResult::with_columns(vec!["n".into()]);
    result.push(record);
    assert_eq!(result.len(), 1);
    let mut stats = VectorQueryStats::default();
    stats.operators.push(VectorOperatorStats {
        operator: "scan".into(),
        input_rows: u64::MAX,
        output_rows: 1,
        inclusive_time_us: 7,
    });
    assert_eq!(
        stats
            .operators_json()
            .as_array()
            .expect("vector stats")
            .len(),
        1
    );
    let first = QueryStats {
        rows_scanned: 2,
        exec_time_us: 3,
        vector: Some(stats),
        ..Default::default()
    };
    let second = QueryStats {
        rows_scanned: 5,
        segments_scanned: 7,
        exec_time_us: 11,
        ..Default::default()
    };
    let merged = QueryStats::merge(&first, &second);
    assert_eq!(merged.rows_scanned, 7);
    assert_eq!(merged.segments_scanned, 7);
    assert_eq!(merged.exec_time_us, 14);
    assert!(merged.vector.is_none());
}

fn projection() {
    let entities = record_corpus::entities();
    let document = &entities[10];
    let projected = runtime_table_record_from_entity_ref_projected(document, &["answer".into()])
        .expect("projected document");
    assert_eq!(
        projected.get("answer"),
        Some(&Value::Integer(42)),
        "binary body overrides shadow field"
    );
    assert!(!projected.contains_column("nested"));
    let schema = Arc::new(vec![
        "answer".into(),
        "created_at".into(),
        "updated_at".into(),
        "tenant_id".into(),
    ]);
    let schema_row = &entities[9];
    let full =
        runtime_table_record_from_entity_ref_with_schema(schema_row, None).expect("schema row");
    assert_eq!(
        full.get("created_at"),
        Some(&Value::UnsignedInteger(1700000000123))
    );
    assert_eq!(full.get("tenant"), Some(&Value::text("org-a")));
    let mut unhydrated = schema_row.clone();
    if let EntityData::Row(row) = &mut unhydrated.data {
        row.schema = None;
    }
    let fallback = runtime_table_record_from_entity_ref_with_schema(&unhydrated, Some(&schema))
        .expect("catalog schema fallback");
    assert_eq!(fallback.get("answer"), Some(&Value::Integer(42)));
    let columns = vec!["answer".into()];
    let indexed = runtime_table_record_with_col_indices(&unhydrated, &columns, &[(0, 0)])
        .expect("persisted positional row");
    assert_eq!(indexed.get("answer"), Some(&Value::Integer(42)));
    assert!(runtime_table_record_with_col_indices(&unhydrated, &columns, &[]).is_none());
    let numeric_edge = runtime_any_record_from_entity_ref(&entities[13]).expect("edge");
    assert_eq!(
        numeric_edge.get("from_rid"),
        Some(&Value::UnsignedInteger(9007199254740993))
    );
}

fn tags_and_identity() {
    assert_eq!(
        decode_stored_tag_value("123"),
        json::Value::String("123".into())
    );
    assert_eq!(
        decode_stored_tag_value("\u{1}broken"),
        json::Value::String("broken".into())
    );
    let entities = record_corpus::entities();
    let series = runtime_any_record_from_entity_ref(&entities[5]).expect("timeseries");
    let Value::Json(tags) = series.get("tags").expect("typed tags") else {
        panic!("tags are JSON")
    };
    let tags: json::Value = json::from_slice(tags).expect("tags JSON");
    assert!(tags["typed"].as_array().is_some());
    assert_eq!(tags["plain"].as_str(), Some("123"));
    assert_eq!(series.get("max"), Some(&Value::Integer(9)));
    assert_eq!(
        series.get("rid"),
        Some(&Value::UnsignedInteger(9007199254740998))
    );
    let vector = runtime_any_record_from_entity_ref(&entities[4]).expect("vector");
    assert_eq!(
        vector.get("_source_node"),
        Some(&Value::NodeRef("target:e77".into()))
    );
    let queue = runtime_any_record_from_entity_ref(&entities[7]).expect("queue");
    assert_eq!(queue.get("acked"), Some(&Value::Boolean(true)));
    assert_eq!(queue.get("priority"), Some(&Value::Integer(-3)));
    assert_eq!(
        runtime_row_entity_type(entities[8].data.as_row().expect("KV row")),
        "kv"
    );
}
