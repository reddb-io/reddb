//! End-to-end: stay-readable during re-bootstrap + causal routing
//! bounce + atomic swap (issue #837, PRD #819).
//!
//! Wires the three pieces that ship in production across the crate
//! boundary, with no mocked seams on the path under test:
//!
//!   1. **Server `SwapDb`** — the re-bootstrapping replica's local
//!      store. It keeps serving non-causal reads from the old data,
//!      refuses causal reads while rebuilding, and swaps atomically.
//!   2. **Server `TopologyAdvertiser` + `reddb-wire` codec** — the
//!      primary advertises the fleet; the rebuilding replica's state
//!      carries `rebootstrapping: true`, which is encoded onto the
//!      wire and decoded by the client (the same v1 blob both
//!      transports carry).
//!   3. **Client `RoutingTable`** — consumes the decoded advertisement
//!      and bounces causal reads off the rebuilding node to a
//!      caught-up peer, then treats it as eligible again once it swaps
//!      and re-advertises without the flag.
//!
//! Gated behind the default `embedded` feature so the test can reach
//! the in-process `reddb-server` crate directly (the npm gates do not
//! run `cargo test`; this is verified via `cargo test`).

#![cfg(feature = "embedded")]

use std::time::Duration;

use reddb_client::bookmark_routing::{BookmarkTarget, CausalReadOptions, RouteKind, RoutingTable};
use reddb_client::topology::TopologyConsumer;
use reddb_server::auth::middleware::{AuthResult, AuthSource};
use reddb_server::auth::Role;
use reddb_server::replication::primary::ReplicaState;
use reddb_server::replication::swap_db::SwapDb;
use reddb_server::replication::topology_advertiser::{LagConfig, TopologyAdvertiser};
use reddb_server::replication::DEFAULT_REPLICA_TIMEOUT_MS;
use reddb_wire::topology::{encode_topology, Endpoint};

/// Pinned clock so health computation is deterministic.
const NOW_MS: u128 = 1_700_000_000_000;

fn authed() -> AuthResult {
    AuthResult::Authenticated {
        username: "operator".into(),
        role: Role::Admin,
        source: AuthSource::Password,
    }
}

fn primary_ep() -> Endpoint {
    Endpoint {
        addr: "primary:5050".into(),
        region: "us-east-1".into(),
    }
}

fn lag() -> LagConfig {
    LagConfig {
        replica_timeout_ms: DEFAULT_REPLICA_TIMEOUT_MS,
        records_per_ms: None,
        now_unix_ms: NOW_MS,
    }
}

/// Build a registry replica state with an explicit re-bootstrap flag
/// and applied frontier.
fn replica_state(id: &str, region: &str, frontier: u64, rebootstrapping: bool) -> ReplicaState {
    ReplicaState {
        id: id.to_string(),
        last_acked_lsn: frontier,
        last_sent_lsn: frontier,
        last_durable_lsn: frontier,
        apply_error_count: 0,
        divergence_count: 0,
        connected_at_unix_ms: NOW_MS,
        last_seen_at_unix_ms: NOW_MS,
        region: Some(region.to_string()),
        rebootstrapping,
    }
}

/// Encode the advertisement the primary would ship for `replicas`,
/// then decode it client-side through the wire codec into a routing
/// table — exercising the exact encode → wire → decode → route path.
fn route_table_from(replicas: &[ReplicaState], epoch: u64, term: u64) -> RoutingTable {
    let topo = TopologyAdvertiser::advertise(
        replicas,
        &authed(),
        epoch,
        primary_ep(),
        /* primary_current_lsn */ 1_000,
        &lag(),
    );
    let bytes = encode_topology(&topo);
    let membership = TopologyConsumer::consume_bytes(&bytes, None).expect("decode advertisement");
    RoutingTable::from_membership(membership, term)
}

