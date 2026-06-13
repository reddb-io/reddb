//! Issue #753 — SQL DDL operations are policy-aware for Red UI.
//!
//! These tests pin the public contract carved out by the issue:
//!
//! 1. DDL actions used by Red UI have stable authorization action names
//!    (`create`, `alter`, `drop`, plus the broader fallback verbs
//!    `schema:write` / `schema:admin`). All five appear in the action
//!    catalog (and therefore in the `red.policy.actions` virtual table
//!    used by Red UI's policy authoring surface).
//! 2. Allowed DDL proceeds when the principal has the required grant.
//! 3. Denied DDL returns a structured `principal=… action=… resource=…
//!    denied by IAM policy` reason that Red UI can render directly.
//! 4. Both the specific per-collection verbs and the broader
//!    `schema:write` / `schema:admin` fallbacks gate their respective
//!    operations.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime_with_auth() -> (RedDBRuntime, Arc<AuthStore>, support::TempDataDir) {
    let dir = support::temp_data_dir("e2e-issue-753");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(dir.join("data.rdb")))
        .expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    // Strict posture so DDL policies must explicitly Allow.
    store.set_enforcement_mode(reddb::auth::enforcement_mode::PolicyEnforcementMode::PolicyOnly);
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Write).unwrap();
    rt.set_auth_store(Arc::clone(&store));
    (rt, store, dir)
}

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

fn attach(store: &AuthStore, id: &str, statements: &str) {
    let policy = format!(r#"{{"id":"{id}","version":1,"statements":{statements}}}"#);
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

fn err_of<T: std::fmt::Debug>(r: Result<T, reddb::RedDBError>) -> String {
    format!("{:?}", r.unwrap_err())
}

#[test]
fn ddl_action_names_are_advertised_in_the_action_catalog() {
    use reddb::auth::action_catalog::{is_valid_action, lookup, ActionCategory};
    for name in &["create", "alter", "drop", "truncate"] {
        assert!(is_valid_action(name), "{name} must be a known action");
    }
    let schema_write = lookup("schema:write").expect("schema:write must exist");
    assert_eq!(schema_write.category, ActionCategory::Schema);
    let schema_admin = lookup("schema:admin").expect("schema:admin must exist");
    assert_eq!(schema_admin.category, ActionCategory::Admin);
}

#[test]
fn allowed_create_table_proceeds_with_grant() {
    let (rt, store, _dir) = runtime_with_auth();
    attach(
        &store,
        "alice-create-users",
        r#"[
            {"effect":"allow","actions":["create"],"resources":["collection:users"]}
        ]"#,
    );

    let result = as_user("alice", Role::Write, || {
        rt.execute_query("CREATE TABLE users (id INT, name TEXT)")
    });
    assert!(result.is_ok(), "CREATE TABLE should succeed: {result:?}");
    assert!(rt.db().collection_contract("users").is_some());
}

#[test]
fn denied_create_table_returns_structured_ui_safe_reason() {
    let (rt, store, _dir) = runtime_with_auth();
    attach(
        &store,
        "alice-no-ddl",
        r#"[{"effect":"allow","actions":["select"],"resources":["*"]}]"#,
    );

    let err = err_of(as_user("alice", Role::Write, || {
        rt.execute_query("CREATE TABLE accounts (id INT)")
    }));
    // The error format is the public Red UI contract — keep it parseable.
    assert!(err.contains("action=`create`"), "got {err}");
    assert!(err.contains("resource=`collection:accounts`"), "got {err}");
    assert!(err.contains("denied by IAM policy"), "got {err}");
    assert!(
        rt.db().collection_contract("accounts").is_none(),
        "denied DDL must not create the collection"
    );
}

#[test]
fn allowed_alter_proceeds_and_explicit_deny_is_structured() {
    let (rt, store, _dir) = runtime_with_auth();
    rt.execute_query("CREATE TABLE accounts (id INT)").unwrap();

    // Two policies: one that allows alter on accounts, one that denies it.
    // The principal carries both, so explicit Deny wins.
    attach(
        &store,
        "alice-alter-and-deny",
        r#"[
            {"effect":"allow","actions":["alter"],"resources":["collection:accounts"]},
            {"effect":"deny","actions":["alter"],"resources":["collection:accounts"]}
        ]"#,
    );
    let err = err_of(as_user("alice", Role::Write, || {
        rt.execute_query("ALTER TABLE accounts ADD COLUMN status TEXT")
    }));
    assert!(err.contains("action=`alter`"), "got {err}");
    assert!(err.contains("resource=`collection:accounts`"), "got {err}");

    // Now flip to a clean allow — same operation should succeed.
    attach(
        &store,
        "alice-alter-only",
        r#"[{"effect":"allow","actions":["alter"],"resources":["collection:accounts"]}]"#,
    );
    // Remove the deny policy so the second invocation sees only the allow.
    store.delete_policy("alice-alter-and-deny").unwrap();

    as_user("alice", Role::Write, || {
        rt.execute_query("ALTER TABLE accounts ADD COLUMN status TEXT")
    })
    .expect("alter with grant should succeed");
}

#[test]
fn denied_create_index_returns_structured_reason_on_the_indexed_table() {
    let (rt, store, _dir) = runtime_with_auth();
    rt.execute_query("CREATE TABLE orders (id INT, status TEXT)")
        .unwrap();
    attach(
        &store,
        "alice-select-only",
        r#"[{"effect":"allow","actions":["select"],"resources":["table:orders"]}]"#,
    );

    let err = err_of(as_user("alice", Role::Write, || {
        rt.execute_query("CREATE INDEX orders_status_idx ON orders (status)")
    }));
    // The resource the policy is checked against is the parent table,
    // because that is the unit Red UI hands to operators when they
    // author a fine-grained DDL policy.
    assert!(err.contains("action=`create`"), "got {err}");
    assert!(err.contains("resource=`collection:orders`"), "got {err}");
}

#[test]
fn schema_admin_fallback_gates_create_schema() {
    // No allow → DefaultDeny under PolicyOnly → structured reason.
    let (rt, store, _dir) = runtime_with_auth();
    attach(
        &store,
        "alice-select-only",
        r#"[{"effect":"allow","actions":["select"],"resources":["*"]}]"#,
    );

    let err = err_of(as_user("alice", Role::Admin, || {
        rt.execute_query("CREATE SCHEMA reporting")
    }));
    assert!(err.contains("action=`schema:admin`"), "got {err}");
    assert!(err.contains("resource=`schema:reporting`"), "got {err}");
    assert!(err.contains("denied by IAM policy"), "got {err}");
}

#[test]
fn schema_admin_grant_allows_create_schema() {
    let (rt, store, _dir) = runtime_with_auth();
    attach(
        &store,
        "alice-schema-admin",
        r#"[{"effect":"allow","actions":["schema:admin"],"resources":["schema:reporting"]}]"#,
    );

    let result = as_user("alice", Role::Admin, || {
        rt.execute_query("CREATE SCHEMA reporting")
    });
    assert!(
        result.is_ok(),
        "CREATE SCHEMA with schema:admin grant should succeed: {result:?}"
    );
}
