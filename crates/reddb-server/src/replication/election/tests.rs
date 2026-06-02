//! Unit tests for the election consensus core (issue #834).
//!
//! These cover the voter-side vote rule (watermark, durable double-vote,
//! dry-run, term staleness), the randomized timeout, and the candidate-side
//! coordinator (probe-then-elect, witness counting, catch-up exclusion,
//! timeout). The *cross-node* invariants — restart-no-double-vote and the
//! no-two-primaries partition proof — are driven through a small in-process
//! cluster harness below and re-asserted end-to-end in
//! `tests/election_consensus_e2e.rs`.

use super::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

// ===============================================================
// Voter-side vote rule
// ===============================================================

#[test]
fn watermark_uncovered_candidate_is_refused() {
    let voter = Voter::new("v", MemoryLastVoteStore::new());
    // Candidate log frontier 90 does not cover watermark 100.
    let req = VoteRequest::real("cand", 5, 90);
    let decision = voter.consider(&req, 100).unwrap();
    assert_eq!(
        decision,
        VoteDecision::Refused(RefusalReason::WatermarkNotCovered {
            candidate_lsn: 90,
            watermark: 100,
        }),
    );
}

#[test]
fn watermark_covered_candidate_is_granted() {
    let voter = Voter::new("v", MemoryLastVoteStore::new());
    // Frontier 120 covers watermark 100 (>=).
    let req = VoteRequest::real("cand", 5, 120);
    assert_eq!(voter.consider(&req, 100).unwrap(), VoteDecision::Granted);
}

#[test]
fn watermark_rule_beats_a_newer_term() {
    // Even a candidate with a much newer term cannot win if its log does
    // not cover the watermark — safety is checked before term.
    let voter = Voter::new("v", MemoryLastVoteStore::new());
    let req = VoteRequest::real("cand", 9999, 10);
    assert!(matches!(
        voter.consider(&req, 100).unwrap(),
        VoteDecision::Refused(RefusalReason::WatermarkNotCovered { .. })
    ));
}

#[test]
fn cannot_double_vote_for_two_candidates_in_same_term() {
    let voter = Voter::new("v", MemoryLastVoteStore::new());
    // First candidate wins the vote in term 7.
    assert_eq!(
        voter
            .consider(&VoteRequest::real("a", 7, 200), 100)
            .unwrap(),
        VoteDecision::Granted
    );
    // Second candidate in the same term is refused — already voted.
    assert_eq!(
        voter
            .consider(&VoteRequest::real("b", 7, 200), 100)
            .unwrap(),
        VoteDecision::Refused(RefusalReason::AlreadyVoted {
            term: 7,
            voted_for: "a".to_string(),
        }),
    );
}

#[test]
fn re_ask_by_same_candidate_in_same_term_is_idempotently_granted() {
    let voter = Voter::new("v", MemoryLastVoteStore::new());
    assert!(voter
        .consider(&VoteRequest::real("a", 7, 200), 100)
        .unwrap()
        .is_granted());
    // A retransmitted request from the *same* candidate must not be a
    // spurious refusal (network retries are normal).
    assert!(voter
        .consider(&VoteRequest::real("a", 7, 200), 100)
        .unwrap()
        .is_granted());
}

#[test]
fn newer_term_clears_the_previous_terms_vote() {
    let voter = Voter::new("v", MemoryLastVoteStore::new());
    assert!(voter
        .consider(&VoteRequest::real("a", 7, 200), 100)
        .unwrap()
        .is_granted());
    // A higher term resets the per-term vote, so a new candidate can win it.
    assert!(voter
        .consider(&VoteRequest::real("b", 8, 200), 100)
        .unwrap()
        .is_granted());
    assert_eq!(voter.current_term().unwrap(), 8);
}

