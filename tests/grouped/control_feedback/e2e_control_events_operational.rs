use std::collections::HashMap;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::control_events::CONTROL_EVENTS_COLLECTION;
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::storage::schema::Value;
use reddb::storage::EntityData;
use reddb::{RedDBOptions, RedDBRuntime};

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

fn control_event_rows(rt: &RedDBRuntime) -> Vec<HashMap<String, Value>> {
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

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

#[test]
fn ddl_and_rls_control_events_record_allowed_denied_and_error_outcomes() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open");
    let auth = std::sync::Arc::new(AuthStore::new(AuthConfig::default()));
    auth.create_user("reader", "p", Role::Read).unwrap();
    rt.set_auth_store(auth);

    rt.execute_query("CREATE TABLE docs (id INT, tenant_id TEXT, body TEXT) TENANT BY (tenant_id)")
        .expect("tenant-scoped DDL should succeed");
    rt.execute_query(
        "CREATE POLICY tenant_docs ON docs FOR SELECT USING (tenant_id = CURRENT_TENANT())",
    )
    .expect("RLS policy DDL should succeed");
    rt.execute_query("DROP POLICY tenant_docs ON docs")
        .expect("RLS policy drop should succeed");
    rt.execute_query("TRUNCATE COLLECTION docs")
        .expect("destructive DDL should succeed");

    let denied = as_user("reader", Role::Read, || {
        rt.execute_query("CREATE TABLE denied_docs (id INT)")
    })
    .expect_err("read-only principal should not create DDL");
    assert!(denied.to_string().contains("permission denied"));

    let duplicate = rt
        .execute_query("CREATE TABLE docs (id INT)")
        .expect_err("duplicate DDL should fail");
    assert!(duplicate.to_string().contains("already exists"));

    let rows = control_event_rows(&rt);
    let ledger_body = format!("{rows:?}");
    assert!(ledger_body.contains("schema.ddl"), "{ledger_body}");
    assert!(ledger_body.contains("tenant.governance"), "{ledger_body}");
    assert!(ledger_body.contains("rls.governance"), "{ledger_body}");
    assert!(ledger_body.contains("create_table"), "{ledger_body}");
    assert!(ledger_body.contains("truncate"), "{ledger_body}");
    assert!(ledger_body.contains("create_policy"), "{ledger_body}");
    assert!(ledger_body.contains("drop_policy"), "{ledger_body}");
    assert!(
        ledger_body.contains("\"outcome\": Text(\"allowed\")"),
        "{ledger_body}"
    );
    assert!(
        ledger_body.contains("\"outcome\": Text(\"denied\")"),
        "{ledger_body}"
    );
    assert!(
        ledger_body.contains("\"outcome\": Text(\"error\")"),
        "{ledger_body}"
    );
    assert!(ledger_body.contains("reader"), "{ledger_body}");
}

#[test]
fn backup_control_event_records_snapshot_and_wal_metadata() {
    let path = support::temp_db_file("control-events-backup-654");

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path.path()))
        .expect("runtime should open");
    rt.execute_query("CREATE TABLE docs (id INT)")
        .expect("table should be created before backup");

    let backup = rt.trigger_backup().expect("backup should succeed");
    assert!(backup.snapshot_id > 0);

    let rows = control_event_rows(&rt);
    let ledger_body = format!("{rows:?}");
    assert!(ledger_body.contains("backup.run"), "{ledger_body}");
    assert!(ledger_body.contains("backup_trigger"), "{ledger_body}");
    assert!(ledger_body.contains("snapshot_id"), "{ledger_body}");
    assert!(ledger_body.contains("current_lsn"), "{ledger_body}");
    assert!(ledger_body.contains("last_archived_lsn"), "{ledger_body}");
    assert!(
        ledger_body.contains("\"outcome\": Text(\"allowed\")"),
        "{ledger_body}"
    );
}
