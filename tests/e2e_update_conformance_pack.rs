#[allow(dead_code)]
mod support;

use std::sync::Arc;

use reddb::auth::enforcement_mode::PolicyEnforcementMode;
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
    auth.set_enforcement_mode(PolicyEnforcementMode::PolicyOnly);
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
        r#"INSERT INTO auth_docs DOCUMENT VALUES ({"name":"doc","score":10})"#,
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
            exec(&rt, "UPDATE auth_rows SET score += 1 WHERE id = 1").affected_rows,
            1
        );
        assert_eq!(
            exec(&rt, "UPDATE auth_docs SET score += 1 WHERE name = 'doc'").affected_rows,
            1
        );
        assert_eq!(
            exec(&rt, "UPDATE auth_kv SET value += 1 WHERE key = 'counter'").affected_rows,
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
        err_string(&rt, "UPDATE auth_denied SET score = 1 WHERE id = 1")
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
            "UPDATE tenant_accounts SET score += 1 RETURNING id, tenant_id, score",
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
            "UPDATE masked_accounts SET status = 'active' WHERE id = 1 RETURNING secret",
        )
    });
    assert!(denied.contains("column:masked_accounts.secret"), "{denied}");
    let unchanged = exec(&rt, "SELECT status FROM masked_accounts WHERE id = 1");
    assert_eq!(text_field(only_record(&unchanged), "status"), "old");
}

#[test]
fn claim_candidate_selection_requires_read_visibility() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE claim_read_tasks (id INT, tenant_id TEXT, rank INT, status TEXT)",
    );
    // ADR 0063: a concurrent CLAIM must order candidates through a compatible
    // index; `ORDER BY rank` requires an index on `rank`.
    exec(
        &rt,
        "CREATE INDEX idx_claim_read_rank ON claim_read_tasks (rank)",
    );
    exec(
        &rt,
        "INSERT INTO claim_read_tasks (id, tenant_id, rank, status) VALUES \
         (1, 'globex', 10, 'ready'), (2, 'acme', 20, 'ready')",
    );
    exec(
        &rt,
        "CREATE POLICY claim_read_visible ON claim_read_tasks FOR SELECT \
         USING (tenant_id = CURRENT_TENANT())",
    );
    exec(
        &rt,
        "CREATE POLICY claim_read_update_all ON claim_read_tasks FOR UPDATE USING (status = 'ready')",
    );
    exec(
        &rt,
        "ALTER TABLE claim_read_tasks ENABLE ROW LEVEL SECURITY",
    );

    let claimed = {
        set_current_tenant("acme".to_string());
        let result = exec(
            &rt,
            "UPDATE claim_read_tasks SET status = 'claimed' WHERE status = 'ready' \
             CLAIM LIMIT 2 ORDER BY rank ASC RETURNING id, tenant_id",
        );
        clear_current_tenant();
        result
    };

    assert_eq!(claimed.affected_rows, 1);
    let row = only_record(&claimed);
    assert_eq!(int_field(row, "id"), 2);
    assert_eq!(text_field(row, "tenant_id"), "acme");
    set_current_tenant("globex".to_string());
    let hidden = exec(&rt, "SELECT status FROM claim_read_tasks WHERE id = 1");
    clear_current_tenant();
    assert_eq!(text_field(only_record(&hidden), "status"), "ready");
}

#[test]
fn claim_exact_miss_counts_only_read_visible_candidates() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE claim_exact_read_tasks (id INT, tenant_id TEXT, rank INT, status TEXT)",
    );
    // ADR 0063: index-backed claim ordering on `rank`.
    exec(
        &rt,
        "CREATE INDEX idx_claim_exact_read_rank ON claim_exact_read_tasks (rank)",
    );
    exec(
        &rt,
        "INSERT INTO claim_exact_read_tasks (id, tenant_id, rank, status) VALUES \
         (1, 'globex', 10, 'ready'), (2, 'acme', 20, 'ready')",
    );
    exec(
        &rt,
        "CREATE POLICY claim_exact_read_visible ON claim_exact_read_tasks FOR SELECT \
         USING (tenant_id = CURRENT_TENANT())",
    );
    exec(
        &rt,
        "CREATE POLICY claim_exact_update_all ON claim_exact_read_tasks FOR UPDATE USING (status = 'ready')",
    );
    exec(
        &rt,
        "ALTER TABLE claim_exact_read_tasks ENABLE ROW LEVEL SECURITY",
    );

    let claimed = {
        set_current_tenant("acme".to_string());
        let result = exec(
            &rt,
            "UPDATE claim_exact_read_tasks SET status = 'claimed' WHERE status = 'ready' \
             CLAIM EXACT 2 ORDER BY rank ASC RETURNING id, tenant_id",
        );
        clear_current_tenant();
        result
    };

    assert_eq!(claimed.affected_rows, 0);
    assert!(claimed.result.records.is_empty());
    set_current_tenant("acme".to_string());
    let acme = exec(
        &rt,
        "SELECT status FROM claim_exact_read_tasks WHERE id = 2",
    );
    clear_current_tenant();
    assert_eq!(text_field(only_record(&acme), "status"), "ready");
    set_current_tenant("globex".to_string());
    let globex = exec(
        &rt,
        "SELECT status FROM claim_exact_read_tasks WHERE id = 1",
    );
    clear_current_tenant();
    assert_eq!(text_field(only_record(&globex), "status"), "ready");
}

