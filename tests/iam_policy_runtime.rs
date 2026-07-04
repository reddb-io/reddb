//! Runtime authorization tests for IAM policies.
//!
//! The kernel tests prove matching semantics. These tests prove the SQL
//! executor actually consults IAM policies instead of only the legacy
//! GRANT table.

#[allow(dead_code)]
mod support;

use std::sync::Arc;
use std::time::Duration;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
    set_current_tenant,
};
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime_with_auth() -> (support::TempDataDir, RedDBRuntime, Arc<AuthStore>) {
    let dir = support::temp_data_dir("iam-policy-runtime");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(dir.join("data.rdb")))
        .expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    // The runtime IAM tests below assume the strict posture — "no
    // matching policy → deny". Under the default `LegacyRbac` mode
    // (#712 / S5A) the principal would fall back to the role-based
    // decision and many tests would accidentally pass via the
    // legacy fallback. Pin `PolicyOnly` so each test exercises the
    // policy evaluator directly.
    store.set_enforcement_mode(reddb::auth::enforcement_mode::PolicyEnforcementMode::PolicyOnly);
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Write).unwrap();
    rt.set_auth_store(Arc::clone(&store));
    (dir, rt, store)
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

#[test]
fn sql_create_attach_and_show_iam_policy_applies_at_runtime() {
    let (_dir, rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT)").unwrap();
    rt.execute_query("INSERT INTO orders (id) VALUES (1)")
        .unwrap();

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM orders")
    });
    assert!(
        denied.is_err(),
        "PolicyOnly user should not read without an attached policy"
    );

    let policy = r#"{"id":"sql-read-orders","version":1,"statements":[{"effect":"allow","actions":["select"],"resources":["table:orders"]}]}"#;
    store.set_enforcement_mode(reddb::auth::enforcement_mode::PolicyEnforcementMode::LegacyRbac);
    as_user("admin", Role::Admin, || {
        rt.execute_query(&format!("CREATE POLICY 'sql-read-orders' AS '{}'", policy))
            .unwrap();
        rt.execute_query("ATTACH POLICY 'sql-read-orders' TO USER alice")
            .unwrap();
    });
    store.set_enforcement_mode(reddb::auth::enforcement_mode::PolicyEnforcementMode::PolicyOnly);

    let show = as_user("admin", Role::Admin, || {
        rt.execute_query("SHOW POLICIES FOR USER alice").unwrap()
    });
    assert!(
        show.result.records.iter().any(|record| {
            matches!(
                record.get("id"),
                Some(reddb::storage::schema::Value::Text(id)) if id.as_ref() == "sql-read-orders"
            )
        }),
        "SHOW POLICIES should expose the SQL-created policy: {:?}",
        show.result.records
    );

    let allowed = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM orders")
    })
    .expect("attached SQL-created policy should allow SELECT");
    assert_eq!(allowed.result.records.len(), 1);
}

fn seed_document(rt: &RedDBRuntime) {
    rt.execute_query(
        r#"INSERT INTO docs DOCUMENT VALUES ({"public":"ok","secret":"no","nested":{"public":"yes","secret":"hidden"}})"#,
    )
    .unwrap();
}

fn setup_users_table(rt: &RedDBRuntime) {
    rt.execute_query("CREATE TABLE users (id INT, name TEXT, email TEXT)")
        .unwrap();
    rt.execute_query("INSERT INTO users (id, name, email) VALUES (1, 'Ada', 'a@example.com')")
        .unwrap();
}

fn err_string<T: std::fmt::Debug>(result: Result<T, reddb::RedDBError>) -> String {
    format!("{:?}", result.unwrap_err())
}

fn audit_body(rt: &RedDBRuntime) -> String {
    assert!(rt.audit_log().wait_idle(Duration::from_secs(2)));
    std::fs::read_to_string(rt.audit_log().path()).unwrap_or_default()
}

