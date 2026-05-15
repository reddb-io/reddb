use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::replication::cdc::ChangeOperation;
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
    set_current_tenant,
};
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

fn err_string(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(sql)
        .expect_err("query should fail")
        .to_string()
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
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected {field} unsigned integer field, got {other:?} in {record:?}"),
    }
}

fn text_field(record: &UnifiedRecord, field: &str) -> String {
    match record.get(field) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected {field} text field, got {other:?} in {record:?}"),
    }
}

fn read_event_payload(rt: &RedDBRuntime, queue: &str) -> serde_json::Value {
    let result = exec(
        rt,
        &format!("QUEUE READ {queue} GROUP evt_readers CONSUMER c1 COUNT 1"),
    );
    let record = result
        .result
        .records
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no event in queue {queue}"));
    match record.get("payload") {
        Some(Value::Json(bytes)) => {
            serde_json::from_slice(bytes).expect("event payload should be valid JSON")
        }
        other => panic!("expected Json payload, got {other:?}"),
    }
}

fn unique_db_path(prefix: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb-{prefix}-{}-{nanos}.rdb", std::process::id()))
}

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

fn attach_alice_policy(store: &AuthStore, id: &str, statements: &str) {
    let policy = format!(
        r#"{{
        "id":"{id}",
        "version":1,
        "statements":{statements}
    }}"#
    );
    store
        .put_policy(reddb::auth::policies::Policy::from_json_str(&policy).unwrap())
        .unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform("alice")),
            id,
        )
        .unwrap();
}

#[test]
fn explicit_targets_are_authorized_as_ordinary_updates() {
    let rt = runtime();
    let auth = Arc::new(AuthStore::new(AuthConfig::default()));
    auth.create_user("alice", "p", Role::Write).unwrap();
    rt.set_auth_store(Arc::clone(&auth));
    attach_alice_policy(
        &auth,
        "explicit-target-update-allow",
        r#"[
            {"effect":"allow","actions":["select","update"],"resources":["table:auth_rows"]},
            {"effect":"allow","actions":["select","update"],"resources":["table:auth_docs"]},
            {"effect":"allow","actions":["select","update"],"resources":["table:auth_kv"]},
            {"effect":"allow","actions":["select","update"],"resources":["table:auth_graph"]}
        ]"#,
    );

    exec(&rt, "CREATE TABLE auth_rows (id INT, score INT)");
    exec(&rt, "INSERT INTO auth_rows (id, score) VALUES (1, 10)");
    exec(&rt, "CREATE DOCUMENT auth_docs");
    exec(
        &rt,
        r#"INSERT INTO auth_docs DOCUMENT (body) VALUES ('{"name":"doc","score":10}')"#,
    );
    exec(&rt, "CREATE KV auth_kv");
    exec(
        &rt,
        "INSERT INTO auth_kv KV (key, value) VALUES ('counter', 10)",
    );
    let alice = exec(
        &rt,
        "INSERT INTO auth_graph NODE (label, node_type, score) VALUES ('alice', 'person', 10) RETURNING rid",
    );
    let alice_rid = uint_field(only_record(&alice), "rid");
    let bob = exec(
        &rt,
        "INSERT INTO auth_graph NODE (label, node_type) VALUES ('bob', 'person') RETURNING rid",
    );
    let bob_rid = uint_field(only_record(&bob), "rid");
    exec(
        &rt,
        &format!(
            "INSERT INTO auth_graph EDGE (label, from_rid, to_rid, weight) \
             VALUES ('knows', {alice_rid}, {bob_rid}, 1.0)"
        ),
    );

    as_user("alice", Role::Write, || {
        assert_eq!(
            exec(&rt, "UPDATE auth_rows ROWS SET score += 1 WHERE id = 1").affected_rows,
            1
        );
        assert_eq!(
            exec(
                &rt,
                "UPDATE auth_docs DOCUMENTS SET score += 1 WHERE name = 'doc'"
            )
            .affected_rows,
            1
        );
        assert_eq!(
            exec(
                &rt,
                "UPDATE auth_kv KV SET value += 1 WHERE key = 'counter'"
            )
            .affected_rows,
            1
        );
        assert_eq!(
            exec(
                &rt,
                "UPDATE auth_graph NODES SET score += 1 WHERE label = 'alice'"
            )
            .affected_rows,
            1
        );
        assert_eq!(
            exec(
                &rt,
                &format!("UPDATE auth_graph EDGES SET weight += 0.5 WHERE from_rid = {alice_rid}")
            )
            .affected_rows,
            1
        );
    });

    let denied = as_user("alice", Role::Write, || {
        err_string(&rt, "UPDATE auth_denied ROWS SET score = 1 WHERE id = 1")
    });
    assert!(denied.contains("table:auth_denied"), "{denied}");
}

