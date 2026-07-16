//! Issue #1836 — durable writes pass through the ownership admission gate.
//! Issue #1847 — planned cooperative handoff promotes a caught-up hot mirror.

use std::time::{Duration, Instant};

use reddb::storage::schema::Value;
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

#[test]
fn cooperative_handoff_refuses_hot_mirror_below_commit_watermark() {
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary().with_term(7)),
    )
    .expect("primary runtime boots");

    runtime
        .execute_query("CREATE TABLE handoff_refusal_items (id INT, name TEXT)")
        .expect("current owner may write");
    runtime
        .execute_query("INSERT INTO handoff_refusal_items (id, name) VALUES (1, 'before')")
        .expect("current owner may write before handoff");
    let watermark = runtime.cdc_current_lsn();
    assert!(watermark > 0, "test needs an acknowledged write watermark");

    runtime
        .write_gate_arc()
        .register_primary_replica_hot_mirror("CN=node-b,O=reddb")
        .expect("target registered as hot mirror");
    let err = runtime
        .write_gate_arc()
        .cooperative_handoff_primary_replica_owner("CN=node-b,O=reddb", 8, watermark - 1, watermark)
        .expect_err("target behind the watermark must be refused");
    let msg = err.to_string();
    assert!(msg.contains("cooperative_handoff_refused"), "{msg}");
    assert!(msg.contains("reason=watermark_not_covered"), "{msg}");
}

#[test]
fn cooperative_handoff_promotes_without_lease_expiry_and_fences_old_owner() {
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::in_memory().with_replication(ReplicationConfig::primary().with_term(7)),
    )
    .expect("primary runtime boots");

    runtime
        .execute_query("CREATE TABLE handoff_items (id INT, name TEXT)")
        .expect("current owner may write");
    runtime
        .execute_query("INSERT INTO handoff_items (id, name) VALUES (1, 'before')")
        .expect("write before handoff is acknowledged");
    runtime
        .execute_query("INSERT INTO handoff_items (id, name) VALUES (2, 'during-drain')")
        .expect("write during planned drain is acknowledged");

    let watermark = runtime.cdc_current_lsn();
    runtime
        .write_gate_arc()
        .register_primary_replica_hot_mirror("CN=node-b,O=reddb")
        .expect("target registered as hot mirror");

    let started = Instant::now();
    let outcome = runtime
        .write_gate_arc()
        .cooperative_handoff_primary_replica_owner("CN=node-b,O=reddb", 8, watermark, watermark)
        .expect("caught-up hot mirror promotes");
    let write_gap = started.elapsed();

    assert!(
        write_gap < Duration::from_millis(50),
        "planned handoff must not wait for lease expiry/election; gap={write_gap:?}"
    );
    assert_eq!(outcome.range_identity, "system.global/0");
    assert_eq!(outcome.previous_epoch, 1);
    assert_eq!(outcome.new_epoch, 2);
    assert_eq!(outcome.commit_watermark, watermark);
    assert_eq!(outcome.target_lsn, watermark);

    let err = runtime
        .execute_query("INSERT INTO handoff_items (id, name) VALUES (3, 'after')")
        .expect_err("old owner must self-fence after handoff");
    let msg = err.to_string();
    assert!(msg.contains("ownership_fenced"), "{msg}");
    assert!(msg.contains("reason=stale_ownership"), "{msg}");
    assert!(msg.contains("current_epoch=2"), "{msg}");

    let rows = runtime
        .execute_query("SELECT id FROM handoff_items")
        .expect("self-fenced owner may still read acknowledged rows");
    let mut ids = rows
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("id") {
            Some(Value::Integer(id)) => Some(*id),
            _ => None,
        })
        .collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2], "acked writes must survive once each");
}
