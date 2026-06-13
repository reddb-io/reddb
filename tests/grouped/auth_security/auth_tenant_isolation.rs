//! Tenant isolation: same `username` in two different tenants must
//! authenticate independently and never resolve to each other.
//!
//! These tests exercise the `AuthStore` directly so they don't depend
//! on a live HTTP server. The HTTP handler layer adds caller-tenant
//! clamping on top of these primitives (covered by HTTP smoke tests
//! in a follow-up).

use reddb::auth::store::AuthStore;
use reddb::auth::{AuthConfig, Role, UserId};

fn store() -> AuthStore {
    let cfg = AuthConfig {
        enabled: true,
        session_ttl_secs: 60,
        require_auth: true,
        auto_encrypt_storage: false,
        vault_enabled: false,
        cert: Default::default(),
        oauth: Default::default(),
    };
    AuthStore::new(cfg)
}

#[test]
fn same_username_two_tenants_are_distinct_identities() {
    let store = store();
    store
        .create_user_in_tenant(Some("acme"), "alice", "pw-acme", Role::Admin)
        .expect("create acme/alice");
    store
        .create_user_in_tenant(Some("globex"), "alice", "pw-globex", Role::Read)
        .expect("create globex/alice");

    // Two distinct entries.
    assert_eq!(store.list_users().len(), 2);

    // Each tenant authenticates its own.
    let session_acme = store
        .authenticate_in_tenant(Some("acme"), "alice", "pw-acme")
        .expect("acme login");
    assert_eq!(session_acme.tenant_id.as_deref(), Some("acme"));
    assert_eq!(session_acme.role, Role::Admin);

    let session_globex = store
        .authenticate_in_tenant(Some("globex"), "alice", "pw-globex")
        .expect("globex login");
    assert_eq!(session_globex.tenant_id.as_deref(), Some("globex"));
    assert_eq!(session_globex.role, Role::Read);

    // Cross-tenant credentials never resolve.
    assert!(store
        .authenticate_in_tenant(Some("acme"), "alice", "pw-globex")
        .is_err());
    assert!(store
        .authenticate_in_tenant(Some("globex"), "alice", "pw-acme")
        .is_err());

    // Session tokens carry the right tenant.
    let (id_acme, _) = store
        .validate_token_full(&session_acme.token)
        .expect("acme token");
    assert_eq!(id_acme, UserId::scoped("acme", "alice"));
    let (id_globex, _) = store
        .validate_token_full(&session_globex.token)
        .expect("globex token");
    assert_eq!(id_globex, UserId::scoped("globex", "alice"));
}

#[test]
fn delete_in_one_tenant_leaves_other_intact() {
    let store = store();
    store
        .create_user_in_tenant(Some("acme"), "alice", "pw", Role::Admin)
        .unwrap();
    store
        .create_user_in_tenant(Some("globex"), "alice", "pw", Role::Admin)
        .unwrap();

    store
        .delete_user_in_tenant(Some("acme"), "alice")
        .expect("delete acme/alice");

    // Acme alice gone.
    assert!(store
        .authenticate_in_tenant(Some("acme"), "alice", "pw")
        .is_err());
    assert!(store.get_user(Some("acme"), "alice").is_none());

    // Globex alice still alive.
    assert!(store
        .authenticate_in_tenant(Some("globex"), "alice", "pw")
        .is_ok());
    assert!(store.get_user(Some("globex"), "alice").is_some());
}

#[test]
fn platform_admin_and_tenant_admin_are_distinct() {
    let store = store();
    store
        .create_user("admin", "platform-pw", Role::Admin)
        .unwrap();
    store
        .create_user_in_tenant(Some("acme"), "admin", "tenant-pw", Role::Admin)
        .unwrap();

    let platform = store
        .authenticate("admin", "platform-pw")
        .expect("platform login");
    assert!(platform.tenant_id.is_none());

    let tenant = store
        .authenticate_in_tenant(Some("acme"), "admin", "tenant-pw")
        .expect("tenant login");
    assert_eq!(tenant.tenant_id.as_deref(), Some("acme"));

    // Platform creds don't unlock the tenant user and vice versa.
    assert!(store.authenticate("admin", "tenant-pw").is_err());
    assert!(store
        .authenticate_in_tenant(Some("acme"), "admin", "platform-pw")
        .is_err());
}

#[test]
fn list_users_scoped_filters_correctly() {
    let store = store();
    store.create_user("root", "pw", Role::Admin).unwrap();
    store
        .create_user_in_tenant(Some("acme"), "alice", "pw", Role::Read)
        .unwrap();
    store
        .create_user_in_tenant(Some("acme"), "bob", "pw", Role::Read)
        .unwrap();
    store
        .create_user_in_tenant(Some("globex"), "carol", "pw", Role::Write)
        .unwrap();

    // No filter: all 4.
    assert_eq!(store.list_users_scoped(None).len(), 4);

    // Platform-only: 1.
    let plat = store.list_users_scoped(Some(None));
    assert_eq!(plat.len(), 1);
    assert_eq!(plat[0].username, "root");
    assert!(plat[0].tenant_id.is_none());

    // Acme: 2.
    let acme = store.list_users_scoped(Some(Some("acme")));
    assert_eq!(acme.len(), 2);
    assert!(acme.iter().all(|u| u.tenant_id.as_deref() == Some("acme")));

    // Globex: 1.
    let globex = store.list_users_scoped(Some(Some("globex")));
    assert_eq!(globex.len(), 1);
    assert_eq!(globex[0].username, "carol");
}

#[test]
fn api_key_is_scoped_to_owner_tenant() {
    let store = store();
    store
        .create_user_in_tenant(Some("acme"), "alice", "pw", Role::Admin)
        .unwrap();
    store
        .create_user_in_tenant(Some("globex"), "alice", "pw", Role::Read)
        .unwrap();

    let key_acme = store
        .create_api_key_in_tenant(Some("acme"), "alice", "deploy", Role::Write)
        .expect("acme key");

    // The token resolves to acme/alice — never globex/alice.
    let (id, role) = store
        .validate_token_full(&key_acme.key)
        .expect("validate acme key");
    assert_eq!(id, UserId::scoped("acme", "alice"));
    assert_eq!(role, Role::Write);

    // Revoking only kills that one key.
    store.revoke_api_key(&key_acme.key).unwrap();
    assert!(store.validate_token_full(&key_acme.key).is_none());

    // Globex alice is untouched and can still log in.
    assert!(store
        .authenticate_in_tenant(Some("globex"), "alice", "pw")
        .is_ok());
}

#[test]
fn lookup_scram_verifier_global_only_finds_platform_user() {
    let store = store();
    store
        .create_user("alice", "platform-pw", Role::Admin)
        .unwrap();
    store
        .create_user_in_tenant(Some("acme"), "alice", "tenant-pw", Role::Admin)
        .unwrap();

    // Global helper resolves the platform user.
    let v = store.lookup_scram_verifier_global("alice");
    assert!(v.is_some());

    let v_platform = store
        .lookup_scram_verifier(&UserId::platform("alice"))
        .expect("platform alice has verifier");
    let v_acme = store
        .lookup_scram_verifier(&UserId::scoped("acme", "alice"))
        .expect("acme alice has verifier");

    // Independent salts + stored keys (different password material).
    assert_ne!(v_platform.salt, v_acme.salt);
    assert_ne!(v_platform.stored_key, v_acme.stored_key);
}