#[test]
fn update_returning_obeys_rls_and_column_policy() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE tenant_accounts (id INT, tenant_id TEXT, score INT)",
    );
    exec(
        &rt,
        "INSERT INTO tenant_accounts (id, tenant_id, score) VALUES (1, 'acme', 10)",
    );
    exec(
        &rt,
        "INSERT INTO tenant_accounts (id, tenant_id, score) VALUES (2, 'globex', 20)",
    );
    exec(
        &rt,
        "CREATE POLICY tenant_update ON tenant_accounts FOR UPDATE USING (tenant_id = CURRENT_TENANT())",
    );
    exec(&rt, "ALTER TABLE tenant_accounts ENABLE ROW LEVEL SECURITY");

    let returned = {
        set_current_tenant("acme".to_string());
        let result = exec(
            &rt,
            "UPDATE tenant_accounts ROWS SET score += 1 RETURNING id, tenant_id, score",
        );
        clear_current_tenant();
        result
    };
    assert_eq!(returned.affected_rows, 1);
    let row = only_record(&returned);
    assert_eq!(int_field(row, "id"), 1);
    assert_eq!(text_field(row, "tenant_id"), "acme");
    assert_eq!(int_field(row, "score"), 11);

    let auth = Arc::new(AuthStore::new(AuthConfig::default()));
    auth.create_user("alice", "p", Role::Write).unwrap();
    rt.set_auth_store(Arc::clone(&auth));
    attach_alice_policy(
        &auth,
        "returning-secret-deny",
        r#"[
            {"effect":"allow","actions":["select","update"],"resources":["table:masked_accounts"]},
            {"effect":"deny","actions":["select"],"resources":["column:masked_accounts.secret"]}
        ]"#,
    );
    exec(
        &rt,
        "CREATE TABLE masked_accounts (id INT, status TEXT, secret TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO masked_accounts (id, status, secret) VALUES (1, 'old', 's1')",
    );

    let denied = as_user("alice", Role::Write, || {
        err_string(
            &rt,
            "UPDATE masked_accounts ROWS SET status = 'active' WHERE id = 1 RETURNING secret",
        )
    });
    assert!(denied.contains("column:masked_accounts.secret"), "{denied}");
    let unchanged = exec(&rt, "SELECT status FROM masked_accounts WHERE id = 1");
    assert_eq!(text_field(only_record(&unchanged), "status"), "old");
}