#[test]
fn admin_explicit_deny_matches_between_simulator_and_sql_path() {
    // Policy-first: an admin principal with an allow-all policy must still
    // be denied by an explicit Deny. The policy simulator and the runtime
    // SQL projection gate must report the same decision for the
    // allow-all-plus-deny shape.
    let (_dir, rt, store) = runtime_with_auth();
    setup_users_table(&rt);
    attach_policy(
        &store,
        reddb::auth::UserId::platform("admin"),
        r#"{
            "id":"admin-allow-all-deny-email",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["*"],"resources":["*"]},
                {"effect":"deny","actions":["select"],"resources":["column:users.email"]}
            ]
        }"#,
    );

    // Runtime SQL path: admin SELECT of the denied column is rejected.
    let denied = as_user("admin", Role::Admin, || {
        rt.execute_query("SELECT id, email FROM users")
    });
    let err = err_string(denied);
    assert!(err.contains("users.email"), "SQL path got {err}");

    // Policy simulator: same principal, same allow-all-plus-deny shape,
    // same Deny decision on the denied column.
    let sim = store.simulate(
        &reddb::auth::UserId::platform("admin"),
        "select",
        &reddb::auth::policies::ResourceRef::new("column", "users.email"),
        reddb::auth::store::SimCtx::default(),
    );
    assert!(
        matches!(sim.decision, reddb::auth::policies::Decision::Deny { .. }),
        "simulator should deny admin on explicit deny, got {:?}",
        sim.decision
    );

    // And the broad allow still holds for a non-denied column.
    let allowed_sim = store.simulate(
        &reddb::auth::UserId::platform("admin"),
        "select",
        &reddb::auth::policies::ResourceRef::new("column", "users.name"),
        reddb::auth::store::SimCtx::default(),
    );
    assert!(
        matches!(
            allowed_sim.decision,
            reddb::auth::policies::Decision::Allow { .. }
                | reddb::auth::policies::Decision::AdminBypass
        ),
        "simulator should allow admin on non-denied resource, got {:?}",
        allowed_sim.decision
    );
}

#[test]
fn select_column_policy_allows_safe_projection() {
    let (_dir, rt, store) = runtime_with_auth();
    setup_users_table(&rt);
    attach_alice_policy(
        &store,
        "users-no-email",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:users"]},
            {"effect":"deny","actions":["select"],"resources":["column:users.email"]}
        ]"#,
    );

    let read = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT id, name FROM users")
    })
    .unwrap();
    assert_eq!(read.result.records.len(), 1);
    assert_eq!(read.result.columns, vec!["id", "name"]);
}

#[test]
fn subscription_create_requires_select_on_source() {
    let (_dir, rt, store) = runtime_with_auth();
    attach_alice_policy(
        &store,
        "events-target-only",
        r#"[
            {"effect":"allow","actions":["write"],"resources":["queue:audit"]},
            {"effect":"allow","actions":["create"],"resources":["collection:users"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("CREATE TABLE users (id INT, email TEXT) WITH EVENTS TO audit")
    }));
    assert!(err.contains("action=`select`"), "got {err}");
    assert!(err.contains("table:users"), "got {err}");
    assert!(
        rt.db().collection_contract("users").is_none(),
        "denied DDL must not create source table"
    );
}

#[test]
fn subscription_alter_requires_write_on_target_queue() {
    let (_dir, rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE users (id INT, email TEXT)")
        .unwrap();
    attach_alice_policy(
        &store,
        "events-source-only",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:users"]},
            {"effect":"allow","actions":["alter"],"resources":["collection:users"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("ALTER TABLE users ADD SUBSCRIPTION audit_sub TO audit")
    }));
    assert!(err.contains("action=`write`"), "got {err}");
    assert!(err.contains("queue:audit"), "got {err}");
    assert!(
        rt.db().collection_contract("audit").is_none(),
        "denied DDL must not auto-create target queue"
    );
}

#[test]
fn subscription_redact_covers_column_policy_without_warning() {
    let (_dir, rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE users (id INT, email TEXT)")
        .unwrap();
    attach_alice_policy(
        &store,
        "events-redact-covered",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:users"]},
            {"effect":"allow","actions":["write"],"resources":["queue:audit"]},
            {"effect":"allow","actions":["alter"],"resources":["collection:users"]},
            {"effect":"deny","actions":["select"],"resources":["column:users.email"]}
        ]"#,
    );

    as_user("alice", Role::Write, || {
        rt.execute_query("ALTER TABLE users ADD SUBSCRIPTION audit_sub TO audit REDACT (email)")
    })
    .unwrap();
    let body = audit_body(&rt);
    assert!(
        !body.contains("subscription_redact_gap"),
        "covered REDACT should not emit warning audit: {body}"
    );
}

#[test]
fn subscription_redact_gap_warns_but_allows_ddl() {
    let (_dir, rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE users (id INT, email TEXT, name TEXT)")
        .unwrap();
    attach_alice_policy(
        &store,
        "events-redact-gap",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:users"]},
            {"effect":"allow","actions":["write"],"resources":["queue:audit"]},
            {"effect":"allow","actions":["alter"],"resources":["collection:users"]},
            {"effect":"deny","actions":["select"],"resources":["column:users.email"]}
        ]"#,
    );

    as_user("alice", Role::Write, || {
        rt.execute_query("ALTER TABLE users ADD SUBSCRIPTION audit_sub TO audit REDACT (name)")
    })
    .unwrap();
    let contract = rt
        .db()
        .collection_contract("users")
        .expect("users contract");
    assert!(
        contract.subscriptions.iter().any(|s| s.name == "audit_sub"),
        "DDL should be allowed despite redact warning"
    );
    let body = audit_body(&rt);
    assert!(
        body.contains("subscription_redact_gap"),
        "audit missing: {body}"
    );
    assert!(body.contains("email"), "gap column missing: {body}");
}

#[test]
fn select_column_policy_denies_explicit_column() {
    let (_dir, rt, store) = runtime_with_auth();
    setup_users_table(&rt);
    attach_alice_policy(
        &store,
        "users-deny-email",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:users"]},
            {"effect":"deny","actions":["select"],"resources":["column:users.email"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("SELECT id, email FROM users")
    }));
    assert!(err.contains("users.email"), "got {err}");
}

#[test]
fn select_column_policy_requires_table_allow_even_with_column_allow() {
    let (_dir, rt, store) = runtime_with_auth();
    setup_users_table(&rt);
    attach_alice_policy(
        &store,
        "users-column-only",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["column:users.id"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("SELECT id FROM users")
    }));
    assert!(err.contains("table:users"), "got {err}");
}

