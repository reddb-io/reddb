//! End-to-end acceptance tests for cascading replication (issue #838,
//! PRD #819, ADR 0030).
//!
//! Acceptance criteria, asserted as black-box behaviour over the public
//! replication API:
//!
//! 1. An async read-replica can stream from an intermediate replica that holds
//!    its slot and forwards the stream.
//! 2. A voting member always streams directly from the primary (cascade
//!    refused).
//! 3. Bookmark frontiers propagate correctly through the chain.
//! 4. This suite covers a cascaded async replica *and* a direct-only voting
//!    member.
//!
//! The harness drives the real [`CascadeRelay`] (the intermediate's slot +
//! forwarding bookkeeping) against a real [`PrimaryReplication`] (the primary's
//! slot/retention machinery). No network: the WAL bytes the intermediate
//! "receives" are read straight from the primary's buffer, and the
//! intermediate's upstream ack is its computed chain frontier — exactly the
//! values the transport would carry.

use std::sync::Arc;

use reddb_server::replication::primary::PrimaryReplication;
use reddb_server::replication::{
    CascadeRefusal, CascadeRelay, CausalBookmark, ReplicaClass, ReplicationConfig, UpstreamChoice,
};

/// Auto-cleaning data path: the returned guard holds a [`tempfile::TempDir`],
/// so the directory and all artifacts are removed on drop (incl. panic). Keep
/// the binding alive for the whole test.
struct TempDataPath {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl std::ops::Deref for TempDataPath {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.path
    }
}

fn temp_data_path(name: &str) -> TempDataPath {
    let dir = tempfile::Builder::new()
        .prefix(&format!("reddb-test-cascade-e2e-{name}-"))
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join(format!("cascade_e2e_{name}.rdb"));
    TempDataPath { _dir: dir, path }
}

const TERM: u64 = 1;

/// Append `count` logical WAL records (lsn 1..=count) to the primary.
fn fill_wal(primary: &PrimaryReplication, count: u64) {
    for lsn in 1..=count {
        primary.append_logical_record(lsn, vec![lsn as u8]);
    }
}

// ---------------------------------------------------------------------------
// Criterion 2: a voting member refuses to cascade and connects to the primary.
// ---------------------------------------------------------------------------

#[test]
fn voting_member_streams_directly_even_when_cascade_source_configured() {
    // A voting member handed a cascade source: class flipped back to Voting
    // models "the operator pointed a quorum member at an intermediate".
    let cfg = ReplicationConfig::replica("http://primary:50051")
        .cascading_from("inter-a", "http://inter-a:50051")
        .with_replica_class(ReplicaClass::Voting);

    let (choice, refusal) = cfg.resolved_upstream("voter-1");

    assert_eq!(
        choice,
        UpstreamChoice::Primary,
        "a voting member must stream directly from the primary"
    );
    assert_eq!(
        refusal,
        Some(CascadeRefusal::VotingMemberDirectOnly),
        "the refusal is surfaced, not silent"
    );
}

