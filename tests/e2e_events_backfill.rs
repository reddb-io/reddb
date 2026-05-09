use reddb::application::ExecuteQueryInput;
use reddb::runtime::mvcc::{clear_current_tenant, set_current_tenant};
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("{sql}: {err}"))
}

fn read_event_payloads(rt: &RedDBRuntime, queue: &str, count: u64) -> Vec<serde_json::Value> {
    let result = exec(rt, &format!("QUEUE PEEK {queue} {count}"));
    result
        .result
        .records
        .into_iter()
        .map(|record| match record.get("payload") {
            Some(Value::Json(bytes)) => {
                serde_json::from_slice(bytes).expect("event payload should be valid JSON")
            }
            other => panic!("expected Json payload, got {other:?}"),
        })
        .collect()
}

fn queue_len(rt: &RedDBRuntime, queue: &str) -> u64 {
    let result = exec(rt, &format!("QUEUE LEN {queue}"));
    match result.result.records[0].get("len") {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => *value as u64,
        other => panic!("expected queue len integer, got {other:?}"),
    }
}

#[test]
fn backfill_enqueues_synthetic_events_with_where_and_limit() {
    let rt = rt();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT, status TEXT)");
    exec(
        &rt,
        "INSERT INTO users (id, email, status) VALUES (1, 'a@example.com', 'active'), (2, 'b@example.com', 'inactive'), (3, 'c@example.com', 'active')",
    );
    exec(&rt, "ALTER TABLE users ADD SUBSCRIPTION audit_sub TO audit");

    let result = exec(
        &rt,
        "EVENTS BACKFILL users WHERE status = 'active' TO audit LIMIT 1",
    );
    assert_eq!(result.affected_rows, 1);

    let payloads = read_event_payloads(&rt, "audit", 5);
    assert_eq!(payloads.len(), 1);
    assert_eq!(payloads[0]["synthetic"], serde_json::Value::Bool(true));
    assert_eq!(
        payloads[0]["op"],
        serde_json::Value::String("insert".into())
    );
    assert_eq!(
        payloads[0]["after"]["status"],
        serde_json::Value::String("active".into())
    );
}

#[test]
fn backfill_rerun_is_idempotent_and_uses_subscription_redact() {
    let rt = rt();
    exec(
        &rt,
        "CREATE TABLE accounts (id INT, email TEXT, label TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO accounts (id, email, label) VALUES (1, 'a@example.com', 'a'), (2, 'b@example.com', 'b')",
    );
    exec(
        &rt,
        "ALTER TABLE accounts ADD SUBSCRIPTION masked TO audit REDACT (email)",
    );

    exec(&rt, "EVENTS BACKFILL accounts TO audit");
    exec(&rt, "EVENTS BACKFILL accounts TO audit");

    assert_eq!(queue_len(&rt, "audit"), 2);
    let payloads = read_event_payloads(&rt, "audit", 5);
    assert_eq!(payloads.len(), 2);
    let event_ids = payloads
        .iter()
        .filter_map(|payload| payload.get("event_id").and_then(|value| value.as_str()))
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(event_ids.len(), 2);
    for payload in payloads {
        assert_eq!(payload["synthetic"], serde_json::Value::Bool(true));
        assert_eq!(
            payload["after"]["email"],
            serde_json::Value::String("[REDACTED]".into())
        );
    }
}

#[test]
fn backfill_respects_tenant_scope() {
    let rt = rt();
    exec(
        &rt,
        "CREATE TABLE users (id INT, tenant_id TEXT, name TEXT) TENANT BY (tenant_id)",
    );

    set_current_tenant("acme".to_string());
    exec(&rt, "INSERT INTO users (id, name) VALUES (1, 'Alice')");
    set_current_tenant("globex".to_string());
    exec(&rt, "INSERT INTO users (id, name) VALUES (2, 'Bob')");
    clear_current_tenant();

    exec(&rt, "ALTER TABLE users ADD SUBSCRIPTION audit_sub TO audit");

    set_current_tenant("acme".to_string());
    exec(&rt, "EVENTS BACKFILL users TO audit");
    clear_current_tenant();

    let payloads = read_event_payloads(&rt, "acme__audit", 5);
    assert_eq!(payloads.len(), 1);
    assert_eq!(
        payloads[0]["tenant"],
        serde_json::Value::String("acme".into())
    );
    assert_eq!(
        payloads[0]["after"]["name"],
        serde_json::Value::String("Alice".into())
    );
    assert!(
        rt.db().collection_contract("globex__audit").is_none(),
        "backfill under acme must not create a globex queue"
    );
}
