use reddb::application::ExecuteQueryInput;
use reddb::catalog::{CollectionModel, SubscriptionOperation};
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

fn exec_err(rt: &RedDBRuntime, sql: &str) -> String {
    match QueryUseCases::new(rt).execute(ExecuteQueryInput {
        query: sql.to_string(),
    }) {
        Ok(_) => panic!("expected error for {sql}"),
        Err(err) => err.to_string(),
    }
}

fn read_event_payload(rt: &RedDBRuntime, queue: &str) -> serde_json::Value {
    let result = exec(
        rt,
        &format!("QUEUE READ {queue} GROUP evt_readers CONSUMER c1 COUNT 1"),
    );
    let record = result
        .result
        .records
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no event in queue {queue}"));
    match record.get("payload") {
        Some(Value::Json(bytes)) => {
            serde_json::from_slice(bytes).expect("event payload should be valid JSON")
        }
        other => panic!("expected Json payload, got {other:?}"),
    }
}

fn setup_event_queue_group(rt: &RedDBRuntime, queue: &str) {
    exec(rt, &format!("QUEUE GROUP CREATE {queue} evt_readers"));
}

fn text(record: &reddb::storage::query::unified::UnifiedRecord, field: &str) -> String {
    match record.get(field) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text field {field}, got {other:?}"),
    }
}

fn uint(record: &reddb::storage::query::unified::UnifiedRecord, field: &str) -> u64 {
    match record.get(field) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer field {field}, got {other:?}"),
    }
}

fn text_array(record: &reddb::storage::query::unified::UnifiedRecord, field: &str) -> Vec<String> {
    match record.get(field) {
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                Value::Text(text) => text.to_string(),
                other => panic!("expected text array item in {field}, got {other:?}"),
            })
            .collect(),
        other => panic!("expected array field {field}, got {other:?}"),
    }
}

#[test]
fn red_subscriptions_lists_event_subscription_status() {
    let rt = rt();

    exec(
        &rt,
        "CREATE TABLE users (id INT, email TEXT, status TEXT) WITH EVENTS (INSERT, UPDATE) REDACT (email) WHERE status = 'active'",
    );

    let result = exec(
        &rt,
        "SELECT name, collection, target_queue, mode, ops_filter, where_filter, redact_fields, enabled, outbox_lag_ms, dlq_count, created_at FROM red.subscriptions",
    );

    assert_eq!(result.result.records.len(), 1);
    let row = &result.result.records[0];
    assert_eq!(text(row, "collection"), "users");
    assert_eq!(text(row, "target_queue"), "users_events");
    assert_eq!(text(row, "mode"), "FANOUT");
    assert_eq!(
        text_array(row, "ops_filter"),
        vec!["INSERT".to_string(), "UPDATE".to_string()]
    );
    assert_eq!(text(row, "where_filter"), "status = 'active'");
    assert_eq!(text_array(row, "redact_fields"), vec!["email".to_string()]);
    assert_eq!(row.get("enabled"), Some(&Value::Boolean(true)));
    assert_eq!(uint(row, "outbox_lag_ms"), 0);
    assert_eq!(uint(row, "dlq_count"), 0);
    assert!(matches!(row.get("created_at"), Some(Value::TimestampMs(_))));
}

#[test]
fn events_status_filters_subscriptions_by_collection() {
    let rt = rt();

    exec(&rt, "CREATE TABLE users (id INT) WITH EVENTS TO users_audit");
    exec(&rt, "CREATE TABLE orders (id INT) WITH EVENTS TO orders_audit");

    let result = exec(&rt, "EVENTS STATUS users");

    assert_eq!(result.result.records.len(), 1);
    assert_eq!(text(&result.result.records[0], "collection"), "users");
    assert_eq!(
        text(&result.result.records[0], "target_queue"),
        "users_audit"
    );
}

