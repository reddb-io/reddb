//! Integration tests for `MIGRATE POLICY MODE TO 'policy_only' [DRY RUN]`
//! (#714 / S5B). Validates the end-to-end SQL path: pre-flight delta
//! shape, the DRY RUN no-op, the refusal path, and the actual mode
//! flip when the delta is empty.

#[allow(dead_code)]
mod support;

use std::sync::Arc;

use reddb::auth::enforcement_mode::PolicyEnforcementMode;
use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime_with_auth() -> (support::TempDataDir, RedDBRuntime, Arc<AuthStore>) {
    let dir = support::temp_data_dir("iam-migrate-mode");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(dir.join("data.rdb")))
        .expect("runtime");
    let store = Arc::new(AuthStore::new(AuthConfig::default()));
    // Default for an existing install is LegacyRbac; pin it so the test
    // is explicit about the precondition.
    store.set_enforcement_mode(PolicyEnforcementMode::LegacyRbac);
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

fn seed_orders(rt: &RedDBRuntime) {
    rt.execute_query("CREATE TABLE orders (id INT, total INT)")
        .unwrap();
}

#[test]
fn dry_run_returns_delta_without_changing_mode() {
    let (_dir, rt, store) = runtime_with_auth();
    seed_orders(&rt);
    let result = as_user("admin", Role::Admin, || {
        rt.execute_query("MIGRATE POLICY MODE TO 'policy_only' DRY RUN")
    })
    .expect("dry-run should succeed");
    // alice (Write, no policies) loses every write action on `orders`.
    assert_eq!(result.statement, "migrate_policy_mode");
    assert!(
        !result.result.records.is_empty(),
        "expected at least one delta row, got {result:?}"
    );
    let columns: Vec<String> = result.result.columns.iter().cloned().collect();
    assert_eq!(
        columns,
        vec![
            "principal".to_string(),
            "role".to_string(),
            "action".to_string(),
            "resource_kind".to_string(),
            "resource_name".to_string(),
        ]
    );
    // Mode must NOT change.
    assert_eq!(store.enforcement_mode(), PolicyEnforcementMode::LegacyRbac);
}

#[test]
fn non_dry_run_refuses_with_delta() {
    let (_dir, rt, store) = runtime_with_auth();
    seed_orders(&rt);
    let result = as_user("admin", Role::Admin, || {
        rt.execute_query("MIGRATE POLICY MODE TO 'policy_only'")
    });
    let err = result.expect_err("non-dry-run with non-empty delta must refuse");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("MIGRATE POLICY MODE refused"),
        "wrong error: {msg}"
    );
    // Refusal must not have flipped the mode.
    assert_eq!(store.enforcement_mode(), PolicyEnforcementMode::LegacyRbac);
}

#[test]
fn non_dry_run_succeeds_when_delta_empty() {
    let (_dir, rt, store) = runtime_with_auth();
    // Attach an allow-all policy to alice so her delta is empty.
    let policy = reddb::auth::policies::Policy::from_json_str(
        r#"{"id":"p-alice-all","version":1,
            "statements":[{"effect":"allow","actions":["*"],"resources":["*"]}]}"#,
    )
    .unwrap();
    store.put_policy(policy).unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform("alice")),
            "p-alice-all",
        )
        .unwrap();
    // Same for admin so the Admin-role principal doesn't appear in the delta.
    let admin_policy = reddb::auth::policies::Policy::from_json_str(
        r#"{"id":"p-admin-all","version":1,
            "statements":[{"effect":"allow","actions":["*"],"resources":["*"]}]}"#,
    )
    .unwrap();
    store.put_policy(admin_policy).unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform("admin")),
            "p-admin-all",
        )
        .unwrap();
    seed_orders(&rt);
    let result = as_user("admin", Role::Admin, || {
        rt.execute_query("MIGRATE POLICY MODE TO 'policy_only'")
    })
    .expect("clean migration must succeed");
    assert_eq!(result.statement, "migrate_policy_mode");
    assert!(
        result.result.records.is_empty(),
        "successful migration returns no delta rows: {result:?}"
    );
    assert_eq!(store.enforcement_mode(), PolicyEnforcementMode::PolicyOnly);
}

#[test]
fn invalid_target_is_rejected() {
    let (_dir, rt, _store) = runtime_with_auth();
    let err = as_user("admin", Role::Admin, || {
        rt.execute_query("MIGRATE POLICY MODE TO 'banana' DRY RUN")
    })
    .expect_err("invalid target must be rejected");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("invalid target") || msg.contains("not supported"),
        "wrong error: {msg}"
    );
}

#[test]
fn legacy_rbac_as_target_is_rejected() {
    // Only `policy_only` is a valid destination — migrating back to
    // legacy_rbac is supported via direct config writes, not this
    // command.
    let (_dir, rt, _store) = runtime_with_auth();
    let err = as_user("admin", Role::Admin, || {
        rt.execute_query("MIGRATE POLICY MODE TO 'legacy_rbac' DRY RUN")
    })
    .expect_err("legacy_rbac target must be rejected");
    let msg = format!("{err:?}");
    assert!(msg.contains("not supported"), "wrong error: {msg}");
}
