use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn temp_db_path(name: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb_{name}_{unique}.rdb"))
}

fn cleanup_related(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let Some(stem) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let Some(name) = entry_path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name == stem || name.starts_with(&format!("{stem}-")) {
                let _ = std::fs::remove_file(&entry_path);
                let _ = std::fs::remove_dir_all(&entry_path);
            }
        }
    }
}

fn open_runtime_with_vault(path: &Path, passphrase: &str) -> (RedDBRuntime, Arc<AuthStore>) {
    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(path)).expect("runtime should open");
    let pager = Arc::clone(
        rt.db()
            .store()
            .pager()
            .expect("persistent runtime should expose pager"),
    );
    let auth = Arc::new(
        AuthStore::with_vault(AuthConfig::default(), pager, Some(passphrase))
            .expect("vault should open"),
    );
    rt.set_auth_store(Arc::clone(&auth));
    (rt, auth)
}

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

fn attach_user_policy(auth: &AuthStore, user: &str, id: &str, statements: &str) {
    let policy = format!(
        r#"{{
        "id":"{id}",
        "version":1,
        "statements":{statements}
    }}"#
    );
    auth.put_policy(reddb::auth::policies::Policy::from_json_str(&policy).unwrap())
        .unwrap();
    auth.attach_policy(
        reddb::auth::store::PrincipalRef::User(UserId::platform(user)),
        id,
    )
    .unwrap();
}

fn field<'a>(row: &'a reddb::storage::query::unified::UnifiedRecord, name: &str) -> &'a Value {
    row.get(name)
        .unwrap_or_else(|| panic!("missing field {name}: {row:?}"))
}

#[test]
fn config_secret_ref_get_is_reference_and_resolve_is_explicit_authorized_and_audited() {
    let path = temp_db_path("config_secret_ref_326");
    cleanup_related(&path);

    let secret = "vault_plaintext_probe_326";
    let (rt, auth) = open_runtime_with_vault(&path, "vault-pass-326");
    auth.create_user("alice", "p", Role::Write).unwrap();
    auth.create_user("bob", "p", Role::Write).unwrap();

    rt.execute_query("CREATE VAULT secrets WITH OWN MASTER KEY")
        .expect("create vault");
    rt.execute_query(&format!("VAULT PUT secrets.api_key = '{secret}'"))
        .expect("vault put");
    rt.execute_query("PUT CONFIG app api_key = SECRET_REF(vault, secrets.api_key)")
        .expect("put config secret ref");

    let get = rt
        .execute_query("GET CONFIG app api_key")
        .expect("get config secret ref");
    let Value::Json(bytes) = field(&get.result.records[0], "value") else {
        panic!("GET CONFIG must return a structured SecretRef");
    };
    let reference: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(reference["type"], "secret_ref");
    assert_eq!(reference["store"], "vault");
    assert_eq!(reference["collection"], "secrets");
    assert_eq!(reference["key"], "api_key");
    assert!(
        !format!("{:?}", get.result.records).contains(secret),
        "GET CONFIG must not resolve referenced plaintext"
    );

    attach_user_policy(
        &auth,
        "bob",
        "vault-only",
        r#"[
            {"effect":"allow","actions":["vault:read"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    let config_denied = as_user("bob", Role::Write, || {
        rt.execute_query("RESOLVE CONFIG app api_key")
    })
    .expect_err("resolve without config:read must fail");
    let config_denied = config_denied.to_string();
    assert!(config_denied.contains("config:read"), "{config_denied}");
    assert!(!config_denied.contains(secret));

    attach_user_policy(
        &auth,
        "alice",
        "config-read-only",
        r#"[
            {"effect":"allow","actions":["config:read"],"resources":["config:app.api_key"]}
        ]"#,
    );
    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("RESOLVE CONFIG app api_key")
    })
    .expect_err("resolve without vault:read must fail");
    let denied = denied.to_string();
    assert!(denied.contains("vault:read"), "{denied}");
    assert!(!denied.contains(secret));

    attach_user_policy(
        &auth,
        "alice",
        "vault-unseal",
        r#"[
            {"effect":"allow","actions":["vault:read"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    let resolved = as_user("alice", Role::Write, || {
        rt.execute_query("RESOLVE CONFIG app api_key")
    })
    .expect("resolve with config read and vault unseal should pass");
    assert_eq!(
        resolved.result.records[0].get("value"),
        Some(&Value::text(secret))
    );

    rt.execute_query("PUT CONFIG app missing_api_key = SECRET_REF(vault, secrets.missing)")
        .expect("put missing secret ref");
    attach_user_policy(
        &auth,
        "alice",
        "config-read-missing",
        r#"[
            {"effect":"allow","actions":["config:read"],"resources":["config:app.missing_api_key"]},
            {"effect":"allow","actions":["vault:read"],"resources":["vault:secrets.missing"]}
        ]"#,
    );
    let missing = as_user("alice", Role::Write, || {
        rt.execute_query("RESOLVE CONFIG app missing_api_key")
    })
    .expect_err("missing target should be typed");
    let missing = missing.to_string();
    assert!(missing.contains("not found"), "{missing}");
    assert!(!missing.contains(secret), "{missing}");

    assert!(rt.audit_log().wait_idle(std::time::Duration::from_secs(2)));
    let audit_body = std::fs::read_to_string(rt.audit_log().path()).unwrap_or_default();
    assert!(audit_body.contains("config/resolve"));
    assert!(audit_body.contains("vault/unseal"));
    assert!(audit_body.contains("\"outcome\":\"denied\""));
    assert!(audit_body.contains("\"outcome\":\"success\""));
    assert!(audit_body.contains("app.api_key"));
    assert!(audit_body.contains("secrets.api_key"));
    assert!(!audit_body.contains(secret));

    cleanup_related(&path);
}
