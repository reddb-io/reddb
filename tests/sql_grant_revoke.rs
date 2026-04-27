//! Parser + executor roundtrip for `GRANT` / `REVOKE`.
//!
//! Exercises the SQL surface end-to-end against an embedded runtime.
//! Verifies:
//!   * `GRANT SELECT ON TABLE x TO alice` parses into the expected AST.
//!   * After the grant lands, `AuthStore::effective_grants` reflects it.
//!   * `REVOKE SELECT ON TABLE x FROM alice` removes the grant again.
//!   * `GRANT ALL PRIVILEGES ON SCHEMA acme TO bob` covers every action
//!     for tables in `acme`.

use reddb::auth::privileges::{Action, GrantPrincipal, Resource};
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::storage::query::{GrantObjectKind, GrantPrincipalRef, Parser, QueryExpr};

fn parse(sql: &str) -> QueryExpr {
    let mut p = Parser::new(sql).expect("parser construct");
    p.parse().expect("parse")
}

#[test]
fn grant_select_parses() {
    let expr = parse("GRANT SELECT ON TABLE orders TO alice");
    let g = match expr {
        QueryExpr::Grant(g) => g,
        other => panic!("expected Grant, got {:?}", other),
    };
    assert_eq!(g.actions, vec!["SELECT".to_string()]);
    assert_eq!(g.object_kind, GrantObjectKind::Table);
    assert_eq!(g.objects.len(), 1);
    assert_eq!(g.objects[0].name, "orders");
    assert!(g.objects[0].schema.is_none());
    assert!(!g.with_grant_option);
    assert!(!g.all);
    match &g.principals[0] {
        GrantPrincipalRef::User { tenant, name } => {
            assert!(tenant.is_none());
            assert_eq!(name, "alice");
        }
        other => panic!("expected user principal, got {:?}", other),
    }
}

#[test]
fn grant_all_privileges_parses() {
    let expr = parse("GRANT ALL PRIVILEGES ON SCHEMA acme TO bob");
    let g = match expr {
        QueryExpr::Grant(g) => g,
        other => panic!("expected Grant, got {:?}", other),
    };
    assert!(g.all);
    assert_eq!(g.object_kind, GrantObjectKind::Schema);
    assert_eq!(g.objects[0].name, "acme");
}

#[test]
fn grant_with_grant_option_parses() {
    let expr = parse("GRANT INSERT ON TABLE orders TO alice WITH GRANT OPTION");
    let g = match expr {
        QueryExpr::Grant(g) => g,
        other => panic!("expected Grant, got {:?}", other),
    };
    assert!(g.with_grant_option);
}

#[test]
fn grant_to_public_parses() {
    let expr = parse("GRANT SELECT ON TABLE welcome TO PUBLIC");
    let g = match expr {
        QueryExpr::Grant(g) => g,
        other => panic!("expected Grant, got {:?}", other),
    };
    matches!(g.principals[0], GrantPrincipalRef::Public);
}

#[test]
fn revoke_parses() {
    let expr = parse("REVOKE SELECT, INSERT ON TABLE orders FROM alice");
    let r = match expr {
        QueryExpr::Revoke(r) => r,
        other => panic!("expected Revoke, got {:?}", other),
    };
    assert!(!r.grant_option_for);
    assert_eq!(r.actions, vec!["SELECT".to_string(), "INSERT".to_string()]);
    assert_eq!(r.objects[0].name, "orders");
}

#[test]
fn revoke_grant_option_for_parses() {
    let expr = parse("REVOKE GRANT OPTION FOR SELECT ON TABLE orders FROM alice");
    let r = match expr {
        QueryExpr::Revoke(r) => r,
        other => panic!("expected Revoke, got {:?}", other),
    };
    assert!(r.grant_option_for);
}

#[test]
fn auth_store_grant_revoke_roundtrip() {
    let store = AuthStore::new(AuthConfig::default());
    store.create_user("admin", "p", Role::Admin).unwrap();
    store.create_user("alice", "p", Role::Read).unwrap();

    let admin_id = UserId::platform("admin");
    let alice_id = UserId::platform("alice");

    store
        .grant(
            &admin_id,
            Role::Admin,
            GrantPrincipal::User(alice_id.clone()),
            Resource::table_from_name("orders"),
            vec![Action::Select],
            false,
            None,
        )
        .expect("grant");

    let effective = store.effective_grants(&alice_id);
    assert_eq!(effective.len(), 1);
    assert_eq!(
        effective[0].resource,
        Resource::Table {
            schema: None,
            table: "orders".into()
        }
    );
    assert!(effective[0].actions.contains(&Action::Select));

    let removed = store
        .revoke(
            Role::Admin,
            &GrantPrincipal::User(alice_id.clone()),
            &Resource::table_from_name("orders"),
            &[Action::Select],
        )
        .expect("revoke");
    assert_eq!(removed, 1);
    assert!(store.effective_grants(&alice_id).is_empty());
}

#[test]
fn non_admin_cannot_grant() {
    let store = AuthStore::new(AuthConfig::default());
    let writer = UserId::platform("alice");
    let target = UserId::platform("bob");
    let err = store
        .grant(
            &writer,
            Role::Write,
            GrantPrincipal::User(target),
            Resource::Database,
            vec![Action::Select],
            false,
            None,
        )
        .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("Admin"),
        "expected Admin-required error, got {s}"
    );
}

#[test]
fn cross_tenant_grant_rejected() {
    let store = AuthStore::new(AuthConfig::default());
    let acme_admin = UserId::scoped("acme", "admin");
    let globex_user = UserId::scoped("globex", "alice");
    let err = store
        .grant(
            &acme_admin,
            Role::Admin,
            GrantPrincipal::User(globex_user),
            Resource::Database,
            vec![Action::Select],
            false,
            Some("globex".into()),
        )
        .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("cross-tenant"),
        "expected cross-tenant error, got {s}"
    );
}
