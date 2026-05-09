//! Runtime authorization tests for IAM policies.
//!
//! The kernel tests prove matching semantics. These tests prove the SQL
//! executor actually consults IAM policies instead of only the legacy
//! GRANT table.

use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
    set_current_tenant,
};
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime_with_auth() -> (RedDBRuntime, Arc<AuthStore>) {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Write).unwrap();
    rt.set_auth_store(Arc::clone(&store));
    (rt, store)
}

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

fn as_tenant_user<T>(tenant: &str, name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_tenant(tenant.to_string());
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    clear_current_tenant();
    out
}

fn attach_platform_policy(store: &AuthStore, policy_json: &str) {
    let policy = reddb::auth::policies::Policy::from_json_str(policy_json).unwrap();
    let policy_id = policy.id.clone();
    store.put_policy(policy).unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform("alice")),
            &policy_id,
        )
        .unwrap();
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

fn attach_tenant_policy(store: &AuthStore, tenant: &str, policy_json: &str) {
    let policy = reddb::auth::policies::Policy::from_json_str(policy_json).unwrap();
    let policy_id = policy.id.clone();
    store.put_policy(policy).unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::from_parts(
                Some(tenant),
                "alice",
            )),
            &policy_id,
        )
        .unwrap();
}

fn text_field(result: &reddb::runtime::RuntimeQueryResult, column: &str) -> String {
    match result.result.records[0].get(column).unwrap() {
        reddb::storage::schema::Value::Text(value) => value.to_string(),
        other => panic!("expected text field {column}, got {other:?}"),
    }
}

fn attach_policy(store: &AuthStore, user: reddb::auth::UserId, policy: &str) {
    let policy = reddb::auth::policies::Policy::from_json_str(policy).expect("valid policy");
    let id = policy.id.clone();
    store.put_policy(policy).unwrap();
    store
        .attach_policy(reddb::auth::store::PrincipalRef::User(user), &id)
        .unwrap();
}

fn seed_document(rt: &RedDBRuntime) {
    rt.execute_query(
        r#"INSERT INTO docs DOCUMENT (body) VALUES ('{"public":"ok","secret":"no","nested":{"public":"yes","secret":"hidden"}}')"#,
    )
    .unwrap();
}

#[test]
fn runtime_uses_attached_iam_policy_for_dml() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT)").unwrap();
    rt.execute_query("INSERT INTO orders (id) VALUES (1)")
        .unwrap();

    let policy = r#"{
        "id":"read-orders",
        "version":1,
        "statements":[
            {"effect":"allow","actions":["select"],"resources":["table:orders"]}
        ]
    }"#;
    store
        .put_policy(reddb::auth::policies::Policy::from_json_str(policy).unwrap())
        .unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform("alice")),
            "read-orders",
        )
        .unwrap();

    let read = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM orders")
    });
    assert!(read.is_ok(), "read should be allowed: {read:?}");

    let write = as_user("alice", Role::Write, || {
        rt.execute_query("INSERT INTO orders (id) VALUES (2)")
    });
    assert!(write.is_err(), "write should be denied by default");
}

#[test]
fn update_set_column_policy_allows_allowed_target() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE accounts (id INT, status TEXT, secret TEXT)")
        .unwrap();
    rt.execute_query("INSERT INTO accounts (id, status, secret) VALUES (1, 'old', 's1')")
        .unwrap();

    attach_platform_policy(
        &store,
        r#"{
            "id":"accounts-update-status",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["select","update"],"resources":["table:accounts"]}
            ]
        }"#,
    );

    let updated = as_user("alice", Role::Write, || {
        rt.execute_query("UPDATE accounts SET status = 'active' WHERE id = 1")
    })
    .expect("status update should be allowed");
    assert_eq!(updated.affected_rows, 1);

    let selected = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT status FROM accounts WHERE id = 1")
    })
    .unwrap();
    assert_eq!(text_field(&selected, "status"), "active");
}

