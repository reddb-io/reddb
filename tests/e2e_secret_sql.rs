mod support;

use std::sync::Arc;

use reddb::auth::vault::Vault;
use reddb::auth::{AuthConfig, AuthStore};
use reddb::storage::schema::Value;
use reddb::storage::StorageDeployPreset;
use reddb::{RedDBOptions, RedDBRuntime};

use support::TempDbFile;

fn pager_backed_options(path: &std::path::Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("serverless storage profile should expose pager")
}

fn open_runtime_with_vault(name: &str) -> (TempDbFile, RedDBRuntime, Arc<AuthStore>) {
    let path = support::temp_db_file(name);
    let rt = RedDBRuntime::with_options(pager_backed_options(path.path())).expect("runtime opens");
    let db = rt.db();
    let store = db.store();
    let pager = Arc::clone(
        store
            .pager()
            .expect("persistent runtime should expose pager"),
    );
    let auth = Arc::new(
        AuthStore::with_vault(AuthConfig::default(), pager, Some("test-pass"))
            .expect("vault should open"),
    );
    rt.set_auth_store(Arc::clone(&auth));
    (path, rt, auth)
}

fn attach_vault(rt: &RedDBRuntime, passphrase: &str) -> Arc<AuthStore> {
    let db = rt.db();
    let store = db.store();
    let pager = Arc::clone(
        store
            .pager()
            .expect("persistent runtime should expose pager"),
    );
    let auth = Arc::new(
        AuthStore::with_vault(AuthConfig::default(), pager, Some(passphrase))
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

    let result = rt
        .execute_query("SHOW SECRET mycompany.stripe")
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

    let state = Vault::unseal_logical_export_with_passphrase(&blob, "test-pass")
        .expect("export should decrypt with source vault passphrase");
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
        let rt = RedDBRuntime::with_options(pager_backed_options(source_path))
            .expect("source runtime should open");
        let _auth = attach_vault(&rt, "cli-pass");
        rt.execute_query("SET CONFIG red.config.demo.enabled = true")
            .expect("SET CONFIG should succeed");
        rt.execute_query("SET SECRET mycompany.payments.key = 'sk_cli_secret'")
            .expect("SET SECRET should succeed");
        rt.checkpoint().expect("source checkpoint should succeed");
    }
    {
        let rt = RedDBRuntime::with_options(pager_backed_options(source_path))
            .expect("source runtime should reopen");
        let auth = attach_vault(&rt, "cli-pass");
        assert_eq!(
            auth.vault_kv_get("mycompany.payments.key").as_deref(),
            Some("sk_cli_secret")
        );
    }

    let red_bin = env!("CARGO_BIN_EXE_red");
    let dump = std::process::Command::new(red_bin)
        .env("REDDB_VAULT_KEY", "cli-pass")
        .arg("dump")
        .arg("--path")
        .arg(&source_path)
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
        .env("REDDB_VAULT_KEY", "cli-pass")
        .arg("restore")
        .arg("--path")
        .arg(&dest_path)
        .arg("--input")
        .arg(&dump_path)
        .output()
        .expect("red restore should run");
    assert!(
        restore.status.success(),
        "red restore failed: {}",
        String::from_utf8_lossy(&restore.stderr)
    );

    let rt = RedDBRuntime::with_options(pager_backed_options(dest_path))
        .expect("dest runtime should open");
    let auth = attach_vault(&rt, "cli-pass");
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
fn dollar_red_secret_reference_uses_full_red_secret_path() {
    let (_path, rt, _auth) = open_runtime_with_vault("secret_sql_dollar_red_ref");

    rt.execute_query("CREATE TABLE tokens (id INT, token TEXT)")
        .expect("create table");
    rt.execute_query("INSERT INTO tokens (id, token) VALUES (1, 'red-match'), (2, 'other')")
        .expect("insert rows");
    rt.execute_query("SET SECRET red.secret.tokens.active = 'red-match'")
        .expect("set red secret");

    let filtered = rt
        .execute_query("SELECT id FROM tokens WHERE token = $red.secret.tokens.active")
        .expect("filter by red secret");
    assert_eq!(filtered.result.records.len(), 1);
    assert_eq!(
        filtered.result.records[0].get("id"),
        Some(&Value::Integer(1))
    );
}

#[test]
fn red_secrets_plural_alias_normalizes_to_red_secret() {
    let (_path, rt, auth) = open_runtime_with_vault("secret_sql_red_secrets_plural");

    rt.execute_query("CREATE TABLE tokens (id INT, token TEXT)")
        .expect("create table");
    rt.execute_query("INSERT INTO tokens (id, token) VALUES (1, 'plural-match'), (2, 'other')")
        .expect("insert rows");
    rt.execute_query("SET SECRET red.secrets.tokens.active = 'plural-match'")
        .expect("set plural red secret");

    assert_eq!(
        auth.vault_kv_get("red.secret.tokens.active").as_deref(),
        Some("plural-match")
    );
    assert!(
        auth.vault_kv_get("red.secrets.tokens.active").is_none(),
        "plural alias should not create a second physical namespace"
    );

    let shown = rt
        .execute_query("SHOW SECRETS red.secrets.tokens")
        .expect("show plural red secrets");
    assert_eq!(shown.result.records.len(), 1);
    assert_eq!(
        shown.result.records[0].get("key"),
        Some(&Value::Text("red.secret.tokens.active".into()))
    );

    let filtered = rt
        .execute_query("SELECT id FROM tokens WHERE token = $red.secrets.tokens.active")
        .expect("filter by plural red secret");
    assert_eq!(filtered.result.records.len(), 1);
    assert_eq!(
        filtered.result.records[0].get("id"),
        Some(&Value::Integer(1))
    );

    rt.execute_query("DELETE SECRET red.secrets.tokens.active")
        .expect("delete plural red secret");
    assert!(auth.vault_kv_get("red.secret.tokens.active").is_none());
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
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
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
