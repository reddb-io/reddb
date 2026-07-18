//! Issue #1836 — durable writes pass through the ownership admission gate.
//! Issue #1847 — planned cooperative handoff promotes a caught-up hot mirror.
//! Issue #1852 — catalog-version hints accelerate topology refresh.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use reddb::cluster::{
    ClientTopology, CollectionId, HintOutcome, NodeIdentity, PlacementMetadata, RangeId,
    RangeOwnership, RefreshOutcome, RequestOperation, RouteDecision, RoutedRequest, RoutingPolicy,
    ShardKeyMode, ShardOwnershipCatalog, TopologySnapshot,
};
use reddb::replication::{
    CatalogVersionHint, SignalPlane, SignalPlaneMessage, SimulatedSignalPlane,
};
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

#[test]
fn signal_plane_catalog_hint_refreshes_routing_after_ownership_transition() {
    let orders = collection("signal_hint_orders");
    let mut catalog = catalog_with([full_range(&orders, 1, "CN=node-a", &["CN=node-b"])]);
    let initial = catalog.topology_snapshot();

    let member_names = ["node-a", "node-b", "node-c"];
    let mut clients = member_names
        .into_iter()
        .map(|member| {
            (
                member_id(member),
                ClientTopology::from_snapshot(initial.clone()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut refreshes = member_names
        .into_iter()
        .map(|member| (member_id(member), 0usize))
        .collect::<BTreeMap<_, _>>();

    let range = catalog.range(&orders, RangeId::new(1)).unwrap().clone();
    catalog
        .apply_update(range.transfer_to(ident("CN=node-b"), [ident("CN=node-a")]))
        .unwrap();
    let fresh = catalog.topology_snapshot();
    assert!(fresh.version() > initial.version());

    for client in clients.values() {
        assert_eq!(client.resolve(&orders, b"k").unwrap(), &ident("CN=node-a"));
    }

    let supervisor = member_id("supervisor");
    let mut plane = SimulatedSignalPlane::new(vec![
        supervisor.clone(),
        member_id("node-a"),
        member_id("node-b"),
        member_id("node-c"),
    ]);

    plane.publish(
        supervisor.clone(),
        catalog_hint("supervisor", initial.version().value()),
    );
    for _ in 0..2 {
        plane.advance_round();
        drain_signal_refreshes(&mut clients, &mut refreshes, &mut plane, &fresh);
    }
    assert!(
        refreshes.values().all(|count| *count == 0),
        "stale catalog-version hints must not trigger refresh"
    );

    let mut stale_route_client = clients.get("node-a").unwrap().clone();
    let request = RoutedRequest::new(orders.clone(), b"k".to_vec(), RequestOperation::Transaction);
    let hint = match catalog.plan_route(&ident("CN=node-a"), &request, &RoutingPolicy::forwarding())
    {
        RouteDecision::Redirect { hint, .. } => hint,
        other => panic!("expected stale-ownership redirect, got {other:?}"),
    };
    assert_eq!(stale_route_client.apply_hint(&hint), HintOutcome::Corrected);
    assert_eq!(
        stale_route_client.resolve(&orders, b"k").unwrap(),
        &ident("CN=node-b")
    );
    assert!(stale_route_client.needs_refresh());

    plane.publish(
        supervisor,
        catalog_hint("supervisor", fresh.version().value()),
    );
    for _ in 0..6 {
        plane.advance_round();
        drain_signal_refreshes(&mut clients, &mut refreshes, &mut plane, &fresh);
        if clients
            .values()
            .all(|client| client.resolve(&orders, b"k") == Some(&ident("CN=node-b")))
        {
            break;
        }
    }

    for (member, client) in &clients {
        assert_eq!(
            client.resolve(&orders, b"k").unwrap(),
            &ident("CN=node-b"),
            "{member} did not converge from signal-plane hint"
        );
        assert!(!client.needs_refresh(), "{member} should be authoritative");
    }
    assert_eq!(
        refreshes.values().copied().collect::<Vec<_>>(),
        vec![1, 1, 1],
        "happy path refreshes once per member after the newer hint, not periodically"
    );
}

fn collection(name: &str) -> CollectionId {
    CollectionId::new(name).unwrap()
}

fn ident(cn: &str) -> NodeIdentity {
    NodeIdentity::from_certificate_subject(cn).unwrap()
}

fn member_id(id: &str) -> String {
    id.to_string()
}

fn full_range(
    collection: &CollectionId,
    id: u64,
    owner: &str,
    replicas: &[&str],
) -> RangeOwnership {
    RangeOwnership::establish(
        collection.clone(),
        RangeId::new(id),
        ShardKeyMode::Hash,
        reddb::cluster::RangeBounds::full(),
        ident(owner),
        replicas
            .iter()
            .map(|replica| ident(replica))
            .collect::<Vec<_>>(),
        PlacementMetadata::with_replication_factor(3),
    )
}

fn catalog_with(ranges: impl IntoIterator<Item = RangeOwnership>) -> ShardOwnershipCatalog {
    let mut catalog = ShardOwnershipCatalog::new();
    for range in ranges {
        catalog.apply_update(range).unwrap();
    }
    catalog
}

fn catalog_hint(member: &str, version: u64) -> SignalPlaneMessage {
    SignalPlaneMessage::CatalogVersionHint(CatalogVersionHint {
        member: member_id(member),
        ownership_catalog_version: version,
        topology_generation: version,
        placement_generation: version,
    })
}

fn drain_signal_refreshes(
    clients: &mut BTreeMap<String, ClientTopology>,
    refreshes: &mut BTreeMap<String, usize>,
    plane: &mut SimulatedSignalPlane,
    fresh: &TopologySnapshot,
) {
    for (member, client) in clients {
        let signals = plane.drain_received(member);
        let outcome = client.refresh_on_newer_catalog_signal(signals, || {
            *refreshes.get_mut(member).unwrap() += 1;
            fresh.clone()
        });
        assert!(
            !matches!(outcome, Some(RefreshOutcome::Ignored)),
            "newer signal should not fetch a stale topology snapshot"
        );
    }
}