#[test]
fn update_set_column_policy_blocks_denied_target_column() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE accounts (id INT, status TEXT, secret TEXT)")
        .unwrap();
    rt.execute_query("INSERT INTO accounts (id, status, secret) VALUES (1, 'old', 's1')")
        .unwrap();

    attach_platform_policy(
        &store,
        r#"{
            "id":"accounts-deny-secret",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["select","update"],"resources":["table:accounts"]},
                {"effect":"deny","actions":["update"],"resources":["column:accounts.secret"]}
            ]
        }"#,
    );

    let err = as_user("alice", Role::Write, || {
        rt.execute_query("UPDATE accounts SET secret = 's2' WHERE id = 1")
    })
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("column:accounts.secret") && msg.contains("denied by IAM column policy"),
        "unexpected error: {msg}"
    );

    let selected = rt
        .execute_query("SELECT secret FROM accounts WHERE id = 1")
        .unwrap();
    assert_eq!(text_field(&selected, "secret"), "s1");
}

#[test]
fn update_set_column_policy_blocks_multi_column_update_when_one_target_is_denied() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE accounts (id INT, status TEXT, secret TEXT)")
        .unwrap();
    rt.execute_query("INSERT INTO accounts (id, status, secret) VALUES (1, 'old', 's1')")
        .unwrap();

    attach_platform_policy(
        &store,
        r#"{
            "id":"accounts-deny-one-target",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["select","update"],"resources":["table:accounts"]},
                {"effect":"deny","actions":["update"],"resources":["column:accounts.secret"]}
            ]
        }"#,
    );

    let err = as_user("alice", Role::Write, || {
        rt.execute_query("UPDATE accounts SET status = 'active', secret = 's2' WHERE id = 1")
    })
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("column:accounts.secret"),
        "unexpected error: {msg}"
    );

    let selected = rt
        .execute_query("SELECT status, secret FROM accounts WHERE id = 1")
        .unwrap();
    assert_eq!(text_field(&selected, "status"), "old");
    assert_eq!(text_field(&selected, "secret"), "s1");
}

#[test]
fn update_set_column_allow_does_not_bypass_missing_table_allow() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE accounts (id INT, status TEXT)")
        .unwrap();
    rt.execute_query("INSERT INTO accounts (id, status) VALUES (1, 'old')")
        .unwrap();

    attach_platform_policy(
        &store,
        r#"{
            "id":"column-only-update-status",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["update"],"resources":["column:accounts.status"]}
            ]
        }"#,
    );

    let err = as_user("alice", Role::Write, || {
        rt.execute_query("UPDATE accounts SET status = 'active' WHERE id = 1")
    })
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("table:accounts") && msg.contains("denied by IAM policy"),
        "unexpected error: {msg}"
    );

    let selected = rt
        .execute_query("SELECT status FROM accounts WHERE id = 1")
        .unwrap();
    assert_eq!(text_field(&selected, "status"), "old");
}

#[test]
fn update_set_column_policy_uses_tenant_context() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE accounts (id INT, status TEXT, secret TEXT)")
        .unwrap();
    rt.execute_query("INSERT INTO accounts (id, status, secret) VALUES (1, 'old', 's1')")
        .unwrap();

    attach_tenant_policy(
        &store,
        "acme",
        r#"{
            "id":"tenant-accounts-update",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["select","update"],"resources":["table:accounts"],"condition":{"tenant_match":true}},
                {"effect":"deny","actions":["update"],"resources":["column:accounts.secret"],"condition":{"tenant_match":true}}
            ]
        }"#,
    );

    let updated = as_tenant_user("acme", "alice", Role::Write, || {
        rt.execute_query("UPDATE accounts SET status = 'active' WHERE id = 1")
    })
    .expect("tenant-scoped status update should be allowed");
    assert_eq!(updated.affected_rows, 1);

    let err = as_tenant_user("acme", "alice", Role::Write, || {
        rt.execute_query("UPDATE accounts SET secret = 's2' WHERE id = 1")
    })
    .unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("column:accounts.secret"),
        "unexpected error: {msg}"
    );

    let selected = rt
        .execute_query("SELECT status, secret FROM accounts WHERE id = 1")
        .unwrap();
    assert_eq!(text_field(&selected, "status"), "active");
    assert_eq!(text_field(&selected, "secret"), "s1");
}

