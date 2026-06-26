//! Issue #1358 — in-process replication network simulator.
//!
//! The simulator is intentionally single-threaded: it injects transport faults
//! without sockets, while the safety checks drive the existing election,
//! failover, lease, and fence state machines through their public seams.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use reddb::replication::election::{
    ElectionCoordinator, ElectionRequest, ElectionTransport, LastVote, Member, MemoryLastVoteStore,
    RefusalReason, VoteDecision, VoteRequest, Voter,
};
use reddb::replication::failover::{
    FailoverCoordinator, FailoverMode, FailoverNode, FailoverRequest, FailoverTransport, NodeRole,
};
use reddb::replication::fence::{
    FenceBoundary, FenceVerdict, MemoryTermStore, StreamHandshake, TermFence,
};
use reddb::replication::lease::WriterLease;
use reddb::replication::simulation::{
    DeliveryOutcome, InProcessReplicationTransport, ReplicationControlMessage,
};

#[test]
fn simulator_injects_partition_delay_reorder_and_loss_by_seed() {
    let mut network = InProcessReplicationTransport::new(13);
    network.set_max_delay(Duration::from_millis(4));
    network.set_reorder_window(4);
    network.partition("primary-a", "replica-b");

    let blocked = network.send(
        "primary-a",
        "replica-b",
        ReplicationControlMessage::PrimaryHeartbeat { term: 1, lsn: 10 },
    );
    assert_eq!(blocked, DeliveryOutcome::Partitioned);

    network.heal("primary-a", "replica-b");
    for lsn in 1..=6 {
        assert_eq!(
            network.send(
                "primary-a",
                "replica-b",
                ReplicationControlMessage::CdcRecord { term: 1, lsn },
            ),
            DeliveryOutcome::Queued
        );
    }

    assert!(
        network.drain_ready("replica-b").is_empty(),
        "delayed messages must not be visible before the simulated clock advances"
    );
    network.advance(Duration::from_millis(5));
    let delivered: Vec<u64> = network
        .drain_ready("replica-b")
        .into_iter()
        .filter_map(|message| match message.payload {
            ReplicationControlMessage::CdcRecord { lsn, .. } => Some(lsn),
            _ => None,
        })
        .collect();
    assert_eq!(delivered.len(), 6);
    assert_ne!(
        delivered,
        vec![1, 2, 3, 4, 5, 6],
        "seeded reorder should perturb FIFO delivery"
    );

    network.set_loss_per_million(1_000_000);
    let lost = network.send(
        "primary-a",
        "replica-b",
        ReplicationControlMessage::LogicalApply { term: 1, lsn: 7 },
    );
    assert_eq!(lost, DeliveryOutcome::Lost);
}

#[test]
fn election_safety_holds_under_seeded_partition() {
    for seed in 0..32 {
        let leaders = run_partitioned_elections(seed, false);
        assert_no_two_leaders_in_same_term(&leaders, seed);
    }
}

#[test]
fn no_split_brain_and_no_lost_committed_writes_under_partition() {
    let seed = 1358;
    let mut cluster = SimElectionCluster::new(seed, 8);
    cluster.network.partition("node-a", "node-d");
    cluster.network.partition("node-a", "node-e");
    cluster.network.partition("node-b", "node-d");
    cluster.network.partition("node-b", "node-e");
    cluster.frontiers.insert("node-a".to_string(), 10);
    cluster.frontiers.insert("node-b".to_string(), 10);
    cluster.frontiers.insert("node-c".to_string(), 10);
    cluster.frontiers.insert("node-d".to_string(), 8);
    cluster.frontiers.insert("node-e".to_string(), 8);

    let leaders = cluster.try_candidates(&["node-d", "node-a", "node-e"], 1);
    assert_no_two_leaders_in_same_term(&leaders, seed);
    for (_, leader) in &leaders {
        assert!(
            cluster.frontiers[leader] >= cluster.commit_watermark,
            "an elected leader must carry every committed write at or below the watermark"
        );
    }

    let mut failover = LaggingFailover::new(10, vec![8, 8, 8]);
    let request = failover_request(FailoverMode::Coordinated {
        catch_up_deadline: Duration::from_millis(3),
    });
    let err = FailoverCoordinator::run(&request, &mut failover)
        .expect_err("coordinated failover must abort before losing committed writes");
    assert!(format!("{err}").contains("no write lost"));
    assert_eq!(failover.role, NodeRole::Primary { term: 1 });
    assert!(failover.writes_resumed);
}