#[test]
fn select_column_policy_denies_wildcard_when_any_declared_column_denied() {
    let (_dir, rt, store) = runtime_with_auth();
    setup_users_table(&rt);
    attach_alice_policy(
        &store,
        "users-wildcard-deny-email",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["table:users"]},
            {"effect":"deny","actions":["select"],"resources":["column:users.email"]}
        ]"#,
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("SELECT * FROM users")
    }));
    assert!(err.contains("users.email"), "got {err}");
}

#[test]
fn select_column_policy_resolves_aliases_across_table_joins() {
    let (_dir, rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE users (id INT, name TEXT, email TEXT)")
        .unwrap();
    rt.execute_query("CREATE TABLE orders (id INT, user_id INT, total INT)")
        .unwrap();
    rt.execute_query("INSERT INTO users (id, name, email) VALUES (1, 'Ada', 'a@example.com')")
        .unwrap();
    rt.execute_query("INSERT INTO orders (id, user_id, total) VALUES (10, 1, 42)")
        .unwrap();
    attach_alice_policy(
        &store,
        "join-users-orders",
        r#"[
            {"effect":"allow","actions":["select"],"resources":["database:*"]},
            {"effect":"allow","actions":["select"],"resources":["table:users"]},
            {"effect":"allow","actions":["select"],"resources":["table:orders"]},
            {"effect":"deny","actions":["select"],"resources":["column:users.email"]}
        ]"#,
    );

    let allowed = as_user("alice", Role::Write, || {
        rt.execute_query("FROM users u JOIN orders o ON u.id = o.user_id RETURN u.name, o.total")
    });
    assert!(
        allowed.is_ok(),
        "join projection should be allowed: {allowed:?}"
    );

    let err = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("FROM users u JOIN orders o ON u.id = o.user_id RETURN u.email, o.total")
    }));
    assert!(err.contains("users.email"), "got {err}");
}

#[test]
fn runtime_uses_attached_iam_policy_for_dml() {
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
    rt.execute_query(
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'secret')",
    )
    .unwrap();

    // Issue #756 added a granular `vector:search` gate on the
    // `vector:<collection>` resource. Grant it so the test continues
    // to exercise the column-level deny that this case pins, rather
    // than failing at the new search gate.
    attach_platform_policy(
        &store,
        r#"{
            "id":"vector-content-deny",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["select"],"resources":["table:embeddings"]},
                {"effect":"allow","actions":["vector:search"],"resources":["vector:embeddings"]},
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
    let (_dir, rt, store) = runtime_with_auth();
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
                {"effect":"allow","actions":["graph:traverse"],"resources":["graph:*"]},
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
    let (_dir, rt, store) = runtime_with_auth();
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
fn kv_invalidate_tags_requires_iam_action() {
    let (_dir, rt, store) = runtime_with_auth();
    rt.execute_query("KV PUT sessions.blob = 'payload' TAGS [user:42]")
        .unwrap();

    attach_platform_policy(
        &store,
        r#"{
            "id":"kv-put-only",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["insert"],"resources":["table:sessions"]}
            ]
        }"#,
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("INVALIDATE TAGS [user:42] FROM sessions")
    });
    let err = denied.expect_err("kv:invalidate should be required");
    assert!(
        err.to_string().contains("kv:invalidate"),
        "unexpected error: {err:?}"
    );

    attach_platform_policy(
        &store,
        r#"{
            "id":"kv-invalidate-sessions",
            "version":1,
            "statements":[
                {"effect":"allow","actions":["kv:invalidate"],"resources":["kv:sessions"]}
            ]
        }"#,
    );

    let allowed = as_user("alice", Role::Write, || {
        rt.execute_query("INVALIDATE TAGS [user:42] FROM sessions")
    })
    .unwrap();
    assert_eq!(allowed.affected_rows, 1);
}

