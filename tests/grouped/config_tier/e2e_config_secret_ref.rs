use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::storage::schema::Value;
use reddb::storage::StorageDeployPreset;
use reddb::{RedDBOptions, RedDBRuntime};

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

fn unique_ident(prefix: &str) -> String {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}_{unique}")
}

fn open_runtime_with_vault(path: &Path, passphrase: &str) -> (RedDBRuntime, Arc<AuthStore>) {
    let options = RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("serverless storage profile should expose pager");
    let rt = RedDBRuntime::with_options(options).expect("runtime should open");
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

fn secret_ref_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[test]
fn config_secret_ref_get_is_reference_and_resolve_is_explicit_authorized_and_audited() {
    let _guard = secret_ref_test_lock().lock().unwrap();
    let path = support::temp_db_file("config-secret-ref-326");

    let secret = "vault_plaintext_probe_326";
    let (rt, auth) = open_runtime_with_vault(path.path(), "vault-pass-326");
    let app = unique_ident("app");
    let secrets = unique_ident("secrets");
    let alice = unique_ident("alice");
    let bob = unique_ident("bob");
    auth.create_user(&alice, "p", Role::Write).unwrap();
    auth.create_user(&bob, "p", Role::Write).unwrap();

    rt.execute_query(&format!("CREATE VAULT {secrets} WITH OWN MASTER KEY"))
        .expect("create vault");
    rt.execute_query(&format!("VAULT PUT {secrets}.api_key = '{secret}'"))
        .expect("vault put");
    rt.execute_query(&format!(
        "PUT CONFIG {app} api_key = SECRET_REF(vault, {secrets}.api_key)"
    ))
    .expect("put config secret ref");

    let get = rt
        .execute_query(&format!("GET CONFIG {app} api_key"))
        .expect("get config secret ref");
    let Value::Json(bytes) = field(&get.result.records[0], "value") else {
        panic!("GET CONFIG must return a structured SecretRef");
    };
    let reference: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(reference["type"], "secret_ref");
    assert_eq!(reference["store"], "vault");
    assert_eq!(reference["collection"], secrets.as_str());
    assert_eq!(reference["key"], "api_key");
    assert!(
        !format!("{:?}", get.result.records).contains(secret),
        "GET CONFIG must not resolve referenced plaintext"
    );

    attach_user_policy(
        &auth,
        &bob,
        "vault-only",
        &format!(
            r#"[
            {{"effect":"allow","actions":["vault:read"],"resources":["vault:{secrets}.api_key"]}}
        ]"#
        ),
    );
    let config_denied = as_user(&bob, Role::Write, || {
        rt.execute_query(&format!("RESOLVE CONFIG {app} api_key"))
    })
    .expect_err("resolve without config:read must fail");
    let config_denied = config_denied.to_string();
    assert!(config_denied.contains("config:read"), "{config_denied}");
    assert!(!config_denied.contains(secret));

    attach_user_policy(
        &auth,
        &alice,
        "config-read-only",
        &format!(
            r#"[
            {{"effect":"allow","actions":["config:read"],"resources":["config:{app}.api_key"]}}
        ]"#
        ),
    );
    let denied = as_user(&alice, Role::Write, || {
        rt.execute_query(&format!("RESOLVE CONFIG {app} api_key"))
    })
    .expect_err("resolve without vault:read must fail");
    let denied = denied.to_string();
    assert!(denied.contains("vault:read"), "{denied}");
    assert!(!denied.contains(secret));

    attach_user_policy(
        &auth,
        &alice,
        "vault-unseal",
        &format!(
            r#"[
            {{"effect":"allow","actions":["vault:read"],"resources":["vault:{secrets}.api_key"]}}
        ]"#
        ),
    );
    let resolved = as_user(&alice, Role::Write, || {
        rt.execute_query(&format!("RESOLVE CONFIG {app} api_key"))
    })
    .expect("resolve with config read and vault unseal should pass");
    assert_eq!(
        resolved.result.records[0].get("value"),
        Some(&Value::text(secret))
    );

    as_user(&alice, Role::Admin, || {
        rt.execute_query(&format!(
            "PUT CONFIG {app} missing_api_key = SECRET_REF(vault, {secrets}.missing)"
        ))
    })
    .expect("put missing secret ref");
    attach_user_policy(
        &auth,
        &alice,
        "config-read-missing",
        &format!(
            r#"[
            {{"effect":"allow","actions":["config:read"],"resources":["config:{app}.missing_api_key"]}},
            {{"effect":"allow","actions":["vault:read"],"resources":["vault:{secrets}.missing"]}}
        ]"#
        ),
    );
    let missing = as_user(&alice, Role::Write, || {
        rt.execute_query(&format!("RESOLVE CONFIG {app} missing_api_key"))
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
    assert!(audit_body.contains(&format!("{app}.api_key")));
    assert!(audit_body.contains(&format!("{secrets}.api_key")));
    assert!(!audit_body.contains(secret));
}

/// `SecretRefGuard` — depth-2 chains must be rejected at config write time
/// with a structured error naming both the offending config key and the
/// vault target it pointed at. Issue #708 acceptance: depth-2 rejected at
/// write + error message includes both keys.
#[test]
fn secret_ref_guard_rejects_depth_two_chain_at_write() {
    let _guard = secret_ref_test_lock().lock().unwrap();
    let path = support::temp_db_file("secret-ref-guard-708-depth2");

    let (rt, _auth) = open_runtime_with_vault(path.path(), "vault-pass-708-d2");
    let app = unique_ident("app");
    let secrets = unique_ident("secrets");

    rt.execute_query(&format!("CREATE VAULT {secrets} WITH OWN MASTER KEY"))
        .expect("create vault");

    // Plant a vault entry whose unsealed value is itself a secret_ref
    // JSON object. This is the "legacy / hostile" precondition the guard
    // exists to catch — a chain link in the store.
    rt.execute_query(&format!(
        r#"VAULT PUT {secrets}.chain = {{"type":"secret_ref","store":"vault","collection":"{secrets}","key":"api_key"}}"#
    ))
    .expect("put chained vault entry");

    let err = rt
        .execute_query(&format!(
            "PUT CONFIG {app} api_key = SECRET_REF(vault, {secrets}.chain)"
        ))
        .expect_err("depth-2 chain must be rejected at write");
    let msg = err.to_string();
    assert!(
        msg.contains("secret_ref chain rejected"),
        "missing rejection marker: {msg}"
    );
    assert!(
        msg.contains(&format!("{app}.api_key")),
        "missing source key: {msg}"
    );
    assert!(
        msg.contains(&format!("{secrets}.chain")),
        "missing target key: {msg}"
    );
}

/// `SecretRefGuard` — a cyclic write (vault target points back into a
/// `secret_ref`) is rejected with the same structured error. Cycle
/// detection becomes trivial because depth is capped at 1.
#[test]
fn secret_ref_guard_rejects_cycle_at_write() {
    let _guard = secret_ref_test_lock().lock().unwrap();
    let path = support::temp_db_file("secret-ref-guard-708-cycle");

    let (rt, _auth) = open_runtime_with_vault(path.path(), "vault-pass-708-cyc");
    let app = unique_ident("app");
    let secrets = unique_ident("secrets");

    rt.execute_query(&format!("CREATE VAULT {secrets} WITH OWN MASTER KEY"))
        .expect("create vault");

    // A vault entry whose unsealed value claims to be a secret_ref
    // pointing back at itself. The write guard treats any secret_ref-shaped
    // target as a chain — depth-1 cap collapses cycles into the same case.
    rt.execute_query(&format!(
        r#"VAULT PUT {secrets}.loop = {{"type":"secret_ref","store":"vault","collection":"{secrets}","key":"loop"}}"#
    ))
    .expect("put cyclic vault entry");

    let err = rt
        .execute_query(&format!(
            "PUT CONFIG {app} loop_key = SECRET_REF(vault, {secrets}.loop)"
        ))
        .expect_err("cyclic ref must be rejected at write");
    let msg = err.to_string();
    assert!(msg.contains("secret_ref chain rejected"), "{msg}");
    assert!(
        msg.contains(&format!("{app}.loop_key")),
        "missing source key: {msg}"
    );
    assert!(
        msg.contains(&format!("{secrets}.loop")),
        "missing target key: {msg}"
    );
}

/// `SecretRefGuard` — defence-in-depth at read. If a chain somehow exists
/// in the store (e.g. vault rotated to a secret_ref shape after the config
/// write was accepted), the resolver returns the structured error instead
/// of recursing. Issue #708 acceptance: resolver backstop + integration
/// coverage through the AI/credential resolution path.
#[test]
fn secret_ref_guard_read_backstop_when_store_contains_chain() {
    let _guard = secret_ref_test_lock().lock().unwrap();
    let path = support::temp_db_file("secret-ref-guard-708-read");

    let (rt, _auth) = open_runtime_with_vault(path.path(), "vault-pass-708-r");
    let app = unique_ident("app");
    let secrets = unique_ident("secrets");

    rt.execute_query(&format!("CREATE VAULT {secrets} WITH OWN MASTER KEY"))
        .expect("create vault");

    // Seed a healthy, depth-1 secret. Config write succeeds because the
    // target is plaintext at this point.
    rt.execute_query(&format!("VAULT PUT {secrets}.api_key = 'sk-original-708'"))
        .expect("seed plaintext vault");
    rt.execute_query(&format!(
        "PUT CONFIG {app} api_key = SECRET_REF(vault, {secrets}.api_key)"
    ))
    .expect("put depth-1 config secret_ref");

    // Mutate vault out from under the config ref so the store contains a
    // chain. The write-time guard cannot intervene because the config
    // write predates the rotation; the resolver-side backstop must catch
    // it. The resolve query has not run yet, so the result cache cannot
    // mask the outcome.
    rt.execute_query(&format!(
        r#"VAULT ROTATE {secrets}.api_key = {{"type":"secret_ref","store":"vault","collection":"{secrets}","key":"deeper"}}"#
    ))
    .expect("rotate vault to a secret_ref shape");

    let err = rt
        .execute_query(&format!("RESOLVE CONFIG {app} api_key"))
        .expect_err("resolver must refuse to follow a chain");
    let msg = err.to_string();
    assert!(msg.contains("secret_ref chain rejected"), "{msg}");
    assert!(
        msg.contains(&format!("{app}.api_key")),
        "missing source key: {msg}"
    );
    assert!(
        msg.contains(&format!("{secrets}.api_key")),
        "missing target key: {msg}"
    );
}

/// `SecretRefGuard` — depth-1 references resolve normally and are not
/// flagged by the new write or read checks. Regression guard against the
/// guard tripping on healthy inputs.
#[test]
fn secret_ref_guard_allows_depth_one_reference() {
    let _guard = secret_ref_test_lock().lock().unwrap();
    let path = support::temp_db_file("secret-ref-guard-708-ok");

    let (rt, _auth) = open_runtime_with_vault(path.path(), "vault-pass-708-ok");
    let app = unique_ident("app");
    let secrets = unique_ident("secrets");

    rt.execute_query(&format!("CREATE VAULT {secrets} WITH OWN MASTER KEY"))
        .expect("create vault");
    rt.execute_query(&format!("VAULT PUT {secrets}.api_key = 'sk-flat-708'"))
        .expect("seed plaintext vault");

    rt.execute_query(&format!(
        "PUT CONFIG {app} api_key = SECRET_REF(vault, {secrets}.api_key)"
    ))
    .expect("depth-1 config write must succeed");

    let resolved = rt
        .execute_query(&format!("RESOLVE CONFIG {app} api_key"))
        .expect("depth-1 resolve must succeed");
    assert_eq!(
        resolved.result.records[0].get("value"),
        Some(&Value::text("sk-flat-708".to_string()))
    );
}
