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

fn json_field(record: &UnifiedRecord, field: &str) -> reddb::json::Value {
    match record.get(field) {
        Some(Value::Json(value)) => {
            reddb::json::from_slice(value).expect("json field should decode")
        }
        other => panic!("expected {field} json field, got {other:?} in {record:?}"),
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
fn unmarked_updates_resolve_row_document_and_kv_models_from_catalog() {
    // ADR 0067 (#1711): the DOCUMENTS/ROWS/KV markers are gone. An unmarked
    // UPDATE resolves the collection's model from the catalog — a table gets
    // row semantics, a document collection gets document semantics, a KV
    // collection gets KV semantics — without any marker.
    let rt = runtime();
    exec(&rt, "CREATE TABLE accounts (id INT, status TEXT)");
    exec(&rt, "INSERT INTO accounts (id, status) VALUES (1, 'new')");
    let rows = exec(&rt, "UPDATE accounts SET status = 'active' WHERE id = 1");
    assert_eq!(rows.affected_rows, 1);
    let selected_row = exec(&rt, "SELECT status FROM accounts WHERE id = 1");
    assert_eq!(text_field(only_record(&selected_row), "status"), "active");

    exec(&rt, "CREATE DOCUMENT docs");
    exec(
        &rt,
        r#"INSERT INTO docs DOCUMENT VALUES ({"name":"alpha","status":"draft"})"#,
    );
    let docs = exec(
        &rt,
        "UPDATE docs SET status = 'published' WHERE name = 'alpha'",
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
        "UPDATE settings SET value = 'on' WHERE key = 'feature'",
    );
    assert_eq!(kv.affected_rows, 1);
    let selected_kv = exec(&rt, "SELECT value FROM settings WHERE key = 'feature'");
    assert_eq!(text_field(only_record(&selected_kv), "value"), "on");
}

#[test]
fn document_update_keeps_body_and_promoted_columns_in_sync() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT docs_sync");
    exec(
        &rt,
        r#"INSERT INTO docs_sync DOCUMENT VALUES ({"name":"orig","score":1})"#,
    );

    let updated = exec(&rt, "UPDATE docs_sync SET score = 99 WHERE name = 'orig'");

    assert_eq!(updated.affected_rows, 1);
    let selected = exec(&rt, "SELECT body, score FROM docs_sync WHERE name = 'orig'");
    let record = only_record(&selected);
    let body = json_field(record, "body");
    assert_eq!(body["score"].as_i64(), Some(99));
    assert_eq!(uint_field(record, "score"), 99);
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

    let row_update = exec(&rt, "UPDATE flexible SET status = 'row-seen' WHERE id = 1");
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
fn removed_document_marker_is_rejected_and_graph_marker_still_gated_by_model() {
    // ADR 0067 (#1711): the DOCUMENTS/ROWS/KV markers are removed, so a
    // `DOCUMENTS` marker on any collection is a didactic parse rejection — it
    // no longer reaches the model-contract gate. NODES/EDGES survive, so a
    // graph marker on a non-graph collection still surfaces the model-contract
    // error before any mutation.
    let rt = runtime();
    exec(&rt, "CREATE TABLE accounts (id INT, status TEXT)");
    exec(&rt, "INSERT INTO accounts (id, status) VALUES (1, 'new')");

    let documents = err_string(
        &rt,
        "UPDATE accounts DOCUMENTS SET status = 'bad' WHERE id = 1",
    );
    assert!(
        documents.contains("has been removed"),
        "expected didactic marker-removal error, got: {documents}"
    );

    let nodes = err_string(&rt, "UPDATE accounts NODES SET status = 'bad' WHERE id = 1");
    assert!(nodes.contains("does not allow 'graph' updates"));

    let selected = exec(&rt, "SELECT status FROM accounts WHERE id = 1");
    assert_eq!(text_field(only_record(&selected), "status"), "new");
}

#[test]
fn dotted_set_target_is_rejected_off_a_document_collection() {
    // ADR 0067 (#1711): dotted assignment targets parse for every collection
    // but are legal only on a document collection; off-model the analyzer
    // rejects them before any mutation.
    let rt = runtime();
    exec(&rt, "CREATE TABLE ledger (id INT, status TEXT)");
    exec(&rt, "INSERT INTO ledger (id, status) VALUES (1, 'new')");

    let dotted = err_string(&rt, "UPDATE ledger SET meta.tier = 'gold' WHERE id = 1");
    assert!(
        dotted.contains("only valid on document collections"),
        "expected off-model dotted-target error, got: {dotted}"
    );

    let selected = exec(&rt, "SELECT status FROM ledger WHERE id = 1");
    assert_eq!(text_field(only_record(&selected), "status"), "new");
}
