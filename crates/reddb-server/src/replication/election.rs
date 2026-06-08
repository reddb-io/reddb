//! Term-based, quorum-gated automatic election (issue #834, PRD #819, ADR 0030).
//!
//! This is the consensus core that turns a primary loss into an automatic,
//! *safe* promotion. It lives in the first-party-but-decoupled control-plane
//! supervisor (ADR 0030) — distinct from the data path — and reuses the two
//! pieces the rest of replication already built:
//!
//! * the **commit watermark** ([`super::commit_waiter`] / [`super::quorum`]) —
//!   the highest LSN durably replicated to a quorum that intersects every
//!   possible election majority. Nothing at or below it may ever be rolled
//!   back; and
//! * the **FAILOVER handover machinery** ([`super::failover`]) — once a
//!   candidate wins, promotion is driven through the same coordinated
//!   role-swap, not a parallel state machine.
//!
//! ## The five hard requirements (ADR 0030, issue #834)
//!
//! 1. **Dry-run probe.** A candidate first asks "*would* you vote for me?"
//!    without bumping any term. Only a real election bumps the term. This
//!    keeps a flapping candidate from burning through terms and lets the
//!    supervisor probe liveness cheaply.
//! 2. **Durable last-vote.** A voter persists `(term, voted_for)` *before*
//!    acknowledging a grant, so a voter that crashes and restarts mid-term
//!    never double-votes — the second request in the same term for a
//!    different candidate is refused from disk.
//! 3. **Watermark vote rule (the safety core).** A voter MUST refuse any
//!    candidate whose log does not cover the commit watermark. An
//!    acknowledged synchronous write sits at or below the watermark, so a
//!    winner necessarily carries it — the write provably survives the
//!    failover. This is the one rule that may not be relaxed.
//! 4. **Randomized election timeouts.** Candidates wait a randomized
//!    interval before standing, so split votes are rare and self-correcting.
//! 5. **Membership rules.** A quorum is a *majority of voting members*.
//!    **Witness** members ([#836]) hold no data but vote, so `2 data + 1
//!    witness` is a valid HA shape. A **catching-up** replica is
//!    *non-voting* until it reaches a healthy state — it neither votes nor
//!    stands.
//!
//! ## No two primaries in a term
//!
//! This invariant is structural, not probabilistic:
//!
//! * a win requires a strict majority of voting members, and two strict
//!   majorities of the same set always intersect; and
//! * the shared voter in any two majorities votes at most once per term
//!   (durable last-vote), so it cannot grant two different candidates the
//!   same term.
//!
//! Therefore at most one candidate can collect a majority in a given term,
//! even under an arbitrary network partition. The partition tests exercise
//! exactly this.
//!
//! ## Module shape
//!
//! Like [`super::failover`], the candidate-side [`ElectionCoordinator::run`]
//! is a **pure state machine**: the clock, the per-peer vote RPC, the
//! durable term bump, and the promotion are injected behind
//! [`ElectionTransport`], so the whole election is exercised
//! deterministically with a scripted fake — no clock, no network, no engine.
//! The voter-side [`Voter`] wraps a [`LastVoteStore`] (durable on disk in
//! production, in-memory in tests) and applies the vote rule.
//!
//! [#836]: https://github.com/reddb-io/reddb/issues/836

use std::time::Duration;

pub use reddb_file::FileLastVoteStore;

// ---------------------------------------------------------------
// Membership model
// ---------------------------------------------------------------

/// Whether a member holds data (and can therefore be promoted to primary)
/// or is a vote-only witness (ADR 0030 — "a node that runs only the
/// supervisor module").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    /// Holds data; can vote and can stand for election.
    Data,
    /// Vote-only witness ([#836]); counts toward quorum, never primary.
    ///
    /// [#836]: https://github.com/reddb-io/reddb/issues/836
    Witness,
}

/// Whether a member currently participates in voting.
///
/// A data replica that is still catching up (has not reached a healthy,
/// watermark-covering state) is [`VotingState::CatchingUp`] and is excluded
/// from the voter set entirely — it neither votes nor counts toward the
/// majority denominator. Once healthy it becomes [`VotingState::Voting`].
/// Witnesses are always [`VotingState::Voting`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VotingState {
    /// Healthy member that participates in quorum.
    Voting,
    /// Replica still syncing; non-voting until healthy.
    CatchingUp,
}

