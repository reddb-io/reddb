//! `ALTER USER` attribute enforcement.
//!
//! Confirms:
//!   * `ALTER USER ... VALID UNTIL '...'` rejects logins after the
//!     deadline (HTTP path: `authenticate_with_attrs`).
//!   * `ALTER USER ... CONNECTION LIMIT n` rejects logins past the
//!     live-session quota.
//!   * `ALTER USER ... DISABLE` flips `User.enabled` so even a valid
//!     password fails authentication.

use reddb::auth::privileges::UserAttributes;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::storage::query::{AlterUserAttribute, Parser, QueryExpr};

fn parse(sql: &str) -> QueryExpr {
    let mut p = Parser::new(sql).expect("parser construct");
    p.parse().expect("parse")
}

#[test]
fn alter_user_valid_until_parses() {
    let expr = parse("ALTER USER alice VALID UNTIL '2030-01-01'");
    let stmt = match expr {
        QueryExpr::AlterUser(s) => s,
        other => panic!("expected AlterUser, got {:?}", other),
    };
    assert_eq!(stmt.username, "alice");
    assert!(stmt.tenant.is_none());
    matches!(
        stmt.attributes[0],
        AlterUserAttribute::ValidUntil(ref s) if s == "2030-01-01"
    );
}

#[test]
fn alter_user_connection_limit_parses() {
    let expr = parse("ALTER USER alice CONNECTION LIMIT 5");
    let stmt = match expr {
        QueryExpr::AlterUser(s) => s,
        other => panic!("expected AlterUser, got {:?}", other),
    };
    matches!(stmt.attributes[0], AlterUserAttribute::ConnectionLimit(5));
}

#[test]
fn alter_user_enable_disable_parses() {
    let expr = parse("ALTER USER alice DISABLE");
    let stmt = match expr {
        QueryExpr::AlterUser(s) => s,
        other => panic!("expected AlterUser, got {:?}", other),
    };
    matches!(stmt.attributes[0], AlterUserAttribute::Disable);
}

#[test]
fn alter_user_search_path_parses() {
    let expr = parse("ALTER USER alice SET search_path = 'public,acme'");
    let stmt = match expr {
        QueryExpr::AlterUser(s) => s,
        other => panic!("expected AlterUser, got {:?}", other),
    };
    matches!(
        stmt.attributes[0],
        AlterUserAttribute::SetSearchPath(ref s) if s == "public,acme"
    );
}

#[test]
fn alter_user_group_membership_parses() {
    let expr = parse("ALTER USER alice ADD GROUP analysts");
    let stmt = match expr {
        QueryExpr::AlterUser(s) => s,
        other => panic!("expected AlterUser, got {:?}", other),
    };
    matches!(
        stmt.attributes[0],
        AlterUserAttribute::AddGroup(ref g) if g == "analysts"
    );

    let expr = parse("ALTER USER alice DROP GROUP analysts");
    let stmt = match expr {
        QueryExpr::AlterUser(s) => s,
        other => panic!("expected AlterUser, got {:?}", other),
    };
    matches!(
        stmt.attributes[0],
        AlterUserAttribute::DropGroup(ref g) if g == "analysts"
    );
}

#[test]
fn alter_user_tenant_qualified_parses() {
    let expr = parse("ALTER USER acme.alice DISABLE");
    let stmt = match expr {
        QueryExpr::AlterUser(s) => s,
        other => panic!("expected AlterUser, got {:?}", other),
    };
    assert_eq!(stmt.tenant.as_deref(), Some("acme"));
    assert_eq!(stmt.username, "alice");
}

#[test]
fn valid_until_blocks_login_after_deadline() {
    let store = AuthStore::new(AuthConfig {
        enabled: true,
        ..AuthConfig::default()
    });
    store.create_user("alice", "secret", Role::Read).unwrap();
    let id = UserId::platform("alice");

    // Set an already-expired deadline.
    let attrs = UserAttributes {
        valid_until: Some(1),
        ..Default::default()
    };
    store.set_user_attributes(&id, attrs).unwrap();

    let err = store
        .authenticate_with_attrs(None, "alice", "secret")
        .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("VALID UNTIL"),
        "expected VALID UNTIL error, got {s}"
    );
}

#[test]
fn valid_until_in_future_allows_login() {
    let store = AuthStore::new(AuthConfig {
        enabled: true,
        ..AuthConfig::default()
    });
    store.create_user("alice", "secret", Role::Read).unwrap();
    let id = UserId::platform("alice");

    // 100 years in the future.
    let far_future: u128 = 1_000_000_000_000_000;
    let attrs = UserAttributes {
        valid_until: Some(far_future),
        ..Default::default()
    };
    store.set_user_attributes(&id, attrs).unwrap();

    let session = store
        .authenticate_with_attrs(None, "alice", "secret")
        .expect("login should succeed before deadline");
    assert!(session.token.starts_with("rs_"));
}

#[test]
fn connection_limit_blocks_login_past_quota() {
    let store = AuthStore::new(AuthConfig {
        enabled: true,
        ..AuthConfig::default()
    });
    store.create_user("alice", "secret", Role::Read).unwrap();
    let id = UserId::platform("alice");

    let attrs = UserAttributes {
        connection_limit: Some(1),
        ..Default::default()
    };
    store.set_user_attributes(&id, attrs).unwrap();

    // First login fits within the quota.
    let _first = store
        .authenticate_with_attrs(None, "alice", "secret")
        .expect("first login should succeed");
    // Second login exceeds the limit.
    let err = store
        .authenticate_with_attrs(None, "alice", "secret")
        .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.contains("CONNECTION LIMIT"),
        "expected CONNECTION LIMIT error, got {s}"
    );

    // After decrementing, the second login should succeed.
    store.decrement_session_count(&id);
    let _second = store
        .authenticate_with_attrs(None, "alice", "secret")
        .expect("second login should succeed after slot frees");
}

#[test]
fn set_user_enabled_disables_login() {
    let store = AuthStore::new(AuthConfig {
        enabled: true,
        ..AuthConfig::default()
    });
    store.create_user("alice", "secret", Role::Read).unwrap();
    let id = UserId::platform("alice");

    // Disable the account.
    store.set_user_enabled(&id, false).unwrap();
    let err = store
        .authenticate_with_attrs(None, "alice", "secret")
        .unwrap_err();
    let s = format!("{err}");
    assert!(
        s.to_ascii_lowercase().contains("invalid")
            || s.to_ascii_lowercase().contains("credentials"),
        "expected invalid-credentials error, got {s}"
    );

    // Re-enable.
    store.set_user_enabled(&id, true).unwrap();
    let session = store
        .authenticate_with_attrs(None, "alice", "secret")
        .expect("login after re-enable should succeed");
    assert!(session.token.starts_with("rs_"));
}