#[test]
fn claim_state_transition_requires_update_policy() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE claim_update_tasks (id INT, tenant_id TEXT, rank INT, status TEXT)",
    );
    // ADR 0063: index-backed claim ordering on `rank`.
    exec(
        &rt,
        "CREATE INDEX idx_claim_update_rank ON claim_update_tasks (rank)",
    );
    exec(
        &rt,
        "INSERT INTO claim_update_tasks (id, tenant_id, rank, status) VALUES \
         (1, 'globex', 10, 'ready'), (2, 'acme', 20, 'ready')",
    );
    exec(
        &rt,
        "CREATE POLICY claim_update_read_all ON claim_update_tasks FOR SELECT USING (status = 'ready')",
    );
    exec(
        &rt,
        "CREATE POLICY claim_update_mutable ON claim_update_tasks FOR UPDATE \
         USING (tenant_id = CURRENT_TENANT())",
    );
    exec(
        &rt,
        "ALTER TABLE claim_update_tasks ENABLE ROW LEVEL SECURITY",
    );

    let claimed = {
        set_current_tenant("acme".to_string());
        let result = exec(
            &rt,
            "UPDATE claim_update_tasks SET status = 'claimed' WHERE status = 'ready' \
             CLAIM LIMIT 2 ORDER BY rank ASC RETURNING id, tenant_id",
        );
        clear_current_tenant();
        result
    };

    assert_eq!(claimed.affected_rows, 1);
    let row = only_record(&claimed);
    assert_eq!(int_field(row, "id"), 2);
    assert_eq!(text_field(row, "tenant_id"), "acme");
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
        r#"INSERT INTO conformance_docs DOCUMENT VALUES ({"name":"doc","score":10}) RETURNING rid"#,
    );
    let rid = uint_field(only_record(&inserted), "rid");
    let start_lsn = rt.cdc_current_lsn();

    exec(
        &rt,
        "UPDATE conformance_docs SET score += 5 WHERE name = 'doc'",
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

fn json_field(record: &UnifiedRecord, field: &str) -> serde_json::Value {
    match record.get(field) {
        Some(Value::Json(bytes)) => {
            serde_json::from_slice(bytes).expect("body field should be valid JSON")
        }
        other => panic!("expected {field} json field, got {other:?} in {record:?}"),
    }
}

#[test]
fn document_update_keeps_body_json_in_sync_with_promoted_column() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT body_sync_docs");
    exec(
        &rt,
        r#"INSERT INTO body_sync_docs DOCUMENT VALUES ({"name":"doc","score":10,"keep":"me"})"#,
    );

    exec(
        &rt,
        "UPDATE body_sync_docs SET score = 42 WHERE name = 'doc'",
    );

    // The promoted top-level column reflects the new value.
    let promoted = exec(&rt, "SELECT score FROM body_sync_docs WHERE name = 'doc'");
    assert_eq!(int_field(only_record(&promoted), "score"), 42);

    // The full document body JSON must be re-serialized so it agrees with the
    // promoted column. Other fields must be preserved untouched.
    let with_body = exec(&rt, "SELECT body FROM body_sync_docs WHERE name = 'doc'");
    let body = json_field(only_record(&with_body), "body");
    assert_eq!(
        body["score"].as_i64(),
        Some(42),
        "body JSON should reflect the updated promoted column, got {body:?}"
    );
    assert_eq!(body["name"].as_str(), Some("doc"));
    assert_eq!(
        body["keep"].as_str(),
        Some("me"),
        "untargeted document fields must survive the UPDATE, got {body:?}"
    );
}

