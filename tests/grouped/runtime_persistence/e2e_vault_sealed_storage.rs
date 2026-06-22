#[path = "../../support/mod.rs"]
#[allow(dead_code)]
mod support;

use std::path::Path;
use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::runtime::control_events::CONTROL_EVENTS_COLLECTION;
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::storage::schema::Value;
use reddb::storage::{EntityData, StorageDeployPreset};
use reddb::{RedDBOptions, RedDBRuntime};

const TEST_CERTIFICATE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

fn pager_backed_options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("serverless storage profile should expose pager")
}

fn open_runtime_with_vault(path: &Path) -> (RedDBRuntime, Arc<AuthStore>) {
    let rt = RedDBRuntime::with_options(pager_backed_options(path)).expect("runtime should open");
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

fn boolean(row: &reddb::storage::query::unified::UnifiedRecord, name: &str) -> bool {
    match field(row, name) {
        Value::Boolean(value) => *value,
        other => panic!("expected boolean field {name}, got {other:?}"),
    }
}

fn control_event_rows(rt: &RedDBRuntime) -> Vec<std::collections::HashMap<String, Value>> {
    rt.db()
        .store()
        .get_collection(CONTROL_EVENTS_COLLECTION)
        .expect("control events collection should exist")
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| match entity.data {
            EntityData::Row(row) => row.named,
            _ => None,
        })
        .collect()
}

fn text_field_eq(
    row: &std::collections::HashMap<String, Value>,
    name: &str,
    expected: &str,
) -> bool {
    matches!(row.get(name), Some(Value::Text(value)) if value.as_ref() == expected)
}

