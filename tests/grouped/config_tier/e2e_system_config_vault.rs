#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

const TEST_CERTIFICATE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn runtime(name: &str) -> (support::TempDbFile, RedDBRuntime) {
    let path = support::temp_db_file(name);
    let rt = crate::config_tier_shared::open_runtime_with_options(
        RedDBOptions::persistent(path.path()),
        "runtime should open",
    );
    (path, rt)
}

fn runtime_with_vault(name: &str) -> (support::TempDbFile, RedDBRuntime, Arc<AuthStore>) {
    let path = support::temp_db_file(name);
    let options = RedDBOptions::persistent(path.path())
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("serverless storage profile should expose pager");
    let rt = crate::config_tier_shared::open_runtime_with_options(options, "runtime should open");
    let pager = Arc::clone(
        rt.db()
            .store()
            .pager()
            .expect("persistent runtime should expose pager"),
    );
    let auth = Arc::new(
        AuthStore::with_vault_certificate(AuthConfig::default(), pager, TEST_CERTIFICATE)
            .expect("vault should open"),
    );
    auth.ensure_vault_secret_key();
    rt.set_auth_store(Arc::clone(&auth));
    (path, rt, auth)
}

fn text(row: &reddb::storage::query::unified::UnifiedRecord, name: &str) -> String {
    match row.get(name) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text field {name}, got {other:?}"),
    }
}

fn boolean(row: &reddb::storage::query::unified::UnifiedRecord, name: &str) -> bool {
    match row.get(name) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected boolean field {name}, got {other:?}"),
    }
}

fn assert_system_schema_denied(rt: &RedDBRuntime, sql: &str) {
    let err = match rt.execute_query(sql) {
        Ok(_) => panic!("{sql} should be denied"),
        Err(err) => err,
    };
    let message = err.to_string();
    assert!(
        message.contains("system schema is read-only"),
        "{sql} returned {message}"
    );
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

#[test]
fn bootstrap_creates_protected_system_config_and_vault_collections() {
    let (_path, rt) = runtime("system_config_vault_bootstrap");

    let rows = rt
        .execute_query(
            "SELECT name, model, internal FROM red.collections \
             WHERE name IN ('red.config', 'red.vault') ORDER BY name",
        )
        .expect("system collections should be visible through red.collections")
        .result
        .records;

    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(text(&rows[0], "name"), "red.config");
    assert_eq!(text(&rows[0], "model"), "config");
    assert!(boolean(&rows[0], "internal"));
    assert_eq!(text(&rows[1], "name"), "red.vault");
    assert_eq!(text(&rows[1], "model"), "vault");
    assert!(boolean(&rows[1], "internal"));
}

#[test]
fn system_config_and_vault_reject_public_create_drop_and_truncate() {
    let (_path, rt) = runtime("system_config_vault_protection");

    for sql in [
        "CREATE CONFIG red.config",
        "CREATE VAULT red.vault",
        "DROP CONFIG red.config",
        "DROP VAULT red.vault",
        "DROP COLLECTION red.config",
        "DROP COLLECTION red.vault",
        "TRUNCATE CONFIG red.config",
        "TRUNCATE VAULT red.vault",
    ] {
        assert_system_schema_denied(&rt, sql);
    }
}

#[test]
fn system_config_reads_and_writes_require_normalized_system_capabilities() {
    let (_path, rt) = runtime("system_config_capabilities");
    let auth = Arc::new(AuthStore::new(AuthConfig::default()));
    auth.create_user("alice", "p", Role::Write).unwrap();
    rt.set_auth_store(Arc::clone(&auth));

    rt.execute_query("PUT CONFIG red.config mode = 'dark'")
        .expect("bootstrap write without IAM policy should seed system config");
    rt.execute_query("CREATE TABLE probes (id INTEGER)")
        .expect("create projection probe table");
    rt.execute_query("INSERT INTO probes (id) VALUES (1)")
        .expect("insert projection probe row");
    let projected = rt
        .execute_query("SELECT $config.mode AS cfg_mode FROM probes")
        .expect("$config alias should read from red.config");
    assert_eq!(
        projected.result.records[0].get("cfg_mode"),
        Some(&Value::text("dark"))
    );

    attach_user_policy(
        &auth,
        "alice",
        "system-config-read",
        r#"[
            {"effect":"allow","actions":["config:read"],"resources":["config:red.config/mode"]}
        ]"#,
    );

    let get = as_user("alice", Role::Write, || {
        rt.execute_query("GET CONFIG red.config mode")
    })
    .expect("normalized system config read capability should allow read");
    assert_eq!(
        get.result.records[0].get("value"),
        Some(&Value::text("dark"))
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("PUT CONFIG red.config mode = 'light'")
    })
    .expect_err("system config write without config:write should fail")
    .to_string();
    assert!(denied.contains("config:write"), "{denied}");
    assert!(denied.contains("config:red.config/mode"), "{denied}");

    attach_user_policy(
        &auth,
        "alice",
        "system-config-write",
        r#"[
            {"effect":"allow","actions":["config:write"],"resources":["config:red.config/mode"]}
        ]"#,
    );
    as_user("alice", Role::Write, || {
        rt.execute_query("PUT CONFIG red.config mode = 'light'")
    })
    .expect("normalized system config write capability should allow write");
}

#[test]
fn system_vault_reads_and_writes_require_normalized_system_capabilities() {
    let (_path, rt, auth) = runtime_with_vault("system_vault_capabilities");
    auth.create_user("alice", "p", Role::Write).unwrap();

    rt.execute_query("VAULT PUT red.vault.api_key = 'first'")
        .expect("bootstrap write without IAM policy should seed system vault");
    attach_user_policy(
        &auth,
        "alice",
        "system-vault-read",
        r#"[
            {"effect":"allow","actions":["vault:read_metadata"],"resources":["vault:red.vault/api_key"]}
        ]"#,
    );

    as_user("alice", Role::Write, || {
        rt.execute_query("VAULT GET red.secret.api_key")
    })
    .expect("red.secret alias should normalize to red.vault read target");

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("VAULT PUT red.secret.api_key = 'second'")
    })
    .expect_err("system vault write without vault:write should fail")
    .to_string();
    assert!(denied.contains("vault:write"), "{denied}");
    assert!(denied.contains("vault:red.vault/api_key"), "{denied}");

    attach_user_policy(
        &auth,
        "alice",
        "system-vault-write",
        r#"[
            {"effect":"allow","actions":["vault:write"],"resources":["vault:red.vault/api_key"]}
        ]"#,
    );
    as_user("alice", Role::Write, || {
        rt.execute_query("VAULT PUT red.secret.api_key = 'second'")
    })
    .expect("red.secret alias should normalize to red.vault write target");
}
