use reddb::application::ExecuteQueryInput;
use reddb::catalog::{CollectionModel, SubscriptionOperation};
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
}

fn exec_err(rt: &RedDBRuntime, sql: &str) -> String {
    match QueryUseCases::new(rt).execute(ExecuteQueryInput {
        query: sql.to_string(),
    }) {
        Ok(_) => panic!("expected error for {sql}"),
        Err(err) => err.to_string(),
    }
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
