//! Issue #1836 — durable writes pass through the ownership admission gate.

use reddb::{RedDBOptions, RedDBRuntime, ReplicationConfig};

#[test]
fn deposed_primary_write_is_rejected_below_routing() {
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary().with_term(7)),
    )
    .expect("primary runtime boots");

    runtime
        .execute_query("CREATE TABLE ownership_gate_items (id INT, name TEXT)")
        .expect("current owner may write");
    runtime
        .execute_query("INSERT INTO ownership_gate_items (id, name) VALUES (1, 'before')")
        .expect("current owner may write before promotion");

    runtime
        .write_gate_arc()
        .promote_primary_replica_owner("CN=node-b,O=reddb", 8)
        .expect("promotion advances ownership");

    let err = runtime
        .execute_query("INSERT INTO ownership_gate_items (id, name) VALUES (2, 'after')")
        .expect_err("deposed primary must be fenced by ownership admission");
    let msg = err.to_string();
    assert!(msg.contains("ownership_fenced"), "{msg}");
    assert!(msg.contains("reason=stale_ownership"), "{msg}");
    assert!(msg.contains("current_epoch=2"), "{msg}");
    assert!(msg.contains("range=system.global/0"), "{msg}");
}

#[test]
fn standalone_single_node_write_is_not_range_gated() {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("standalone runtime boots");

    runtime
        .execute_query("CREATE TABLE standalone_items (id INT, name TEXT)")
        .expect("standalone DDL still works");
    runtime
        .execute_query("INSERT INTO standalone_items (id, name) VALUES (1, 'ok')")
        .expect("standalone DML still works");
}