#[test]
fn events_status_reports_outbox_dlq_count() {
    let rt = rt();

    exec(&rt, "CREATE QUEUE audit_events FANOUT MAX_SIZE 1");
    exec(&rt, "CREATE TABLE users (id INT) WITH EVENTS TO audit_events");
    exec(&rt, "INSERT INTO users (id) VALUES (1)");
    exec(&rt, "INSERT INTO users (id) VALUES (2)");

    let result = exec(&rt, "EVENTS STATUS users");

    assert_eq!(result.result.records.len(), 1);
    assert_eq!(uint(&result.result.records[0], "dlq_count"), 1);
}

#[test]
fn create_table_with_events_persists_subscription_and_auto_queue() {
    let rt = rt();

    exec(
        &rt,
        "CREATE TABLE users (id INT, email TEXT, phone TEXT, status TEXT) WITH EVENTS (INSERT, UPDATE) REDACT (email, phone) WHERE status = 'active'",
    );

    let users = rt
        .db()
        .collection_contract("users")
        .expect("users contract");
    assert_eq!(users.subscriptions.len(), 1);
    let subscription = &users.subscriptions[0];
    assert_eq!(subscription.source, "users");
    assert_eq!(subscription.target_queue, "users_events");
    assert_eq!(
        subscription.ops_filter,
        vec![
            SubscriptionOperation::Insert,
            SubscriptionOperation::Update
        ]
    );
    assert_eq!(subscription.redact_fields, vec!["email", "phone"]);
    assert_eq!(subscription.where_filter.as_deref(), Some("status = 'active'"));

    let queue = rt
        .db()
        .collection_contract("users_events")
        .expect("auto-created event queue");
    assert_eq!(queue.declared_model, CollectionModel::Queue);
    assert!(
        rt.db()
            .store()
            .get_config("queue.users_events.mode")
            .is_some_and(|value| format!("{value:?}").contains("fanout")),
        "auto-created queue should be marked fanout"
    );
}

#[test]
fn explicit_event_queue_is_created_and_alter_can_disable_events() {
    let rt = rt();

    exec(&rt, "CREATE TABLE users (id INT) WITH EVENTS TO audit_log");
    assert!(rt.db().collection_contract("audit_log").is_some());

    exec(&rt, "ALTER TABLE users DISABLE EVENTS");
    let users = rt
        .db()
        .collection_contract("users")
        .expect("users contract");
    assert!(!users.subscriptions[0].enabled);
}

#[test]
fn event_subscription_cycle_is_rejected() {
    let rt = rt();

    exec(&rt, "CREATE TABLE users (id INT)");
    exec(&rt, "CREATE TABLE audit (id INT) WITH EVENTS TO users");

    let err = exec_err(&rt, "ALTER TABLE users ENABLE EVENTS TO audit");
    assert!(
        err.contains("subscription would create cycle"),
        "unexpected error: {err}"
    );
}

// ── REDACT conformance ─────────────────────────────────────────────────────

#[test]
fn redact_flat_field_removed_from_insert_event() {
    let rt = rt();
    exec(
        &rt,
        "CREATE TABLE users (id INT, email TEXT, name TEXT) WITH EVENTS (INSERT) REDACT (email)",
    );
    setup_event_queue_group(&rt, "users_events");

    exec(
        &rt,
        "INSERT INTO users (id, email, name) VALUES (1, 'alice@example.com', 'Alice')",
    );

    let payload = read_event_payload(&rt, "users_events");
    let after = payload.get("after").expect("after field present");
    assert_eq!(
        after.get("email").and_then(|v| v.as_str()),
        Some("[REDACTED]"),
        "email must be redacted in insert event"
    );
    assert_eq!(
        after.get("name").and_then(|v| v.as_str()),
        Some("Alice"),
        "non-redacted field must be intact"
    );
}