/// A cluster member as seen by the supervisor's membership view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// Stable node identity (matches the replica registry / ack id).
    pub id: String,
    pub kind: MemberKind,
    pub state: VotingState,
}

impl Member {
    pub fn data_voting(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: MemberKind::Data,
            state: VotingState::Voting,
        }
    }

    pub fn data_catching_up(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: MemberKind::Data,
            state: VotingState::CatchingUp,
        }
    }

    pub fn witness(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: MemberKind::Witness,
            state: VotingState::Voting,
        }
    }

    /// Does this member count toward quorum? Only healthy members vote;
    /// a catching-up replica is non-voting (ADR 0030).
    pub fn is_voter(&self) -> bool {
        matches!(self.state, VotingState::Voting)
    }

    /// May this member stand for election? Only a healthy, data-bearing
    /// member can become primary — a witness holds no data and a
    /// catching-up replica is not healthy.
    pub fn is_electable(&self) -> bool {
        self.kind == MemberKind::Data && self.is_voter()
    }
}

/// Quorum threshold for a set of members: a strict majority of the
/// *voting* members. Witnesses count; catching-up replicas do not.
///
/// For `n` voting members the threshold is `floor(n/2) + 1`, the smallest
/// count such that two qualifying sets always intersect — the structural
/// basis for "no two primaries in a term".
pub fn quorum_threshold(members: &[Member]) -> usize {
    let voters = members.iter().filter(|m| m.is_voter()).count();
    voters / 2 + 1
}

// ---------------------------------------------------------------
// Durable last-vote
// ---------------------------------------------------------------

/// A node's durable voting record: the highest term it has participated in
/// and who, if anyone, it granted that term. Persisted so a restart cannot
/// erase the fact that a vote was already cast (requirement 2).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LastVote {
    /// Highest term this node has observed in a (real) vote request.
    pub term: u64,
    /// Who this node granted `term` to, if anyone yet.
    pub voted_for: Option<String>,
}

impl LastVote {
    fn from_file(value: reddb_file::DurableLastVote) -> Self {
        Self {
            term: value.term,
            voted_for: value.voted_for,
        }
    }

    fn to_file(&self) -> reddb_file::DurableLastVote {
        reddb_file::DurableLastVote::new(self.term, self.voted_for.clone())
    }
}

#[derive(Debug)]
pub enum LastVoteError {
    Io(std::io::Error),
    InvalidFormat(String),
}

impl std::fmt::Display for LastVoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "last-vote io error: {err}"),
            Self::InvalidFormat(msg) => write!(f, "invalid last-vote format: {msg}"),
        }
    }
}

impl std::error::Error for LastVoteError {}

impl From<reddb_file::RdbFileError> for LastVoteError {
    fn from(value: reddb_file::RdbFileError) -> Self {
        match value {
            reddb_file::RdbFileError::Io(err) => Self::Io(err),
            reddb_file::RdbFileError::InvalidOperation(msg) => Self::InvalidFormat(msg),
        }
    }
}

/// Durable store for a node's last vote. The contract is narrow on purpose:
/// `load` returns the persisted record (or the default `term 0, voted_for
/// None` when nothing was ever written), and `persist` makes a record
/// durable *before* the caller acknowledges a grant.
pub trait LastVoteStore {
    fn load(&self) -> Result<LastVote, LastVoteError>;
    fn persist(&self, vote: &LastVote) -> Result<(), LastVoteError>;
}

/// In-memory last-vote store for tests and witnesses that do not need
/// cross-restart durability. (A witness *should* still persist in
/// production; the file store is used there.)
#[derive(Debug, Default)]
pub struct MemoryLastVoteStore {
    inner: std::sync::Mutex<LastVote>,
}

impl MemoryLastVoteStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed an initial record — used by tests to simulate a node that
    /// already voted before a restart.
    pub fn seeded(vote: LastVote) -> Self {
        Self {
            inner: std::sync::Mutex::new(vote),
        }
    }
}

impl LastVoteStore for MemoryLastVoteStore {
    fn load(&self) -> Result<LastVote, LastVoteError> {
        Ok(self.inner.lock().expect("last-vote mutex").clone())
    }

    fn persist(&self, vote: &LastVote) -> Result<(), LastVoteError> {
        *self.inner.lock().expect("last-vote mutex") = vote.clone();
        Ok(())
    }
}

impl LastVoteStore for FileLastVoteStore {
    fn load(&self) -> Result<LastVote, LastVoteError> {
        self.load_file()
            .map(LastVote::from_file)
            .map_err(LastVoteError::from)
    }

