use std::collections::HashMap;

use reddb::auth::Role;
use reddb::runtime::control_events::CONTROL_EVENTS_COLLECTION;
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
    set_current_tenant,
};
use reddb::runtime::query_audit::{QueryAuditConfig, QueryAuditRule, QUERY_AUDIT_COLLECTION};
use reddb::storage::schema::Value;
use reddb::storage::EntityData;
use reddb::{RedDBOptions, RedDBRuntime};

fn rows(rt: &RedDBRuntime, collection: &str) -> Vec<HashMap<String, Value>> {
    let Some(manager) = rt.db().store().get_collection(collection) else {
        return Vec::new();
    };
    manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| match entity.data {
            EntityData::Row(row) => row.named,
            _ => None,
        })
        .collect()
}

fn as_user<T>(tenant: &str, name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_tenant(tenant.to_string());
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    clear_current_tenant();
    out
}

#[test]
fn scoped_query_audit_records_metadata_without_raw_query_text() {
    let mut options = RedDBOptions::in_memory();
    options.query_audit = QueryAuditConfig::enabled_with_rules(vec![QueryAuditRule::new()
        .actor("alice")
        .tenant("acme")
        .collection("docs")
        .action("select")]);
    let rt = RedDBRuntime::with_options(options).expect("runtime should open");

    rt.execute_query("CREATE TABLE docs (id INT, tenant_id TEXT, body TEXT) TENANT BY (tenant_id)")
        .expect("table should be created");
    as_user("acme", "alice", Role::Write, || {
        rt.execute_query(
            "INSERT INTO docs (id, tenant_id, body) VALUES (1, 'acme', 'secret-text')",
        )
        .expect("insert should not match select-only rule");
    });

    let control_before = rows(&rt, CONTROL_EVENTS_COLLECTION).len();
    as_user("acme", "alice", Role::Read, || {
        rt.execute_query("SELECT id FROM docs WHERE body = 'secret-text'")
            .expect("select should be audited");
    });
    as_user("globex", "alice", Role::Read, || {
        rt.execute_query("SELECT id FROM docs WHERE body = 'secret-text'")
            .expect("tenant mismatch should not be audited");
    });
    as_user("acme", "bob", Role::Read, || {
        rt.execute_query("SELECT id FROM docs WHERE body = 'secret-text'")
            .expect("actor mismatch should not be audited");
    });

    let audit_rows = rows(&rt, QUERY_AUDIT_COLLECTION);
    assert_eq!(audit_rows.len(), 1, "{audit_rows:?}");
    let row = &audit_rows[0];
    assert_eq!(row.get("actor"), Some(&Value::text("alice")));
    assert_eq!(row.get("tenant"), Some(&Value::text("acme")));
    assert_eq!(row.get("statement_kind"), Some(&Value::text("select")));
    assert_eq!(row.get("touched_collections"), Some(&Value::text("docs")));
    assert_eq!(row.get("row_count"), Some(&Value::UnsignedInteger(1)));
    assert!(matches!(
        row.get("duration_ms"),
        Some(Value::UnsignedInteger(_))
    ));
    assert!(matches!(row.get("request_id"), Some(Value::Text(_))));
    assert!(matches!(row.get("query_hash"), Some(Value::Text(_))));
    assert!(
        !format!("{audit_rows:?}").contains("secret-text"),
        "query audit must not persist raw query text: {audit_rows:?}"
    );

    let control_after = rows(&rt, CONTROL_EVENTS_COLLECTION).len();
    assert_eq!(
        control_before, control_after,
        "query audit must not write data-plane events to the control ledger"
    );
}