#[test]
fn vector_search_blocks_denied_content_projection() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query(
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'secret')",
    )
    .unwrap();

    attach_platform_policy(
        &store,
        r#"{
            "id":"vector-content-deny",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["select"],"resources":["table:embeddings"]},
                {"effect":"deny","actions":["select"],"resources":["column:embeddings.content"]}
            ]
        }"#,
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("VECTOR SEARCH embeddings SIMILAR TO [1.0, 0.0] LIMIT 1")
    });
    let err = denied.expect_err("content projection should be denied");
    assert!(
        err.to_string().contains("column:embeddings.content"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn graph_match_blocks_denied_node_property_projection() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query(
        "INSERT INTO social NODE (label, name, secret) VALUES ('User', 'alice', 'pii')",
    )
    .unwrap();

    attach_platform_policy(
        &store,
        r#"{
            "id":"graph-secret-deny",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["select"],"resources":["table:graph"]},
                {"effect":"deny","actions":["select"],"resources":["column:graph.secret"]}
            ]
        }"#,
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("MATCH (n:User) RETURN n.secret")
    });
    let err = denied.expect_err("graph property projection should be denied");
    assert!(
        err.to_string().contains("column:graph.secret"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn timeseries_select_blocks_denied_tags_projection() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TIMESERIES metrics RETENTION 7 d")
        .unwrap();
    rt.execute_query(
        "INSERT INTO metrics (metric, value, tags, timestamp) VALUES \
         ('cpu', 50.0, {tenant: 'acme', host: 'a1'}, 1704067200000000000)",
    )
    .unwrap();

    attach_platform_policy(
        &store,
        r#"{
            "id":"timeseries-tags-deny",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["select"],"resources":["table:metrics"]},
                {"effect":"deny","actions":["select"],"resources":["column:metrics.tags"]}
            ]
        }"#,
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT tags FROM metrics")
    });
    let err = denied.expect_err("timeseries tags projection should be denied");
    assert!(
        err.to_string().contains("column:metrics.tags"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn group_policy_applies_through_alter_user_membership() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT)").unwrap();
    rt.execute_query("INSERT INTO orders (id) VALUES (1)")
        .unwrap();

    let policy = r#"{
        "id":"group-read-orders",
        "version":1,
        "statements":[
            {"effect":"allow","actions":["select"],"resources":["table:orders"]}
        ]
    }"#;
    store
        .put_policy(reddb::auth::policies::Policy::from_json_str(policy).unwrap())
        .unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::Group("analysts".to_string()),
            "group-read-orders",
        )
        .unwrap();

    let show = as_user("admin", Role::Admin, || {
        rt.execute_query("SHOW POLICIES FOR GROUP analysts")
            .unwrap()
    });
    assert_eq!(show.result.records.len(), 1);

    as_user("admin", Role::Admin, || {
        rt.execute_query("ALTER USER alice ADD GROUP analysts")
            .unwrap();
    });

    let read = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM orders")
    });
    assert!(read.is_ok(), "group policy should allow read: {read:?}");

    let write = as_user("alice", Role::Write, || {
        rt.execute_query("INSERT INTO orders (id) VALUES (2)")
    });
    assert!(write.is_err(), "group policy should not allow writes");

    as_user("admin", Role::Admin, || {
        rt.execute_query("ALTER USER alice DROP GROUP analysts")
            .unwrap();
    });

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM orders")
    });
    assert!(denied.is_err(), "dropping the group should remove access");
}

#[test]
fn grant_to_public_compiles_to_implicit_public_policy() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT)").unwrap();
    rt.execute_query("INSERT INTO orders (id) VALUES (1)")
        .unwrap();

    as_user("admin", Role::Admin, || {
        rt.execute_query("GRANT SELECT ON TABLE orders TO PUBLIC")
            .unwrap();
    });

    assert!(store.iam_authorization_enabled());
    let public_read = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM orders")
    });
    assert!(
        public_read.is_ok(),
        "PUBLIC grant should allow every principal: {public_read:?}"
    );

    as_user("admin", Role::Admin, || {
        rt.execute_query("REVOKE SELECT ON TABLE orders FROM PUBLIC")
            .unwrap();
    });

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM orders")
    });
    assert!(denied.is_err(), "PUBLIC revoke should remove the IAM allow");
}

