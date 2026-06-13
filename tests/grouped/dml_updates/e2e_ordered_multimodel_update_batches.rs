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

fn text_field(record: &UnifiedRecord, field: &str) -> String {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref().to_string(),
        other => panic!("expected {field} text field, got {other:?} in {record:?}"),
    }
}

fn uint_field(record: &UnifiedRecord, field: &str) -> u64 {
    match record.get(field) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => u64::try_from(*value).expect("i64 should fit u64"),
        other => panic!("expected {field} unsigned integer field, got {other:?} in {record:?}"),
    }
}

fn selected_texts(rt: &RedDBRuntime, collection: &str, field: &str) -> Vec<String> {
    exec(
        rt,
        &format!("SELECT {field} FROM {collection} WHERE touched = 1 ORDER BY {field} ASC"),
    )
    .result
    .records
    .iter()
    .map(|record| text_field(record, field))
    .collect()
}

fn err_string(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(sql)
        .expect_err("query should fail")
        .to_string()
}

#[test]
fn document_update_order_by_limit_uses_top_level_document_fields() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT ordered_docs");
    exec(
        &rt,
        r#"INSERT INTO ordered_docs DOCUMENT (body) VALUES ('{"name":"zeta","score":10,"touched":0}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO ordered_docs DOCUMENT (body) VALUES ('{"name":"alpha","score":30,"touched":0}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO ordered_docs DOCUMENT (body) VALUES ('{"name":"beta","score":20,"touched":0}')"#,
    );

    let updated = exec(
        &rt,
        "UPDATE ordered_docs DOCUMENTS SET touched = 1 ORDER BY score ASC LIMIT 2",
    );

    assert_eq!(updated.affected_rows, 2);
    assert_eq!(
        selected_texts(&rt, "ordered_docs", "name"),
        vec!["beta", "zeta"]
    );
}

#[test]
fn kv_update_order_by_limit_uses_key_value_fields() {
    let rt = runtime();
    exec(&rt, "CREATE KV ordered_kv");
    exec(
        &rt,
        "INSERT INTO ordered_kv KV (key, value) VALUES ('a', 1)",
    );
    exec(
        &rt,
        "INSERT INTO ordered_kv KV (key, value) VALUES ('c', 3)",
    );
    exec(
        &rt,
        "INSERT INTO ordered_kv KV (key, value) VALUES ('b', 2)",
    );

    let updated = exec(
        &rt,
        "UPDATE ordered_kv KV SET value += 100 ORDER BY value DESC LIMIT 2",
    );

    assert_eq!(updated.affected_rows, 2);
    let selected: Vec<String> = exec(
        &rt,
        "SELECT key FROM ordered_kv WHERE value > 100 ORDER BY key ASC",
    )
    .result
    .records
    .iter()
    .map(|record| text_field(record, "key"))
    .collect();
    assert_eq!(selected, vec!["b", "c"]);
}

#[test]
fn node_update_order_by_limit_uses_graph_properties_before_rid_tie_break() {
    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO ordered_graph_nodes NODE (label, node_type, score, touched) \
         VALUES ('alice', 'person', 10, 0)",
    );
    exec(
        &rt,
        "INSERT INTO ordered_graph_nodes NODE (label, node_type, score, touched) \
         VALUES ('bob', 'person', 30, 0)",
    );
    exec(
        &rt,
        "INSERT INTO ordered_graph_nodes NODE (label, node_type, score, touched) \
         VALUES ('cara', 'person', 20, 0)",
    );

    let updated = exec(
        &rt,
        "UPDATE ordered_graph_nodes NODES SET touched = 1 ORDER BY score DESC LIMIT 2",
    );

    assert_eq!(updated.affected_rows, 2);
    assert_eq!(
        selected_texts(&rt, "ordered_graph_nodes", "label"),
        vec!["bob", "cara"]
    );
}

#[test]
fn edge_update_order_by_limit_uses_graph_edge_properties() {
    let rt = runtime();
    let alice = exec(
        &rt,
        "INSERT INTO ordered_graph_edges NODE (label, node_type) \
         VALUES ('alice', 'person') RETURNING rid",
    );
    let alice_rid = uint_field(&alice.result.records[0], "rid");
    let bob = exec(
        &rt,
        "INSERT INTO ordered_graph_edges NODE (label, node_type) \
         VALUES ('bob', 'person') RETURNING rid",
    );
    let bob_rid = uint_field(&bob.result.records[0], "rid");
    for (label, score) in [("high", 30), ("low", 10), ("mid", 20)] {
        exec(
            &rt,
            &format!(
                "INSERT INTO ordered_graph_edges EDGE (label, from_rid, to_rid, score, touched) \
                 VALUES ('{label}', {alice_rid}, {bob_rid}, {score}, 0)"
            ),
        );
    }

    let updated = exec(
        &rt,
        "UPDATE ordered_graph_edges EDGES SET touched = 1 ORDER BY score ASC LIMIT 2",
    );

    assert_eq!(updated.affected_rows, 2);
    assert_eq!(
        selected_texts(&rt, "ordered_graph_edges", "label"),
        vec!["low", "mid"]
    );
}

#[test]
fn ordered_non_row_updates_require_limit_and_top_level_fields() {
    let rt = runtime();

    for (target, collection) in [
        ("DOCUMENTS", "ordered_docs"),
        ("KV", "ordered_kv"),
        ("NODES", "ordered_graph_nodes"),
        ("EDGES", "ordered_graph_edges"),
    ] {
        let without_limit = err_string(
            &rt,
            &format!("UPDATE {collection} {target} SET touched = 1 ORDER BY score"),
        );
        assert!(
            without_limit.contains("ORDER BY requires LIMIT"),
            "{target}: {without_limit}"
        );

        let expression = err_string(
            &rt,
            &format!("UPDATE {collection} {target} SET touched = 1 ORDER BY score + 1 LIMIT 1"),
        );
        assert!(
            expression.contains("top-level fields"),
            "{target}: {expression}"
        );

        let nested = err_string(
            &rt,
            &format!("UPDATE {collection} {target} SET touched = 1 ORDER BY body.score LIMIT 1"),
        );
        assert!(nested.contains("top-level fields"), "{target}: {nested}");
    }
}

#[test]
fn document_update_order_by_limit_breaks_ties_by_implicit_rid_asc() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT ordered_doc_ties");
    exec(
        &rt,
        r#"INSERT INTO ordered_doc_ties DOCUMENT (body) VALUES ('{"name":"first","score":7,"touched":0}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO ordered_doc_ties DOCUMENT (body) VALUES ('{"name":"second","score":7,"touched":0}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO ordered_doc_ties DOCUMENT (body) VALUES ('{"name":"third","score":7,"touched":0}')"#,
    );

    let updated = exec(
        &rt,
        "UPDATE ordered_doc_ties DOCUMENTS SET touched = 1 ORDER BY score ASC LIMIT 2",
    );

    assert_eq!(updated.affected_rows, 2);
    assert_eq!(
        selected_texts(&rt, "ordered_doc_ties", "name"),
        vec!["first", "second"]
    );
}
