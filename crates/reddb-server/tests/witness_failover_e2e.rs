//! End-to-end acceptance test for the witness runtime profile
//! (issue #836, PRD #819, ADR 0030).
//!
//! Deploys `2 data nodes + 1 witness`, kills the primary, and asserts the
//! surviving data node is elected **with the witness's vote** — the exact
//! HA shape the witness profile exists to enable. The witness here is the
//! real [`WitnessSupervisor`]: it boots no data plane, holds no data, and
//! authenticates with the shared per-node [`NodeIdentity`]. The data voters
//! are real durable [`Voter`]s and the candidate side is the real
//! [`ElectionCoordinator`] — only the clock and reachability are injected,
//! so the assertions are deterministic with no clock and no network.
//!
//! Acceptance criteria covered (issue #836):
//!   * a witness profile boots the supervisor/vote path only, no data plane;
//!   * a witness vote counts toward election quorum;
//!   * `2 data + 1 witness` elects correctly on primary loss;
//!   * the witness authenticates with the shared per-node identity;
//!   * this integration test covers the `2 data + 1 witness` failover.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reddb_server::cluster::NodeIdentity;
use reddb_server::replication::election::MemoryLastVoteStore;
use reddb_server::replication::{
    randomized_election_timeout, ElectionCoordinator, ElectionOutcome, ElectionRequest,
    ElectionTransport, FileLastVoteStore, Member, RefusalReason, RuntimeProfile, VoteDecision,
    VoteRequest, Voter, WitnessSupervisor,
};

const LONG: Duration = Duration::from_secs(60);

// Shared per-node mTLS identities — the witness and the data members draw
// from the *same* identity type (X.509 subject), not separate namespaces.
const DATA_A: &str = "CN=data-a,O=reddb";
const DATA_B: &str = "CN=data-b,O=reddb";
const WITNESS: &str = "CN=witness-1,O=reddb";

fn identity(subject: &str) -> NodeIdentity {
    NodeIdentity::from_certificate_subject(subject).expect("non-empty subject")
}

/// Auto-cleaning temp dir for one test's per-node durable last-vote files. The
/// returned [`tempfile::TempDir`] guard removes the directory and all files
/// under it on drop (incl. panic); the caller keeps the binding alive for the
/// whole test.
fn temp_dir(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-witness-e2e-{tag}-"))
        .tempdir()
        .expect("temp dir")
}

/// A `2 data + 1 witness` cluster. The two data members vote through durable
/// file-backed [`Voter`]s; the witness votes through the real
/// [`WitnessSupervisor`] (no data plane). A `reachable` flag models a node
/// being killed or partitioned away.
struct WitnessCluster {
    // Retained for construction symmetry; the TempDir guard in the test owns
    // cleanup now, so this field is no longer read.
    #[allow(dead_code)]
    dir: std::path::PathBuf,
    members: Vec<Member>,
    /// data member id -> (durable last-vote path, reachable)
    data: HashMap<String, (std::path::PathBuf, bool)>,
    /// the single witness supervisor + its reachability
    witness: WitnessSupervisor<MemoryLastVoteStore>,
    witness_reachable: bool,
    watermark: u64,
    elapsed: Arc<Mutex<Duration>>,
    tick: Duration,
    bumped: Vec<u64>,
    promoted: Option<u64>,
}

impl WitnessCluster {
    fn new(dir: std::path::PathBuf, watermark: u64) -> Self {
        let members = vec![
            Member::data_voting(DATA_A),
            Member::data_voting(DATA_B),
            Member::witness(WITNESS),
        ];
        let mut data = HashMap::new();
        for id in [DATA_A, DATA_B] {
            data.insert(
                id.to_string(),
                (dir.join(format!("{id}.lastvote.json")), true),
            );
        }
        let witness = WitnessSupervisor::new(identity(WITNESS), MemoryLastVoteStore::new());
        Self {
            dir,
            members,
            data,
            witness,
            witness_reachable: true,
            watermark,
            elapsed: Arc::new(Mutex::new(Duration::ZERO)),
            tick: Duration::from_millis(10),
            bumped: Vec::new(),
            promoted: None,
        }
    }

    /// Kill / partition away a node so the candidate can no longer reach it.
    fn kill(&mut self, id: &str) {
        if id == WITNESS {
            self.witness_reachable = false;
        } else if let Some(entry) = self.data.get_mut(id) {
            entry.1 = false;
        }
    }
}

impl ElectionTransport for WitnessCluster {
    fn members(&self) -> Vec<Member> {
        self.members.clone()
    }