#[test]
fn redact_multiple_fields_all_removed() {
    let rt = rt();
    exec(
        &rt,
        "CREATE TABLE accounts (id INT, email TEXT, phone TEXT, ssn TEXT, label TEXT) WITH EVENTS (INSERT) REDACT (email, phone, ssn)",
    );
    setup_event_queue_group(&rt, "accounts_events");

    exec(
        &rt,
        "INSERT INTO accounts (id, email, phone, ssn, label) VALUES (1, 'x@x.com', '555', '123-45-6789', 'main')",
    );

    let payload = read_event_payload(&rt, "accounts_events");
    let after = payload.get("after").expect("after field present");
    assert_eq!(
        after.get("email").and_then(|v| v.as_str()),
        Some("[REDACTED]")
    );
    assert_eq!(
        after.get("phone").and_then(|v| v.as_str()),
        Some("[REDACTED]")
    );
    assert_eq!(
        after.get("ssn").and_then(|v| v.as_str()),
        Some("[REDACTED]")
    );
    assert_eq!(
        after.get("label").and_then(|v| v.as_str()),
        Some("main"),
        "non-redacted field must remain"
    );
}

#[test]
fn redact_nested_dotted_path() {
    let rt = rt();
    // body is stored as a JSON document column; we redact body.user.email
    exec(
        &rt,
        "CREATE TABLE docs (id INT, body JSON) WITH EVENTS (INSERT) REDACT (body.user.email)",
    );
    setup_event_queue_group(&rt, "docs_events");

    exec(
        &rt,
        r#"INSERT INTO docs (id, body) VALUES (1, '{"user":{"email":"secret@example.com","name":"Bob"},"title":"hello"}')"#,
    );

    let payload = read_event_payload(&rt, "docs_events");
    let after = payload.get("after").expect("after field present");
    let user = after
        .get("body")
        .and_then(|b| b.get("user"))
        .expect("body.user present");
    assert_eq!(
        user.get("email").and_then(|v| v.as_str()),
        Some("[REDACTED]"),
        "nested body.user.email must be redacted"
    );
    assert_eq!(
        user.get("name").and_then(|v| v.as_str()),
        Some("Bob"),
        "sibling field must be intact"
    );
}

#[test]
fn redact_wildcard_nested_path() {
    let rt = rt();
    // body.*.email strips email from every sub-object inside body
    exec(
        &rt,
        "CREATE TABLE events_log (id INT, body JSON) WITH EVENTS (INSERT) REDACT (body.*.email)",
    );
    setup_event_queue_group(&rt, "events_log_events");

    exec(
        &rt,
        r#"INSERT INTO events_log (id, body) VALUES (1, '{"sender":{"email":"a@x.com","name":"A"},"recipient":{"email":"b@x.com","name":"B"}}')"#,
    );

    let payload = read_event_payload(&rt, "events_log_events");
    let after = payload.get("after").expect("after field present");
    let body = after.get("body").expect("body present");
    assert_eq!(
        body.get("sender")
            .and_then(|v| v.get("email"))
            .and_then(|v| v.as_str()),
        Some("[REDACTED]"),
        "body.sender.email must be redacted via wildcard"
    );
    assert_eq!(
        body.get("recipient")
            .and_then(|v| v.get("email"))
            .and_then(|v| v.as_str()),
        Some("[REDACTED]"),
        "body.recipient.email must be redacted via wildcard"
    );
    assert_eq!(
        body.get("sender")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str()),
        Some("A"),
        "non-redacted sibling must remain"
    );
}

