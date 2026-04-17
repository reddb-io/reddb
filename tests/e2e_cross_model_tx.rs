//! Phase 1 MVCC universal — cross-model atomic transactions.
//!
//! Validates that `BEGIN / COMMIT / ROLLBACK` applies visibility
//! filters and xmin stamping across every entity kind — not just
//! tables. An uncommitted write to a graph node / vector / queue
//! message must stay invisible to other connections until COMMIT,
//! and `ROLLBACK` must hide every writer's mutations regardless of
//! model.
//!
//! Two connections are simulated on the same thread by toggling the
//! thread-local connection id via `set_current_connection_id`.

use reddb::runtime::mvcc::{
    clear_current_connection_id, entity_visible_with_context, set_current_connection_id,
    SnapshotContext,
};
use reddb::storage::EntityKind;
use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory())
        .expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn visible_entities(rt: &RedDBRuntime, collection: &str) -> usize {
    let store = rt.db().store();
    let Some(mgr) = store.get_collection(collection) else {
        return 0;
    };
    // Build the same context `execute_query` would install for this
    // connection — `capture_current_snapshot` returns None outside
    // the execute_query scope, so we assemble it explicitly here.
    let ctx = SnapshotContext {
        snapshot: rt.current_snapshot(),
        manager: rt.snapshot_manager(),
        own_xids: rt.current_txn_own_xids(),
    };
    mgr.query_all(move |e| entity_visible_with_context(Some(&ctx), e))
        .len()
}

#[test]
fn graph_node_hidden_until_commit_across_connections() {
    let rt = open_runtime();

    // Connection A: open txn + insert node via SQL.
    set_current_connection_id(101);
    exec(&rt, "BEGIN");
    exec(
        &rt,
        "INSERT INTO social NODE (label, name) VALUES ('User', 'alice')",
    );

    let own_count = visible_entities(&rt, "social");
    assert!(
        own_count >= 1,
        "writer must see own pre-commit node (found {own_count})"
    );

    // Connection B: fresh autocommit snapshot — writer's xid is
    // in `in_progress`, so the node must be hidden.
    set_current_connection_id(202);
    let other_count = visible_entities(&rt, "social");
    assert_eq!(
        other_count, 0,
        "other connection must not see pre-commit node"
    );

    // Back to A: commit. Now B sees it.
    set_current_connection_id(101);
    exec(&rt, "COMMIT");

    set_current_connection_id(202);
    let after_count = visible_entities(&rt, "social");
    assert_eq!(after_count, 1, "post-commit node must be visible");

    clear_current_connection_id();
}

#[test]
fn queue_ack_inside_tx_is_tombstone_not_physical_delete() {
    let rt = open_runtime();
    exec(&rt, "CREATE QUEUE jobs");

    // Autocommit push — visible everywhere immediately.
    set_current_connection_id(0);
    exec(&rt, "QUEUE PUSH jobs {payload: 'ship-order-1'}");

    let base = visible_entities(&rt, "jobs");
    assert!(base >= 1, "queue message should be visible pre-tx");

    // Conn A: BEGIN + POP (which ACKs → tombstone inside a tx).
    set_current_connection_id(505);
    exec(&rt, "BEGIN");
    exec(&rt, "QUEUE POP jobs");

    // Conn B: other connection still sees the original message
    // (tombstoned only from A's xid onward).
    set_current_connection_id(606);
    let other_count = visible_entities(&rt, "jobs");
    assert!(
        other_count >= 1,
        "queue tombstone must stay invisible to other sessions until COMMIT (saw {other_count})"
    );

    // Conn A rolls back — the message revives.
    set_current_connection_id(505);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(707);
    let revived = visible_entities(&rt, "jobs");
    assert!(
        revived >= 1,
        "rolled-back queue ACK must revive the message (saw {revived})"
    );

    clear_current_connection_id();
}

#[test]
fn cross_model_atomic_rollback() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT)");
    exec(&rt, "CREATE QUEUE notifications");

    // Conn A writes to multiple models in one txn, then rolls back.
    set_current_connection_id(808);
    exec(&rt, "BEGIN");
    exec(&rt, "INSERT INTO users (id, email) VALUES (1, 'alice@x.com')");
    exec(&rt, "QUEUE PUSH notifications {to: 'alice', type: 'welcome'}");
    exec(
        &rt,
        "INSERT INTO social NODE (label, name) VALUES ('User', 'alice')",
    );
    exec(&rt, "ROLLBACK");

    // Fresh connection: nothing should survive the rollback.
    set_current_connection_id(909);
    let users_visible = visible_entities(&rt, "users");
    let queue_visible = visible_entities(&rt, "notifications");
    let nodes_visible = rt
        .db()
        .store()
        .get_collection("social")
        .map(|mgr| {
            let ctx = SnapshotContext {
                snapshot: rt.current_snapshot(),
                manager: rt.snapshot_manager(),
                own_xids: rt.current_txn_own_xids(),
            };
            mgr.query_all(move |e| {
                matches!(e.kind, EntityKind::GraphNode(_))
                    && entity_visible_with_context(Some(&ctx), e)
            })
            .len()
        })
        .unwrap_or(0);

    assert_eq!(users_visible, 0, "rolled-back row must vanish");
    assert_eq!(queue_visible, 0, "rolled-back queue message must vanish");
    assert_eq!(nodes_visible, 0, "rolled-back graph node must vanish");

    clear_current_connection_id();
}
