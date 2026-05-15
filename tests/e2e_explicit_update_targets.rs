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

fn text_field(record: &UnifiedRecord, field: &str) -> String {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref().to_string(),
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

fn err_string(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(sql)
        .expect_err("query should fail")
        .to_string()
}

#[test]
fn explicit_rows_documents_and_kv_targets_update_compatible_collections() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE accounts (id INT, status TEXT)");
    exec(&rt, "INSERT INTO accounts (id, status) VALUES (1, 'new')");
    let rows = exec(
        &rt,
        "UPDATE accounts ROWS SET status = 'active' WHERE id = 1",
    );
    assert_eq!(rows.affected_rows, 1);
    let selected_row = exec(&rt, "SELECT status FROM accounts WHERE id = 1");
    assert_eq!(text_field(only_record(&selected_row), "status"), "active");

    exec(&rt, "CREATE DOCUMENT docs");
    exec(
        &rt,
        r#"INSERT INTO docs DOCUMENT (body) VALUES ('{"name":"alpha","status":"draft"}')"#,
    );
    let docs = exec(
        &rt,
        "UPDATE docs DOCUMENTS SET status = 'published' WHERE name = 'alpha'",
    );
    assert_eq!(docs.affected_rows, 1);
    let selected_doc = exec(&rt, "SELECT status FROM docs WHERE name = 'alpha'");
    assert_eq!(
        text_field(only_record(&selected_doc), "status"),
        "published"
    );

    exec(&rt, "CREATE KV settings");
    exec(
        &rt,
        "INSERT INTO settings KV (key, value) VALUES ('feature', 'off')",
    );
    let kv = exec(
        &rt,
        "UPDATE settings KV SET value = 'on' WHERE key = 'feature'",
    );
    assert_eq!(kv.affected_rows, 1);
    let selected_kv = exec(&rt, "SELECT value FROM settings WHERE key = 'feature'");
    assert_eq!(text_field(only_record(&selected_kv), "value"), "on");
}

#[test]
fn explicit_nodes_and_edges_targets_update_graph_collections_independently() {
    let rt = runtime();
    let alice = exec(
        &rt,
        "INSERT INTO social NODE (label, name, status) VALUES ('user', 'Alice', 'new') RETURNING *",
    );
    let alice_rid = uint_field(only_record(&alice), "rid");
    let bob = exec(
        &rt,
        "INSERT INTO social NODE (label, name, status) VALUES ('user', 'Bob', 'new') RETURNING *",
    );
    let bob_rid = uint_field(only_record(&bob), "rid");
    exec(
        &rt,
        &format!(
            "INSERT INTO social EDGE (label, from_rid, to_rid, status) \
             VALUES ('knows', {alice_rid}, {bob_rid}, 'new')"
        ),
    );

    let nodes = exec(
        &rt,
        "UPDATE social NODES SET status = 'seen' WHERE name = 'Alice'",
    );
    assert_eq!(nodes.affected_rows, 1);
    let selected_node = exec(&rt, "SELECT status FROM social WHERE name = 'Alice'");
    assert_eq!(text_field(only_record(&selected_node), "status"), "seen");

    let edges = exec(
        &rt,
        &format!("UPDATE social EDGES SET status = 'linked' WHERE from_rid = {alice_rid}"),
    );
    assert_eq!(edges.affected_rows, 1);
    let selected_edge = exec(
        &rt,
        &format!("SELECT status FROM social WHERE from_rid = {alice_rid}"),
    );
    assert_eq!(text_field(only_record(&selected_edge), "status"), "linked");
}

#[test]
fn implicit_dynamic_collections_accept_supported_explicit_targets() {
    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO flexible (id, status) VALUES (1, 'row-new')",
    );
    let node = exec(
        &rt,
        "INSERT INTO flexible NODE (label, name, status) VALUES ('user', 'Ada', 'node-new') RETURNING *",
    );
    let node_rid = uint_field(only_record(&node), "rid");

    let row_update = exec(
        &rt,
        "UPDATE flexible ROWS SET status = 'row-seen' WHERE id = 1",
    );
    assert_eq!(row_update.affected_rows, 1);
    let selected_row = exec(&rt, "SELECT status FROM flexible WHERE id = 1");
    assert_eq!(text_field(only_record(&selected_row), "status"), "row-seen");

    let node_update = exec(
        &rt,
        &format!("UPDATE flexible NODES SET status = 'node-seen' WHERE rid = {node_rid}"),
    );
    assert_eq!(node_update.affected_rows, 1);
    let selected_node = exec(&rt, "SELECT status FROM flexible WHERE name = 'Ada'");
    assert_eq!(
        text_field(only_record(&selected_node), "status"),
        "node-seen"
    );
}

#[test]
fn explicit_update_targets_reject_incompatible_collection_models_before_mutation() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE accounts (id INT, status TEXT)");
    exec(&rt, "INSERT INTO accounts (id, status) VALUES (1, 'new')");

    let documents = err_string(
        &rt,
        "UPDATE accounts DOCUMENTS SET status = 'bad' WHERE id = 1",
    );
    assert!(documents.contains("does not allow 'document' updates"));

    let nodes = err_string(&rt, "UPDATE accounts NODES SET status = 'bad' WHERE id = 1");
    assert!(nodes.contains("does not allow 'graph' updates"));

    let selected = exec(&rt, "SELECT status FROM accounts WHERE id = 1");
    assert_eq!(text_field(only_record(&selected), "status"), "new");
}