#[test]
fn redact_applied_to_before_in_delete_event() {
    let rt = rt();
    exec(
        &rt,
        "CREATE TABLE profiles (id INT, email TEXT, role TEXT) WITH EVENTS (INSERT, DELETE) REDACT (email)",
    );
    setup_event_queue_group(&rt, "profiles_events");

    exec(
        &rt,
        "INSERT INTO profiles (id, email, role) VALUES (42, 'del@example.com', 'admin')",
    );
    // drain the insert event
    read_event_payload(&rt, "profiles_events");

    exec(&rt, "DELETE FROM profiles WHERE id = 42");
    let payload = read_event_payload(&rt, "profiles_events");
    assert_eq!(
        payload.get("op").and_then(|v| v.as_str()),
        Some("delete")
    );
    let before = payload.get("before").expect("before field present in delete event");
    assert_eq!(
        before.get("email").and_then(|v| v.as_str()),
        Some("[REDACTED]"),
        "email must be redacted in before of delete event"
    );
    assert_eq!(
        before.get("role").and_then(|v| v.as_str()),
        Some("admin"),
        "non-redacted field must remain in before"
    );
}

// ── Tenant isolation conformance ───────────────────────────────────────────

/// Conformance case 1: tenant A INSERT lands in acme__users_events only, not in globex__users_events.
#[test]
fn tenant_insert_routes_to_tenant_scoped_queue() {
    let rt = rt();
    exec(&rt, "CREATE TABLE users (id INT, name TEXT) WITH EVENTS");

    // In acme context: event must land in acme__users_events
    set_current_tenant("acme".to_string());
    exec(&rt, "INSERT INTO users (id, name) VALUES (1, 'Alice')");
    clear_current_tenant();

    // Create group and read from scoped queue
    exec(&rt, "QUEUE GROUP CREATE acme__users_events evt_readers");
    let payload = read_event_payload(&rt, "acme__users_events");
    assert_eq!(
        payload.get("tenant").and_then(|v| v.as_str()),
        Some("acme"),
        "event payload must carry acme tenant"
    );
    assert_eq!(
        payload.get("op").and_then(|v| v.as_str()),
        Some("insert")
    );

    // globex__users_events must not exist (no event was inserted under globex)
    let globex_queue = rt.db().collection_contract("globex__users_events");
    assert!(
        globex_queue.is_none(),
        "globex queue must not exist: acme event must not bleed"
    );
}

/// Conformance case 2: cross-tenant subscription rejected when tenant context active.
#[test]
fn cross_tenant_subscription_rejected_without_capability() {
    let rt = rt();
    exec(&rt, "CREATE TABLE users (id INT) WITH EVENTS");

    // In tenant context, ON ALL TENANTS must be rejected
    set_current_tenant("acme".to_string());
    let err = exec_err(
        &rt,
        "ALTER TABLE users ENABLE EVENTS TO global_audit ON ALL TENANTS",
    );
    clear_current_tenant();

    assert!(
        err.contains("cross-tenant") || err.contains("cluster-admin"),
        "unexpected error: {err}"
    );
}

/// Conformance case 3: cluster-admin (no tenant) can create all-tenants subscription.
#[test]
fn cross_tenant_subscription_allowed_for_cluster_admin() {
    let rt = rt();
    exec(&rt, "CREATE TABLE users (id INT) WITH EVENTS");

    // No tenant set = cluster-admin context: ON ALL TENANTS must succeed
    exec(
        &rt,
        "ALTER TABLE users ENABLE EVENTS TO global_audit ON ALL TENANTS",
    );

    let contract = rt
        .db()
        .collection_contract("users")
        .expect("users contract");
    let all_tenants_sub = contract
        .subscriptions
        .iter()
        .find(|s| s.target_queue == "global_audit")
        .expect("global_audit subscription should exist");
    assert!(all_tenants_sub.all_tenants, "subscription must be all_tenants");
}

// ── Issue #296: multi-subscription per collection ──────────────────────────

