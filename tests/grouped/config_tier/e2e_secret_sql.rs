#[path = "../../support/mod.rs"]
mod support;

use std::sync::Arc;

use reddb::auth::vault::Vault;
use reddb::auth::{AuthConfig, AuthStore};
use reddb::storage::schema::Value;
use reddb::storage::StorageDeployPreset;
use reddb::{RedDBOptions, RedDBRuntime};

use support::TempDbFile;

const TEST_CERTIFICATE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const CLI_CERTIFICATE: &str = "1f1e1d1c1b1a191817161514131211100f0e0d0c0b0a09080706050403020100";

fn pager_backed_options(path: &std::path::Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("serverless storage profile should expose pager")
}

fn open_runtime_with_vault(name: &str) -> (TempDbFile, RedDBRuntime, Arc<AuthStore>) {
    let path = support::temp_db_file(name);
    let rt = crate::config_tier_shared::open_runtime_with_options(
        pager_backed_options(path.path()),
        "runtime opens",
    );
    let db = rt.db();
    let store = db.store();
    let pager = Arc::clone(
        store
            .pager()
            .expect("persistent runtime should expose pager"),
    );
    let auth = Arc::new(
        AuthStore::with_vault_certificate(AuthConfig::default(), pager, TEST_CERTIFICATE)
            .expect("vault should open"),
    );
    rt.set_auth_store(Arc::clone(&auth));
    (path, rt, auth)
}

fn attach_vault(rt: &RedDBRuntime, certificate_hex: &str) -> Arc<AuthStore> {
    let db = rt.db();
    let store = db.store();
    let pager = Arc::clone(
        store
            .pager()
            .expect("persistent runtime should expose pager"),
    );
    let auth = Arc::new(
        AuthStore::with_vault_certificate(AuthConfig::default(), pager, certificate_hex)
            .expect("vault should open"),
    );
    rt.set_auth_store(Arc::clone(&auth));
    auth
}

#[test]
fn set_secret_persists_to_vault_and_show_masks_value() {
    let (_path, rt, auth) = open_runtime_with_vault("secret_sql_set_show");

    let set = rt
        .execute_query("SET SECRET mycompany.stripe.key = 'sk_live'")
        .expect("SET SECRET should succeed");
    assert_eq!(set.statement, "set_secret");

    assert_eq!(
        auth.vault_kv_get("mycompany.stripe.key").as_deref(),
        Some("sk_live"),
        "vault keys after SET SECRET: {:?}",
        auth.vault_kv_keys()
    );
    auth.vault_kv_try_set(
        "red.secret.aes_key".to_string(),
        "vault-aes-key".to_string(),
    )
    .expect("seed internal red.secret key");
    auth.vault_kv_try_set(
        "red.config.ai.default.provider".to_string(),
        "anthropic".to_string(),
    )
    .expect("seed internal red.config key");

    let result = rt
        .execute_query("SHOW SECRETS")
        .expect("SHOW SECRET should succeed");
    assert_eq!(result.result.records.len(), 1);
    let record = &result.result.records[0];
    assert_eq!(
        record.get("key"),
        Some(&Value::Text("mycompany.stripe.key".into()))
    );
    assert_eq!(record.get("value"), Some(&Value::Text("***".into())));

    rt.execute_query("DELETE SECRET mycompany.stripe.key")
        .expect("DELETE SECRET should succeed");
    assert!(auth.vault_kv_get("mycompany.stripe.key").is_none());
}

#[test]
fn vault_kv_logical_export_is_encrypted_and_roundtrips() {
    let (_path, rt, auth) = open_runtime_with_vault("secret_sql_logical_export");
    rt.execute_query("SET SECRET mycompany.stripe.key = 'sk_live_export'")
        .expect("SET SECRET should succeed");

    let blob = auth
        .vault_kv_export_encrypted()
        .expect("vault export should succeed")
        .expect("vault export should contain KV");
    assert!(
        !blob.contains("sk_live_export"),
        "logical export blob must not expose plaintext"
    );

    let state = Vault::unseal_logical_export_with_certificate(&blob, TEST_CERTIFICATE)
        .expect("export should decrypt with source vault certificate");
    assert_eq!(
        state.kv.get("mycompany.stripe.key").map(String::as_str),
        Some("sk_live_export")
    );

    let (_dest_path, _dest_rt, dest_auth) =
        open_runtime_with_vault("secret_sql_logical_export_dest");
    let count = dest_auth
        .vault_kv_try_import(state.kv)
        .expect("vault KV import should persist");
    assert_eq!(count, 1);
    assert_eq!(
        dest_auth.vault_kv_get("mycompany.stripe.key").as_deref(),
        Some("sk_live_export")
    );
}

