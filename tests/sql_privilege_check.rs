//! Privilege-check semantics: granted users can read, ungranted users
//! get `PermissionDenied`. Drives the algorithm in
//! `auth::privileges::check_grant` directly so the test does not depend
//! on the runtime dispatch wiring (which is exercised separately in
//! `sql_grant_revoke.rs`).

use reddb::auth::privileges::{
    check_grant, Action, AuthzContext, Grant, GrantPrincipal, GrantsView, PermissionCache, Resource,
};
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};

fn ctx<'a>(user: &'a str, role: Role) -> AuthzContext<'a> {
    AuthzContext {
        principal: user,
        effective_role: role,
        tenant: None,
    }
}

fn grant_select_to(user: &str, table: &str) -> Grant {
    Grant::single(
        GrantPrincipal::User(UserId::platform(user)),
        Resource::table_from_name(table),
        Action::Select,
        "admin".into(),
        0,
        None,
    )
}

#[test]
fn admin_bypasses_explicit_check() {
    let view = GrantsView {
        user_grants: &[],
        public_grants: &[],
    };
    assert!(check_grant(
        &ctx("root", Role::Admin),
        Action::Delete,
        &Resource::Database,
        &view
    )
    .is_ok());
}

#[test]
fn legacy_fallback_when_no_grants() {
    let view = GrantsView {
        user_grants: &[],
        public_grants: &[],
    };
    // Read role can SELECT (legacy behaviour for a pre-grant deployment).
    assert!(check_grant(
        &ctx("alice", Role::Read),
        Action::Select,
        &Resource::table_from_name("orders"),
        &view
    )
    .is_ok());
    // Read role cannot INSERT.
    assert!(check_grant(
        &ctx("alice", Role::Read),
        Action::Insert,
        &Resource::table_from_name("orders"),
        &view
    )
    .is_err());
}

#[test]
fn granted_user_can_select_resource() {
    let g = grant_select_to("alice", "orders");
    let view = GrantsView {
        user_grants: std::slice::from_ref(&g),
        public_grants: &[],
    };
    assert!(check_grant(
        &ctx("alice", Role::Read),
        Action::Select,
        &Resource::table_from_name("orders"),
        &view
    )
    .is_ok());
}

#[test]
fn ungranted_user_gets_permission_denied() {
    // A non-empty, non-matching per-user grant disables legacy fallback
    // without authorising the requested resource.
    let g_alice = grant_select_to("alice", "other_orders");
    let view = GrantsView {
        user_grants: std::slice::from_ref(&g_alice),
        public_grants: &[],
    };
    let err = check_grant(
        &ctx("bob", Role::Read),
        Action::Select,
        &Resource::table_from_name("orders"),
        &view,
    )
    .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("permission denied"),
        "expected permission denied, got {s}"
    );
}

#[test]
fn permission_cache_populates_on_allow() {
    let store = AuthStore::new(AuthConfig::default());
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Read).unwrap();

    let admin = UserId::platform("admin");
    store
        .grant(
            &admin,
            Role::Admin,
            GrantPrincipal::User(UserId::platform("alice")),
            Resource::table_from_name("orders"),
            vec![Action::Select],
            false,
            None,
        )
        .unwrap();

    let ctx = ctx("alice", Role::Read);
    let r = Resource::table_from_name("orders");
    // First check populates the cache.
    assert!(store.check_grant(&ctx, Action::Select, &r).is_ok());
    // Second check hits the cache and still allows.
    assert!(store.check_grant(&ctx, Action::Select, &r).is_ok());
    // INSERT was never granted — denied.
    assert!(store.check_grant(&ctx, Action::Insert, &r).is_err());
}

#[test]
fn revoke_invalidates_cache() {
    let store = AuthStore::new(AuthConfig::default());
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Read).unwrap();

    let admin = UserId::platform("admin");
    let alice = UserId::platform("alice");
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

    let ctx_alice = ctx("alice", Role::Read);
    let r = Resource::table_from_name("orders");
    assert!(store.check_grant(&ctx_alice, Action::Select, &r).is_ok());

    // Revoke and confirm the cache is rebuilt as denying.
    store
        .revoke(
            Role::Admin,
            &GrantPrincipal::User(alice.clone()),
            &r,
            &[Action::Select],
        )
        .unwrap();
    // Once the grants disappear the legacy fallback kicks in for Read
    // role — Read is allowed to Select even without explicit grants
    // when no other grants exist anywhere. So we must add a sentinel
    // grant for someone else so the table isn't fully ungated.
    store
        .grant(
            &admin,
            Role::Admin,
            GrantPrincipal::User(UserId::platform("bob")),
            Resource::table_from_name("hosts"),
            vec![Action::Select],
            false,
            None,
        )
        .unwrap();
    let err = store
        .check_grant(&ctx_alice, Action::Select, &r)
        .unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("permission denied"), "expected denial, got {s}");
}

#[test]
fn permission_cache_unit_expansion() {
    // ALL → every concrete action populated.
    let mut actions = std::collections::BTreeSet::new();
    actions.insert(Action::All);
    let g = Grant {
        principal: GrantPrincipal::User(UserId::platform("alice")),
        resource: Resource::table_from_name("orders"),
        actions,
        with_grant_option: false,
        granted_by: "admin".into(),
        granted_at: 0,
        tenant: None,
        columns: None,
    };
    let cache = PermissionCache::build(std::slice::from_ref(&g), &[]);
    assert!(cache.allows(&Resource::table_from_name("orders"), Action::Select));
    assert!(cache.allows(&Resource::table_from_name("orders"), Action::Insert));
    assert!(!cache.allows(&Resource::table_from_name("nope"), Action::Select));
}