#[test]
fn add_two_subscriptions_both_receive_insert_event() {
    let rt = rt();

    exec(
        &rt,
        "CREATE TABLE orders (id INT, customer TEXT, amount INT)",
    );
    exec(
        &rt,
        "ALTER TABLE orders ADD SUBSCRIPTION s1 TO q1",
    );
    exec(
        &rt,
        "ALTER TABLE orders ADD SUBSCRIPTION s2 TO q2",
    );

    let contract = rt.db().collection_contract("orders").unwrap();
    assert_eq!(contract.subscriptions.len(), 2);
    assert!(contract.subscriptions.iter().any(|s| s.name == "s1" && s.target_queue == "q1"));
    assert!(contract.subscriptions.iter().any(|s| s.name == "s2" && s.target_queue == "q2"));
    assert!(
        rt.db()
            .store()
            .get_config("queue.q1.mode")
            .is_some_and(|value| format!("{value:?}").contains("fanout")),
        "q1 should be a fanout event queue"
    );
    assert!(
        rt.db()
            .store()
            .get_config("queue.q2.mode")
            .is_some_and(|value| format!("{value:?}").contains("fanout")),
        "q2 should be a fanout event queue"
    );

    setup_event_queue_group(&rt, "q1");
    setup_event_queue_group(&rt, "q2");
    exec(&rt, "INSERT INTO orders (id, customer, amount) VALUES (1, 'alice', 100)");

    let e1 = read_event_payload(&rt, "q1");
    let e2 = read_event_payload(&rt, "q2");
    assert_eq!(e1["op"], "insert");
    assert_eq!(e2["op"], "insert");
    assert_eq!(e1["collection"], "orders");
    assert_eq!(e2["collection"], "orders");
}

#[test]
fn redact_applied_per_subscription_independently() {
    let rt = rt();

    exec(
        &rt,
        "CREATE TABLE users2 (id INT, email TEXT, phone TEXT)",
    );
    exec(
        &rt,
        "ALTER TABLE users2 ADD SUBSCRIPTION masked TO q_masked REDACT (email)",
    );
    exec(
        &rt,
        "ALTER TABLE users2 ADD SUBSCRIPTION unredacted TO q_unredacted",
    );

    setup_event_queue_group(&rt, "q_masked");
    setup_event_queue_group(&rt, "q_unredacted");
    exec(
        &rt,
        "INSERT INTO users2 (id, email, phone) VALUES (1, 'a@b.com', '555')",
    );

    let masked = read_event_payload(&rt, "q_masked");
    let unredacted = read_event_payload(&rt, "q_unredacted");

    assert_eq!(
        masked["after"]["email"],
        serde_json::Value::String("[REDACTED]".to_string())
    );
    assert_eq!(
        unredacted["after"]["email"],
        serde_json::Value::String("a@b.com".to_string())
    );
}

#[test]
fn drop_subscription_stops_events_to_that_queue() {
    let rt = rt();

    exec(
        &rt,
        "CREATE TABLE events3 (id INT, val TEXT)",
    );
    exec(&rt, "ALTER TABLE events3 ADD SUBSCRIPTION s1 TO e3_q1");
    exec(&rt, "ALTER TABLE events3 ADD SUBSCRIPTION s2 TO e3_q2");
    exec(&rt, "ALTER TABLE events3 DROP SUBSCRIPTION s1");

    let contract = rt.db().collection_contract("events3").unwrap();
    assert_eq!(contract.subscriptions.len(), 1);
    assert_eq!(contract.subscriptions[0].name, "s2");

    setup_event_queue_group(&rt, "e3_q2");
    exec(&rt, "INSERT INTO events3 (id, val) VALUES (1, 'x')");

    // e3_q2 must have the event
    let e2 = read_event_payload(&rt, "e3_q2");
    assert_eq!(e2["op"], "insert");

    // e3_q1 must be empty: create a group then verify no records come out
    exec(&rt, "QUEUE GROUP CREATE e3_q1 evt_readers");
    let result = QueryUseCases::new(&rt)
        .execute(ExecuteQueryInput {
            query: "QUEUE READ e3_q1 GROUP evt_readers CONSUMER c1 COUNT 1".to_string(),
        })
        .unwrap_or_else(|err| panic!("{err}"));
    assert!(
        result.result.records.is_empty(),
        "e3_q1 should be empty after s1 was dropped"
    );
}