#[test]
fn explicit_document_update_emits_event_and_cdc_identity() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT conformance_docs");
    exec(
        &rt,
        "ALTER TABLE conformance_docs ADD SUBSCRIPTION update_events TO conformance_doc_events",
    );
    exec(&rt, "QUEUE GROUP CREATE conformance_doc_events evt_readers");
    let inserted = exec(
        &rt,
        r#"INSERT INTO conformance_docs DOCUMENT (body) VALUES ('{"name":"doc","score":10}') RETURNING rid"#,
    );
    let rid = uint_field(only_record(&inserted), "rid");
    let start_lsn = rt.cdc_current_lsn();

    exec(
        &rt,
        "UPDATE conformance_docs DOCUMENTS SET score += 5 WHERE name = 'doc'",
    );

    let payload = read_event_payload(&rt, "conformance_doc_events");
    assert_eq!(payload["op"].as_str(), Some("update"));
    assert_eq!(payload["collection"].as_str(), Some("conformance_docs"));
    assert_eq!(payload["rid"].as_u64(), Some(rid));
    assert_eq!(payload["kind"].as_str(), Some("document"));
    assert_eq!(payload["after"]["score"].as_i64(), Some(15));

    let events = rt.cdc_poll(start_lsn, 10);
    let update = events
        .iter()
        .find(|event| {
            event.collection == "conformance_docs" && event.operation == ChangeOperation::Update
        })
        .expect("document update CDC event");
    assert_eq!(update.rid(), rid);
    assert_eq!(update.collection, "conformance_docs");
    assert_eq!(update.kind(), "document");
}

#[test]
fn explicit_updates_recheck_changed_indexed_fields() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE indexed_rows (id INT, score INT)");
    exec(&rt, "INSERT INTO indexed_rows (id, score) VALUES (1, 10)");
    exec(
        &rt,
        "CREATE INDEX idx_indexed_rows_score ON indexed_rows (score) USING HASH",
    );
    exec(&rt, "UPDATE indexed_rows ROWS SET score = 15 WHERE id = 1");
    let row = exec(&rt, "SELECT id, score FROM indexed_rows WHERE score = 15");
    assert_eq!(int_field(only_record(&row), "id"), 1);

    exec(&rt, "CREATE DOCUMENT indexed_docs");
    exec(
        &rt,
        r#"INSERT INTO indexed_docs DOCUMENT (body) VALUES ('{"name":"doc","score":10}')"#,
    );
    exec(
        &rt,
        "CREATE INDEX idx_indexed_docs_score ON indexed_docs (score) USING HASH",
    );
    exec(
        &rt,
        "UPDATE indexed_docs DOCUMENTS SET score = 15 WHERE name = 'doc'",
    );
    let doc = exec(&rt, "SELECT name, score FROM indexed_docs WHERE score = 15");
    assert_eq!(text_field(only_record(&doc), "name"), "doc");
}

#[test]
fn explicit_multimodel_compound_updates_survive_reopen_as_post_images() {
    let path = unique_db_path("update-conformance-recovery");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("rdb-uwal"));
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("persistent runtime");
        exec(&rt, "CREATE DOCUMENT recovery_docs");
        exec(
            &rt,
            r#"INSERT INTO recovery_docs DOCUMENT (body) VALUES ('{"name":"doc","score":10}')"#,
        );
        exec(&rt, "CREATE KV recovery_kv");
        exec(
            &rt,
            "INSERT INTO recovery_kv KV (key, value) VALUES ('counter', 10)",
        );
        exec(
            &rt,
            "INSERT INTO recovery_graph NODE (label, node_type, score) VALUES ('node', 'item', 10)",
        );

        exec(
            &rt,
            "UPDATE recovery_docs DOCUMENTS SET score += 5 WHERE name = 'doc'",
        );
        exec(
            &rt,
            "UPDATE recovery_kv KV SET value += 7 WHERE key = 'counter'",
        );
        exec(
            &rt,
            "UPDATE recovery_graph NODES SET score += 2 WHERE label = 'node'",
        );
    }

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
        .expect("reopened persistent runtime");
    let doc = exec(&rt, "SELECT score FROM recovery_docs WHERE name = 'doc'");
    assert_eq!(int_field(only_record(&doc), "score"), 15);
    let kv = exec(&rt, "SELECT value FROM recovery_kv WHERE key = 'counter'");
    assert_eq!(int_field(only_record(&kv), "value"), 17);
    let node = exec(&rt, "SELECT score FROM recovery_graph WHERE label = 'node'");
    assert_eq!(int_field(only_record(&node), "score"), 12);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("rdb-uwal"));
}
