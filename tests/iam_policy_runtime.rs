//! Runtime authorization tests for IAM policies.
//!
//! The kernel tests prove matching semantics. These tests prove the SQL
//! executor actually consults IAM policies instead of only the legacy
//! GRANT table.

use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
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