#[test]
fn rebootstrap_stay_readable_causal_bounce_then_atomic_swap() {
    // The replica's local store starts with the old dataset.
    let store: SwapDb<Vec<u32>> = SwapDb::new(vec![10, 20, 30]);

    // ---- 1. Enter re-bootstrap: stays readable, refuses causal. ----
    store.begin_rebootstrap();
    assert!(store.is_rebootstrapping());
    // Non-causal reads keep flowing from the OLD data.
    assert_eq!(
        *store.read_noncausal(),
        vec![10, 20, 30],
        "must keep serving non-causal reads from old data during rebuild"
    );
    // Causal reads are refused locally — the node won't serve a
    // bookmark from data it is about to discard.
    assert!(
        store.read_causal().is_err(),
        "must refuse causal reads while re-bootstrapping"
    );

    // ---- 2. Causal routing bounces off the rebuilding node. ----
    // The rebuilding replica is far ahead on its advertised frontier
    // (500) but flagged; a healthy peer sits at 300. The bookmark
    // needs LSN 100, which BOTH frontiers cover — yet causal routing
    // must pick the peer, never the rebuilding node.
    let rebuilding = replica_state("rebuild:5050", "us-east-1", 500, true);
    let peer = replica_state("peer:5050", "us-east-1", 300, false);
    let table = route_table_from(&[rebuilding.clone(), peer.clone()], 1, 1);

    // The advertisement carried the flag across the wire.
    let bookmark = BookmarkTarget::new(1, 100);
    let reads = table.read_endpoints(bookmark);
    let rebuild_read = reads.iter().find(|r| r.addr == "rebuild:5050").unwrap();
    assert!(rebuild_read.rebootstrapping, "flag must survive the wire");
    assert_eq!(rebuild_read.frontier_lsn, 500);
    assert!(
        !rebuild_read.bookmark_eligible,
        "rebuilding node must be ineligible despite a covering frontier"
    );

    let mut waiter = NeverPollWaiter::default();
    let decision = table.route_causal_read(
        bookmark,
        &CausalReadOptions::with_deadline(Duration::from_millis(500)),
        &mut waiter,
    );
    assert_eq!(
        decision.endpoint.addr, "peer:5050",
        "causal read bounced to the caught-up peer"
    );
    assert!(
        !decision.kind.is_fallback(),
        "peer is the chosen target, not a fallback"
    );
    assert!(
        waiter.polled.iter().all(|a| a != "rebuild:5050"),
        "must never wait/poll on the rebuilding node"
    );

    // If the peer were also rebuilding, the read must still resolve —
    // to the primary.
    let peer_rebuilding = replica_state("peer:5050", "us-east-1", 300, true);
    let all_rebuilding = route_table_from(&[rebuilding.clone(), peer_rebuilding], 1, 1);
    let mut waiter2 = NeverPollWaiter::default();
    let fallback = all_rebuilding.route_causal_read(
        bookmark,
        &CausalReadOptions::with_deadline(Duration::from_millis(500)),
        &mut waiter2,
    );
    assert_eq!(fallback.kind, RouteKind::FallbackPrimary);
    assert_eq!(fallback.endpoint.addr, "primary:5050");

    // ---- 3. Atomic swap: new data installed, node eligible again. --
    let old = store.complete_rebootstrap(vec![11, 22, 33, 44]);
    assert_eq!(*old, vec![10, 20, 30], "swap returns the prior dataset");
    assert!(!store.is_rebootstrapping());
    // Both read paths now serve the fresh data.
    assert_eq!(*store.read_noncausal(), vec![11, 22, 33, 44]);
    assert_eq!(
        *store.read_causal().expect("causal ok"),
        vec![11, 22, 33, 44]
    );

    // The replica re-advertises without the flag (and a higher epoch),
    // and the routing table now treats it as causally eligible.
    let recovered = replica_state("rebuild:5050", "us-east-1", 500, false);
    let table_after = route_table_from(&[recovered, peer], 2, 1);
    let reads_after = table_after.read_endpoints(bookmark);
    let rebuilt = reads_after
        .iter()
        .find(|r| r.addr == "rebuild:5050")
        .unwrap();
    assert!(!rebuilt.rebootstrapping);
    assert!(
        rebuilt.bookmark_eligible,
        "after the swap the node is eligible for causal reads again"
    );

    let mut waiter3 = NeverPollWaiter::default();
    let decision_after = table_after.route_causal_read(
        bookmark,
        &CausalReadOptions::with_deadline(Duration::from_millis(500)),
        &mut waiter3,
    );
    assert_eq!(decision_after.kind, RouteKind::EligibleTarget);
    assert_eq!(decision_after.endpoint.addr, "rebuild:5050");
}

/// A waiter that asserts the fast/exclusion paths never need to poll.
/// If the routing table ever tried to wait on a node here, `polled`
/// would record it and the test assertions would catch it.
#[derive(Default)]
struct NeverPollWaiter {
    polled: Vec<String>,
}

impl reddb_client::bookmark_routing::BookmarkWaiter for NeverPollWaiter {
    fn elapsed(&self) -> Duration {
        // Report the deadline as already blown so any wait loop exits
        // immediately rather than spinning — the node selection, not
        // the wait, is what this test exercises.
        Duration::from_secs(3600)
    }
    fn poll(&mut self, target_addr: &str) -> u64 {
        self.polled.push(target_addr.to_string());
        0
    }
}
