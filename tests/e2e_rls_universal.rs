//! Phase 2.5.5 RLS universal — `CREATE POLICY ... ON <KIND> OF <col>`.
//!
//! Prove that queue MESSAGES receive their own RLS gate separate
//! from TABLE policies on the same collection, and that the gate
//! actually filters messages the caller shouldn't see.

use reddb::runtime::mvcc::{
    clear_current_connection_id, clear_current_tenant, set_current_connection_id,
    set_current_tenant,
};
use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory())
        .expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
fn policy_on_messages_of_queue_gates_consumers() {
    let rt = open_runtime();
    exec(&rt, "CREATE QUEUE jobs");

    // A MESSAGES-scoped policy filtering by `payload.tenant`.
    // `payload` is stored as the queue message's JSON value, so the
    // dotted path resolver navigates into it.
    exec(
        &rt,
        "CREATE POLICY tenant_only ON MESSAGES OF jobs \
         USING (payload.tenant = CURRENT_TENANT())",
    );
    exec(&rt, "ALTER TABLE jobs ENABLE ROW LEVEL SECURITY");

    // Seed two messages with distinct tenants.
    exec(&rt, "QUEUE PUSH jobs {tenant: 'acme', task: 'ship'}");
    exec(&rt, "QUEUE PUSH jobs {tenant: 'globex', task: 'audit'}");

    set_current_connection_id(701);

    // Acme consumer: only sees the acme message.
    set_current_tenant("acme".to_string());
    let len = rt
        .execute_query("QUEUE LEN jobs")
        .expect("QUEUE LEN acme");
    let len_value = len
        .result
        .records
        .first()
        .and_then(|r| r.values.get("len"))
        .cloned();
    assert_eq!(
        len_value,
        Some(reddb::storage::schema::Value::UnsignedInteger(1)),
        "acme should see 1 message (its own)"
    );

    // Globex consumer: sees the globex message.
    set_current_tenant("globex".to_string());
    let len = rt
        .execute_query("QUEUE LEN jobs")
        .expect("QUEUE LEN globex");
    let len_value = len
        .result
        .records
        .first()
        .and_then(|r| r.values.get("len"))
        .cloned();
    assert_eq!(
        len_value,
        Some(reddb::storage::schema::Value::UnsignedInteger(1)),
        "globex should see 1 message (its own)"
    );

    // Unbound tenant: policy evaluates against NULL ⇒ zero rows.
    clear_current_tenant();
    let len = rt
        .execute_query("QUEUE LEN jobs")
        .expect("QUEUE LEN unbound");
    let len_value = len
        .result
        .records
        .first()
        .and_then(|r| r.values.get("len"))
        .cloned();
    assert_eq!(
        len_value,
        Some(reddb::storage::schema::Value::UnsignedInteger(0)),
        "unbound tenant should see zero messages"
    );

    clear_current_connection_id();
}