    fn persist(&self, vote: &LastVote) -> Result<(), LastVoteError> {
        self.persist_file(&vote.to_file())
            .map_err(LastVoteError::from)
    }
}

// ---------------------------------------------------------------
// Vote request / decision
// ---------------------------------------------------------------

/// A request for a vote, sent by a candidate to a voter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoteRequest {
    /// The candidate's stable identity.
    pub candidate_id: String,
    /// The term the candidate is standing for. For a real election this is
    /// `current_term + 1`; for a dry-run probe it is the *prospective* term
    /// the candidate would stand for, evaluated without committing to it.
    pub term: u64,
    /// The candidate's log frontier — the highest LSN durably in its log.
    /// The watermark rule compares this against the commit watermark.
    pub last_log_lsn: u64,
    /// A dry-run probe gathers a would-be vote without persisting it or
    /// advancing the voter's term (requirement 1). A real election sets
    /// this `false`, which is the only path that persists a last-vote.
    pub dry_run: bool,
}

impl VoteRequest {
    pub fn probe(candidate_id: impl Into<String>, term: u64, last_log_lsn: u64) -> Self {
        Self {
            candidate_id: candidate_id.into(),
            term,
            last_log_lsn,
            dry_run: true,
        }
    }

    pub fn real(candidate_id: impl Into<String>, term: u64, last_log_lsn: u64) -> Self {
        Self {
            candidate_id: candidate_id.into(),
            term,
            last_log_lsn,
            dry_run: false,
        }
    }
}

/// Why a voter refused a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefusalReason {
    /// The candidate's log does not cover the commit watermark, so an
    /// acknowledged synchronous write could be lost — the safety core
    /// refuses (requirement 3).
    WatermarkNotCovered { candidate_lsn: u64, watermark: u64 },
    /// The candidate's term is not newer than a term this voter already
    /// participated in, and the voter already granted that term to someone
    /// else (durable double-vote guard, requirement 2).
    AlreadyVoted { term: u64, voted_for: String },
    /// The candidate's term is older than the voter's current term — a
    /// stale candidate from a superseded term.
    StaleTerm {
        candidate_term: u64,
        voter_term: u64,
    },
}

/// The outcome of a voter considering a [`VoteRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoteDecision {
    /// Vote granted. For a real (non-dry-run) request the grant has already
    /// been persisted durably before this value was produced.
    Granted,
    /// Vote refused, with the reason.
    Refused(RefusalReason),
}

impl VoteDecision {
    pub fn is_granted(&self) -> bool {
        matches!(self, VoteDecision::Granted)
    }
}

// ---------------------------------------------------------------
// Voter (voter-side vote rule)
// ---------------------------------------------------------------

/// A voting member. Wraps the durable [`LastVoteStore`] and applies the
/// vote rule. The voter is the seat of correctness: the watermark rule and
/// the durable double-vote guard both live here.
pub struct Voter<S: LastVoteStore> {
    id: String,
    store: S,
}

