use std::path::Path;
use std::sync::Arc;

use reddb_server::auth::{AuthConfig, AuthStore};
use reddb_server::runtime::mvcc::{clear_current_auth_identity, clear_current_tenant};
use reddb_server::storage::StorageDeployPreset;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

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
        "CREATE TABLE probes (id INTEGER, token SECRET)",
        "INSERT INTO probes (id, token) VALUES (1, SECRET('eight123'))",
    ] {
        runtime.execute_query(sql).expect(sql);
    }
    (runtime, auth)
}

#[test]
fn scalar_subquery_secret_computations_stay_typed_and_masked() {
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, auth) = runtime(&directory.path().join("scalar.rdb"));
    for (expression, expected) in [
        ("(SELECT $secrets.default.token)", "'eight123'"),
        ("(SELECT token FROM probes)", "'eight123'"),
        ("LENGTH((SELECT $secrets.default.token))", "8"),
        ("LENGTH((SELECT token FROM probes))", "8"),
        ("UPPER((SELECT token FROM probes))", "'EIGHT123'"),
        ("(SELECT LENGTH($secrets.default.token)) + 2", "10"),
        ("(SELECT $secrets.default.token) = 'eight123'", "true"),
        (
            "CASE WHEN (SELECT LENGTH($secrets.default.token)) > 0 THEN 'yes' ELSE 'no' END",
            "'yes'",
        ),
        (
            "CASE WHEN (SELECT token FROM probes) = 'eight123' THEN 42 ELSE 0 END",
            "42",
        ),
    ] {
        for source in ["", " FROM probes"] {
            let sql = format!("SELECT {expression} AS value{source}");
            let result = runtime.execute_query(&sql).expect(&sql);
            assert_eq!(result.result.records.len(), 1, "{sql}");
            let value = result.result.records[0].get("value").expect("value");
            assert!(matches!(value, Value::Secret(_)), "{sql}: {value:?}");
            assert_eq!(value.display_string(), "***", "{sql}");

            let Value::Secret(payload) = value else {
                panic!("sensitive result must remain encrypted: {sql}");
            };
            let nonce: &[u8; 12] = payload[..12].try_into().expect("nonce");
            let plaintext = reddb_server::crypto::aes256_gcm_decrypt(
                &auth.vault_secret_key().expect("fixture key"),
                nonce,
                b"reddb.secret.value",
                &payload[12..],
            )
            .expect("encrypted typed result");
            let (actual, consumed) = Value::from_bytes(&plaintext).expect("typed result");
            assert_eq!(consumed, plaintext.len());
            let expected_value = match expected {
                "true" => Value::Boolean(true),
                value if value.starts_with('\'') => Value::text(value.trim_matches('\'')),
                value => Value::Integer(value.parse().expect("expected integer")),
            };
            assert_eq!(actual, expected_value, "actual computation: {sql}");

            // CTEs with a source table are supported. Constant-only CTE
            // cardinality is an existing, separate executor limitation.
            if source.is_empty() {
                continue;
            }
            let sql = format!(
                "WITH sensitive AS (SELECT {expression} AS value{source}) \
                 SELECT value FROM sensitive WHERE value = {expected}"
            );
            let result = runtime.execute_query(&sql).expect(&sql);
            assert_eq!(result.result.records.len(), 1, "actual computation: {sql}");
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
    for predicate in [
        "(SELECT token FROM probes) = 'eight123'",
        "LENGTH((SELECT $secrets.default.token)) = 8",
        "(SELECT LENGTH($secrets.default.token)) + 2 = 10",
    ] {
        let sql = format!("SELECT id FROM probes WHERE {predicate}");
        let result = runtime.execute_query(&sql).expect(&sql);
        assert_eq!(result.result.records.len(), 1, "{sql}");
        assert_eq!(result.result.records[0].get("id"), Some(&Value::Integer(1)));
    }
    let result = runtime
        .execute_query("VAULT REVEAL red.vault.token")
        .expect("explicit reveal");
    assert_eq!(
        result.result.records[0].get("value"),
        Some(&Value::text("eight123"))
    );
}

#[test]
fn ordinary_scalar_subquery_literals_keep_public_values_and_errors_do_not_reveal_secrets() {
    let directory = tempfile::tempdir().expect("directory");
    let (runtime, _) = runtime(&directory.path().join("ordinary.rdb"));
    let result = runtime
        .execute_query("SELECT LENGTH((SELECT 'eight123')) AS value")
        .expect("ordinary scalar");
    assert_eq!(
        result.result.records[0].get("value"),
        Some(&Value::Integer(8))
    );
    for source in ["", " FROM probes"] {
        let sql = format!("SELECT CAST((SELECT token FROM probes) AS INTEGER) AS value{source}");
        match runtime.execute_query(&sql) {
            Err(error) => assert!(!error.to_string().contains("eight123"), "{error}"),
            Ok(result) => {
                assert!(!source.is_empty(), "source-free invalid casts still fail");
                assert!(
                    result
                        .result
                        .records
                        .iter()
                        .all(|record| { matches!(record.get("value"), Some(Value::Null)) }),
                    "invalid cast must never return a decrypted value"
                );
            }
        }
    }
    assert!(runtime
        .execute_query("SELECT LENGTH((SELECT token FROM probes WHERE probes.id = p.id)) AS value FROM probes p")
        .expect_err("correlated subqueries retain their explicit unsupported boundary")
        .to_string()
        .contains("correlated subqueries"));
}