#[test]
fn revoke_removes_synthetic_grant_policy() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT)").unwrap();
    rt.execute_query("INSERT INTO orders (id) VALUES (1)")
        .unwrap();

    let grant_result = as_user("admin", Role::Admin, || {
        rt.execute_query("GRANT SELECT ON TABLE orders TO alice")
            .unwrap()
    });
    assert_eq!(
        grant_result.statement_type, "grant",
        "grant result: statement={} engine={} query={}",
        grant_result.statement, grant_result.engine, grant_result.query
    );
    assert_eq!(grant_result.affected_rows, 0);
    assert_eq!(
        store
            .effective_grants(&reddb::auth::UserId::platform("alice"))
            .len(),
        1
    );
    let after_grant: Vec<String> = store.list_policies().iter().map(|p| p.id.clone()).collect();
    assert!(
        after_grant.iter().any(|id| id.starts_with("_grant_")),
        "GRANT should create a synthetic policy, got {after_grant:?}"
    );
    assert!(store.iam_authorization_enabled());

    let read = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM orders")
    });
    assert!(
        read.is_ok(),
        "grant should compile to an allow policy: {read:?}"
    );

    as_user("admin", Role::Admin, || {
        rt.execute_query("REVOKE SELECT ON TABLE orders FROM alice")
            .unwrap();
    });

    let remaining: Vec<String> = store.list_policies().iter().map(|p| p.id.clone()).collect();
    assert!(
        remaining.is_empty(),
        "synthetic policies should be removed, still have {remaining:?}"
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM orders")
    });
    assert!(denied.is_err(), "revoke should remove the allow policy");
}

#[test]
fn insert_column_policy_allows_named_columns_with_table_allow() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT, note TEXT)")
        .unwrap();

    attach_policy(
        &store,
        reddb::auth::UserId::platform("alice"),
        r#"{
            "id":"insert-orders",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["insert"],"resources":["table:orders"]}
            ]
        }"#,
    );

    let insert = as_user("alice", Role::Write, || {
        rt.execute_query("INSERT INTO orders (id, note) VALUES (1, 'ok')")
    });
    assert!(
        insert.is_ok(),
        "table allow should allow insert: {insert:?}"
    );
}

#[test]
fn insert_column_policy_denies_explicit_denied_column() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT, public TEXT, secret TEXT)")
        .unwrap();

    attach_policy(
        &store,
        reddb::auth::UserId::platform("alice"),
        r#"{
            "id":"insert-orders-with-secret-deny",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["insert"],"resources":["table:orders"]},
                {"effect":"deny","actions":["insert"],"resources":["column:orders.secret"]}
            ]
        }"#,
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("INSERT INTO orders (id, secret) VALUES (1, 'nope')")
    });
    let err = denied.expect_err("denied column should block insert");
    assert!(
        err.to_string().contains("column:orders.secret"),
        "expected denied column in error, got {err}"
    );
}

#[test]
fn insert_column_policy_ignores_omitted_denied_columns() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT, public TEXT, secret TEXT)")
        .unwrap();

    attach_policy(
        &store,
        reddb::auth::UserId::platform("alice"),
        r#"{
            "id":"insert-orders-omit-secret",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["insert"],"resources":["table:orders"]},
                {"effect":"deny","actions":["insert"],"resources":["column:orders.secret"]}
            ]
        }"#,
    );

    let insert = as_user("alice", Role::Write, || {
        rt.execute_query("INSERT INTO orders (id, public) VALUES (1, 'ok')")
    });
    assert!(
        insert.is_ok(),
        "omitted denied column should not be treated as a write target: {insert:?}"
    );
}

