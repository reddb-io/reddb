use std::path::Path;
use std::sync::Arc;

use reddb_server::auth::policies::Policy;
use reddb_server::auth::store::PrincipalRef;
use reddb_server::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb_server::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
};
use reddb_server::storage::StorageDeployPreset;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

struct IdentityScope;

impl Drop for IdentityScope {
    fn drop(&mut self) {
        clear_current_auth_identity();
        clear_current_tenant();
    }
}

fn runtime(path: &Path) -> (RedDBRuntime, Arc<AuthStore>) {
    clear_current_auth_identity();
    clear_current_tenant();
    let options = RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("storage profile");
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    let pager = runtime.db().store().pager().cloned().expect("pager");
    let auth = Arc::new(
        AuthStore::with_vault_certificate(
            AuthConfig::default(),
            pager,
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )
        .expect("vault"),
    );
    auth.ensure_vault_secret_key();
    runtime.set_auth_store(Arc::clone(&auth));
    for sql in [
        "SET SECRET token = 'eight123'",
        "CREATE VAULT app",
        "VAULT PUT app.token = 'eight123'",
        "CREATE TABLE probes (id INTEGER, token SECRET)",
        "INSERT INTO probes (id, token) VALUES (1, SECRET('eight123'))",
        "CREATE TABLE matches (id INTEGER, token TEXT)",
        "INSERT INTO matches (id, token) VALUES (2, 'eight123')",
    ] {
        runtime.execute_query(sql).expect(sql);
    }
    (runtime, auth)
}

#[test]
fn join_projections_preserve_secret_sensitivity_and_computed_values() {
    let _identity = IdentityScope;
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, auth) = runtime(&directory.path().join("joins.rdb"));
    let join = "FROM probes p JOIN matches m ON p.token = m.token";
    for expression in [
        "p.token",
        "$secrets.default.token",
        "$secrets.app.token",
        "UPPER(p.token)",
        "CONCAT('prefix-', p.token)",
        "LENGTH($secrets.app.token)",
        "p.token = m.token",
        "CASE WHEN p.token = m.token THEN 'matched' ELSE 'other' END",
        "CASE WHEN p.id = 1 THEN 'public' ELSE p.token END",
    ] {
        let sql = format!("SELECT {expression} AS value {join}");
        let result = runtime.execute_query(&sql).expect(&sql);
        assert_eq!(result.result.records.len(), 1, "{sql}");
        let value = result.result.records[0].get("value").expect("value");
        assert!(matches!(value, Value::Secret(_)), "{sql}: {value:?}");
        assert_eq!(value.to_json().as_str(), Some("***"), "{sql}");
    }

    for (expression, expected) in [
        ("UPPER(p.token)", Value::text("EIGHT123")),
        ("LENGTH($secrets.app.token)", Value::Integer(8)),
        (
            "CASE WHEN p.token = m.token THEN 'matched' ELSE 'other' END",
            Value::text("matched"),
        ),
        ("p.token = m.token", Value::Boolean(true)),
    ] {
        let sql = format!("SELECT {expression} AS value {join}");
        let result = runtime.execute_query(&sql).expect(&sql);
        let Value::Secret(payload) = result.result.records[0].get("value").expect("value") else {
            panic!("sensitive output must remain encrypted: {sql}");
        };
        // Inspect encrypted output with the fixture's key to verify actual
        // computation without adding unsupported nested JOIN execution.
        let nonce: &[u8; 12] = payload[..12].try_into().expect("nonce");
        let plaintext = reddb_server::crypto::aes256_gcm_decrypt(
            &auth.vault_secret_key().expect("fixture key"),
            nonce,
            b"reddb.secret.value",
            &payload[12..],
        )
        .expect("encrypted typed output");
        let (actual, consumed) = Value::from_bytes(&plaintext).expect("typed value");
        assert_eq!(consumed, plaintext.len());
        assert_eq!(actual, expected, "{sql}");
    }
    assert!(runtime
        .execute_query(&format!(
            "WITH sensitive AS (SELECT UPPER(p.token) AS value {join}) \
             SELECT value FROM sensitive"
        ))
        .expect_err("nested JOIN source remains unsupported")
        .to_string()
        .contains("subquery of kind join is not yet supported"));

    let result = runtime
        .execute_query(&format!("SELECT p.id AS value {join}"))
        .expect("public projection");
    assert_eq!(
        result.result.records[0].get("value"),
        Some(&Value::Integer(1))
    );
}

#[test]
fn join_secret_references_do_not_reveal_to_use_only_principals() {
    let _identity = IdentityScope;
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, auth) = runtime(&directory.path().join("permissions.rdb"));
    auth.create_user("reader", "synthetic-password", Role::Read)
        .expect("reader");
    auth.put_policy(
        Policy::from_json_str(
            r#"{"id":"use-only","version":1,"statements":[
                {"effect":"allow","actions":["select"],"resources":["*"]},
                {"effect":"allow","actions":["vault:use"],"resources":["vault:app.*","vault:red.vault/*"]}
            ]}"#,
        )
        .expect("policy"),
    )
    .expect("store policy");
    auth.attach_policy(PrincipalRef::User(UserId::platform("reader")), "use-only")
        .expect("attach policy");
    set_current_auth_identity("reader".into(), Role::Read);

    for collection in ["app", "red.vault"] {
        assert!(runtime
            .execute_query(&format!("VAULT REVEAL {collection}.token"))
            .expect_err("use permission does not grant reveal")
            .to_string()
            .contains("vault:reveal"));
    }
    for expression in [
        "$secrets.default.token",
        "$secrets.app.token",
        "UPPER(p.token)",
    ] {
        let sql = format!(
            "SELECT {expression} AS value FROM probes p JOIN matches m ON p.token = m.token \
             WHERE m.token = $secrets.app.token"
        );
        let result = runtime.execute_query(&sql).expect(&sql);
        assert_eq!(result.result.records.len(), 1, "{sql}");
        assert_eq!(
            result.result.records[0]
                .get("value")
                .expect("value")
                .display_string(),
            "***",
            "{sql}"
        );
    }
}