impl<S: LastVoteStore> Voter<S> {
    pub fn new(id: impl Into<String>, store: S) -> Self {
        Self {
            id: id.into(),
            store,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// This voter's current term — the highest term it has durably recorded.
    pub fn current_term(&self) -> Result<u64, LastVoteError> {
        Ok(self.store.load()?.term)
    }

    /// Consider a vote request against the current `commit_watermark`.
    ///
    /// The decision order is deliberate:
    ///
    /// 1. **Watermark first** — the safety core. A candidate that cannot
    ///    carry an acknowledged write is refused regardless of term, so the
    ///    durability guarantee can never be traded away for liveness.
    /// 2. **Stale term** — reject candidates from a superseded term.
    /// 3. **Double-vote guard** — within a term, a voter grants exactly one
    ///    candidate; a re-ask by the *same* candidate is idempotently
    ///    re-granted.
    ///
    /// For a real (non-dry-run) grant, the new `(term, candidate)` is
    /// persisted **before** `Granted` is returned, so the durability holds
    /// across a crash at any point after the caller observes the grant.
    pub fn consider(
        &self,
        req: &VoteRequest,
        commit_watermark: u64,
    ) -> Result<VoteDecision, LastVoteError> {
        // 1. Watermark rule — never relaxed, checked before anything else.
        if req.last_log_lsn < commit_watermark {
            return Ok(VoteDecision::Refused(RefusalReason::WatermarkNotCovered {
                candidate_lsn: req.last_log_lsn,
                watermark: commit_watermark,
            }));
        }

        let last = self.store.load()?;

        // 2. Stale term — the candidate is behind a term we already moved past.
        if req.term < last.term {
            return Ok(VoteDecision::Refused(RefusalReason::StaleTerm {
                candidate_term: req.term,
                voter_term: last.term,
            }));
        }

        // 3. Double-vote guard within the *same* term.
        if req.term == last.term {
            match &last.voted_for {
                // Already voted for someone else this term — refuse.
                Some(other) if other != &req.candidate_id => {
                    return Ok(VoteDecision::Refused(RefusalReason::AlreadyVoted {
                        term: last.term,
                        voted_for: other.clone(),
                    }));
                }
                // Already voted for this same candidate — idempotent re-grant.
                Some(_) => return Ok(VoteDecision::Granted),
                // Same term observed but no vote cast yet — fall through to grant.
                None => {}
            }
        }

        // Grant. A dry-run probe must not persist or advance the term
        // (requirement 1); a real grant persists durably before acking.
        if !req.dry_run {
            self.store.persist(&LastVote {
                term: req.term,
                voted_for: Some(req.candidate_id.clone()),
            })?;
        }
        Ok(VoteDecision::Granted)
    }
}

// ---------------------------------------------------------------
// Randomized election timeout
// ---------------------------------------------------------------

/// A randomized election timeout in `[base, base + jitter)`.
///
/// Randomization keeps candidates from standing in lockstep, which is what
/// makes split votes rare and self-correcting (requirement 4). The function
/// is pure in `seed` so tests pin a deterministic value; production passes
/// an entropy-derived seed.
pub fn randomized_election_timeout(base: Duration, jitter: Duration, seed: u64) -> Duration {
    if jitter.is_zero() {
        return base;
    }
    let jitter_ms = jitter.as_millis().max(1) as u64;
    base + Duration::from_millis(seed % jitter_ms)
}

// ---------------------------------------------------------------
// ElectionCoordinator (candidate-side state machine)
// ---------------------------------------------------------------

/// A request to run an election on behalf of `candidate`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElectionRequest {
    /// The candidate standing for election. Must be electable (a healthy,
    /// data-bearing member) or [`ElectionCoordinator::run`] refuses up front.
    pub candidate: Member,
    /// The term the cluster is serving now. A real election stands for
    /// `current_term + 1`.
    pub current_term: u64,
    /// The candidate's log frontier — the highest LSN durably in its log.
    pub last_log_lsn: u64,
    /// The commit watermark the candidate believes is in force. The
    /// candidate must itself cover it to be electable; voters re-check
    /// against their own watermark view.
    pub commit_watermark: u64,
}

impl ElectionRequest {
    /// The term a successful election produces.
    pub fn new_term(&self) -> u64 {
        self.current_term + 1
    }
}

/// The result of an election attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElectionOutcome {
    /// Won a majority and was promoted under `term`. `votes`/`needed` record
    /// the tally (including the candidate's self-vote).
    Elected {
        term: u64,
        votes: usize,
        needed: usize,
    },
    /// The dry-run probe did not reach a majority, so no term was bumped and
    /// no real election was attempted (requirement 1).
    ProbeFailed { votes: usize, needed: usize },
    /// A real election was attempted (term bumped) but did not reach a
    /// majority — e.g. votes split or peers came online between probe and
    /// election. The term has advanced; a later attempt stands for a higher
    /// term.
    Lost {
        term: u64,
        votes: usize,
        needed: usize,
    },
    /// The candidate is not electable (a witness, or a catching-up replica,
    /// or its own log does not cover the watermark). No probe was sent.
    NotElectable,
    /// The election ran past its randomized timeout before collecting a
    /// majority. No promotion happened.
    TimedOut { votes: usize, needed: usize },
}

impl ElectionOutcome {
    pub fn is_elected(&self) -> bool {
        matches!(self, ElectionOutcome::Elected { .. })
    }
}

/// Cluster operations the candidate drives, injected so the state machine
/// stays pure and deterministically testable. Production backs these onto
/// the membership view, the per-peer vote RPC, the durable term store, and
/// the FAILOVER handover; tests back them onto a scripted fake.
pub trait ElectionTransport {
    /// The candidate's current view of cluster membership. The denominator
    /// for the majority is the *voting* members of this set.
    fn members(&self) -> Vec<Member>;