#[test]
fn document_path_update_keeps_nested_sibling_fields() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT body_path_docs");
    exec(
        &rt,
        r#"INSERT INTO body_path_docs DOCUMENT
           VALUES ({"name":"doc","profile":{"address":{"city":"Porto","zip":"4000"},"active":true},"keep":"me"})"#,
    );

    let updated = exec(
        &rt,
        "UPDATE body_path_docs SET profile.address.city = 'Lisbon' WHERE name = 'doc'",
    );
    assert_eq!(updated.affected_rows, 1);

    let with_body = exec(&rt, "SELECT body FROM body_path_docs WHERE name = 'doc'");
    let body = json_field(only_record(&with_body), "body");
    assert_eq!(body["profile"]["address"]["city"].as_str(), Some("Lisbon"));
    assert_eq!(body["profile"]["address"]["zip"].as_str(), Some("4000"));
    assert_eq!(body["profile"]["active"].as_bool(), Some(true));
    assert_eq!(body["keep"].as_str(), Some("me"));
}

// #1712: a nested SET creates the intermediate objects it traverses, keeps
// sibling keys, and the flattened top-level read surface reflects the change
// immediately (the promoted `settings` column is the freshly created object).
#[test]
fn document_nested_set_creates_intermediate_objects() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT body_intermediate_docs");
    exec(
        &rt,
        r#"INSERT INTO body_intermediate_docs DOCUMENT VALUES ({"name":"doc","keep":"me"})"#,
    );

    let updated = exec(
        &rt,
        "UPDATE body_intermediate_docs SET settings.notifications.email = 'on' WHERE name = 'doc'",
    );
    assert_eq!(updated.affected_rows, 1);

    let with_body = exec(&rt, "SELECT body FROM body_intermediate_docs WHERE name = 'doc'");
    let body = json_field(only_record(&with_body), "body");
    assert_eq!(
        body["settings"]["notifications"]["email"].as_str(),
        Some("on"),
        "intermediate objects must be created by the nested SET, got {body:?}"
    );
    assert_eq!(body["keep"].as_str(), Some("me"), "siblings must survive");

    // Immediate flattened read: the newly created top-level `settings` object is
    // promoted as a column and reflects the merge without a re-open.
    let promoted = exec(
        &rt,
        "SELECT settings FROM body_intermediate_docs WHERE name = 'doc'",
    );
    let settings = json_field(only_record(&promoted), "settings");
    assert_eq!(settings["notifications"]["email"].as_str(), Some("on"));
}

// #1712 / ADR 0066: a nested SET may not introduce a reserved envelope name at
// the top level — it is rejected with the upgraded reserved-field error and the
// document is left untouched.
#[test]
fn document_nested_set_rejects_reserved_top_level() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT body_reserved_docs");
    exec(
        &rt,
        r#"INSERT INTO body_reserved_docs DOCUMENT VALUES ({"name":"doc","keep":"me"})"#,
    );

    let error = err_string(
        &rt,
        "UPDATE body_reserved_docs SET kind = 'nope' WHERE name = 'doc'",
    );
    assert!(
        error.contains("reserved") && error.contains("kind"),
        "reserved top-level SET must be rejected with the ADR 0066 error, got: {error}"
    );

    // The rejected UPDATE must not have mutated the document body.
    let with_body = exec(&rt, "SELECT body FROM body_reserved_docs WHERE name = 'doc'");
    let body = json_field(only_record(&with_body), "body");
    assert_eq!(body["keep"].as_str(), Some("me"));
    assert!(
        body.get("kind").is_none(),
        "reserved field must not have been written, got {body:?}"
    );
}

// #1712: setting a path *through* a scalar (here `count` is a number) fails with
// a clear error and leaves the document untouched — consistent with the HTTP
// JSON-patch contract, which never clobbers a scalar into an object.
#[test]
fn document_nested_set_through_scalar_rejected() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT body_scalar_docs");
    exec(
        &rt,
        r#"INSERT INTO body_scalar_docs DOCUMENT VALUES ({"name":"doc","count":5})"#,
    );

    let error = err_string(
        &rt,
        "UPDATE body_scalar_docs SET count.total = 9 WHERE name = 'doc'",
    );
    assert!(
        error.contains("not an object"),
        "path-through-scalar must fail with a clear error, got: {error}"
    );

    let with_body = exec(&rt, "SELECT body FROM body_scalar_docs WHERE name = 'doc'");
    let body = json_field(only_record(&with_body), "body");
    assert_eq!(
        body["count"].as_i64(),
        Some(5),
        "the scalar must be untouched by the rejected SET, got {body:?}"
    );
}

