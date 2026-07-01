//! End-to-end coverage for the `$kv.*` plain user KV store (#1602).
//!
//! Sibling to `e2e_secret_sql.rs`. Where `$secret.*` reads the encrypted
//! vault, `$kv.*` reads an independent plain (non-encrypted) flat-map
//! backed by the `red_kv` collection. The IAM gates mirror the secret
//! gates on the `kv:read` / `kv:write` verbs and the `kv:<key>` resource.

#[path = "../../support/mod.rs"]
mod support;

use std::sync::Arc;

use reddb::auth::enforcement_mode::PolicyEnforcementMode;
use reddb::auth::{AuthConfig, AuthStore, Role, UserId};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::storage::schema::Value;
use reddb::storage::StorageDeployPreset;
use reddb::{RedDBOptions, RedDBRuntime};

use support::TempDbFile;

const TEST_CERTIFICATE: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

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

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

fn attach_policy_to_user(
    auth: &AuthStore,
    user: &str,
    id: &str,
    actions: &[&str],
    resources: &[&str],
) {
    if auth.get_user(None, user).is_none() {
        auth.create_user(user, "p", Role::Write).unwrap();
    }
    let actions = actions
        .iter()
        .map(|action| format!(r#""{action}""#))
        .collect::<Vec<_>>()
        .join(",");
    let resources = resources
        .iter()
        .map(|resource| format!(r#""{resource}""#))
        .collect::<Vec<_>>()
        .join(",");
    let policy = format!(
        r#"{{
            "id":"{id}",
            "version":1,
            "statements":[{{"effect":"allow","actions":[{actions}],"resources":[{resources}]}}]
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
fn set_kv_persists_plainly_and_delete_removes() {
    let (_path, rt, _auth) = open_runtime_with_vault("kv_sql_set_delete");

    let set = rt
        .execute_query("SET KV acme.key = 'plain-value'")
        .expect("SET KV should succeed");
    assert_eq!(set.statement, "set_kv");

    // Stored unencrypted in the dedicated `red_kv` collection, separate
    // from the vault — `$kv.*` reads it back verbatim (no masking).
    let read = rt
        .execute_query("SELECT $kv.acme.key AS s")
        .expect("KV read should succeed");
    assert_eq!(read.result.records.len(), 1);
    assert_eq!(
        read.result.records[0].get("s"),
        Some(&Value::Text("plain-value".into()))
    );

    rt.execute_query("DELETE KV acme.key")
        .expect("DELETE KV should succeed");
    let after = rt
        .execute_query("SELECT $kv.acme.key AS s")
        .expect("KV read after delete should succeed");
    assert_eq!(after.result.records.len(), 1);
    assert_eq!(after.result.records[0].get("s"), Some(&Value::Null));
}

#[test]
fn dollar_kv_reference_requires_kv_read_policy_in_policy_only_mode() {
    let (_path, rt, auth) = open_runtime_with_vault("kv_sql_iam_read_policy_only");
    auth.set_enforcement_mode(PolicyEnforcementMode::PolicyOnly);

    rt.db().store().set_kv_entry("acme.key", Value::text("match-me"));
    rt.execute_query("CREATE TABLE tokens (id INT, token TEXT)")
        .expect("create table");
    rt.execute_query("INSERT INTO tokens (id, token) VALUES (1, 'match-me')")
        .expect("insert row");
    attach_policy_to_user(
        &auth,
        "alice",
        "select-tokens",
        &["select"],
        &["table:tokens", "column:tokens.id", "column:tokens.token"],
    );

    // Denied `kv:read` resolves the reference to NULL, so the row never
    // matches — no error is raised.
    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT id FROM tokens WHERE token = $kv.acme.key")
    })
    .expect("denied KV reads resolve as missing values");
    assert!(denied.result.records.is_empty());

    attach_policy_to_user(
        &auth,
        "alice",
        "kv-read-acme",
        &["kv:read"],
        &["kv:acme.*"],
    );
    let allowed = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT id FROM tokens WHERE token = $kv.acme.key")
    })
    .expect("kv:read policy should allow the KV reference");
    assert_eq!(allowed.result.records.len(), 1);
    assert_eq!(
        allowed.result.records[0].get("id"),
        Some(&Value::Integer(1))
    );
}

#[test]
fn kv_writes_require_kv_write_policy_in_policy_only_mode() {
    let (_path, rt, auth) = open_runtime_with_vault("kv_sql_iam_write_policy_only");
    auth.set_enforcement_mode(PolicyEnforcementMode::PolicyOnly);

    let denied_set = as_user("alice", Role::Write, || {
        rt.execute_query("SET KV acme.key = 'val'")
    })
    .expect_err("SET KV should require kv:write");
    assert!(denied_set.to_string().contains("kv:write"));

    attach_policy_to_user(
        &auth,
        "alice",
        "kv-write-acme",
        &["kv:write"],
        &["kv:acme.key"],
    );
    as_user("alice", Role::Write, || {
        rt.execute_query("SET KV acme.key = 'val'")
    })
    .expect("kv:write should allow SET KV");

    // A different principal without the grant cannot delete.
    let denied_delete = as_user("bob", Role::Write, || {
        rt.execute_query("DELETE KV acme.key")
    })
    .expect_err("DELETE KV should require kv:write");
    assert!(denied_delete.to_string().contains("kv:write"));

    as_user("alice", Role::Write, || {
        rt.execute_query("DELETE KV acme.key")
    })
    .expect("kv:write should allow DELETE KV");
}

#[test]
fn legacy_rbac_admin_can_read_and_write_plain_kv_without_policy() {
    let (_path, rt, auth) = open_runtime_with_vault("kv_sql_iam_legacy_admin");
    auth.set_enforcement_mode(PolicyEnforcementMode::LegacyRbac);

    as_user("admin", Role::Admin, || {
        rt.execute_query("SET KV acme.key = 'legacy'")
    })
    .expect("legacy admin should write plain KV without a policy");

    auth.create_user("admin", "p", Role::Admin).unwrap();
    attach_policy_to_user(&auth, "admin", "select-any", &["select"], &["table:any"]);
    let read = as_user("admin", Role::Admin, || {
        rt.execute_query("SELECT $kv.acme.key AS s")
    })
    .expect("legacy admin should read plain KV without a policy");
    assert_eq!(read.result.records.len(), 1);
    assert_eq!(
        read.result.records[0].get("s"),
        Some(&Value::Text("legacy".into()))
    );

    as_user("admin", Role::Admin, || {
        rt.execute_query("DELETE KV acme.key")
    })
    .expect("legacy admin should delete plain KV without a policy");
}