#[test]
fn vault_kv_logical_restore_placeholders_use_false() {
    let (_path, _rt, auth) = open_runtime_with_vault("secret_sql_logical_placeholder");
    let keys = vec![
        "mycompany.stripe.key".to_string(),
        "red.secret.ai.openai.default.api_key".to_string(),
    ];

    let count = auth
        .vault_kv_try_import_placeholders(&keys)
        .expect("placeholder import should persist");
    assert_eq!(count, 2);
    assert_eq!(
        auth.vault_kv_get("mycompany.stripe.key").as_deref(),
        Some("false")
    );
    assert_eq!(
        auth.vault_kv_get("red.secret.ai.openai.default.api_key")
            .as_deref(),
        Some("false")
    );
}

#[test]
fn cli_dump_restore_includes_plaintext_config_and_encrypted_vault_kv() {
    let source_guard = support::temp_db_file("secret-cli-dump-source");
    let dest_guard = support::temp_db_file("secret-cli-dump-dest");
    let dump_guard = support::temp_db_file("secret-cli-dump-jsonl");
    let source_path = source_guard.path();
    let dest_path = dest_guard.path();
    let dump_path = dump_guard.path();

    {
        let rt = crate::config_tier_shared::open_runtime_with_options(
            pager_backed_options(source_path),
            "source runtime should open",
        );
        let _auth = attach_vault(&rt, CLI_CERTIFICATE);
        rt.execute_query("SET CONFIG red.config.demo.enabled = true")
            .expect("SET CONFIG should succeed");
        rt.execute_query("SET SECRET mycompany.payments.key = 'sk_cli_secret'")
            .expect("SET SECRET should succeed");
        rt.checkpoint().expect("source checkpoint should succeed");
    }
    {
        let rt = crate::config_tier_shared::open_runtime_with_options(
            pager_backed_options(source_path),
            "source runtime should reopen",
        );
        let auth = attach_vault(&rt, CLI_CERTIFICATE);
        assert_eq!(
            auth.vault_kv_get("mycompany.payments.key").as_deref(),
            Some("sk_cli_secret")
        );
    }

    let red_bin = env!("CARGO_BIN_EXE_red");
    let dump = std::process::Command::new(red_bin)
        .env("REDDB_CERTIFICATE", CLI_CERTIFICATE)
        .arg("dump")
        .arg("--path")
        .arg(&source_path)
        .arg("--storage-preset")
        .arg("serverless")
        .arg("--output")
        .arg(&dump_path)
        .output()
        .expect("red dump should run");
    assert!(
        dump.status.success(),
        "red dump failed: {}",
        String::from_utf8_lossy(&dump.stderr)
    );

    let dump_text = std::fs::read_to_string(&dump_path).expect("dump should be readable");
    assert!(dump_text.contains("red_config"));
    assert!(dump_text.contains("red.config.demo.enabled"));
    assert!(dump_text.contains("reddb.vault_kv.v1"));
    assert!(
        !dump_text.contains("sk_cli_secret"),
        "dump must not contain secret plaintext: {dump_text}"
    );

    let restore = std::process::Command::new(red_bin)
        .env("REDDB_CERTIFICATE", CLI_CERTIFICATE)
        .arg("restore")
        .arg("--path")
        .arg(&dest_path)
        .arg("--storage-preset")
        .arg("serverless")
        .arg("--input")
        .arg(&dump_path)
        .output()
        .expect("red restore should run");
    assert!(
        restore.status.success(),
        "red restore failed: {}",
        String::from_utf8_lossy(&restore.stderr)
    );

    let rt = crate::config_tier_shared::open_runtime_with_options(
        pager_backed_options(dest_path),
        "dest runtime should open",
    );
    let auth = attach_vault(&rt, CLI_CERTIFICATE);
    assert_eq!(
        auth.vault_kv_get("mycompany.payments.key").as_deref(),
        Some("sk_cli_secret")
    );
    let config = rt
        .execute_query("SELECT $red.config.demo.enabled")
        .expect("config should restore");
    assert_eq!(config.result.records.len(), 1);

    drop(source_guard);
    drop(dest_guard);
    drop(dump_guard);
}

#[test]
fn dollar_secret_reference_masks_projection_and_resolves_in_filter() {
    let (_path, rt, _auth) = open_runtime_with_vault("secret_sql_dollar_ref");

    rt.execute_query("CREATE TABLE tokens (id INT, token TEXT)")
        .expect("create table");
    rt.execute_query("INSERT INTO tokens (id, token) VALUES (1, 'match-me'), (2, 'other')")
        .expect("insert rows");
    rt.execute_query("SET SECRET mycompany.tokens.active = 'match-me'")
        .expect("set secret");

    let projected = rt
        .execute_query("SELECT $secret.mycompany.tokens.active AS secret_value FROM tokens LIMIT 1")
        .expect("project secret");
    assert_eq!(
        projected.result.records[0].get("secret_value"),
        Some(&Value::Text("***".into()))
    );

    let filtered = rt
        .execute_query("SELECT id FROM tokens WHERE token = $secret.mycompany.tokens.active")
        .expect("filter by secret");
    assert_eq!(filtered.result.records.len(), 1);
    assert_eq!(
        filtered.result.records[0].get("id"),
        Some(&Value::Integer(1))
    );
}