// #1712: array positional paths stay unsupported on the SQL nested-SET surface,
// matching the HTTP patch contract; the array is left untouched.
#[test]
fn document_nested_set_array_positional_rejected() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT body_array_docs");
    exec(
        &rt,
        r#"INSERT INTO body_array_docs DOCUMENT VALUES ({"name":"doc","tags":["alpha","beta"]})"#,
    );

    let error = err_string(
        &rt,
        "UPDATE body_array_docs SET tags.0 = 'gamma' WHERE name = 'doc'",
    );
    assert!(
        error.contains("array positional"),
        "array-positional SET must be rejected with a clear error, got: {error}"
    );

    let with_body = exec(&rt, "SELECT body FROM body_array_docs WHERE name = 'doc'");
    let body = json_field(only_record(&with_body), "body");
    assert_eq!(
        body["tags"],
        serde_json::json!(["alpha", "beta"]),
        "the array must be untouched by the rejected SET, got {body:?}"
    );
}

// #1712: RETURNING reflects the post-merge document body.
#[test]
fn document_nested_set_returning_reflects_merge() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT body_returning_docs");
    exec(
        &rt,
        r#"INSERT INTO body_returning_docs DOCUMENT
           VALUES ({"name":"doc","user":{"address":{"city":"Porto"},"active":true}})"#,
    );

    let returned = exec(
        &rt,
        "UPDATE body_returning_docs SET user.address.city = 'SP' WHERE name = 'doc' RETURNING body",
    );
    let body = json_field(only_record(&returned), "body");
    assert_eq!(
        body["user"]["address"]["city"].as_str(),
        Some("SP"),
        "RETURNING must reflect the post-merge body, got {body:?}"
    );
    assert_eq!(
        body["user"]["active"].as_bool(),
        Some(true),
        "RETURNING must preserve siblings, got {body:?}"
    );
}

#[test]
fn document_compound_update_keeps_body_json_in_sync() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT body_compound_docs");
    exec(
        &rt,
        r#"INSERT INTO body_compound_docs DOCUMENT VALUES ({"name":"doc","score":10})"#,
    );

    exec(
        &rt,
        "UPDATE body_compound_docs SET score += 5 WHERE name = 'doc'",
    );

    let promoted = exec(
        &rt,
        "SELECT score FROM body_compound_docs WHERE name = 'doc'",
    );
    assert_eq!(int_field(only_record(&promoted), "score"), 15);

    let with_body = exec(
        &rt,
        "SELECT body FROM body_compound_docs WHERE name = 'doc'",
    );
    let body = json_field(only_record(&with_body), "body");
    assert_eq!(
        body["score"].as_i64(),
        Some(15),
        "body JSON should reflect the compound-updated column, got {body:?}"
    );
    assert_eq!(body["name"].as_str(), Some("doc"));
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
    exec(&rt, "UPDATE indexed_rows SET score = 15 WHERE id = 1");
    let row = exec(&rt, "SELECT id, score FROM indexed_rows WHERE score = 15");
    assert_eq!(int_field(only_record(&row), "id"), 1);

    exec(&rt, "CREATE DOCUMENT indexed_docs");
    exec(
        &rt,
        r#"INSERT INTO indexed_docs DOCUMENT VALUES ({"name":"doc","score":10})"#,
    );
    exec(
        &rt,
        "CREATE INDEX idx_indexed_docs_score ON indexed_docs (score) USING HASH",
    );
    exec(&rt, "UPDATE indexed_docs SET score = 15 WHERE name = 'doc'");
    let doc = exec(&rt, "SELECT name, score FROM indexed_docs WHERE score = 15");
    assert_eq!(text_field(only_record(&doc), "name"), "doc");
}

#[test]
fn explicit_multimodel_compound_updates_survive_reopen_as_post_images() {
    let path = support::temp_db_file("update-conformance-recovery");
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("persistent runtime");
        exec(&rt, "CREATE DOCUMENT recovery_docs");
        exec(
            &rt,
            r#"INSERT INTO recovery_docs DOCUMENT VALUES ({"name":"doc","score":10})"#,
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
            "UPDATE recovery_docs SET score += 5 WHERE name = 'doc'",
        );
        exec(
            &rt,
            "UPDATE recovery_kv SET value += 7 WHERE key = 'counter'",
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
}
