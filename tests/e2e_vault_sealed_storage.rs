use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::storage::schema::Value;
use reddb::storage::EntityData;
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

fn read_related_bytes(path: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(parent) = path.parent() else {
        return out;
    };
    let Some(stem) = path.file_name().and_then(|name| name.to_str()) else {
        return out;
    };
    if let Ok(entries) = std::fs::read_dir(parent) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            let Some(name) = entry_path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if (name == stem || name.starts_with(&format!("{stem}-"))) && entry_path.is_file() {
                if let Ok(bytes) = std::fs::read(&entry_path) {
                    out.extend_from_slice(&bytes);
                }
            }
        }
    }
    out
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
        reddb::auth::store::PrincipalRef::User(UserId::platform("alice")),
        id,
    )
    .unwrap();
}

#[test]
fn vault_put_seals_payload_before_persistence() {
    let path = temp_db_path("vault_sealed_storage");
    cleanup_related(&path);

    let secret = "vault_plaintext_probe_324";
    {
        let (rt, auth) = open_runtime_with_vault(&path, "vault-pass");
        rt.execute_query("CREATE VAULT secrets WITH OWN MASTER KEY")
            .expect("create vault");
        assert!(
            auth.vault_kv_get("red.vault.secrets.master_key").is_some(),
            "per-vault key material should be stored in the encrypted auth vault"
        );
        rt.execute_query(&format!("VAULT PUT secrets.api_key = '{secret}'"))
            .expect("vault put");

        let manager = rt
            .db()
            .store()
            .get_collection("secrets")
            .expect("vault collection should exist");
        let rows = manager.query_all(|_| true);
        assert_eq!(rows.len(), 1);
        let EntityData::Row(row) = &rows[0].data else {
            panic!("vault entry should be stored as a row");
        };
        let value = row
            .named
            .as_ref()
            .and_then(|named| named.get("value"))
            .expect("vault row should have value");
        match value {
            Value::Secret(payload) => {
                assert!(!payload
                    .windows(secret.len())
                    .any(|w| w == secret.as_bytes()));
            }
            other => panic!("vault value must be sealed before storage, got {other:?}"),
        }

        let get = rt
            .execute_query("VAULT GET secrets.api_key")
            .expect("vault get");
        let record = &get.result.records[0];
        assert_eq!(record.get("value"), Some(&Value::text("***")));
        assert_eq!(record.get("status"), Some(&Value::text("sealed")));
        assert_eq!(record.get("key"), Some(&Value::text("api_key")));
        assert!(matches!(record.get("version"), Some(Value::Integer(_))));
        assert!(matches!(record.get("fingerprint"), Some(Value::Text(_))));
        assert!(matches!(record.get("tags"), Some(Value::Array(tags)) if tags.is_empty()));
        assert!(matches!(
            record.get("created_at"),
            Some(Value::TimestampMs(_))
        ));
        assert!(matches!(
            record.get("updated_at"),
            Some(Value::TimestampMs(_))
        ));
        rt.checkpoint()
            .expect("checkpoint should flush sealed payload");
    }

    let persisted = read_related_bytes(&path);
    assert!(
        !persisted
            .windows(secret.len())
            .any(|w| w == secret.as_bytes()),
        "persistent database artifacts must not contain vault plaintext"
    );

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("runtime should reopen without key provider");
        let get = rt
            .execute_query("VAULT GET secrets.api_key")
            .expect("metadata read should not require key material");
        let record = &get.result.records[0];
        assert_eq!(record.get("value"), Some(&Value::text("***")));
        assert_eq!(
            record.get("status"),
            Some(&Value::text("sealed_unavailable"))
        );
        assert!(matches!(record.get("fingerprint"), Some(Value::Text(_))));
    }

    cleanup_related(&path);
}

