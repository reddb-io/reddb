use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn only_record(result: &reddb::runtime::RuntimeQueryResult) -> &UnifiedRecord {
    assert_eq!(result.result.records.len(), 1, "expected one row");
    &result.result.records[0]
}

fn text_field<'a>(record: &'a UnifiedRecord, field: &str) -> &'a str {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref(),
        other => panic!("expected {field} text field, got {other:?} in {record:?}"),
    }
}

fn uint_field(record: &UnifiedRecord, field: &str) -> u64 {
    match record.get(field) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected {field} unsigned integer field, got {other:?} in {record:?}"),
    }
}

fn assert_public_graph_envelope(record: &UnifiedRecord, collection: &str, kind: &str) -> u64 {
    let rid = uint_field(record, "rid");
    assert_eq!(text_field(record, "collection"), collection);
    assert_eq!(text_field(record, "kind"), kind);
    assert_eq!(record.get("tenant"), Some(&Value::Null));
    assert!(record.get("created_at").is_some(), "missing created_at");
    assert!(record.get("updated_at").is_some(), "missing updated_at");
    assert!(
        record.get("red_entity_id").is_none(),
        "public graph envelope should not expose red_entity_id: {record:?}"
    );
    rid
}

#[test]
fn graph_node_and_edge_returning_and_reads_use_public_envelope() {
    let rt = runtime();

    let alice = exec(
        &rt,
        "INSERT INTO social NODE (label, name) VALUES ('alice', 'Alice') RETURNING *",
    );
    let alice_rid = assert_public_graph_envelope(only_record(&alice), "social", "node");
    assert_eq!(text_field(only_record(&alice), "label"), "alice");

    let bob = exec(
        &rt,
        "INSERT INTO social NODE (label, name) VALUES ('bob', 'Bob') RETURNING *",
    );
    let bob_rid = assert_public_graph_envelope(only_record(&bob), "social", "node");

    let edge = exec(
        &rt,
        "INSERT INTO social EDGE (label, from_rid, to_rid, weight) \
         VALUES ('knows', 'alice', 'bob', 1.0) RETURNING *",
    );
    let edge_record = only_record(&edge);
    assert_public_graph_envelope(edge_record, "social", "edge");
    assert_eq!(uint_field(edge_record, "from_rid"), alice_rid);
    assert_eq!(uint_field(edge_record, "to_rid"), bob_rid);
    assert!(edge_record.get("from").is_none(), "edge exposed from");
    assert!(edge_record.get("to").is_none(), "edge exposed to");

    let selected_node = exec(&rt, "SELECT * FROM social WHERE label = 'alice'");
    assert_eq!(
        assert_public_graph_envelope(only_record(&selected_node), "social", "node"),
        alice_rid
    );

    let selected_edge = exec(
        &rt,
        &format!("SELECT * FROM social WHERE from_rid = {alice_rid}"),
    );
    let selected_edge_record = only_record(&selected_edge);
    assert_public_graph_envelope(selected_edge_record, "social", "edge");
    assert_eq!(uint_field(selected_edge_record, "from_rid"), alice_rid);
    assert_eq!(uint_field(selected_edge_record, "to_rid"), bob_rid);
    assert!(
        selected_edge_record.get("from").is_none(),
        "edge exposed from"
    );
    assert!(selected_edge_record.get("to").is_none(), "edge exposed to");
}

#[test]
fn graph_identity_and_topology_fields_are_immutable_in_sql_update() {
    let rt = runtime();
    let alice = exec(
        &rt,
        "INSERT INTO immutable_graph NODE (label, name) VALUES ('alice', 'Alice') RETURNING *",
    );
    let alice_rid = assert_public_graph_envelope(only_record(&alice), "immutable_graph", "node");
    exec(
        &rt,
        "INSERT INTO immutable_graph NODE (label, name) VALUES ('bob', 'Bob')",
    );
    exec(
        &rt,
        "INSERT INTO immutable_graph EDGE (label, from_rid, to_rid) \
         VALUES ('knows', 'alice', 'bob')",
    );

    let statements = vec![
        "UPDATE immutable_graph NODES SET rid = 999 WHERE label = 'alice'".to_string(),
        format!("UPDATE immutable_graph NODES SET label = 'mallory' WHERE rid = {alice_rid}"),
        "UPDATE immutable_graph EDGES SET from_rid = 999 WHERE label = 'knows'".to_string(),
        "UPDATE immutable_graph EDGES SET to_rid = 999 WHERE label = 'knows'".to_string(),
    ];

    for sql in statements {
        let err = match rt.execute_query(&sql) {
            Ok(_) => panic!("{sql} should reject immutable graph fields"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("immutable graph field"),
            "unexpected error for {sql}: {err}"
        );
    }

    let selected = exec(&rt, "SELECT * FROM immutable_graph WHERE label = 'alice'");
    assert_eq!(text_field(only_record(&selected), "label"), "alice");
}