    /// Ask one peer for its vote. The candidate never asks itself (it always
    /// self-grants). Implementors route this to the peer's [`Voter`].
    fn request_vote(&mut self, peer_id: &str, req: &VoteRequest) -> VoteDecision;

    /// Time elapsed since the election began, so the coordinator enforces
    /// the randomized timeout without owning a clock.
    fn elapsed(&self) -> Duration;

    /// Durably advance this node's current term to `new_term`. Called once,
    /// only when a real election begins (never for a dry-run). Persisted
    /// alongside the node's other durable replication state.
    fn bump_term(&mut self, new_term: u64);

    /// Promote the candidate to primary under `new_term`, reusing the
    /// FAILOVER handover machinery ([`super::failover`]). Called only after
    /// a majority is collected in the real election.
    fn promote(&mut self, new_term: u64);
}

/// The quorum-gated election state machine.
pub struct ElectionCoordinator;

impl ElectionCoordinator {
    /// Run an election for `req`, driving the cluster through `tx`, bounded
    /// by `timeout` (use [`randomized_election_timeout`]).
    ///
    /// The flow is: electability guard → dry-run probe (no term bump) →
    /// real election (bump term, collect votes) → promote on majority. See
    /// the module docs for the full contract.
    pub fn run(
        req: &ElectionRequest,
        tx: &mut dyn ElectionTransport,
        timeout: Duration,
    ) -> ElectionOutcome {
        // Electability guard. A witness or catching-up replica may not
        // stand; nor may a candidate whose own log does not cover the
        // watermark (it would violate the safety core the instant it won).
        if !req.candidate.is_electable() || req.last_log_lsn < req.commit_watermark {
            return ElectionOutcome::NotElectable;
        }

        let members = tx.members();
        let needed = quorum_threshold(&members);
        let new_term = req.new_term();

        // The peers we ask: every *other* voting member. The candidate
        // self-grants, so it is one vote without an RPC.
        let peers: Vec<String> = members
            .iter()
            .filter(|m| m.is_voter() && m.id != req.candidate.id)
            .map(|m| m.id.clone())
            .collect();

        // ---- Phase 1: dry-run probe (does NOT bump the term) ----
        let probe = VoteRequest::probe(&req.candidate.id, new_term, req.last_log_lsn);
        let probe_votes = match Self::collect(tx, &peers, &probe, needed, timeout) {
            CollectResult::Reached(v) => v,
            CollectResult::Exhausted(v) => {
                return ElectionOutcome::ProbeFailed { votes: v, needed }
            }
            CollectResult::TimedOut(v) => return ElectionOutcome::TimedOut { votes: v, needed },
        };
        debug_assert!(probe_votes >= needed);

        // ---- Phase 2: real election (bumps the term, then collects) ----
        tx.bump_term(new_term);
        let ballot = VoteRequest::real(&req.candidate.id, new_term, req.last_log_lsn);
        match Self::collect(tx, &peers, &ballot, needed, timeout) {
            CollectResult::Reached(votes) => {
                tx.promote(new_term);
                ElectionOutcome::Elected {
                    term: new_term,
                    votes,
                    needed,
                }
            }
            CollectResult::Exhausted(votes) => ElectionOutcome::Lost {
                term: new_term,
                votes,
                needed,
            },
            CollectResult::TimedOut(votes) => ElectionOutcome::TimedOut { votes, needed },
        }
    }

    /// Collect votes from `peers`, starting at 1 for the candidate's
    /// self-vote, until `needed` is reached, the peers are exhausted, or the
    /// timeout elapses. Stops early on success — no need to ask the rest.
    fn collect(
        tx: &mut dyn ElectionTransport,
        peers: &[String],
        req: &VoteRequest,
        needed: usize,
        timeout: Duration,
    ) -> CollectResult {
        let mut votes = 1usize; // self-vote
        if votes >= needed {
            return CollectResult::Reached(votes);
        }
        for peer in peers {
            if tx.elapsed() >= timeout {
                return CollectResult::TimedOut(votes);
            }
            if tx.request_vote(peer, req).is_granted() {
                votes += 1;
                if votes >= needed {
                    return CollectResult::Reached(votes);
                }
            }
        }
        CollectResult::Exhausted(votes)
    }
}

enum CollectResult {
    Reached(usize),
    Exhausted(usize),
    TimedOut(usize),
}

#[cfg(test)]
mod tests;
