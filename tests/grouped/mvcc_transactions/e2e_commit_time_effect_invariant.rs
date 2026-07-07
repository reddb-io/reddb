//! Commit-time observable-effect invariant.
//!
//! ADR 0071 depends on every side channel firing only at COMMIT. These
//! e2e checks pin the public effects that can otherwise leak from an
//! execute-and-abort transaction: event subscriptions, queue messages,
//! and queue-wait notifications.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn read_count(rt: &RedDBRuntime, sql: &str) -> usize {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .result
        .records
        .len()
}

#[test]
fn subscription_events_are_observable_only_after_commit() {
    let rt = open_runtime();
    exec(
        &rt,
        "CREATE TABLE users (id INT, email TEXT) WITH EVENTS TO users_events",
    );
    exec(&rt, "QUEUE GROUP CREATE users_events readers");

    set_current_connection_id(179001);
    exec(&rt, "BEGIN");
    exec(
        &rt,
        "INSERT INTO users (id, email) VALUES (1, 'abort@example.com')",
    );

    set_current_connection_id(179002);
    assert_eq!(
        read_count(
            &rt,
            "QUEUE READ users_events GROUP readers CONSUMER c1 COUNT 1"
        ),
        0,
        "subscription event must not be observable before commit"
    );

    set_current_connection_id(179001);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(179002);
    assert_eq!(
        read_count(
            &rt,
            "QUEUE READ users_events GROUP readers CONSUMER c1 COUNT 1"
        ),
        0,
        "aborted subscription event must not be observable"
    );

    set_current_connection_id(179001);
    exec(&rt, "BEGIN");
    exec(
        &rt,
        "INSERT INTO users (id, email) VALUES (2, 'commit@example.com')",
    );
    exec(&rt, "COMMIT");

    set_current_connection_id(179002);
    assert_eq!(
        read_count(
            &rt,
            "QUEUE READ users_events GROUP readers CONSUMER c1 COUNT 1"
        ),
        1,
        "committed subscription event must be observable"
    );

    clear_current_connection_id();
}

#[test]
fn queue_messages_are_observable_only_after_commit() {
    let rt = open_runtime();
    exec(&rt, "CREATE QUEUE jobs");
    exec(&rt, "QUEUE GROUP CREATE jobs workers");

    set_current_connection_id(179011);
    exec(&rt, "BEGIN");
    exec(&rt, "QUEUE PUSH jobs 'abort'");

    set_current_connection_id(179012);
    assert_eq!(
        read_count(&rt, "QUEUE READ jobs GROUP workers CONSUMER c1 COUNT 1"),
        0,
        "queue message must not be observable before commit"
    );

    set_current_connection_id(179011);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(179012);
    assert_eq!(
        read_count(&rt, "QUEUE READ jobs GROUP workers CONSUMER c1 COUNT 1"),
        0,
        "aborted queue message must not be observable"
    );

    set_current_connection_id(179011);
    exec(&rt, "BEGIN");
    exec(&rt, "QUEUE PUSH jobs 'commit'");
    exec(&rt, "COMMIT");

    set_current_connection_id(179012);
    assert_eq!(
        read_count(&rt, "QUEUE READ jobs GROUP workers CONSUMER c1 COUNT 1"),
        1,
        "committed queue message must be observable"
    );

    clear_current_connection_id();
}

#[test]
fn queue_wait_notifications_fire_only_after_commit() {
    let rt = Arc::new(open_runtime());
    exec(&rt, "CREATE QUEUE wakeups");
    exec(&rt, "QUEUE GROUP CREATE wakeups workers");

    let producer_rt = Arc::clone(&rt);
    let producer = thread::spawn(move || {
        set_current_connection_id(179021);
        thread::sleep(Duration::from_millis(60));
        exec(&producer_rt, "BEGIN");
        exec(&producer_rt, "QUEUE PUSH wakeups 'abort'");
        exec(&producer_rt, "ROLLBACK");
        clear_current_connection_id();
    });

    set_current_connection_id(179022);
    let started = Instant::now();
    let rolled_back = rt
        .execute_query("QUEUE READ wakeups GROUP workers CONSUMER c1 COUNT 1 WAIT 350ms")
        .expect("rollback wait");
    let elapsed = started.elapsed();
    producer.join().expect("rollback producer joined");

    assert!(
        rolled_back.result.records.is_empty(),
        "rolled-back enqueue must not deliver through a wait notification"
    );
    assert!(
        elapsed >= Duration::from_millis(300),
        "rollback must not notify the waiter early (elapsed={elapsed:?})"
    );

    let producer_rt = Arc::clone(&rt);
    let producer = thread::spawn(move || {
        set_current_connection_id(179023);
        thread::sleep(Duration::from_millis(80));
        exec(&producer_rt, "BEGIN");
        exec(&producer_rt, "QUEUE PUSH wakeups 'commit'");
        exec(&producer_rt, "COMMIT");
        clear_current_connection_id();
    });

    let started = Instant::now();
    let committed = rt
        .execute_query("QUEUE READ wakeups GROUP workers CONSUMER c1 COUNT 1 WAIT 5s")
        .expect("commit wait");
    let elapsed = started.elapsed();
    producer.join().expect("commit producer joined");

    assert_eq!(
        committed.result.records.len(),
        1,
        "committed enqueue must notify the waiter and deliver"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "commit notification should wake before the WAIT budget (elapsed={elapsed:?})"
    );

    clear_current_connection_id();
}