#[test]
fn async_read_replica_resolves_to_the_intermediate() {
    let cfg = ReplicationConfig::replica("http://primary:50051")
        .cascading_from("inter-a", "http://inter-a:50051");

    let (choice, refusal) = cfg.resolved_upstream("leaf-1");

    assert!(refusal.is_none(), "a permitted cascade carries no refusal");
    match choice {
        UpstreamChoice::Intermediate(up) => {
            assert_eq!(up.node_id, "inter-a");
            assert_eq!(up.addr, "http://inter-a:50051");
        }
        other => panic!("expected a cascade to the intermediate, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Criteria 1 + 3: a sub-replica syncs through an intermediate that holds its
// slot and forwards the stream, and the chain's frontier propagates so the
// primary never prunes WAL the leaf still needs.
// ---------------------------------------------------------------------------

#[test]
fn cascaded_leaf_syncs_through_intermediate_and_frontier_propagates() {
    let path = temp_data_path("chain");
    let primary = PrimaryReplication::new(Some(&path));

    // The intermediate connects directly to the primary; the primary holds a
    // single slot for it, blind to how many sub-replicas hang off it.
    primary.register_replica("inter-a".to_string());
    fill_wal(&primary, 10);
    assert_eq!(primary.current_logical_lsn(), 10);

    // The intermediate streams the WAL and applies it.
    let batch: Vec<(u64, Vec<u8>)> = primary.wal_buffer.read_since(0, 1000);
    assert_eq!(batch.len(), 10, "intermediate pulls the full WAL");
    let mut relay = CascadeRelay::new("inter-a");
    let highest = batch.iter().map(|(lsn, _)| *lsn).max().unwrap();
    relay.record_self_applied(highest);

    // The leaf registers with the intermediate — the intermediate holds the
    // sub-replica's slot (criterion 1).
    let resume = relay.register_downstream("leaf-1", 0);
    assert_eq!(resume, 0, "fresh leaf resumes from the start");

    // The intermediate forwards the records the leaf hasn't seen yet, bounded
    // by what the intermediate itself holds.
    let forwarded = relay.records_to_forward(0, &batch);
    let forwarded_lsns: Vec<u64> = forwarded.iter().map(|(lsn, _)| *lsn).collect();
    assert_eq!(forwarded_lsns, (1..=10).collect::<Vec<_>>());

    // The leaf is slow: it has durably applied only up to LSN 4 so far.
    relay.note_forwarded("leaf-1", 10);
    relay.record_downstream_ack("leaf-1", 4);

    // Criterion 3 — frontier propagation. The intermediate acks the primary
    // with its *chain* frontier (min of its own applied and every leaf's
    // confirmed), not its own applied position.
    let chain_frontier = relay.upstream_confirmed_lsn();
    assert_eq!(
        chain_frontier, 4,
        "the slow leaf, not the intermediate, sets the chain frontier"
    );
    primary.ack_replica_lsn("inter-a", chain_frontier, chain_frontier);

    // The primary therefore pins retention at LSN 4 — it will not prune WAL
    // the leaf still needs, exactly as if the leaf were connected directly.
    assert_eq!(
        primary.retention_floor_lsn(),
        Some(4),
        "the cascaded slow leaf holds the primary's slot open"
    );

    // The leaf catches up; the chain frontier — and the primary's retention
    // floor — advance together.
    relay.record_downstream_ack("leaf-1", 10);
    let chain_frontier = relay.upstream_confirmed_lsn();
    assert_eq!(chain_frontier, 10);
    primary.ack_replica_lsn("inter-a", chain_frontier, chain_frontier);
    assert_eq!(primary.retention_floor_lsn(), Some(10));
}

#[test]
fn bookmark_read_routes_by_visible_frontier_through_the_chain() {
    let path = temp_data_path("bookmark");
    let primary = PrimaryReplication::new(Some(&path));
    primary.register_replica("inter-a".to_string());
    fill_wal(&primary, 10);

    let mut relay = CascadeRelay::new("inter-a");
    relay.record_self_applied(10);
    relay.register_downstream("leaf-1", 0);

    // Leaf has applied up to LSN 6.
    relay.record_downstream_ack("leaf-1", 6);
    assert_eq!(relay.downstream_visible_frontier("leaf-1"), Some(6));

    // A causal read at or below the leaf's visible frontier can be served by
    // the leaf; one beyond it cannot (the reader must wait or route upstream).
    assert!(relay.downstream_can_serve("leaf-1", &CausalBookmark::new(TERM, 6)));
    assert!(!relay.downstream_can_serve("leaf-1", &CausalBookmark::new(TERM, 7)));

    // The intermediate, sitting one hop up, covers the higher bookmark — the
    // frontier is monotonically non-increasing down the chain.
    let inter_bookmark = relay.upstream_confirmed_bookmark(TERM);
    assert_eq!(inter_bookmark.commit_lsn(), 6); // pinned by the leaf
    assert_eq!(inter_bookmark.term(), TERM);
}

#[test]
fn intermediate_fan_out_is_one_stream_at_the_primary() {
    let path = temp_data_path("fanout");
    let primary = PrimaryReplication::new(Some(&path));

    // Three sub-replicas cascade through one intermediate. The primary sees a
    // single replica regardless — the whole point of cascading.
    primary.register_replica("inter-a".to_string());
    fill_wal(&primary, 20);

    let mut relay = CascadeRelay::new("inter-a");
    relay.record_self_applied(20);
    for leaf in ["leaf-1", "leaf-2", "leaf-3"] {
        relay.register_downstream(leaf, 0);
    }
    relay.record_downstream_ack("leaf-1", 20);
    relay.record_downstream_ack("leaf-2", 15);
    relay.record_downstream_ack("leaf-3", 12);

    assert_eq!(primary.replica_count(), 1, "primary fan-out stays at one");
    assert_eq!(
        relay.downstream_count(),
        3,
        "three leaves on the intermediate"
    );

    // The slowest of the three sets the chain frontier.
    assert_eq!(relay.upstream_confirmed_lsn(), 12);
    primary.ack_replica_lsn("inter-a", 12, 12);
    assert_eq!(primary.retention_floor_lsn(), Some(12));
}

/// Sanity that the shared `Arc<[u8]>` fan-out path the production transport
/// uses also works through the relay's forwarding filter.
#[test]
fn forwarding_works_over_shared_payload_handles() {
    let path = temp_data_path("shared");
    let primary = PrimaryReplication::new(Some(&path));
    primary.register_replica("inter-a".to_string());
    fill_wal(&primary, 5);

    let mut relay = CascadeRelay::new("inter-a");
    relay.record_self_applied(3); // only applied 3 of 5 so far

    let shared: Vec<(u64, Arc<[u8]>)> = primary.wal_buffer.read_since_shared(0, 1000);
    let forwarded = relay.records_to_forward(0, &shared);
    let lsns: Vec<u64> = forwarded.iter().map(|(lsn, _)| *lsn).collect();
    assert_eq!(
        lsns,
        vec![1, 2, 3],
        "the intermediate withholds records it has not yet applied"
    );
}
