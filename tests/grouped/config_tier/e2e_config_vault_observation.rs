use std::path::Path;
use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::replication::cdc::ChangeOperation;
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
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

fn field<'a>(row: &'a reddb::storage::query::unified::UnifiedRecord, name: &str) -> &'a Value {
    row.get(name)
        .unwrap_or_else(|| panic!("missing field {name}: {row:?}"))
}

fn text(row: &reddb::storage::query::unified::UnifiedRecord, name: &str) -> String {
    match field(row, name) {
        Value::Text(value) => value.to_string(),
        other => panic!("expected text field {name}, got {other:?}"),
    }
}

fn integer(row: &reddb::storage::query::unified::UnifiedRecord, name: &str) -> i64 {
    match field(row, name) {
        Value::Integer(value) => *value,
        other => panic!("expected integer field {name}, got {other:?}"),
    }
}

fn tags(row: &reddb::storage::query::unified::UnifiedRecord) -> Vec<String> {
    match field(row, "tags") {
        Value::Array(values) => values
            .iter()
            .map(|value| match value {
                Value::Text(text) => text.to_string(),
                other => panic!("expected text tag, got {other:?}"),
            })
            .collect(),
        other => panic!("expected tag array, got {other:?}"),
    }
}

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

fn attach_alice_policy(auth: &AuthStore, id: &str, statements: &str) {
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
        reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform("alice")),
        id,
    )
    .unwrap();
}

#[test]
fn list_config_prefix_paginates_values_and_tags() {
    let rt = rt();

    rt.execute_query("PUT CONFIG app feature_a = 'alpha' TAGS [scope:prod]")
        .expect("put feature_a");
    rt.execute_query("PUT CONFIG app feature_b = 'beta' TAGS [scope:dev]")
        .expect("put feature_b");
    rt.execute_query("PUT CONFIG app other = 'ignored'")
        .expect("put other");

    let listed = rt
        .execute_query("LIST CONFIG app PREFIX feature LIMIT 1 OFFSET 1")
        .expect("list config");
    assert_eq!(listed.result.records.len(), 1);
    let row = &listed.result.records[0];
    assert_eq!(text(row, "collection"), "app");
    assert_eq!(text(row, "key"), "feature_b");
    assert_eq!(field(row, "value"), &Value::text("beta"));
    assert_eq!(integer(row, "version"), 1);
    assert_eq!(tags(row), vec!["scope:dev".to_string()]);

    let err = rt
        .execute_query("INVALIDATE TAGS [scope:prod] FROM app")
        .expect_err("config collections must reject KV invalidation")
        .to_string();
    assert!(err.contains("expected kv, got config"), "{err}");
}

#[test]
fn watch_config_events_include_values_only_when_read_is_allowed() {
    let rt = rt();
    let auth = Arc::new(AuthStore::new(AuthConfig::default()));
    auth.create_user("alice", "p", Role::Write).unwrap();
    rt.set_auth_store(Arc::clone(&auth));

    let start = rt.cdc_current_lsn();
    rt.execute_query("PUT CONFIG app flag = 'off'")
        .expect("put config");
    rt.execute_query("ROTATE CONFIG app flag = 'on'")
        .expect("rotate config");

    let anonymous = rt.config_watch_events_since("app", "flag", start, 10);
    assert_eq!(anonymous.len(), 2);
    assert_eq!(anonymous[0].op, ChangeOperation::Insert);
    assert_eq!(
        anonymous[0].after.as_ref().and_then(|value| value.as_str()),
        Some("off")
    );
    assert_eq!(anonymous[1].op, ChangeOperation::Update);
    assert_eq!(
        anonymous[1]
            .before
            .as_ref()
            .and_then(|value| value.as_str()),
        Some("off")
    );
    assert_eq!(
        anonymous[1].after.as_ref().and_then(|value| value.as_str()),
        Some("on")
    );

    attach_alice_policy(
        &auth,
        "unrelated-config-read",
        r#"[
            {"effect":"allow","actions":["config:read"],"resources":["config:app.other"]}
        ]"#,
    );
    let denied = as_user("alice", Role::Write, || {
        rt.config_watch_events_since("app", "flag", start, 10)
    });
    assert_eq!(denied.len(), 2);
    assert!(denied.iter().all(|event| event.before.is_none()));
    assert!(denied.iter().all(|event| event.after.is_none()));

    attach_alice_policy(
        &auth,
        "config-read-flag",
        r#"[
            {"effect":"allow","actions":["config:read"],"resources":["config:app.flag"]}
        ]"#,
    );
    let allowed = as_user("alice", Role::Write, || {
        rt.config_watch_events_since("app", "flag", start, 10)
    });
    assert_eq!(
        allowed[1].after.as_ref().and_then(|value| value.as_str()),
        Some("on")
    );
}

#[test]
fn list_and_watch_vault_are_metadata_only() {
    let path = support::temp_db_file("config-vault-observation");
    let secret = "vault_plaintext_observation_probe";
    let (rt, _auth) = open_runtime_with_vault(path.path(), "vault-observation-pass");

    rt.execute_query("CREATE VAULT secrets WITH OWN MASTER KEY")
        .expect("create vault");
    let start = rt.cdc_current_lsn();
    rt.execute_query(&format!(
        "VAULT PUT secrets.api_key = '{secret}' TAGS [scope:prod]"
    ))
    .expect("vault put");
    rt.execute_query("ROTATE VAULT secrets.api_key = 'next_secret' TAGS [scope:prod]")
        .expect("vault rotate");
    rt.execute_query("VAULT PUT secrets.api_other = 'other_secret' TAGS [scope:dev]")
        .expect("vault put other");

    let listed = rt
        .execute_query("LIST VAULT secrets PREFIX api_ LIMIT 1 OFFSET 1")
        .expect("list vault");
    assert_eq!(listed.result.records.len(), 1);
    let row = &listed.result.records[0];
    assert_eq!(text(row, "collection"), "secrets");
    assert_eq!(text(row, "key"), "api_other");
    assert!(!listed.result.columns.contains(&"value".to_string()));
    assert!(listed.result.columns.contains(&"fingerprint".to_string()));
    assert_eq!(tags(row), vec!["scope:dev".to_string()]);

    let events = rt.vault_watch_events_since("secrets", "api_key", start, 10);
    assert_eq!(events.len(), 2);
    let debug = format!("{events:?}");
    assert!(
        !debug.contains(secret),
        "vault watch exposed plaintext: {debug}"
    );
    assert!(
        !debug.contains("next_secret"),
        "vault watch exposed plaintext: {debug}"
    );
    assert!(
        debug.contains("fingerprint"),
        "vault watch lacks metadata: {debug}"
    );
}