#[test]
fn destructive_ddl_requires_drop_or_truncate_policy_before_mutation() {
    let (_dir, rt, store) = runtime_with_auth();
    rt.execute_query("CREATE TABLE ddl_drop_guard (id INT)")
        .unwrap();
    rt.execute_query("INSERT INTO ddl_drop_guard (id) VALUES (1)")
        .unwrap();
    rt.execute_query("CREATE TABLE ddl_truncate_guard (id INT)")
        .unwrap();
    rt.execute_query("INSERT INTO ddl_truncate_guard (id) VALUES (1)")
        .unwrap();

    attach_alice_policy(
        &store,
        "ddl-wrong-actions",
        r#"[
            {"effect":"allow","actions":["truncate"],"resources":["collection:ddl_drop_guard"]},
            {"effect":"allow","actions":["drop"],"resources":["collection:ddl_truncate_guard"]}
        ]"#,
    );

    let denied_drop = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("DROP TABLE ddl_drop_guard")
    }));
    assert!(denied_drop.contains("action=`drop`"), "got {denied_drop}");
    assert!(
        denied_drop.contains("collection:ddl_drop_guard"),
        "got {denied_drop}"
    );
    assert!(
        rt.db().collection_contract("ddl_drop_guard").is_some(),
        "denied DROP must leave the table contract intact"
    );

    let denied_truncate = err_string(as_user("alice", Role::Write, || {
        rt.execute_query("TRUNCATE TABLE ddl_truncate_guard")
    }));
    assert!(
        denied_truncate.contains("action=`truncate`"),
        "got {denied_truncate}"
    );
    assert!(
        denied_truncate.contains("collection:ddl_truncate_guard"),
        "got {denied_truncate}"
    );
    let rows_after_denied_truncate = rt
        .execute_query("SELECT id FROM ddl_truncate_guard")
        .unwrap();
    assert_eq!(
        rows_after_denied_truncate.result.records.len(),
        1,
        "denied TRUNCATE must leave rows intact"
    );

    attach_alice_policy(
        &store,
        "ddl-destructive-actions",
        r#"[
            {"effect":"allow","actions":["drop"],"resources":["collection:ddl_drop_guard"]},
            {"effect":"allow","actions":["truncate"],"resources":["collection:ddl_truncate_guard"]}
        ]"#,
    );

    as_user("alice", Role::Write, || {
        rt.execute_query("TRUNCATE TABLE ddl_truncate_guard")
    })
    .unwrap();
    let rows_after_allowed_truncate = rt
        .execute_query("SELECT id FROM ddl_truncate_guard")
        .unwrap();
    assert!(rows_after_allowed_truncate.result.records.is_empty());
    assert!(
        rt.db().collection_contract("ddl_truncate_guard").is_some(),
        "allowed TRUNCATE must preserve the table contract"
    );

    as_user("alice", Role::Write, || {
        rt.execute_query("DROP COLLECTION ddl_drop_guard")
    })
    .unwrap();
    assert!(
        rt.db().collection_contract("ddl_drop_guard").is_none(),
        "allowed polymorphic DROP COLLECTION must remove the table contract"
    );

    let audit = audit_body(&rt);
    assert!(audit.contains("\"principal\":\"alice\""), "audit: {audit}");
    assert!(audit.contains("\"action\":\"drop\""), "audit: {audit}");
    assert!(audit.contains("\"action\":\"truncate\""), "audit: {audit}");
    assert!(audit.contains("\"outcome\":\"denied\""), "audit: {audit}");
    assert!(audit.contains("\"outcome\":\"success\""), "audit: {audit}");
}

#[test]
fn group_policy_applies_through_alter_user_membership() {
    let (_dir, rt, store) = runtime_with_auth();
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
    // #712 / S5A: SHOW POLICIES now prepends a synthetic header row
    // that reports the active enforcement mode. The group's actual
    // policies follow — one in this case.
    assert_eq!(show.result.records.len(), 2);
    let header_id = match show.result.records[0].get("id").unwrap() {
        reddb::storage::schema::Value::Text(text) => text.to_string(),
        other => panic!("expected text id, got {other:?}"),
    };
    assert_eq!(header_id, "<enforcement_mode>");
    let header_json = match show.result.records[0].get("json").unwrap() {
        reddb::storage::schema::Value::Text(text) => text.to_string(),
        other => panic!("expected text json, got {other:?}"),
    };
    assert!(
        header_json.contains("\"enforcement_mode\":\"policy_only\""),
        "header reports active mode: {header_json}",
    );

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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
    let (_dir, rt, store) = runtime_with_auth();
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