#[test]
fn vault_put_seals_payload_before_persistence() {
    let guard = support::temp_db_file("vault-sealed-storage");
    let path = guard.path();

    let secret = "vault_plaintext_probe_324";
    {
        let (rt, auth) = open_runtime_with_vault(path);
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

    let persisted = read_related_bytes(path);
    assert!(
        !persisted
            .windows(secret.len())
            .any(|w| w == secret.as_bytes()),
        "persistent database artifacts must not contain vault plaintext"
    );

    {
        let rt = RedDBRuntime::with_options(pager_backed_options(path))
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

    drop(guard);
}

#[test]
fn vault_metadata_and_unseal_control_events_minimize_evidence() {
    let guard = support::temp_db_file("vault-control-events-unseal-653");
    let path = guard.path();

    let secret = "tok_live_653_probe BEGIN_PRIVATE_KEY_653 BEGIN_CERTIFICATE_653";
    let (rt, auth) = open_runtime_with_vault(path);
    auth.create_user("alice", "p", Role::Write).unwrap();
    rt.execute_query("CREATE VAULT secrets WITH OWN MASTER KEY")
        .expect("create vault");
    rt.execute_query(&format!("VAULT PUT secrets.api_key = '{secret}'"))
        .expect("vault put");

    attach_alice_policy(
        &auth,
        "vault-control-events-unrelated",
        r#"[
            {"effect":"allow","actions":["vault:read_metadata"],"resources":["vault:other.key"]}
        ]"#,
    );
    let denied_get = as_user("alice", Role::Write, || {
        rt.execute_query("VAULT GET secrets.api_key")
    })
    .expect_err("metadata read without policy must fail");
    assert!(denied_get.to_string().contains("vault:read_metadata"));

    attach_alice_policy(
        &auth,
        "vault-control-events-metadata",
        r#"[
            {"effect":"allow","actions":["vault:read_metadata"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    as_user("alice", Role::Write, || {
        rt.execute_query("VAULT GET secrets.api_key")
    })
    .expect("metadata read should be allowed");

    let denied_unseal = as_user("alice", Role::Write, || {
        rt.execute_query("UNSEAL VAULT secrets.api_key")
    })
    .expect_err("unseal without policy must fail");
    assert!(denied_unseal.to_string().contains("vault:read"));

    attach_alice_policy(
        &auth,
        "vault-control-events-unseal",
        r#"[
            {"effect":"allow","actions":["vault:read"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    as_user("alice", Role::Write, || {
        rt.execute_query("UNSEAL VAULT secrets.api_key")
    })
    .expect("unseal should be allowed");

    let sealed_auth = Arc::new(AuthStore::new(AuthConfig::default()));
    sealed_auth.create_user("alice", "p", Role::Write).unwrap();
    attach_alice_policy(
        &sealed_auth,
        "vault-control-events-unseal-error",
        r#"[
            {"effect":"allow","actions":["vault:read"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    rt.set_auth_store(sealed_auth);
    let error_unseal = as_user("alice", Role::Write, || {
        rt.execute_query("UNSEAL VAULT secrets.api_key")
    })
    .expect_err("sealed key provider should produce an unseal error");
    assert!(error_unseal
        .to_string()
        .contains("vault sealed_unavailable"));

    let rows = control_event_rows(&rt);
    let ledger_body = format!("{rows:?}");
    assert!(ledger_body.contains("vault.metadata_read"), "{ledger_body}");
    assert!(ledger_body.contains("vault.read"), "{ledger_body}");
    assert!(
        ledger_body.contains("\"outcome\": Text(\"denied\")"),
        "{ledger_body}"
    );
    assert!(
        ledger_body.contains("\"outcome\": Text(\"allowed\")"),
        "{ledger_body}"
    );
    assert!(
        ledger_body.contains("\"outcome\": Text(\"error\")"),
        "{ledger_body}"
    );
    assert!(ledger_body.contains("secrets.api_key"), "{ledger_body}");
    assert!(ledger_body.contains("fingerprint"), "{ledger_body}");
    assert!(ledger_body.contains("version"), "{ledger_body}");
    assert!(rows
        .iter()
        .any(|row| text_field_eq(row, "actor_user_id", "alice")));
    assert!(
        rows.iter()
            .all(|row| row.contains_key("scope") && row.contains_key("resource")),
        "{ledger_body}"
    );
    for forbidden in [
        secret,
        "tok_live_653_probe",
        "BEGIN_PRIVATE_KEY_653",
        "BEGIN_CERTIFICATE_653",
    ] {
        assert!(
            !ledger_body.contains(forbidden),
            "control events must not store raw secret evidence `{forbidden}`: {ledger_body}"
        );
    }

    drop(guard);
}

#[test]
fn vault_rotation_and_purge_control_events_minimize_evidence() {
    let guard = support::temp_db_file("vault-control-events-lifecycle-653");
    let path = guard.path();

    let secret_v1 = "rotate_tok_live_653 BEGIN_PRIVATE_KEY_ROTATE_653";
    let secret_v2 = "purge_certificate_probe_653 BEGIN_CERTIFICATE_PURGE_653";
    let (rt, auth) = open_runtime_with_vault(path);
    auth.create_user("alice", "p", Role::Write).unwrap();

    rt.execute_query("CREATE VAULT secrets WITH OWN MASTER KEY")
        .expect("create vault");
    as_user("alice", Role::Write, || {
        rt.execute_query(&format!("VAULT PUT secrets.api_key = '{secret_v1}'"))
    })
    .expect("vault put");
    as_user("alice", Role::Write, || {
        rt.execute_query(&format!("ROTATE VAULT secrets.api_key = '{secret_v2}'"))
    })
    .expect("vault rotate");

    attach_alice_policy(
        &auth,
        "vault-control-events-purge-unrelated",
        r#"[
            {"effect":"allow","actions":["vault:read_metadata"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    let denied_purge = as_user("alice", Role::Write, || {
        rt.execute_query("PURGE VAULT secrets.api_key")
    })
    .expect_err("purge without policy must fail");
    assert!(denied_purge.to_string().contains("vault:purge"));

    attach_alice_policy(
        &auth,
        "vault-control-events-purge",
        r#"[
            {"effect":"allow","actions":["vault:purge"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    as_user("alice", Role::Write, || {
        rt.execute_query("PURGE VAULT secrets.api_key")
    })
    .expect("vault purge");

    let rows = control_event_rows(&rt);
    let ledger_body = format!("{rows:?}");
    assert!(ledger_body.contains("vault.rotate"), "{ledger_body}");
    assert!(ledger_body.contains("vault.purge"), "{ledger_body}");
    assert!(
        ledger_body.contains("\"outcome\": Text(\"denied\")"),
        "{ledger_body}"
    );
    assert!(
        ledger_body.contains("\"outcome\": Text(\"allowed\")"),
        "{ledger_body}"
    );
    assert!(ledger_body.contains("secrets.api_key"), "{ledger_body}");
    assert!(ledger_body.contains("fingerprint"), "{ledger_body}");
    assert!(ledger_body.contains("version"), "{ledger_body}");
    assert!(ledger_body.contains("purged"), "{ledger_body}");
    for forbidden in [
        secret_v1,
        secret_v2,
        "rotate_tok_live_653",
        "BEGIN_PRIVATE_KEY_ROTATE_653",
        "purge_certificate_probe_653",
        "BEGIN_CERTIFICATE_PURGE_653",
    ] {
        assert!(
            !ledger_body.contains(forbidden),
            "control events must not store raw lifecycle evidence `{forbidden}`: {ledger_body}"
        );
    }

    drop(guard);
}

#[test]
fn vault_get_is_metadata_only_and_unseal_is_capability_gated_and_audited() {
    let guard = support::temp_db_file("vault-unseal-audit");
    let path = guard.path();

    let secret = "vault_plaintext_probe_330";
    let ciphertext_hex;
    let rt;
    let auth;
    {
        let opened = open_runtime_with_vault(path);
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
    .expect_err("unseal without vault:read must fail");
    assert!(denied.to_string().contains("vault:read"));

    attach_alice_policy(
        &auth,
        "vault-unseal",
        r#"[
            {"effect":"allow","actions":["vault:read"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    let unsealed = as_user("alice", Role::Write, || {
        rt.execute_query("UNSEAL VAULT secrets.api_key")
    })
    .expect("unseal should pass with vault:read");
    assert_eq!(
        unsealed.result.records[0].get("value"),
        Some(&Value::text(secret))
    );

    assert!(rt.audit_log().wait_idle(std::time::Duration::from_secs(2)));
    let audit_body = std::fs::read_to_string(rt.audit_log().path()).unwrap_or_default();
    assert!(audit_body.contains("vault/unseal")); // audit_log path retains the legacy "unseal" subpath name for value-reveal events
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

    drop(guard);
}

#[test]
fn vault_lifecycle_versions_history_purge_and_historical_unseal_are_audited() {
    let guard = support::temp_db_file("vault-lifecycle-325");
    let path = guard.path();

    let secret_v1 = "vault_plaintext_probe_325_v1";
    let secret_v2 = "vault_plaintext_probe_325_v2";
    let (rt, auth) = open_runtime_with_vault(path);
    auth.create_user("alice", "p", Role::Write).unwrap();

    rt.execute_query("CREATE VAULT secrets WITH OWN MASTER KEY")
        .expect("create vault");
    as_user("alice", Role::Write, || {
        rt.execute_query(&format!("VAULT PUT secrets.api_key = '{secret_v1}'"))
    })
    .expect("vault put");
    as_user("alice", Role::Write, || {
        rt.execute_query(&format!("ROTATE VAULT secrets.api_key = '{secret_v2}'"))
    })
    .expect("vault rotate");

    attach_alice_policy(
        &auth,
        "vault-lifecycle-current",
        r#"[
            {"effect":"allow","actions":["vault:read_metadata"],"resources":["vault:secrets.api_key"]},
            {"effect":"allow","actions":["vault:read"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );

    let current = as_user("alice", Role::Write, || {
        rt.execute_query("VAULT GET secrets.api_key")
    })
    .expect("vault metadata");
    let row = &current.result.records[0];
    assert_eq!(integer(row, "version"), 2);
    assert_eq!(text(row, "status"), "sealed");
    assert!(!boolean(row, "tombstone"));
    assert_eq!(text(row, "op"), "rotate");

    let unsealed_current = as_user("alice", Role::Write, || {
        rt.execute_query("UNSEAL VAULT secrets.api_key")
    })
    .expect("current unseal");
    assert_eq!(
        unsealed_current.result.records[0].get("value"),
        Some(&Value::text(secret_v2))
    );

    let history = as_user("alice", Role::Write, || {
        rt.execute_query("HISTORY VAULT secrets.api_key")
    })
    .expect("vault history");
    assert_eq!(history.result.records.len(), 2);
    assert!(!history.result.columns.contains(&"value".to_string()));
    assert_eq!(integer(&history.result.records[0], "version"), 1);
    assert_eq!(text(&history.result.records[0], "op"), "put");
    assert_eq!(integer(&history.result.records[1], "version"), 2);
    assert_eq!(text(&history.result.records[1], "op"), "rotate");
    let history_debug = format!("{:?}", history.result.records);
    assert!(!history_debug.contains(secret_v1));
    assert!(!history_debug.contains(secret_v2));
    assert!(!history_debug.contains("Secret("));

    let denied_old = as_user("alice", Role::Write, || {
        rt.execute_query("UNSEAL VAULT secrets.api_key VERSION 1")
    })
    .expect_err("historical unseal needs stronger capability");
    assert!(denied_old.to_string().contains("vault:unseal_history"));

    attach_alice_policy(
        &auth,
        "vault-lifecycle-history",
        r#"[
            {"effect":"allow","actions":["vault:unseal_history"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    let old = as_user("alice", Role::Write, || {
        rt.execute_query("UNSEAL VAULT secrets.api_key VERSION 1")
    })
    .expect("historical unseal");
    assert_eq!(
        old.result.records[0].get("value"),
        Some(&Value::text(secret_v1))
    );

    as_user("alice", Role::Write, || {
        rt.execute_query("DELETE VAULT secrets.api_key")
    })
    .expect("vault tombstone delete");
    let deleted = as_user("alice", Role::Write, || {
        rt.execute_query("VAULT GET secrets.api_key")
    })
    .expect("deleted metadata");
    let row = &deleted.result.records[0];
    assert_eq!(integer(row, "version"), 3);
    assert_eq!(text(row, "status"), "deleted");
    assert!(boolean(row, "tombstone"));
    assert_eq!(text(row, "op"), "delete");

    let denied_purge = as_user("alice", Role::Write, || {
        rt.execute_query("PURGE VAULT secrets.api_key")
    })
    .expect_err("purge needs stronger capability");
    let denied_purge = denied_purge.to_string();
    assert!(denied_purge.contains("vault:purge"), "{denied_purge}");

    attach_alice_policy(
        &auth,
        "vault-lifecycle-purge",
        r#"[
            {"effect":"allow","actions":["vault:purge"],"resources":["vault:secrets.api_key"]}
        ]"#,
    );
    let purged = as_user("alice", Role::Write, || {
        rt.execute_query("PURGE VAULT secrets.api_key")
    })
    .expect("vault purge");
    assert_eq!(integer(&purged.result.records[0], "purged"), 3);

    let history_after_purge = as_user("alice", Role::Write, || {
        rt.execute_query("HISTORY VAULT secrets.api_key")
    })
    .expect("history after purge");
    assert!(history_after_purge.result.records.is_empty());

    assert!(rt.audit_log().wait_idle(std::time::Duration::from_secs(2)));
    let audit_body = std::fs::read_to_string(rt.audit_log().path()).unwrap_or_default();
    assert!(audit_body.contains("vault/rotate"));
    assert!(audit_body.contains("vault/delete"));
    assert!(audit_body.contains("vault/purge"));
    assert!(audit_body.contains("\"outcome\":\"denied\""));
    assert!(audit_body.contains("\"outcome\":\"success\""));
    assert!(!audit_body.contains(secret_v1));
    assert!(!audit_body.contains(secret_v2));

    drop(guard);
}

#[test]
fn create_vault_requires_unsealed_key_provider() {
    let guard = support::temp_db_file("vault-create-requires-key");
    let rt = RedDBRuntime::with_options(pager_backed_options(guard.path()))
        .expect("runtime should open");

    let err = rt
        .execute_query("CREATE VAULT secrets")
        .expect_err("CREATE VAULT must not simulate key material");
    assert!(err.to_string().contains("enabled, unsealed vault"));

    drop(guard);
}
