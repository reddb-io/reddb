//! End-to-end acceptance tests for the term-based, quorum-gated election
//! (issue #834, PRD #819, ADR 0030).
//!
//! Where the in-crate unit tests exercise each rule in isolation, this suite
//! drives the public election API exactly as the control-plane supervisor
//! would, asserting the issue's acceptance criteria as black-box behaviour:
//!
//! 1. On primary loss, a replica covering the commit watermark is elected by
//!    majority within the election timeout.
//! 2. A candidate not covering the commit watermark cannot win.
//! 3. Last-vote is durable; a restarted voter does not double-vote in a term.
//! 4. Dry-run probing does not bump the term; witnesses count toward quorum;
//!    a catching-up replica is non-voting until healthy.
//! 5. No two primaries in a term — demonstrated under partition.
//!
//! The harness wires real [`Voter`]s (over durable [`FileLastVoteStore`]s, so
//! the restart criterion is genuinely on-disk) behind the public
//! [`ElectionTransport`]; the candidate side is the real
//! [`ElectionCoordinator`]. No clock, no network — the clock and reachability
//! are injected so the assertions are deterministic.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reddb_server::replication::{
    randomized_election_timeout, ElectionCoordinator, ElectionOutcome, ElectionRequest,
    ElectionTransport, FileLastVoteStore, Member, RefusalReason, VoteDecision, VoteRequest, Voter,
};

const LONG: Duration = Duration::from_secs(60);