#[test]
fn insert_column_policy_denies_tenant_auto_fill_target() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE events (id INT, tenant_id TEXT) TENANT BY (tenant_id)")
        .unwrap();

    attach_policy(
        &store,
        reddb::auth::UserId::from_parts(Some("acme"), "alice"),
        r#"{
            "id":"insert-events-deny-tenant-column",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["insert"],"resources":["table:tenant/acme/events"]},
                {"effect":"deny","actions":["insert"],"resources":["column:tenant/acme/events.tenant_id"]}
            ]
        }"#,
    );

    set_current_tenant("acme".to_string());
    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("INSERT INTO events (id) VALUES (1)")
    });
    clear_current_tenant();

    let err = denied.expect_err("auto-filled tenant column should be policy checked");
    assert!(
        err.to_string().contains("column:events.tenant_id"),
        "expected implicit tenant column in error, got {err}"
    );
}

#[test]
fn insert_column_policy_applies_to_multi_row_insert() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT, note TEXT)")
        .unwrap();

    attach_policy(
        &store,
        reddb::auth::UserId::platform("alice"),
        r#"{
            "id":"insert-orders-multi",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["insert"],"resources":["table:orders"]}
            ]
        }"#,
    );

    let insert = as_user("alice", Role::Write, || {
        rt.execute_query("INSERT INTO orders (id, note) VALUES (1, 'a'), (2, 'b')")
    })
    .expect("multi-row insert should be allowed");
    assert_eq!(insert.affected_rows, 2);
}

#[test]
fn insert_column_allow_does_not_bypass_missing_table_allow() {
    let (rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT)").unwrap();

    attach_policy(
        &store,
        reddb::auth::UserId::platform("alice"),
        r#"{
            "id":"insert-column-only",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["insert"],"resources":["column:orders.id"]}
            ]
        }"#,
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("INSERT INTO orders (id) VALUES (1)")
    });
    let err = denied.expect_err("column allow must not replace table allow");
    assert!(
        err.to_string().contains("table:orders"),
        "expected missing table allow denial, got {err}"
    );
}

#[test]
fn document_json_path_projection_allows_non_denied_path() {
    let (rt, store) = runtime_with_auth();
    seed_document(&rt);
    attach_alice_policy(
        &store,
        "doc-path-public",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:docs"]},
            {"effect":"deny","actions":["select"],"resources":["column:docs.body.secret"]}
        ]"#,
    );

    let read = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT body.public FROM docs")
    });
    assert!(read.is_ok(), "allowed path should read: {read:?}");
}

#[test]
fn document_json_path_projection_denies_explicit_path() {
    let (rt, store) = runtime_with_auth();
    seed_document(&rt);
    attach_alice_policy(
        &store,
        "doc-path-deny",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:docs"]},
            {"effect":"deny","actions":["select"],"resources":["column:docs.body.secret"]}
        ]"#,
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT body.secret FROM docs")
    });
    assert!(denied.is_err(), "denied path should fail");
}

#[test]
fn document_json_column_projection_denies_base_document_column() {
    let (rt, store) = runtime_with_auth();
    seed_document(&rt);
    attach_alice_policy(
        &store,
        "doc-column-deny",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:docs"]},
            {"effect":"deny","actions":["select"],"resources":["column:docs.body"]}
        ]"#,
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT body FROM docs")
    });
    assert!(denied.is_err(), "denied document column should fail");
}

#[test]
fn document_json_wildcard_projection_checks_column_wildcard_policy() {
    let (rt, store) = runtime_with_auth();
    seed_document(&rt);
    attach_alice_policy(
        &store,
        "doc-wildcard-deny",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:docs"]},
            {"effect":"deny","actions":["select"],"resources":["column:docs.*"]}
        ]"#,
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM docs")
    });
    assert!(
        denied.is_err(),
        "wildcard projection should consult column:*"
    );
}

#[test]
fn document_json_table_allow_does_not_bypass_document_path_deny() {
    let (rt, store) = runtime_with_auth();
    seed_document(&rt);
    attach_alice_policy(
        &store,
        "doc-no-table-bypass",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:docs"]},
            {"effect":"deny","actions":["select"],"resources":["column:docs.body.nested.secret"]}
        ]"#,
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT body.nested.secret FROM docs")
    });
    assert!(denied.is_err(), "table allow must not bypass path deny");
}
