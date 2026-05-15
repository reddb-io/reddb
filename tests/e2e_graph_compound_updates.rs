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
    assert_eq!(
        result.result.records.len(),
        1,
        "{:?}",
        result.result.records
    );
    &result.result.records[0]
}

fn int_field(record: &UnifiedRecord, field: &str) -> i64 {
    match record.get(field) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => i64::try_from(*value).expect("u64 should fit i64"),
        other => panic!("expected {field} integer field, got {other:?} in {record:?}"),
    }
}

fn uint_field(record: &UnifiedRecord, field: &str) -> u64 {
    match record.get(field) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => u64::try_from(*value).expect("i64 should fit u64"),
        other => panic!("expected {field} unsigned integer field, got {other:?} in {record:?}"),
    }
}

fn float_field(record: &UnifiedRecord, field: &str) -> f64 {
    match record.get(field) {
        Some(Value::Float(value)) => *value,
        other => panic!("expected {field} float field, got {other:?} in {record:?}"),
    }
}

fn text_field(record: &UnifiedRecord, field: &str) -> String {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref().to_string(),
        other => panic!("expected {field} text field, got {other:?} in {record:?}"),
    }
}

fn err_string(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(sql)
        .expect_err("query should fail")
        .to_string()
}

#[test]
fn node_compound_update_uses_graph_shape_where_and_post_image_returning() {
    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO graph_scores NODE (label, node_type, name, score) \
         VALUES ('alice', 'person', 'Alice', 10)",
    );
    exec(
        &rt,
        "INSERT INTO graph_scores NODE (label, node_type, name, score) \
         VALUES ('router', 'device', 'Router', 100)",
    );

    let updated = exec(
        &rt,
        "UPDATE graph_scores NODES SET score += 5 \
         WHERE node_type = 'person' RETURNING label, node_type, name, score",
    );

    assert_eq!(updated.affected_rows, 1);
    let returned = only_record(&updated);
    assert_eq!(text_field(returned, "label"), "alice");
    assert_eq!(text_field(returned, "node_type"), "person");
    assert_eq!(text_field(returned, "name"), "Alice");
    assert_eq!(int_field(returned, "score"), 15);

    let selected = exec(&rt, "SELECT score FROM graph_scores WHERE label = 'alice'");
    assert_eq!(int_field(only_record(&selected), "score"), 15);
    let untouched = exec(&rt, "SELECT score FROM graph_scores WHERE label = 'router'");
    assert_eq!(int_field(only_record(&untouched), "score"), 100);
}

#[test]
fn edge_weight_compound_update_uses_graph_shape_where_and_post_image_returning() {
    let rt = runtime();
    let alice = exec(
        &rt,
        "INSERT INTO graph_weights NODE (label, node_type) VALUES ('alice', 'person') RETURNING rid",
    );
    let alice_rid = uint_field(only_record(&alice), "rid");
    let bob = exec(
        &rt,
        "INSERT INTO graph_weights NODE (label, node_type) VALUES ('bob', 'person') RETURNING rid",
    );
    let bob_rid = uint_field(only_record(&bob), "rid");
    exec(
        &rt,
        &format!(
            "INSERT INTO graph_weights EDGE (label, from_rid, to_rid, weight, score) \
             VALUES ('knows', {alice_rid}, {bob_rid}, 1.5, 10)"
        ),
    );
    exec(
        &rt,
        &format!(
            "INSERT INTO graph_weights EDGE (label, from_rid, to_rid, weight, score) \
             VALUES ('ignores', {bob_rid}, {alice_rid}, 5.0, 99)"
        ),
    );

    let updated = exec(
        &rt,
        &format!(
            "UPDATE graph_weights EDGES SET weight += 0.25, score += 2 \
             WHERE from_rid = {alice_rid} RETURNING label, from_rid, to_rid, weight, score"
        ),
    );

    assert_eq!(updated.affected_rows, 1);
    let returned = only_record(&updated);
    assert_eq!(text_field(returned, "label"), "knows");
    assert_eq!(uint_field(returned, "from_rid"), alice_rid);
    assert_eq!(uint_field(returned, "to_rid"), bob_rid);
    assert_eq!(float_field(returned, "weight"), 1.75);
    assert_eq!(int_field(returned, "score"), 12);

    let selected = exec(
        &rt,
        &format!("SELECT weight, score FROM graph_weights WHERE to_rid = {bob_rid}"),
    );
    assert_eq!(float_field(only_record(&selected), "weight"), 1.75);
    assert_eq!(int_field(only_record(&selected), "score"), 12);
    let untouched = exec(
        &rt,
        &format!("SELECT weight, score FROM graph_weights WHERE to_rid = {alice_rid}"),
    );
    assert_eq!(float_field(only_record(&untouched), "weight"), 5.0);
    assert_eq!(int_field(only_record(&untouched), "score"), 99);
}