/// Auto-cleaning temp dir for one test's per-node durable last-vote files.
/// The returned [`tempfile::TempDir`] guard removes the directory (and every
/// last-vote file under it) on drop, including on panic; the caller binds it to
/// a local and keeps it alive for the whole test.
fn temp_cluster_dir(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-election-e2e-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

struct PeerState {
    /// Durable path so a "restart" re-opens the same on-disk record.
    path: std::path::PathBuf,
    watermark: u64,
    reachable: bool,
}

/// Black-box cluster: each peer is a real durable [`Voter`]; the candidate is
/// the real coordinator. Clock advances a fixed tick per RPC.
struct Cluster {
    // Retained for construction symmetry; the TempDir guard in the test owns
    // cleanup now, so this field is no longer read.
    #[allow(dead_code)]
    dir: std::path::PathBuf,
    members: Vec<Member>,
    peers: HashMap<String, PeerState>,
    elapsed: Arc<Mutex<Duration>>,
    tick: Duration,
    bumped: Vec<u64>,
    promoted: Option<u64>,
}

impl Cluster {
    fn new(dir: std::path::PathBuf, members: Vec<Member>, watermark: u64) -> Self {
        let peers = members
            .iter()
            .map(|m| {
                (
                    m.id.clone(),
                    PeerState {
                        path: dir.join(format!("{}.lastvote.json", m.id)),
                        watermark,
                        reachable: true,
                    },
                )
            })
            .collect();
        Self {
            dir,
            members,
            peers,
            elapsed: Arc::new(Mutex::new(Duration::ZERO)),
            tick: Duration::from_millis(10),
            bumped: Vec::new(),
            promoted: None,
        }
    }

    fn partition_away(&mut self, id: &str) {
        self.peers.get_mut(id).unwrap().reachable = false;
    }
}

impl ElectionTransport for Cluster {
    fn members(&self) -> Vec<Member> {
        self.members.clone()
    }

    fn request_vote(&mut self, peer_id: &str, req: &VoteRequest) -> VoteDecision {
        let p = self.peers.get(peer_id).expect("known peer");
        if !p.reachable {
            // Unreachable peer in a partition: no grant, durable store untouched.
            return VoteDecision::Refused(RefusalReason::StaleTerm {
                candidate_term: req.term,
                voter_term: u64::MAX,
            });
        }
        let voter = Voter::new(peer_id, FileLastVoteStore::new(&p.path));
        voter.consider(req, p.watermark).expect("durable store")
    }

    fn elapsed(&self) -> Duration {
        let mut e = self.elapsed.lock().unwrap();
        let now = *e;
        *e += self.tick;
        now
    }

    fn bump_term(&mut self, new_term: u64) {
        self.bumped.push(new_term);
    }

    fn promote(&mut self, new_term: u64) {
        self.promoted = Some(new_term);
    }
}

fn candidate(id: &str, current_term: u64, lsn: u64, watermark: u64) -> ElectionRequest {
    ElectionRequest {
        candidate: Member::data_voting(id),
        current_term,
        last_log_lsn: lsn,
        commit_watermark: watermark,
    }
}

// Criterion 1: primary loss → covering replica elected by majority in time.
#[test]
fn covering_replica_is_elected_by_majority_within_timeout() {
    let dir = temp_cluster_dir("elect");
    let members = vec![
        Member::data_voting("r1"),
        Member::data_voting("r2"),
        Member::data_voting("r3"),
    ];
    let mut cluster = Cluster::new(dir.path().to_path_buf(), members, 100);
    // The old primary is gone; replica r1 covers the watermark (frontier 150).
    let req = candidate("r1", 4, 150, 100);

    let timeout =
        randomized_election_timeout(Duration::from_millis(150), Duration::from_millis(150), 7);
    let outcome = ElectionCoordinator::run(&req, &mut cluster, timeout);

    assert!(
        matches!(outcome, ElectionOutcome::Elected { term: 5, .. }),
        "got {outcome:?}",
    );
    assert_eq!(cluster.promoted, Some(5));
}

// Criterion 2: a candidate below the watermark cannot win.
#[test]
fn candidate_below_watermark_cannot_win() {
    let dir = temp_cluster_dir("below");
    let members = vec![
        Member::data_voting("r1"),
        Member::data_voting("r2"),
        Member::data_voting("r3"),
    ];
    let mut cluster = Cluster::new(dir.path().to_path_buf(), members, 100);
    // r1's frontier 90 does not cover watermark 100.
    let req = candidate("r1", 4, 90, 100);

    let outcome = ElectionCoordinator::run(&req, &mut cluster, LONG);

    assert_eq!(outcome, ElectionOutcome::NotElectable);
    assert!(cluster.bumped.is_empty(), "no term burned");
    assert_eq!(cluster.promoted, None);
}

// Criterion 3: durable last-vote survives restart — no double-vote.
#[test]
fn restarted_voter_does_not_double_vote() {
    let dir = temp_cluster_dir("restart");
    let path = dir.path().join("r2.lastvote.json");

    // r2 grants candidate "alpha" in term 5, persisted to disk.
    {
        let voter = Voter::new("r2", FileLastVoteStore::new(&path));
        assert!(voter
            .consider(&VoteRequest::real("alpha", 5, 200), 100)
            .unwrap()
            .is_granted());
    }
    // r2 "restarts": a fresh Voter over the same durable file. A different
    // candidate asking for term 5 is refused from disk.
    {
        let voter = Voter::new("r2", FileLastVoteStore::new(&path));
        assert_eq!(
            voter
                .consider(&VoteRequest::real("beta", 5, 200), 100)
                .unwrap(),
            VoteDecision::Refused(RefusalReason::AlreadyVoted {
                term: 5,
                voted_for: "alpha".to_string(),
            }),
        );
    }
}

// Criterion 4a: a failed dry-run probe does not bump the term.
#[test]
fn failed_probe_does_not_bump_term() {
    let dir = temp_cluster_dir("probe");
    let members = vec![
        Member::data_voting("r1"),
        Member::data_voting("r2"),
        Member::data_voting("r3"),
    ];
    let mut cluster = Cluster::new(dir.path().to_path_buf(), members, 100);
    cluster.partition_away("r2");
    cluster.partition_away("r3");
    let req = candidate("r1", 4, 150, 100);

    let outcome = ElectionCoordinator::run(&req, &mut cluster, LONG);

    assert!(
        matches!(outcome, ElectionOutcome::ProbeFailed { .. }),
        "got {outcome:?}"
    );
    assert!(
        cluster.bumped.is_empty(),
        "dry-run probe must not bump the term"
    );
}

// Criterion 4b: a witness vote counts toward quorum.
#[test]
fn witness_completes_the_quorum() {
    let dir = temp_cluster_dir("witness");
    let members = vec![
        Member::data_voting("r1"),
        Member::data_voting("r2"),
        Member::witness("w1"),
    ];
    let mut cluster = Cluster::new(dir.path().to_path_buf(), members, 100);
    cluster.partition_away("r2"); // only candidate + witness reachable
    let req = candidate("r1", 4, 150, 100);

    let outcome = ElectionCoordinator::run(&req, &mut cluster, LONG);

    assert!(
        matches!(
            outcome,
            ElectionOutcome::Elected {
                term: 5,
                votes: 2,
                needed: 2
            }
        ),
        "witness vote should complete the majority, got {outcome:?}",
    );
}

// Criterion 4c: a catching-up replica is non-voting and out of the denominator.
#[test]
fn catching_up_replica_is_non_voting() {
    let dir = temp_cluster_dir("catchup");
    let members = vec![
        Member::data_voting("r1"),
        Member::data_voting("r2"),
        Member::data_catching_up("r3"), // syncing — excluded
    ];
    let mut cluster = Cluster::new(dir.path().to_path_buf(), members, 100);
    let req = candidate("r1", 4, 150, 100);

    let outcome = ElectionCoordinator::run(&req, &mut cluster, LONG);

    // Majority is 2 of the 2 healthy voters; r3 neither votes nor raises it.
    assert!(
        matches!(
            outcome,
            ElectionOutcome::Elected {
                term: 5,
                needed: 2,
                ..
            }
        ),
        "got {outcome:?}",
    );
}

// Criterion 5: no two primaries in a term, under partition. Two candidates on
// opposite sides stand for the SAME term over SHARED durable voter state; only
// the majority side wins, and the durable last-vote forbids the other.
#[test]
fn no_two_primaries_in_a_term_under_partition() {
    let dir = temp_cluster_dir("partition");
    let ids = ["a", "b", "c", "d", "e"];
    let members: Vec<Member> = ids.iter().map(|id| Member::data_voting(*id)).collect();

    // Both partitions read/write the SAME per-node durable files (one disk
    // per node). Build two clusters pointed at the same dir.
    let mut majority = Cluster::new(dir.path().to_path_buf(), members.clone(), 100);
    majority.partition_away("a");
    majority.partition_away("b");
    let c_outcome = ElectionCoordinator::run(&candidate("c", 4, 150, 100), &mut majority, LONG);
    assert!(
        matches!(
            c_outcome,
            ElectionOutcome::Elected {
                term: 5,
                votes: 3,
                needed: 3
            }
        ),
        "majority side elects c for term 5, got {c_outcome:?}",
    );
    assert_eq!(majority.promoted, Some(5));

    let mut minority = Cluster::new(dir.path().to_path_buf(), members, 100);
    minority.partition_away("c");
    minority.partition_away("d");
    minority.partition_away("e");
    let a_outcome = ElectionCoordinator::run(&candidate("a", 4, 150, 100), &mut minority, LONG);
    assert!(
        !matches!(a_outcome, ElectionOutcome::Elected { .. }),
        "minority side must not also elect a primary for term 5, got {a_outcome:?}",
    );
    assert_eq!(minority.promoted, None, "no second primary in term 5");

    // Durable barrier: a voter d, which granted c in term 5, refuses a in 5
    // even across a process restart (fresh Voter over the same file).
    let d_path = dir.path().join("d.lastvote.json");
    let d = Voter::new("d", FileLastVoteStore::new(&d_path));
    assert_eq!(
        d.consider(&VoteRequest::real("a", 5, 150), 100).unwrap(),
        VoteDecision::Refused(RefusalReason::AlreadyVoted {
            term: 5,
            voted_for: "c".to_string(),
        }),
    );
}
