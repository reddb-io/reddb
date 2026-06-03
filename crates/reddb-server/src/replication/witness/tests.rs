//! Unit tests for the witness runtime profile (issue #836).
//!
//! These pin the structural guarantees: a witness boots no data plane, is a
//! vote-only member that counts toward quorum but cannot stand for election,
//! votes through the same durable last-vote rule a data voter uses, and
//! authenticates with the shared per-node [`NodeIdentity`].

use super::*;
use crate::replication::election::{
    quorum_threshold, LastVote, MemoryLastVoteStore, RefusalReason, VotingState,
};

fn identity(subject: &str) -> NodeIdentity {
    NodeIdentity::from_certificate_subject(subject).expect("non-empty subject")
}

fn witness(subject: &str) -> WitnessSupervisor<MemoryLastVoteStore> {
    WitnessSupervisor::new(identity(subject), MemoryLastVoteStore::new())
}

// ---------------------------------------------------------------
// RuntimeProfile: the data-plane branch
// ---------------------------------------------------------------

#[test]
fn data_profile_boots_data_plane_witness_does_not() {
    assert!(RuntimeProfile::Data.boots_data_plane());
    assert!(!RuntimeProfile::Witness.boots_data_plane());
    // Every profile runs the supervisor — that is the decoupled control plane.
    assert!(RuntimeProfile::Data.boots_supervisor());
    assert!(RuntimeProfile::Witness.boots_supervisor());
}

#[test]
fn profile_maps_to_its_member_kind() {
    assert_eq!(RuntimeProfile::Data.member_kind(), MemberKind::Data);
    assert_eq!(RuntimeProfile::Witness.member_kind(), MemberKind::Witness);
}

// ---------------------------------------------------------------
// WitnessSupervisor: supervisor-only, no data plane
// ---------------------------------------------------------------

#[test]
fn witness_supervisor_boots_no_data_plane() {
    let w = witness("CN=witness-1");
    assert_eq!(w.profile(), RuntimeProfile::Witness);
    assert!(!w.boots_data_plane(), "a witness holds no data plane");
}

#[test]
fn witness_member_is_vote_only_and_not_electable() {
    let w = witness("CN=witness-1");
    let m = w.member();
    assert_eq!(m.kind, MemberKind::Witness);
    assert_eq!(m.state, VotingState::Voting, "a witness always votes");
    assert!(m.is_voter(), "a witness counts toward quorum");
    assert!(
        !m.is_electable(),
        "a witness holds no data, so it can never be primary",
    );
}

#[test]
fn witness_vote_counts_toward_quorum_denominator() {
    // 2 data + 1 witness → the witness is in the denominator, so the strict
    // majority is 2, the whole point of the HA shape.
    let members = vec![
        Member::data_voting("CN=data-a"),
        Member::data_voting("CN=data-b"),
        witness("CN=witness-1").member(),
    ];
    assert_eq!(quorum_threshold(&members), 2);
}

// ---------------------------------------------------------------
// WitnessSupervisor: applies the real vote rule
// ---------------------------------------------------------------

#[test]
fn witness_grants_a_covering_candidate() {
    let w = witness("CN=witness-1");
    // Candidate frontier 150 covers watermark 100.
    let decision = w
        .consider_vote(&VoteRequest::real("CN=data-a", 5, 150), 100)
        .unwrap();
    assert_eq!(decision, VoteDecision::Granted);
    // A real grant advanced the witness's durable term.
    assert_eq!(w.current_term().unwrap(), 5);
}

#[test]
fn witness_refuses_a_candidate_below_the_watermark() {
    let w = witness("CN=witness-1");
    // Frontier 90 does not cover watermark 100 — the safety core refuses,
    // exactly as a data voter would.
    let decision = w
        .consider_vote(&VoteRequest::real("CN=data-a", 5, 90), 100)
        .unwrap();
    assert_eq!(
        decision,
        VoteDecision::Refused(RefusalReason::WatermarkNotCovered {
            candidate_lsn: 90,
            watermark: 100,
        }),
    );
}

#[test]
fn witness_does_not_double_vote_in_a_term() {
    let w = witness("CN=witness-1");
    assert!(w
        .consider_vote(&VoteRequest::real("CN=data-a", 7, 200), 100)
        .unwrap()
        .is_granted());
    // A second candidate in the same term is refused — already voted.
    assert_eq!(
        w.consider_vote(&VoteRequest::real("CN=data-b", 7, 200), 100)
            .unwrap(),
        VoteDecision::Refused(RefusalReason::AlreadyVoted {
            term: 7,
            voted_for: "CN=data-a".to_string(),
        }),
    );
}

#[test]
fn witness_probe_does_not_advance_its_term() {
    let w = witness("CN=witness-1");
    assert!(w
        .consider_vote(&VoteRequest::probe("CN=data-a", 9, 200), 100)
        .unwrap()
        .is_granted());
    // A dry-run probe is observation only — no term advance, no persisted vote.
    assert_eq!(w.current_term().unwrap(), 0);
}

// ---------------------------------------------------------------
// Shared per-node identity
// ---------------------------------------------------------------

#[test]
fn witness_authenticates_with_the_shared_node_identity() {
    let id = identity("CN=node-7,O=reddb");
    let w = WitnessSupervisor::new(id.clone(), MemoryLastVoteStore::new());
    // The witness's identity is the same NodeIdentity type a data member
    // presents over mTLS — same namespace, not a parallel one.
    assert_eq!(w.identity(), &id);
    // Its membership id is the certificate subject, so its votes land under
    // the same identity its acks would.
    assert_eq!(w.member().id, "CN=node-7,O=reddb");
}

#[test]
fn durable_witness_does_not_double_vote_across_restart() {
    let dir = std::env::temp_dir().join(format!(
        "reddb-witness-{}-{}",
        std::process::id(),
        crate::utils::now_unix_nanos()
    ));
    let path = dir.join("witness.lastvote.json");
    let id = identity("CN=witness-1");

    // Boot a durable witness, grant "data-a" in term 7.
    {
        let w = WitnessSupervisor::with_durable_store(id.clone(), &path);
        assert!(w
            .consider_vote(&VoteRequest::real("CN=data-a", 7, 200), 100)
            .unwrap()
            .is_granted());
    }
    // The witness process restarts: a fresh supervisor over the same file
    // refuses a different candidate in term 7, read back from disk.
    {
        let w = WitnessSupervisor::with_durable_store(id, &path);
        assert_eq!(
            w.consider_vote(&VoteRequest::real("CN=data-b", 7, 200), 100)
                .unwrap(),
            VoteDecision::Refused(RefusalReason::AlreadyVoted {
                term: 7,
                voted_for: "CN=data-a".to_string(),
            }),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn seeded_store_reports_its_recorded_term() {
    let w = WitnessSupervisor::new(
        identity("CN=witness-1"),
        MemoryLastVoteStore::seeded(LastVote {
            term: 4,
            voted_for: Some("CN=data-a".to_string()),
        }),
    );
    assert_eq!(w.current_term().unwrap(), 4);
}
