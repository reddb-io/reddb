use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::auth::{AuthConfig, AuthStore};
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
    }

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
