//! Issue #1016 — replica-aware read routing freshness tracer.

use std::time::Duration;

use reddb_client::bookmark_routing::{BookmarkWaiter, CausalReadOptions, RouteKind, RoutingTable};
use reddb_client::topology::ClusterMembership;
use reddb_wire::topology::{Endpoint, ReplicaInfo};

fn primary() -> Endpoint {
    Endpoint {
        addr: "primary:5050".into(),
        region: "us-east-1".into(),
    }
}

fn replica(addr: &str, region: &str, healthy: bool, last_applied_lsn: u64) -> ReplicaInfo {
    ReplicaInfo {
        addr: addr.into(),
        region: region.into(),
        healthy,
        lag_ms: if healthy { 5 } else { u32::MAX },
        last_applied_lsn,
        rebootstrapping: false,
    }
}

fn table(replicas: Vec<ReplicaInfo>) -> RoutingTable {
    RoutingTable::from_membership(
        ClusterMembership {
            primary: primary(),
            replicas,
            epoch: 1016,
        },
        1,
    )
}

struct ScriptedWaiter {
    frontiers: Vec<u64>,
    idx: usize,
    tick: Duration,
    elapsed: Duration,
    polled: Vec<String>,
}

impl ScriptedWaiter {
    fn new(frontiers: Vec<u64>, tick: Duration) -> Self {
        Self {
            frontiers,
            idx: 0,
            tick,
            elapsed: Duration::ZERO,
            polled: Vec::new(),
        }
    }
}

impl BookmarkWaiter for ScriptedWaiter {
    fn elapsed(&self) -> Duration {
        self.elapsed
    }

    fn poll(&mut self, target_addr: &str) -> u64 {
        self.polled.push(target_addr.to_string());
        self.elapsed += self.tick;
        let frontier = self
            .frontiers
            .get(self.idx)
            .copied()
            .or_else(|| self.frontiers.last().copied())
            .unwrap_or(0);
        self.idx += 1;
        frontier
    }
}

#[test]
fn eventual_read_routes_to_healthy_replica_without_waiting() {
    let table = table(vec![
        replica("down:5050", "us-east-1", false, 900),
        replica("healthy:5050", "us-east-1", true, 10),
    ]);

    let decision = table.route_eventual_read(None);

    assert_eq!(decision.kind, RouteKind::EventualReplica);
    assert_eq!(decision.endpoint.addr, "healthy:5050");
    assert_eq!(decision.waited, Duration::ZERO);
}

#[test]
fn required_lsn_read_avoids_stale_replica_and_uses_caught_up_peer() {
    let table = table(vec![
        replica("stale:5050", "us-east-1", true, 80),
        replica("caught-up:5050", "us-west-2", true, 120),
    ]);
    let mut waiter = ScriptedWaiter::new(vec![80, 85, 90], Duration::from_millis(10));

    let decision = table.route_required_lsn_read(
        100,
        &CausalReadOptions::with_deadline(Duration::from_millis(25)).prefer_region("us-east-1"),
        &mut waiter,
    );

    assert_eq!(decision.kind, RouteKind::FallbackReplica);
    assert_eq!(decision.endpoint.addr, "caught-up:5050");
    assert!(waiter.polled.iter().all(|addr| addr == "stale:5050"));
}

#[test]
fn required_lsn_read_falls_back_to_primary_after_bounded_wait() {
    let table = table(vec![
        replica("stale-a:5050", "us-east-1", true, 40),
        replica("stale-b:5050", "us-west-2", true, 60),
    ]);
    let mut waiter = ScriptedWaiter::new(vec![50, 70, 90], Duration::from_millis(10));

    let decision = table.route_required_lsn_read(
        100,
        &CausalReadOptions::with_deadline(Duration::from_millis(25)),
        &mut waiter,
    );

    assert_eq!(decision.kind, RouteKind::FallbackPrimary);
    assert_eq!(decision.endpoint.addr, "primary:5050");
    assert!(decision.waited >= Duration::from_millis(25));
}