#[test]
fn node_type_update_changes_structural_graph_type() {
    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO graph_node_types NODE (label, node_type, score) \
         VALUES ('web-01', 'host', 10)",
    );

    let updated = exec(
        &rt,
        "UPDATE graph_node_types NODES SET node_type = 'service' \
         WHERE node_type = 'host' RETURNING label, node_type",
    );

    assert_eq!(updated.affected_rows, 1);
    let returned = only_record(&updated);
    assert_eq!(text_field(returned, "label"), "web-01");
    assert_eq!(text_field(returned, "node_type"), "service");

    let selected = exec(
        &rt,
        "SELECT label, node_type FROM graph_node_types WHERE node_type = 'service'",
    );
    assert_eq!(text_field(only_record(&selected), "node_type"), "service");
    let old_type = exec(
        &rt,
        "SELECT label FROM graph_node_types WHERE node_type = 'host'",
    );
    assert_eq!(old_type.result.records.len(), 0);
}

#[test]
fn graph_identity_and_topology_update_fields_are_rejected() {
    let rt = runtime();
    let alice = exec(
        &rt,
        "INSERT INTO graph_immutable NODE (label, node_type) \
         VALUES ('alice', 'person') RETURNING rid",
    );
    let alice_rid = uint_field(only_record(&alice), "rid");
    let bob = exec(
        &rt,
        "INSERT INTO graph_immutable NODE (label, node_type) \
         VALUES ('bob', 'person') RETURNING rid",
    );
    let bob_rid = uint_field(only_record(&bob), "rid");
    exec(
        &rt,
        &format!(
            "INSERT INTO graph_immutable EDGE (label, from_rid, to_rid, weight) \
             VALUES ('knows', {alice_rid}, {bob_rid}, 1.0)"
        ),
    );

    let statements = [
        "UPDATE graph_immutable NODES SET rid = 999 WHERE label = 'alice'".to_string(),
        "UPDATE graph_immutable NODES SET label = 'mallory' WHERE label = 'alice'".to_string(),
        "UPDATE graph_immutable EDGES SET from_rid = 999 WHERE label = 'knows'".to_string(),
        "UPDATE graph_immutable EDGES SET to_rid = 999 WHERE label = 'knows'".to_string(),
    ];

    for sql in statements {
        let err = err_string(&rt, &sql);
        assert!(
            err.contains("immutable graph field"),
            "unexpected error for {sql}: {err}"
        );
    }

    let node = exec(
        &rt,
        "SELECT label FROM graph_immutable WHERE label = 'alice'",
    );
    assert_eq!(text_field(only_record(&node), "label"), "alice");
    let edge = exec(
        &rt,
        "SELECT from_rid, to_rid FROM graph_immutable WHERE label = 'knows'",
    );
    assert_eq!(uint_field(only_record(&edge), "from_rid"), alice_rid);
    assert_eq!(uint_field(only_record(&edge), "to_rid"), bob_rid);
}

#[test]
fn graph_compound_failure_aborts_without_partial_write() {
    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO graph_atomic NODE (label, node_type, batch, score) \
         VALUES ('ok', 'person', 'same', 10)",
    );
    exec(
        &rt,
        "INSERT INTO graph_atomic NODE (label, node_type, batch, score) \
         VALUES ('bad', 'person', 'same', 'text')",
    );

    let err = err_string(
        &rt,
        "UPDATE graph_atomic NODES SET score += 1 WHERE batch = 'same'",
    );
    assert!(err.contains("numeric field 'score'"), "{err}");

    let ok = exec(&rt, "SELECT score FROM graph_atomic WHERE label = 'ok'");
    assert_eq!(int_field(only_record(&ok), "score"), 10);
    let bad = exec(&rt, "SELECT score FROM graph_atomic WHERE label = 'bad'");
    assert_eq!(text_field(only_record(&bad), "score"), "text");
}

#[test]
fn graph_compound_updates_respect_update_rls_policy() {
    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO graph_rls NODE (label, node_type, tenant_id, score) \
         VALUES ('a', 'person', 'acme', 10)",
    );
    exec(
        &rt,
        "INSERT INTO graph_rls NODE (label, node_type, tenant_id, score) \
         VALUES ('g', 'person', 'globex', 20)",
    );
    exec(
        &rt,
        "CREATE POLICY tenant_update ON graph_rls FOR UPDATE USING (tenant_id = CURRENT_TENANT())",
    );
    exec(&rt, "ALTER TABLE graph_rls ENABLE ROW LEVEL SECURITY");

    let updated = exec(
        &rt,
        "WITHIN TENANT 'acme' UPDATE graph_rls NODES SET score += 1 RETURNING label, score",
    );

    assert_eq!(updated.affected_rows, 1);
    assert_eq!(text_field(only_record(&updated), "label"), "a");
    assert_eq!(int_field(only_record(&updated), "score"), 11);
}
