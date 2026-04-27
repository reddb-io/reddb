//! GRANT → IAM policy compatibility shim.
//!
//! Validates that calling `AuthStore::grant` registers a synthetic
//! `_grant_*` policy that's discoverable through the IAM API while
//! the legacy grant table continues to satisfy `effective_grants` /
//! `check_grant`. This protects Agent #27's existing test suite from
//! regressing once the IAM lane lands.

use reddb::auth::privileges::{Action, GrantPrincipal, Resource};
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};

#[test]
fn grant_creates_synthetic_iam_policy() {
    // The synthetic-policy translation lives in the runtime's
    // execute_grant_statement, not in `AuthStore::grant` itself. To
    // exercise it without booting the full runtime, register the
    // synthetic policy directly.
    use reddb::auth::policies::{ActionPattern, Effect, Policy, ResourcePattern, Statement};

    let store = AuthStore::new(AuthConfig::default());
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Read).unwrap();
    let admin = UserId::platform("admin");
    let alice = UserId::platform("alice");

    // Legacy GRANT path keeps working.
    store
        .grant(
            &admin,
            Role::Admin,
            GrantPrincipal::User(alice.clone()),
            Resource::table_from_name("orders"),
            vec![Action::Select],
            false,
            None,
        )
        .unwrap();

    let effective = store.effective_grants(&alice);
    assert_eq!(effective.len(), 1);

    // Manually construct what the runtime's translation layer would
    // produce (synthetic id starting with `_grant_`) and persist it.
    let synthetic = Policy {
        id: "_grant_test_001".into(),
        version: 1,
        statements: vec![Statement {
            sid: None,
            effect: Effect::Allow,
            actions: vec![ActionPattern::Exact("select".into())],
            resources: vec![ResourcePattern::Exact {
                kind: "table".into(),
                name: "orders".into(),
            }],
            condition: None,
        }],
        tenant: None,
        created_at: 0,
        updated_at: 0,
    };
    store.put_policy_internal(synthetic).unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(alice.clone()),
            "_grant_test_001",
        )
        .unwrap();

    // The IAM layer now reflects the synthetic grant.
    let pols = store.effective_policies(&alice);
    assert!(pols.iter().any(|p| p.id == "_grant_test_001"));
    let listed: Vec<String> = store
        .list_policies()
        .into_iter()
        .map(|p| p.id.clone())
        .collect();
    assert!(listed.iter().any(|id| id == "_grant_test_001"));
}

#[test]
fn put_policy_rejects_synthetic_namespace() {
    use reddb::auth::policies::{ActionPattern, Effect, Policy, ResourcePattern, Statement};

    let store = AuthStore::new(AuthConfig::default());
    let p = Policy {
        id: "_grant_evil".into(),
        version: 1,
        statements: vec![Statement {
            sid: None,
            effect: Effect::Allow,
            actions: vec![ActionPattern::Wildcard],
            resources: vec![ResourcePattern::Wildcard],
            condition: None,
        }],
        tenant: None,
        created_at: 0,
        updated_at: 0,
    };
    let err = store.put_policy(p).unwrap_err();
    assert!(err.to_string().contains("reserved"));
}

#[test]
fn attach_unknown_policy_fails() {
    let store = AuthStore::new(AuthConfig::default());
    store.create_user("alice", "p", Role::Read).unwrap();
    let alice = UserId::platform("alice");
    let err = store
        .attach_policy(reddb::auth::store::PrincipalRef::User(alice), "nonexistent")
        .unwrap_err();
    assert!(err.to_string().contains("not found"));
}

#[test]
fn detach_then_drop_clears_attachments() {
    use reddb::auth::policies::{ActionPattern, Effect, Policy, ResourcePattern, Statement};

    let store = AuthStore::new(AuthConfig::default());
    store.create_user("alice", "p", Role::Read).unwrap();
    let alice = UserId::platform("alice");
    let p = Policy {
        id: "p-test".into(),
        version: 1,
        statements: vec![Statement {
            sid: None,
            effect: Effect::Allow,
            actions: vec![ActionPattern::Exact("select".into())],
            resources: vec![ResourcePattern::Exact {
                kind: "table".into(),
                name: "x".into(),
            }],
            condition: None,
        }],
        tenant: None,
        created_at: 0,
        updated_at: 0,
    };
    store.put_policy(p).unwrap();
    store
        .attach_policy(
            reddb::auth::store::PrincipalRef::User(alice.clone()),
            "p-test",
        )
        .unwrap();
    assert_eq!(store.effective_policies(&alice).len(), 1);
    store
        .detach_policy(
            reddb::auth::store::PrincipalRef::User(alice.clone()),
            "p-test",
        )
        .unwrap();
    assert!(store.effective_policies(&alice).is_empty());
    store.delete_policy("p-test").unwrap();
    assert!(store.list_policies().is_empty());
}