#[test]
fn vault_get_is_metadata_only_and_unseal_is_capability_gated_and_audited() {
    let path = temp_db_path("vault_unseal_audit");
    cleanup_related(&path);

    let secret = "vault_plaintext_probe_330";
    let ciphertext_hex;
    let rt;
    let auth;
    {
        let opened = open_runtime_with_vault(&path, "vault-pass-330");
        rt = opened.0;
        auth = opened.1;
    }
    auth.create_user("alice", "p", Role::Write).unwrap();
    rt.execute_query("CREATE VAULT secrets WITH OWN MASTER KEY")
        .expect("create vault");
    rt.execute_query(&format!("VAULT PUT secrets.api_key = '{secret}'"))
        .expect("vault put");

    {
        let manager = rt
            .db()
            .store()
            .get_collection("secrets")
            .expect("vault collection should exist");
        let rows = manager.query_all(|_| true);
        let EntityData::Row(row) = &rows[0].data else {
            panic!("vault entry should be stored as a row");
        };
        let Value::Secret(payload) = row
            .named
            .as_ref()
            .and_then(|named| named.get("value"))
            .expect("vault row should have sealed value")
        else {
            panic!("vault value must be stored as Value::Secret");
        };
        ciphertext_hex = reddb::utils::to_hex(payload);
    }

    attach_alice_policy(
        &auth,
        "vault-metadata-only",
        r#"[
            {"effect":"allow","actions":["vault:read_metadata"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );

    let get = as_user("alice", Role::Write, || {
        rt.execute_query("VAULT GET secrets.api_key")
    })
    .expect("metadata read should be allowed");
    let get_debug = format!("{:?}", get.result.records);
    assert!(get.result.columns.contains(&"fingerprint".to_string()));
    assert!(get.result.columns.contains(&"tags".to_string()));
    assert!(get.result.columns.contains(&"created_at".to_string()));
    assert!(get.result.columns.contains(&"updated_at".to_string()));
    assert!(
        !get_debug.contains(secret),
        "metadata result must not include plaintext: {get_debug}"
    );
    assert!(
        !get_debug.contains(&ciphertext_hex),
        "metadata result must not include ciphertext: {get_debug}"
    );
    assert!(
        !get_debug.contains("Secret("),
        "metadata result must not expose Value::Secret: {get_debug}"
    );

    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("UNSEAL VAULT secrets.api_key")
    })
    .expect_err("unseal without vault:unseal must fail");
    assert!(denied.to_string().contains("vault:unseal"));

    attach_alice_policy(
        &auth,
        "vault-unseal",
        r#"[
            {"effect":"allow","actions":["vault:unseal"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    let unsealed = as_user("alice", Role::Write, || {
        rt.execute_query("UNSEAL VAULT secrets.api_key")
    })
    .expect("unseal should pass with vault:unseal");
    assert_eq!(
        unsealed.result.records[0].get("value"),
        Some(&Value::text(secret))
    );

    assert!(rt.audit_log().wait_idle(std::time::Duration::from_secs(2)));
    let audit_body = std::fs::read_to_string(rt.audit_log().path()).unwrap_or_default();
    assert!(audit_body.contains("vault/unseal"));
    assert!(audit_body.contains("\"outcome\":\"denied\""));
    assert!(audit_body.contains("\"outcome\":\"success\""));
    assert!(audit_body.contains("alice"));
    assert!(audit_body.contains("secrets.api_key"));
    assert!(audit_body.contains("request_id"));
    assert!(audit_body.contains("sequence_id"));
    assert!(
        !audit_body.contains(secret),
        "audit must not include plaintext: {audit_body}"
    );
    assert!(
        !audit_body.contains(&ciphertext_hex),
        "audit must not include ciphertext: {audit_body}"
    );

    cleanup_related(&path);
}

#[test]
fn create_vault_requires_unsealed_key_provider() {
    let path = temp_db_path("vault_create_requires_key");
    cleanup_related(&path);
    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime should open");

    let err = rt
        .execute_query("CREATE VAULT secrets")
        .expect_err("CREATE VAULT must not simulate key material");
    assert!(err.to_string().contains("enabled, unsealed vault"));

    cleanup_related(&path);
}