#[test]
fn lease_fencing_rejects_partitioned_stale_primary() {
    let mut network = InProcessReplicationTransport::new(99);
    network.partition("old-primary", "replica-b");
    network.partition("old-primary", "replica-c");

    let stale = network.send(
        "old-primary",
        "replica-b",
        ReplicationControlMessage::LeaseFence {
            holder_id: "old-primary".to_string(),
            term: 5,
        },
    );
    assert_eq!(stale, DeliveryOutcome::Partitioned);

    let fence = TermFence::new(MemoryTermStore::seeded(6));
    let rejected = match fence
        .admit_stream_handshake(&StreamHandshake::new("old-primary", 5))
        .expect("term store readable")
    {
        FenceVerdict::Fenced(rejection) => rejection,
        other => panic!("expected stale primary to be fenced, got {other:?}"),
    };
    assert_eq!(rejected.boundary, FenceBoundary::Handshake);

    let stale_lease = WriterLease {
        database_key: "main".to_string(),
        holder_id: "old-primary".to_string(),
        term: 5,
        generation: 7,
        acquired_at_ms: 0,
        expires_at_ms: u64::MAX,
    };
    assert!(
        stale_lease.fenced_by_term(6),
        "a partitioned old primary must fail closed even if its local TTL has not expired"
    );
}

#[test]
#[ignore = "heavy seeded replication-network safety profile; scheduled nightly in CI"]
fn replication_network_simulator_heavy_profile() {
    for seed in 0..512 {
        let leaders = run_partitioned_elections(seed, true);
        assert_no_two_leaders_in_same_term(&leaders, seed);
    }
}

fn run_partitioned_elections(seed: u64, lossy: bool) -> Vec<(u64, String)> {
    let mut cluster = SimElectionCluster::new(seed, 8);
    cluster.network.partition("node-a", "node-d");
    cluster.network.partition("node-a", "node-e");
    cluster.network.partition("node-b", "node-d");
    cluster.network.partition("node-b", "node-e");
    if lossy {
        cluster.network.set_loss_per_million(250_000);
        cluster.network.set_max_delay(Duration::from_millis(3));
        cluster.network.set_reorder_window(3);
    }
    cluster.try_candidates(&["node-a", "node-b", "node-c", "node-d", "node-e"], 1)
}

fn assert_no_two_leaders_in_same_term(leaders: &[(u64, String)], seed: u64) {
    let mut seen = BTreeMap::new();
    for (term, node) in leaders {
        let previous = seen.insert(*term, node.clone());
        assert!(
            previous.is_none(),
            "seed {seed} elected two leaders in term {term}: {previous:?} and {node}"
        );
    }
}

struct SimElectionCluster {
    network: InProcessReplicationTransport<ReplicationControlMessage>,
    voters: BTreeMap<String, Voter<MemoryLastVoteStore>>,
    frontiers: BTreeMap<String, u64>,
    members: Vec<Member>,
    commit_watermark: u64,
    promoted: Vec<(u64, String)>,
    refused_self_votes: BTreeSet<(u64, String)>,
}

impl SimElectionCluster {
    fn new(seed: u64, commit_watermark: u64) -> Self {
        let ids = ["node-a", "node-b", "node-c", "node-d", "node-e"];
        let voters = ids
            .iter()
            .map(|id| {
                (
                    (*id).to_string(),
                    Voter::new(*id, MemoryLastVoteStore::seeded(LastVote::default())),
                )
            })
            .collect();
        let frontiers = ids.iter().map(|id| ((*id).to_string(), 10)).collect();
        let members = ids.iter().map(|id| Member::data_voting(*id)).collect();
        Self {
            network: InProcessReplicationTransport::new(seed),
            voters,
            frontiers,
            members,
            commit_watermark,
            promoted: Vec::new(),
            refused_self_votes: BTreeSet::new(),
        }
    }