    fn request_vote(&mut self, peer_id: &str, req: &VoteRequest) -> VoteDecision {
        // The witness votes through its real supervisor — no data plane.
        if peer_id == self.witness.member().id {
            if !self.witness_reachable {
                return unreachable_refusal(req);
            }
            return self
                .witness
                .consider_vote(req, self.watermark)
                .expect("memory store infallible");
        }
        // A data member votes through its durable file-backed voter.
        let (path, reachable) = self.data.get(peer_id).expect("known data peer");
        if !reachable {
            return unreachable_refusal(req);
        }
        let voter = Voter::new(peer_id, FileLastVoteStore::new(path));
        voter.consider(req, self.watermark).expect("durable store")
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

/// An unreachable peer surfaces to the candidate as a no-grant; its durable
/// store is never touched.
fn unreachable_refusal(req: &VoteRequest) -> VoteDecision {
    VoteDecision::Refused(RefusalReason::StaleTerm {
        candidate_term: req.term,
        voter_term: u64::MAX,
    })
}

fn surviving_candidate(id: &str, current_term: u64, lsn: u64, watermark: u64) -> ElectionRequest {
    ElectionRequest {
        candidate: Member::data_voting(id),
        current_term,
        last_log_lsn: lsn,
        commit_watermark: watermark,
    }
}

// The headline acceptance test: 2 data + 1 witness, primary killed, the
// surviving data node is elected with the witness's vote.
#[test]
fn two_data_plus_witness_elects_surviving_node_on_primary_loss() {
    let dir = temp_dir("failover");
    let mut cluster = WitnessCluster::new(dir.path().to_path_buf(), 100);

    // data-a was the primary; it is killed. data-b survives and stands.
    cluster.kill(DATA_A);
    // data-b covers the commit watermark (frontier 150 >= 100).
    let req = surviving_candidate(DATA_B, 4, 150, 100);

    let timeout =
        randomized_election_timeout(Duration::from_millis(150), Duration::from_millis(150), 3);
    let outcome = ElectionCoordinator::run(&req, &mut cluster, timeout);

    // data-b self-vote + the witness vote = 2 = strict majority of 3.
    assert_eq!(
        outcome,
        ElectionOutcome::Elected {
            term: 5,
            votes: 2,
            needed: 2,
        },
        "the witness vote completes the majority after primary loss",
    );
    assert_eq!(
        cluster.promoted,
        Some(5),
        "the surviving data node is promoted"
    );
    assert_eq!(
        cluster.bumped,
        vec![5],
        "the real election bumped exactly one term"
    );
}

// The witness genuinely holds no data plane and can never be the one promoted.
#[test]
fn witness_holds_no_data_and_is_never_electable() {
    let witness = WitnessSupervisor::new(identity(WITNESS), MemoryLastVoteStore::new());
    assert_eq!(witness.profile(), RuntimeProfile::Witness);
    assert!(!witness.boots_data_plane(), "a witness boots no data plane");
    assert!(
        !witness.member().is_electable(),
        "a witness can never be promoted to primary",
    );
    // It still counts as a voter toward quorum.
    assert!(witness.member().is_voter());
    // And it authenticates with the shared per-node identity.
    assert_eq!(witness.identity(), &identity(WITNESS));
}

// Without the witness the two-data cluster cannot form a majority once the
// primary dies — this is exactly the gap the witness closes.
#[test]
fn two_data_alone_cannot_elect_when_the_witness_is_also_down() {
    let dir = temp_dir("no-witness");
    let mut cluster = WitnessCluster::new(dir.path().to_path_buf(), 100);

    // Primary data-a is killed AND the witness is unreachable: data-b reaches
    // only itself = 1 < majority 2. No promotion — the cluster correctly
    // refuses to elect a minority primary.
    cluster.kill(DATA_A);
    cluster.kill(WITNESS);
    let req = surviving_candidate(DATA_B, 4, 150, 100);

    let outcome = ElectionCoordinator::run(&req, &mut cluster, LONG);

    assert!(
        matches!(
            outcome,
            ElectionOutcome::ProbeFailed {
                votes: 1,
                needed: 2
            }
        ),
        "no majority without the witness, got {outcome:?}",
    );
    assert!(cluster.bumped.is_empty(), "a failed probe burns no term");
    assert_eq!(cluster.promoted, None, "no minority primary");
}

// The witness vote is durable: once it grants the surviving node a term, a
// restart of the witness must not grant a competing candidate the same term.
#[test]
fn witness_vote_is_durable_across_restart() {
    let dir = temp_dir("durable");
    let path = dir.path().join("witness.lastvote.json");
    let id = identity(WITNESS);

    // The witness grants data-b in term 5 and the grant is persisted.
    {
        let witness = WitnessSupervisor::with_durable_store(id.clone(), &path);
        assert!(witness
            .consider_vote(&VoteRequest::real(DATA_B, 5, 150), 100)
            .unwrap()
            .is_granted());
    }
    // The witness restarts (fresh supervisor, same file). A competing
    // candidate asking for term 5 is refused from disk — no second primary.
    {
        let witness = WitnessSupervisor::with_durable_store(id, &path);
        assert_eq!(
            witness
                .consider_vote(&VoteRequest::real(DATA_A, 5, 150), 100)
                .unwrap(),
            VoteDecision::Refused(RefusalReason::AlreadyVoted {
                term: 5,
                voted_for: DATA_B.to_string(),
            }),
        );
    }
}