#[test]
fn stale_term_candidate_is_refused() {
    let voter = Voter::new(
        "v",
        MemoryLastVoteStore::seeded(LastVote {
            term: 10,
            voted_for: Some("x".to_string()),
        }),
    );
    let decision = voter
        .consider(&VoteRequest::real("old", 6, 200), 100)
        .unwrap();
    assert_eq!(
        decision,
        VoteDecision::Refused(RefusalReason::StaleTerm {
            candidate_term: 6,
            voter_term: 10,
        }),
    );
}

#[test]
fn dry_run_probe_does_not_persist_or_advance_term() {
    let voter = Voter::new("v", MemoryLastVoteStore::new());
    assert!(voter
        .consider(&VoteRequest::probe("a", 7, 200), 100)
        .unwrap()
        .is_granted());
    // Term must still be 0 and no vote recorded — a probe is observation only.
    assert_eq!(voter.current_term().unwrap(), 0);
    // Because the probe did not persist, a *different* candidate can still
    // win the real vote in term 7.
    assert!(voter
        .consider(&VoteRequest::real("b", 7, 200), 100)
        .unwrap()
        .is_granted());
}

// ===============================================================
// Durable last-vote: restart does not double-vote
// ===============================================================

#[test]
fn file_store_round_trips_and_defaults_to_empty() {
    let dir = std::env::temp_dir().join(format!(
        "reddb-lastvote-{}-{}",
        std::process::id(),
        crate::utils::now_unix_nanos()
    ));
    let path = dir.join("node.lastvote.json");
    let store = FileLastVoteStore::new(&path);

    // No file yet → default (term 0, no vote).
    assert_eq!(store.load().unwrap(), LastVote::default());

    store
        .persist(&LastVote {
            term: 12,
            voted_for: Some("cand-7".to_string()),
        })
        .unwrap();
    // A *fresh* store over the same path (simulating a process restart)
    // reads back the durable record.
    let reopened = FileLastVoteStore::new(&path);
    assert_eq!(
        reopened.load().unwrap(),
        LastVote {
            term: 12,
            voted_for: Some("cand-7".to_string()),
        }
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restarted_voter_does_not_double_vote_in_a_term() {
    let dir = std::env::temp_dir().join(format!(
        "reddb-lastvote-restart-{}-{}",
        std::process::id(),
        crate::utils::now_unix_nanos()
    ));
    let path = dir.join("node.lastvote.json");

    // Voter grants candidate "a" in term 7, persisting durably.
    {
        let voter = Voter::new("v", FileLastVoteStore::new(&path));
        assert!(voter
            .consider(&VoteRequest::real("a", 7, 200), 100)
            .unwrap()
            .is_granted());
    }

    // Process restarts: a brand-new Voter over the same durable file.
    {
        let voter = Voter::new("v", FileLastVoteStore::new(&path));
        // A different candidate asks for the same term — must be refused
        // from disk, even though this is a fresh in-memory voter.
        assert_eq!(
            voter
                .consider(&VoteRequest::real("b", 7, 200), 100)
                .unwrap(),
            VoteDecision::Refused(RefusalReason::AlreadyVoted {
                term: 7,
                voted_for: "a".to_string(),
            }),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ===============================================================
// Membership / quorum threshold
// ===============================================================

#[test]
fn quorum_threshold_is_strict_majority_of_voting_members() {
    let three = vec![
        Member::data_voting("a"),
        Member::data_voting("b"),
        Member::data_voting("c"),
    ];
    assert_eq!(quorum_threshold(&three), 2);

    // 2 data + 1 witness is a valid HA shape; witness counts → majority 2.
    let two_plus_witness = vec![
        Member::data_voting("a"),
        Member::data_voting("b"),
        Member::witness("w"),
    ];
    assert_eq!(quorum_threshold(&two_plus_witness), 2);

    // A catching-up replica is excluded from the denominator entirely.
    let with_catch_up = vec![
        Member::data_voting("a"),
        Member::data_voting("b"),
        Member::data_voting("c"),
        Member::data_catching_up("d"),
    ];
    assert_eq!(quorum_threshold(&with_catch_up), 2, "3 voters, not 4");
}

#[test]
fn electability_excludes_witness_and_catching_up() {
    assert!(Member::data_voting("a").is_electable());
    assert!(
        !Member::witness("w").is_electable(),
        "witness holds no data"
    );
    assert!(
        !Member::data_catching_up("d").is_electable(),
        "catch-up replica is not healthy"
    );
}

// ===============================================================
// Randomized timeout
// ===============================================================

#[test]
fn randomized_timeout_stays_within_band() {
    let base = Duration::from_millis(150);
    let jitter = Duration::from_millis(150);
    for seed in 0..1000u64 {
        let t = randomized_election_timeout(base, jitter, seed);
        assert!(t >= base, "never below base");
        assert!(t < base + jitter, "never at or beyond base+jitter");
    }
    // Distinct seeds spread out (anti-lockstep): two different seeds in the
    // band give two different timeouts.
    assert_ne!(
        randomized_election_timeout(base, jitter, 10),
        randomized_election_timeout(base, jitter, 11),
    );
    // Zero jitter degenerates to exactly base.
    assert_eq!(randomized_election_timeout(base, Duration::ZERO, 999), base);
}

// ===============================================================
// Cluster harness for the candidate-side coordinator
// ===============================================================

/// An in-process cluster: each voting peer is a real [`Voter`] over its own
/// durable store, with a per-peer commit-watermark view and an optional
/// "partitioned away" flag that makes the candidate unable to reach it.
struct ClusterTransport {
    members: Vec<Member>,
    /// peer_id -> Voter state, watermark, reachable
    peers: HashMap<String, PeerState>,
    elapsed_steps: Rc<RefCell<Duration>>,
    /// per-RPC clock tick, so timeouts are exercised deterministically.
    tick: Duration,
    bumped_term: Option<u64>,
    promoted_term: Option<u64>,
}

struct PeerState {
    store: Rc<MemoryLastVoteStore>,
    watermark: u64,
    reachable: bool,
}

impl ClusterTransport {
    fn new(members: Vec<Member>) -> Self {
        let mut peers = HashMap::new();
        for m in &members {
            peers.insert(
                m.id.clone(),
                PeerState {
                    store: Rc::new(MemoryLastVoteStore::new()),
                    watermark: 0,
                    reachable: true,
                },
            );
        }
        Self {
            members,
            peers,
            elapsed_steps: Rc::new(RefCell::new(Duration::ZERO)),
            tick: Duration::from_millis(10),
            bumped_term: None,
            promoted_term: None,
        }
    }

    fn with_watermark(mut self, watermark: u64) -> Self {
        for p in self.peers.values_mut() {
            p.watermark = watermark;
        }
        self
    }

    /// Make a peer unreachable (other side of a partition).
    fn partition_away(&mut self, peer_id: &str) {
        if let Some(p) = self.peers.get_mut(peer_id) {
            p.reachable = false;
        }
    }

    /// Share a peer's durable store so a second coordinator (a competing
    /// candidate reaching the same voters) sees the same persisted votes.
    fn share_store(&mut self, peer_id: &str, store: Rc<MemoryLastVoteStore>) {
        if let Some(p) = self.peers.get_mut(peer_id) {
            p.store = store;
        }
    }
}

impl ElectionTransport for ClusterTransport {
    fn members(&self) -> Vec<Member> {
        self.members.clone()
    }

    fn request_vote(&mut self, peer_id: &str, req: &VoteRequest) -> VoteDecision {
        let p = self.peers.get(peer_id).expect("known peer");
        if !p.reachable {
            // Unreachable peer: model as a silent no-grant (a timeout would
            // surface as a refusal at the candidate). It still must not have
            // its durable store touched.
            return VoteDecision::Refused(RefusalReason::StaleTerm {
                candidate_term: req.term,
                voter_term: u64::MAX,
            });
        }
        // Each peer applies the real vote rule over its own durable store.
        let voter = VoterRef {
            store: Rc::clone(&p.store),
        };
        voter.consider(req, p.watermark)
    }

    fn elapsed(&self) -> Duration {
        let mut e = self.elapsed_steps.borrow_mut();
        let now = *e;
        *e += self.tick;
        now
    }

    fn bump_term(&mut self, new_term: u64) {
        self.bumped_term = Some(new_term);
    }

    fn promote(&mut self, new_term: u64) {
        self.promoted_term = Some(new_term);
    }
}

/// Thin wrapper so the harness can build a [`Voter`] over an `Rc`-shared
/// store without moving it.
struct VoterRef {
    store: Rc<MemoryLastVoteStore>,
}

impl VoterRef {
    fn consider(&self, req: &VoteRequest, watermark: u64) -> VoteDecision {
        // Reuse the real rule by delegating to a Voter over a borrowed store.
        let voter = Voter::new("peer", RcStore(Rc::clone(&self.store)));
        voter
            .consider(req, watermark)
            .expect("memory store infallible")
    }
}

/// `LastVoteStore` over an `Rc<MemoryLastVoteStore>` so multiple coordinators
/// can share one durable record (used for the partition double-vote proof).
struct RcStore(Rc<MemoryLastVoteStore>);

impl LastVoteStore for RcStore {
    fn load(&self) -> Result<LastVote, LastVoteError> {
        self.0.load()
    }
    fn persist(&self, vote: &LastVote) -> Result<(), LastVoteError> {
        self.0.persist(vote)
    }
}

fn candidate_request(id: &str, current_term: u64, lsn: u64, watermark: u64) -> ElectionRequest {
    ElectionRequest {
        candidate: Member::data_voting(id),
        current_term,
        last_log_lsn: lsn,
        commit_watermark: watermark,
    }
}

const LONG: Duration = Duration::from_secs(60);

// ===============================================================
// Candidate-side coordinator
// ===============================================================

#[test]
fn covering_candidate_is_elected_by_majority() {
    let members = vec![
        Member::data_voting("a"),
        Member::data_voting("b"),
        Member::data_voting("c"),
    ];
    let mut tx = ClusterTransport::new(members).with_watermark(100);
    // Candidate "a" covers the watermark (frontier 150 >= 100).
    let req = candidate_request("a", 4, 150, 100);

    let outcome = ElectionCoordinator::run(&req, &mut tx, LONG);

    assert_eq!(
        outcome,
        ElectionOutcome::Elected {
            term: 5,
            votes: 2,
            needed: 2,
        }
    );
    assert_eq!(tx.bumped_term, Some(5), "real election bumps the term");
    assert_eq!(tx.promoted_term, Some(5), "winner is promoted via handover");
}

#[test]
fn candidate_not_covering_watermark_cannot_win() {
    let members = vec![
        Member::data_voting("a"),
        Member::data_voting("b"),
        Member::data_voting("c"),
    ];
    let mut tx = ClusterTransport::new(members).with_watermark(100);
    // Candidate "a" frontier 80 < watermark 100 → not electable, no probe.
    let req = candidate_request("a", 4, 80, 100);

    let outcome = ElectionCoordinator::run(&req, &mut tx, LONG);

    assert_eq!(outcome, ElectionOutcome::NotElectable);
    assert_eq!(
        tx.bumped_term, None,
        "no term bump for an unelectable candidate"
    );
    assert_eq!(tx.promoted_term, None);
}

#[test]
fn voters_below_watermark_refuse_so_lagging_candidate_loses() {
    // Candidate covers its own watermark belief, but the *voters* hold a
    // higher watermark the candidate does not cover, so they refuse and the
    // candidate cannot assemble a majority. Demonstrates the rule is enforced
    // at every voter, not just self-asserted by the candidate.
    let members = vec![
        Member::data_voting("a"),
        Member::data_voting("b"),
        Member::data_voting("c"),
    ];
    let mut tx = ClusterTransport::new(members).with_watermark(500);
    // Candidate believes watermark is 100 and covers it (frontier 120), but
    // its frontier 120 < the voters' real watermark 500.
    let req = candidate_request("a", 4, 120, 100);

    let outcome = ElectionCoordinator::run(&req, &mut tx, LONG);

    // Self-vote only (1), needed 2 → probe fails, term never bumped.
    assert_eq!(
        outcome,
        ElectionOutcome::ProbeFailed {
            votes: 1,
            needed: 2
        }
    );
    assert_eq!(tx.bumped_term, None);
}

#[test]
fn dry_run_probe_does_not_bump_term_when_it_fails() {
    // 5 members, but 3 voters are partitioned away → only 2 reachable
    // (candidate + 1), below the majority of 3. Probe fails, term untouched.
    let members = vec![
        Member::data_voting("a"),
        Member::data_voting("b"),
        Member::data_voting("c"),
        Member::data_voting("d"),
        Member::data_voting("e"),
    ];
    let mut tx = ClusterTransport::new(members).with_watermark(100);
    tx.partition_away("c");
    tx.partition_away("d");
    tx.partition_away("e");
    let req = candidate_request("a", 4, 150, 100);

    let outcome = ElectionCoordinator::run(&req, &mut tx, LONG);

    assert_eq!(
        outcome,
        ElectionOutcome::ProbeFailed {
            votes: 2,
            needed: 3
        }
    );
    assert_eq!(
        tx.bumped_term, None,
        "a failed probe must NOT bump the term"
    );
    assert_eq!(tx.promoted_term, None);
}

#[test]
fn witness_vote_counts_toward_quorum() {
    // 2 data + 1 witness. Candidate "a" + witness "w" = 2 = majority of 3.
    let members = vec![
        Member::data_voting("a"),
        Member::data_voting("b"),
        Member::witness("w"),
    ];
    let mut tx = ClusterTransport::new(members).with_watermark(100);
    // "b" is partitioned away; the witness alone supplies the second vote.
    tx.partition_away("b");
    let req = candidate_request("a", 4, 150, 100);

    let outcome = ElectionCoordinator::run(&req, &mut tx, LONG);

    assert_eq!(
        outcome,
        ElectionOutcome::Elected {
            term: 5,
            votes: 2,
            needed: 2,
        },
        "the witness vote completes the majority",
    );
}

#[test]
fn catching_up_replica_neither_votes_nor_enlarges_quorum() {
    // 3 voters + 1 catching-up. Majority is 2 (of the 3 voters). The
    // catch-up replica must not be asked and must not raise the denominator.
    let members = vec![
        Member::data_voting("a"),
        Member::data_voting("b"),
        Member::data_catching_up("c"),
    ];
    let mut tx = ClusterTransport::new(members).with_watermark(100);
    // Even if "c" *would* grant, it is excluded; "b" supplies the 2nd vote.
    let req = candidate_request("a", 4, 150, 100);

    let outcome = ElectionCoordinator::run(&req, &mut tx, LONG);

    assert_eq!(
        outcome,
        ElectionOutcome::Elected {
            term: 5,
            votes: 2,
            needed: 2,
        }
    );
}

#[test]
fn election_times_out_without_a_majority() {
    // Candidate alone reachable among 3; with a tiny timeout the coordinator
    // gives up mid-collection rather than hanging.
    let members = vec![
        Member::data_voting("a"),
        Member::data_voting("b"),
        Member::data_voting("c"),
    ];
    let mut tx = ClusterTransport::new(members).with_watermark(100);
    tx.partition_away("b");
    tx.partition_away("c");
    // tick is 10ms/poll; a 5ms timeout trips on the first peer poll.
    let req = candidate_request("a", 4, 150, 100);

    let outcome = ElectionCoordinator::run(&req, &mut tx, Duration::from_millis(5));

    assert!(
        matches!(outcome, ElectionOutcome::TimedOut { needed: 2, .. }),
        "got {outcome:?}",
    );
    assert_eq!(tx.bumped_term, None, "timed-out probe does not bump term");
}

// ===============================================================
// No two primaries in a term — partition proof
// ===============================================================

#[test]
fn partition_minority_candidate_cannot_win() {
    // 5 voters partitioned {a,b} | {c,d,e}. A candidate in the minority
    // side reaches only itself + b = 2 < majority 3 → cannot win.
    let members: Vec<Member> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|id| Member::data_voting(*id))
        .collect();
    let mut tx = ClusterTransport::new(members).with_watermark(100);
    tx.partition_away("c");
    tx.partition_away("d");
    tx.partition_away("e");
    let req = candidate_request("a", 4, 150, 100);

    let outcome = ElectionCoordinator::run(&req, &mut tx, LONG);

    assert_eq!(
        outcome,
        ElectionOutcome::ProbeFailed {
            votes: 2,
            needed: 3
        }
    );
    assert_eq!(tx.promoted_term, None, "minority side promotes no one");
}

#[test]
fn partition_majority_candidate_wins_and_minority_cannot_take_same_term() {
    // The decisive proof. 5 voters. Two candidates stand for the SAME term
    // (current_term 4 → term 5) on opposite sides of a partition, sharing
    // the voters' durable stores. Only the majority side may win term 5, and
    // the shared voter's durable last-vote blocks the other from also
    // winning term 5 — so no two primaries hold term 5.
    let ids = ["a", "b", "c", "d", "e"];
    let members: Vec<Member> = ids.iter().map(|id| Member::data_voting(*id)).collect();

    // One shared durable store per voter, so both coordinators observe the
    // same persisted votes (models the real cluster's per-node disk).
    let stores: HashMap<&str, Rc<MemoryLastVoteStore>> = ids
        .iter()
        .map(|id| (*id, Rc::new(MemoryLastVoteStore::new())))
        .collect();

    // --- Majority side: candidate "c" reaches c,d,e (and a,b are away). ---
    let mut majority = ClusterTransport::new(members.clone()).with_watermark(100);
    for id in &ids {
        majority.share_store(id, Rc::clone(&stores[id]));
    }
    majority.partition_away("a");
    majority.partition_away("b");
    let c_req = candidate_request("c", 4, 150, 100);
    let c_outcome = ElectionCoordinator::run(&c_req, &mut majority, LONG);
    assert_eq!(
        c_outcome,
        ElectionOutcome::Elected {
            term: 5,
            votes: 3,
            needed: 3,
        },
        "majority side elects c for term 5",
    );
    assert_eq!(majority.promoted_term, Some(5));

    // --- Minority side: candidate "a" reaches only a,b for the SAME term. ---
    let mut minority = ClusterTransport::new(members).with_watermark(100);
    for id in &ids {
        minority.share_store(id, Rc::clone(&stores[id]));
    }
    minority.partition_away("c");
    minority.partition_away("d");
    minority.partition_away("e");
    let a_req = candidate_request("a", 4, 150, 100);
    let a_outcome = ElectionCoordinator::run(&a_req, &mut minority, LONG);

    // a reaches only a + b = 2 < 3, so it loses regardless. The structural
    // guarantee: even had it reached a third voter that already granted c,
    // that voter's durable last-vote refuses a in term 5.
    assert!(
        !a_outcome.is_elected(),
        "minority candidate must not also win term 5, got {a_outcome:?}",
    );
    assert_eq!(minority.promoted_term, None, "no second primary for term 5");

    // Direct durable proof: candidate "c" self-votes without an RPC, so its
    // own store is untouched, but the peers it reached (d, e) persisted the
    // grant. Any such voter refuses a competing candidate "a" in term 5 —
    // this is the per-node durable barrier that forbids a second primary.
    let shared_voter = Voter::new("d", RcStore(Rc::clone(&stores["d"])));
    assert_eq!(
        shared_voter
            .consider(&VoteRequest::real("a", 5, 150), 100)
            .unwrap(),
        VoteDecision::Refused(RefusalReason::AlreadyVoted {
            term: 5,
            voted_for: "c".to_string(),
        }),
        "a voter that granted c in term 5 blocks a second primary in term 5",
    );
}
