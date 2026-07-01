//! `$kv.*` plain (non-encrypted) user KV store — resolver + `SET KV` /
//! `DELETE KV` DDL + `kv:read` / `kv:write` IAM gates (#1602).
//!
//! Sibling to `e2e_secret_sql.rs`: same shape, but the KV store is a
//! separate flat-map with no vault encryption, so projected values are
//! returned verbatim (not masked with `***`).

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

fn open_runtime_with_auth(name: &str) -> (TempDbFile, RedDBRuntime, Arc<AuthStore>) {
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
fn set_kv_stores_plain_entry_and_dollar_kv_resolves_verbatim() {
    let (_path, rt, auth) = open_runtime_with_auth("kv_sql_set_and_resolve");

    rt.execute_query("CREATE TABLE items (id INT, val TEXT)")
        .expect("create table");
    rt.execute_query("INSERT INTO items (id, val) VALUES (1, 'plain-val'), (2, 'other')")
        .expect("insert rows");
    rt.execute_query("SET KV acme.key = 'plain-val'")
        .expect("SET KV should succeed");

    // Stored in the plain KV flat-map, NOT the encrypted vault (#1602).
    assert_eq!(auth.plain_kv_get("acme.key").as_deref(), Some("plain-val"));
    assert!(auth.vault_kv_get("acme.key").is_none());

    // Unlike secrets, the projected value is returned verbatim (not `***`).
    let projected = rt
        .execute_query("SELECT $kv.acme.key AS kv_value FROM items LIMIT 1")
        .expect("project kv");
    assert_eq!(
        projected.result.records[0].get("kv_value"),
        Some(&Value::Text("plain-val".into()))
    );

    // Resolves in filter position too.
    let filtered = rt
        .execute_query("SELECT id FROM items WHERE val = $kv.acme.key")
        .expect("filter by kv");
    assert_eq!(filtered.result.records.len(), 1);
    assert_eq!(
        filtered.result.records[0].get("id"),
        Some(&Value::Integer(1))
    );
}

#[test]
fn delete_kv_removes_entry() {
    let (_path, rt, auth) = open_runtime_with_auth("kv_sql_delete");

    rt.execute_query("SET KV acme.key = 'v'")
        .expect("SET KV should succeed");
    assert_eq!(auth.plain_kv_get("acme.key").as_deref(), Some("v"));

    rt.execute_query("DELETE KV acme.key")
        .expect("DELETE KV should succeed");
    assert!(auth.plain_kv_get("acme.key").is_none());
}

#[test]
fn dollar_kv_reference_requires_kv_read_policy_in_policy_only_mode() {
    let (_path, rt, auth) = open_runtime_with_auth("kv_sql_iam_read_policy_only");
    auth.set_enforcement_mode(PolicyEnforcementMode::PolicyOnly);

    auth.plain_kv_set("acme.key".to_string(), "match-me".to_string());
    rt.execute_query("CREATE TABLE items (id INT, val TEXT)")
        .expect("create table");
    rt.execute_query("INSERT INTO items (id, val) VALUES (1, 'match-me')")
        .expect("insert row");
    attach_policy_to_user(
        &auth,
        "alice",
        "select-items",
        &["select"],
        &["table:items", "column:items.id", "column:items.val"],
    );

    // Denied reads resolve to NULL — the filter matches nothing, no error.
    let denied = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT id FROM items WHERE val = $kv.acme.key")
    })
    .expect("denied kv reads resolve as missing values");
    assert!(denied.result.records.is_empty());

    attach_policy_to_user(&auth, "alice", "kv-read-acme", &["kv:read"], &["kv:acme.*"]);

    let allowed = as_user("alice", Role::Write, || {
        rt.execute_query("SELECT id FROM items WHERE val = $kv.acme.key")
    })
    .expect("kv:read policy should allow the kv reference");
    assert_eq!(allowed.result.records.len(), 1);
    assert_eq!(
        allowed.result.records[0].get("id"),
        Some(&Value::Integer(1))
    );
}

#[test]
fn kv_writes_require_kv_write_policy_in_policy_only_mode() {
    let (_path, rt, auth) = open_runtime_with_auth("kv_sql_iam_write_policy_only");
    auth.set_enforcement_mode(PolicyEnforcementMode::PolicyOnly);

    let denied_set = as_user("alice", Role::Write, || {
        rt.execute_query("SET KV acme.key = 'val'")
    })
    .expect_err("SET KV should require kv:write");
    assert!(denied_set.to_string().contains("kv:write"));
    assert!(auth.plain_kv_get("acme.key").is_none());

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
    assert_eq!(auth.plain_kv_get("acme.key").as_deref(), Some("val"));

    let denied_delete = as_user("bob", Role::Write, || {
        rt.execute_query("DELETE KV acme.key")
    })
    .expect_err("DELETE KV should require kv:write");
    assert!(denied_delete.to_string().contains("kv:write"));

    as_user("alice", Role::Write, || {
        rt.execute_query("DELETE KV acme.key")
    })
    .expect("kv:write should allow DELETE KV");
    assert!(auth.plain_kv_get("acme.key").is_none());
}

#[test]
fn legacy_rbac_admin_can_read_and_write_plain_kv_without_policy() {
    let (_path, rt, auth) = open_runtime_with_auth("kv_sql_iam_legacy_admin");
    auth.set_enforcement_mode(PolicyEnforcementMode::LegacyRbac);

    as_user("admin", Role::Admin, || {
        rt.execute_query("SET KV acme.key = 'legacy'")
    })
    .expect("legacy admin should write plain KV");
    assert_eq!(auth.plain_kv_get("acme.key").as_deref(), Some("legacy"));

    auth.create_user("admin", "p", Role::Admin).unwrap();
    attach_policy_to_user(&auth, "admin", "select-any", &["select"], &["table:any"]);

    let read = as_user("admin", Role::Admin, || {
        rt.execute_query("SELECT $kv.acme.key AS s")
    })
    .expect("legacy admin should read plain KV");
    assert_eq!(
        read.result.records[0].get("s"),
        Some(&Value::Text("legacy".into()))
    );

    as_user("admin", Role::Admin, || {
        rt.execute_query("DELETE KV acme.key")
    })
    .expect("legacy admin should delete plain KV");
    assert!(auth.plain_kv_get("acme.key").is_none());
}