    fn try_candidates(&mut self, candidates: &[&str], current_term: u64) -> Vec<(u64, String)> {
        for candidate in candidates {
            let req = ElectionRequest {
                candidate: Member::data_voting(*candidate),
                current_term,
                last_log_lsn: self
                    .frontiers
                    .get(*candidate)
                    .copied()
                    .expect("candidate frontier seeded"),
                commit_watermark: self.commit_watermark,
            };
            let timeout = Duration::from_millis(100);
            let _ = ElectionCoordinator::run(&req, self, timeout);
        }
        self.promoted.clone()
    }
}

impl ElectionTransport for SimElectionCluster {
    fn members(&self) -> Vec<Member> {
        self.members.clone()
    }

    fn request_vote(&mut self, peer_id: &str, req: &VoteRequest) -> VoteDecision {
        if self
            .refused_self_votes
            .contains(&(req.term, req.candidate_id.clone()))
        {
            return VoteDecision::Refused(RefusalReason::AlreadyVoted {
                term: req.term,
                voted_for: "durable self-vote refused".to_string(),
            });
        }
        let payload = ReplicationControlMessage::ElectionVoteRequest(req.clone());
        if self.network.send(&req.candidate_id, peer_id, payload) != DeliveryOutcome::Queued {
            return VoteDecision::Refused(RefusalReason::StaleTerm {
                candidate_term: req.term,
                voter_term: req.term,
            });
        }

        self.network.advance(Duration::from_millis(10));
        let Some(message) = self.network.drain_ready(peer_id).into_iter().next() else {
            return VoteDecision::Refused(RefusalReason::StaleTerm {
                candidate_term: req.term,
                voter_term: req.term,
            });
        };
        let ReplicationControlMessage::ElectionVoteRequest(vote) = message.payload else {
            panic!("unexpected simulator payload");
        };
        self.voters[peer_id]
            .consider(&vote, self.commit_watermark)
            .expect("in-memory voter cannot fail")
    }

    fn elapsed(&self) -> Duration {
        self.network.now()
    }

    fn bump_term(&mut self, new_term: u64) {
        let Some(candidate) = self.network.last_sender().map(str::to_string) else {
            return;
        };
        let self_vote = VoteRequest::real(
            candidate.clone(),
            new_term,
            self.frontiers
                .get(&candidate)
                .copied()
                .expect("candidate frontier seeded"),
        );
        if !self.voters[&candidate]
            .consider(&self_vote, self.commit_watermark)
            .expect("in-memory voter cannot fail")
            .is_granted()
        {
            self.refused_self_votes.insert((new_term, candidate));
        }
    }

    fn promote(&mut self, new_term: u64) {
        let candidate = self
            .network
            .last_sender()
            .unwrap_or("<unknown>")
            .to_string();
        self.promoted.push((new_term, candidate));
    }
}

struct LaggingFailover {
    frontier: u64,
    target_readings: Vec<u64>,
    elapsed: Duration,
    writes_resumed: bool,
    role: NodeRole,
}

impl LaggingFailover {
    fn new(frontier: u64, target_readings: Vec<u64>) -> Self {
        Self {
            frontier,
            target_readings,
            elapsed: Duration::ZERO,
            writes_resumed: false,
            role: NodeRole::Primary { term: 1 },
        }
    }
}

impl FailoverTransport for LaggingFailover {
    fn freeze_primary(&mut self) -> u64 {
        self.frontier
    }

    fn resume_primary(&mut self) {
        self.writes_resumed = true;
    }

    fn elapsed(&self) -> Duration {
        self.elapsed
    }

    fn poll_target_frontier(&mut self) -> u64 {
        self.elapsed += Duration::from_millis(1);
        self.target_readings.pop().unwrap_or(0)
    }

    fn commit_handover(&mut self, new_term: u64) {
        self.role = NodeRole::Primary { term: new_term };
    }
}

fn failover_request(mode: FailoverMode) -> FailoverRequest {
    FailoverRequest {
        old_primary: FailoverNode::new("old", "http://old:55055", "us-east"),
        target: FailoverNode::new("new", "http://new:55055", "us-west"),
        current_term: 1,
        target_frontier_hint: 8,
        timeline_history: reddb::TimelineHistory::new(10),
        mode,
    }
}