#[test]
fn dollar_secret_reference_does_not_resolve_reserved_red_secret_namespace() {
    let (_path, rt, auth) = open_runtime_with_vault("secret_sql_dollar_reserved_red_ref");

    rt.execute_query("CREATE TABLE tokens (id INT, token TEXT)")
        .expect("create table");
    rt.execute_query("INSERT INTO tokens (id, token) VALUES (1, 'user-match'), (2, 'other')")
        .expect("insert rows");
    rt.execute_query("SET SECRET acme.key = 'user-match'")
        .expect("set user secret");
    auth.vault_kv_try_set(
        "red.secret.aes_key".to_string(),
        "vault-aes-key".to_string(),
    )
    .expect("seed internal AES key");
    auth.vault_kv_try_set(
        "red.secret.ai.anthropic.default.api_key".to_string(),
        "provider-key".to_string(),
    )
    .expect("seed internal provider key");

    let aes_alias = rt
        .execute_query("SELECT $secret.aes_key AS secret_value FROM tokens LIMIT 1")
        .expect("project AES alias");
    assert_eq!(
        aes_alias.result.records[0].get("secret_value"),
        Some(&Value::Null)
    );
    let explicit_reserved = rt
        .execute_query("SELECT $secret.red.secret.aes_key AS secret_value FROM tokens LIMIT 1")
        .expect("project explicit reserved secret");
    assert_eq!(
        explicit_reserved.result.records[0].get("secret_value"),
        Some(&Value::Null)
    );
    let provider_key = rt
        .execute_query(
            "SELECT $secret.ai.anthropic.default.api_key AS secret_value FROM tokens LIMIT 1",
        )
        .expect("project provider secret");
    assert_eq!(
        provider_key.result.records[0].get("secret_value"),
        Some(&Value::Null)
    );

    let filtered = rt
        .execute_query("SELECT id FROM tokens WHERE token = $secret.acme.key")
        .expect("filter by user secret");
    assert_eq!(filtered.result.records.len(), 1);
    assert_eq!(
        filtered.result.records[0].get("id"),
        Some(&Value::Integer(1))
    );
}

#[test]
fn dollar_config_reference_resolves_plaintext_config() {
    let (_path, rt, _auth) = open_runtime_with_vault("secret_sql_dollar_config_ref");

    rt.execute_query("CREATE TABLE tokens (id INT, token TEXT)")
        .expect("create table");
    rt.execute_query("INSERT INTO tokens (id, token) VALUES (1, 'cfg-match'), (2, 'other')")
        .expect("insert rows");
    rt.execute_query("SET CONFIG red.config.tokens.active = 'cfg-match'")
        .expect("set config");

    let projected = rt
        .execute_query("SELECT $red.config.tokens.active AS active FROM tokens LIMIT 1")
        .expect("project config");
    assert_eq!(
        projected.result.records[0].get("active"),
        Some(&Value::Text("cfg-match".into()))
    );

    let filtered = rt
        .execute_query("SELECT id FROM tokens WHERE token = $red.config.tokens.active")
        .expect("filter by config");
    assert_eq!(filtered.result.records.len(), 1);
    assert_eq!(
        filtered.result.records[0].get("id"),
        Some(&Value::Integer(1))
    );
}

#[test]
fn set_secret_requires_vault() {
    let rt =
        crate::config_tier_shared::open_runtime_with_options(RedDBOptions::in_memory(), "runtime");
    let err = rt
        .execute_query("SET SECRET mycompany.stripe.key = 'sk_live'")
        .expect_err("SET SECRET without vault should fail");
    assert!(err
        .to_string()
        .contains("requires an enabled, unsealed vault"));
}

#[test]
fn config_and_secret_reserved_prefixes_do_not_cross() {
    let (_path, rt, _auth) = open_runtime_with_vault("secret_sql_reserved_prefixes");

    let err = rt
        .execute_query("SET SECRET red.config.foo = 'x'")
        .expect_err("SET SECRET must reject red.config");
    assert!(err.to_string().contains("red.config.* is reserved"));

    let err = rt
        .execute_query("SET CONFIG red.secret.foo = 'x'")
        .expect_err("SET CONFIG must reject red.secret");
    assert!(err.to_string().contains("red.secret.* is reserved"));

    let err = rt
        .execute_query("SET CONFIG red.secrets.foo = 'x'")
        .expect_err("SET CONFIG must reject red.secrets");
    assert!(err.to_string().contains("red.secrets.*"));
}
