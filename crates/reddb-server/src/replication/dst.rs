//! Deterministic simulation testing helpers for the replication control plane.
//!
//! The simulator is intentionally in-process and single-threaded. It gives
//! control-plane tests a transport that can inject partition, delay, reorder,
//! and message loss without opening sockets or depending on Tokio scheduler
//! ordering.

use std::collections::BTreeSet;
use std::time::Duration;

use crate::replication::{VoteDecision, VoteRequest};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicationControlMessage {
    ElectionVoteRequest(VoteRequest),
    ElectionVoteDecision(VoteDecision),
    LogicalCommit {
        term: u64,
        lsn: u64,
        payload_hash: String,
    },
    LeaseProbe {
        holder_id: String,
        term: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkFaults {
    pub loss_per_million: u32,
    pub max_delay_ms: u64,
    pub reorder: bool,
}

impl NetworkFaults {
    pub fn reliable() -> Self {
        Self {
            loss_per_million: 0,
            max_delay_ms: 0,
            reorder: false,
        }
    }

    pub fn lossy(loss_per_million: u32) -> Self {
        Self {
            loss_per_million,
            max_delay_ms: 0,
            reorder: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimulationClock {
    now_ms: u64,
}

impl SimulationClock {
    pub fn new() -> Self {
        Self { now_ms: 0 }
    }

    pub fn now_ms(&self) -> u64 {
        self.now_ms
    }

    pub fn elapsed(&self) -> Duration {
        Duration::from_millis(self.now_ms)
    }

    pub fn advance(&mut self, by: Duration) {
        let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        self.now_ms = self.now_ms.saturating_add(millis);
    }

    fn advance_to(&mut self, now_ms: u64) {
        self.now_ms = self.now_ms.max(now_ms);
    }
}

impl Default for SimulationClock {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivered<M> {
    pub from: String,
    pub to: String,
    pub message: M,
    pub delivered_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    Accepted { deliver_at_ms: u64 },
    Dropped(DropReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    Partition,
    Loss,
}

#[derive(Debug, Clone)]
struct Pending<M> {
    from: String,
    to: String,
    message: M,
    deliver_at_ms: u64,
    order: u64,
}

#[derive(Debug, Clone)]
pub struct InProcessReplicationNetwork<M> {
    clock: SimulationClock,
    rng: SeededRng,
    faults: NetworkFaults,
    partitions: BTreeSet<(String, String)>,
    pending: Vec<Pending<M>>,
    sequence: u64,
}

impl<M> InProcessReplicationNetwork<M> {
    pub fn new(seed: u64, faults: NetworkFaults) -> Self {
        Self {
            clock: SimulationClock::new(),
            rng: SeededRng::new(seed),
            faults,
            partitions: BTreeSet::new(),
            pending: Vec::new(),
            sequence: 0,
        }
    }

    pub fn clock(&self) -> SimulationClock {
        self.clock
    }

    pub fn advance(&mut self, by: Duration) {
        self.clock.advance(by);
    }

    pub fn partition(&mut self, a: impl Into<String>, b: impl Into<String>) {
        self.partitions.insert(partition_key(a.into(), b.into()));
    }

    pub fn heal(&mut self, a: &str, b: &str) {
        self.partitions
            .remove(&partition_key(a.to_string(), b.to_string()));
    }

    pub fn send(
        &mut self,
        from: impl Into<String>,
        to: impl Into<String>,
        message: M,
    ) -> SendOutcome {
        let from = from.into();
        let to = to.into();
        if self
            .partitions
            .contains(&partition_key(from.clone(), to.clone()))
        {
            return SendOutcome::Dropped(DropReason::Partition);
        }
        if self.faults.loss_per_million > 0
            && self.rng.next_bounded(1_000_000) < u64::from(self.faults.loss_per_million)
        {
            return SendOutcome::Dropped(DropReason::Loss);
        }

        self.sequence = self.sequence.saturating_add(1);
        let delay = if self.faults.max_delay_ms == 0 {
            0
        } else {
            self.rng
                .next_bounded(self.faults.max_delay_ms.saturating_add(1))
        };
        let deliver_at_ms = self.clock.now_ms().saturating_add(delay);
        let order = if self.faults.reorder {
            self.rng.next_u64()
        } else {
            self.sequence
        };
        self.pending.push(Pending {
            from,
            to,
            message,
            deliver_at_ms,
            order,
        });
        SendOutcome::Accepted { deliver_at_ms }
    }

    pub fn advance_to_next_delivery(&mut self) -> bool {
        let Some(next) = self.pending.iter().map(|p| p.deliver_at_ms).min() else {
            return false;
        };
        self.clock.advance_to(next);
        true
    }

    pub fn drain_ready_for(&mut self, recipient: &str) -> Vec<Delivered<M>> {
        let now = self.clock.now_ms();
        let mut ready = Vec::new();
        let mut pending = Vec::with_capacity(self.pending.len());
        for msg in self.pending.drain(..) {
            if msg.to == recipient && msg.deliver_at_ms <= now {
                ready.push(msg);
            } else {
                pending.push(msg);
            }
        }
        self.pending = pending;
        ready.sort_by_key(|msg| (msg.deliver_at_ms, msg.order));
        ready
            .into_iter()
            .map(|msg| Delivered {
                from: msg.from,
                to: msg.to,
                message: msg.message,
                delivered_at_ms: msg.deliver_at_ms,
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
struct SeededRng {
    state: u64,
}

impl SeededRng {
    fn new(seed: u64) -> Self {
        let state = if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        };
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn next_bounded(&mut self, upper_exclusive: u64) -> u64 {
        if upper_exclusive == 0 {
            0
        } else {
            self.next_u64() % upper_exclusive
        }
    }
}

fn partition_key(a: String, b: String) -> (String, String) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::rc::Rc;
    use std::sync::Arc;

    use super::*;
    use crate::replication::{
        ElectionCoordinator, ElectionOutcome, ElectionRequest, ElectionTransport, LastVote,
        LastVoteError, LastVoteStore, LeaseError, LeaseStore, Member, MemoryLastVoteStore,
        RefusalReason, Voter, WriterLease,
    };

    #[test]
    fn fault_injection_is_seed_reproducible() {
        let trace_a = delivery_trace(0xD57, 0);
        let trace_b = delivery_trace(0xD57, 0);
        let trace_c = delivery_trace(0xD58, 0);

        assert_eq!(trace_a, trace_b, "same seed must reproduce the trace");
        assert_ne!(
            trace_a, trace_c,
            "different seed should explore a different trace"
        );

        let mut partitioned = InProcessReplicationNetwork::new(1, NetworkFaults::reliable());
        partitioned.partition("a", "b");
        assert_eq!(
            partitioned.send("a", "b", 1u64),
            SendOutcome::Dropped(DropReason::Partition)
        );

        let mut lossy = InProcessReplicationNetwork::new(1, NetworkFaults::lossy(1_000_000));
        assert_eq!(
            lossy.send("a", "b", 1u64),
            SendOutcome::Dropped(DropReason::Loss)
        );
    }

    fn delivery_trace(seed: u64, loss_per_million: u32) -> Vec<(u64, u64)> {
        let faults = NetworkFaults {
            loss_per_million,
            max_delay_ms: 25,
            reorder: true,
        };
        let mut network = InProcessReplicationNetwork::new(seed, faults);
        for value in 0..12u64 {
            let _ = network.send("a", "b", value);
        }
        network.advance(Duration::from_millis(25));
        network
            .drain_ready_for("b")
            .into_iter()
            .map(|msg| (msg.delivered_at_ms, msg.message))
            .collect()
    }

    #[test]
    fn election_safety_under_partition_has_at_most_one_leader_per_term() {
        let members = five_voters();
        let stores = shared_vote_stores(&members);
        let mut network = partitioned_network(0x1358);
        for peer in ["d", "e"] {
            network.partition("a", peer);
            network.partition("b", peer);
            network.partition("c", peer);
        }

        let mut leaders = BTreeMap::new();
        for candidate in [
            candidate_request("a", 4, 120, 100),
            candidate_request("d", 4, 120, 100),
        ] {
            let mut tx = NetworkElectionTransport::new(
                &mut network,
                members.clone(),
                stores.clone(),
                candidate.candidate.id.clone(),
                100,
            );
            let outcome = ElectionCoordinator::run(&candidate, &mut tx, Duration::from_secs(60));
            if let ElectionOutcome::Elected { term, .. } = outcome {
                let previous = leaders.insert(term, candidate.candidate.id.clone());
                assert_eq!(
                    previous, None,
                    "two leaders elected in term {term}: {previous:?} and {:?}",
                    candidate.candidate.id
                );
            }
        }

        assert_eq!(leaders.get(&5), Some(&"a".to_string()));
    }

    #[test]
    fn partitioned_elections_do_not_split_brain_or_lose_committed_writes() {
        let committed_watermark = 100;
        let committed_writes: BTreeSet<u64> = (1..=committed_watermark).collect();

        for seed in 1..=24 {
            let members = five_voters();
            let stores = shared_vote_stores(&members);
            let mut network = partitioned_network(seed);
            let mut elected = Vec::new();
            let candidates = if seed % 2 == 0 {
                [("a", 120), ("d", 80)]
            } else {
                [("d", 80), ("a", 120)]
            };

            for (id, lsn) in candidates {
                let req = candidate_request(id, 4, lsn, committed_watermark);
                let mut tx = NetworkElectionTransport::new(
                    &mut network,
                    members.clone(),
                    stores.clone(),
                    id.to_string(),
                    committed_watermark,
                );
                let outcome = ElectionCoordinator::run(&req, &mut tx, Duration::from_secs(60));
                if let ElectionOutcome::Elected { term, .. } = outcome {
                    elected.push((term, id.to_string(), lsn));
                }
            }

            let mut leaders_by_term = BTreeSet::new();
            for (term, id, lsn) in elected {
                assert!(
                    leaders_by_term.insert(term),
                    "split-brain in term {term} under seed {seed}"
                );
                assert!(
                    lsn >= committed_watermark,
                    "leader {id} lost committed writes under seed {seed}"
                );
                assert!(
                    committed_writes.iter().all(|committed| *committed <= lsn),
                    "leader {id} does not cover all committed writes under seed {seed}"
                );
            }
        }
    }

    #[test]
    fn lease_fencing_holds_when_a_partitioned_primary_returns_stale() {
        let members = five_voters();
        let stores = shared_vote_stores(&members);
        let mut network = partitioned_network(0x715);
        for peer in ["a", "b"] {
            network.partition("old-primary", peer);
        }
        let promoted = candidate_request("a", 4, 150, 100);
        let mut tx =
            NetworkElectionTransport::new(&mut network, members, stores, "a".to_string(), 100);

        let outcome = ElectionCoordinator::run(&promoted, &mut tx, Duration::from_secs(60));
        let ElectionOutcome::Elected { term: new_term, .. } = outcome else {
            panic!("expected a replacement primary, got {outcome:?}");
        };

        let store = lease_store("dst-fence");
        let lease = store
            .try_acquire_for_term("main", "new-primary", 60_000, new_term)
            .expect("new primary lease");
        assert_eq!(lease.term, new_term);

        let err = store
            .try_acquire_for_term("main", "old-primary", 60_000, new_term - 1)
            .expect_err("stale partitioned primary must be fenced");
        assert!(
            matches!(
                err,
                LeaseError::Fenced {
                    current_term,
                    ..
                } if current_term == new_term
            ),
            "got {err:?}"
        );

        let stale_lease = WriterLease {
            database_key: "main".to_string(),
            holder_id: "old-primary".to_string(),
            term: new_term - 1,
            generation: 1,
            acquired_at_ms: 0,
            expires_at_ms: u64::MAX,
        };
        assert!(stale_lease.fenced_by_term(new_term));
    }

    #[test]
    #[ignore = "heavy seed sweep runs nightly in CI"]
    fn dst_seed_sweep_election_safety_no_split_brain_no_lost_committed_writes() {
        for seed in 1..=256 {
            let members = five_voters();
            let stores = shared_vote_stores(&members);
            let mut network = partitioned_network(seed);
            let mut leaders = BTreeMap::new();
            for (id, lsn) in [("a", 125), ("b", 130), ("d", 90), ("e", 95)] {
                let req = candidate_request(id, 7, lsn, 100);
                let mut tx = NetworkElectionTransport::new(
                    &mut network,
                    members.clone(),
                    stores.clone(),
                    id.to_string(),
                    100,
                );
                if let ElectionOutcome::Elected { term, .. } =
                    ElectionCoordinator::run(&req, &mut tx, Duration::from_secs(60))
                {
                    assert!(lsn >= 100, "seed {seed}: elected {id} below watermark");
                    assert_eq!(
                        leaders.insert(term, id.to_string()),
                        None,
                        "seed {seed}: more than one leader in term {term}"
                    );
                }
            }
        }
    }

    struct NetworkElectionTransport<'a> {
        network: &'a mut InProcessReplicationNetwork<ReplicationControlMessage>,
        members: Vec<Member>,
        stores: BTreeMap<String, Rc<MemoryLastVoteStore>>,
        candidate_id: String,
        watermark: u64,
        bumped_term: Option<u64>,
        promoted_term: Option<u64>,
    }

    impl<'a> NetworkElectionTransport<'a> {
        fn new(
            network: &'a mut InProcessReplicationNetwork<ReplicationControlMessage>,
            members: Vec<Member>,
            stores: BTreeMap<String, Rc<MemoryLastVoteStore>>,
            candidate_id: String,
            watermark: u64,
        ) -> Self {
            Self {
                network,
                members,
                stores,
                candidate_id,
                watermark,
                bumped_term: None,
                promoted_term: None,
            }
        }
    }

    impl ElectionTransport for NetworkElectionTransport<'_> {
        fn members(&self) -> Vec<Member> {
            self.members.clone()
        }

        fn request_vote(&mut self, peer_id: &str, req: &VoteRequest) -> VoteDecision {
            let outcome = self.network.send(
                self.candidate_id.clone(),
                peer_id.to_string(),
                ReplicationControlMessage::ElectionVoteRequest(req.clone()),
            );
            if !matches!(outcome, SendOutcome::Accepted { .. }) {
                return unreachable_refusal(req);
            }
            if !self.network.advance_to_next_delivery() {
                return unreachable_refusal(req);
            }
            let requests = self.network.drain_ready_for(peer_id);
            let Some(request) = requests
                .into_iter()
                .find_map(|delivery| match delivery.message {
                    ReplicationControlMessage::ElectionVoteRequest(request) => Some(request),
                    _ => None,
                })
            else {
                return unreachable_refusal(req);
            };

            let store = self.stores.get(peer_id).expect("known voter").clone();
            let voter = Voter::new(peer_id, RcStore(store));
            let decision = voter
                .consider(&request, self.watermark)
                .expect("memory vote store");
            let outcome = self.network.send(
                peer_id.to_string(),
                self.candidate_id.clone(),
                ReplicationControlMessage::ElectionVoteDecision(decision.clone()),
            );
            if !matches!(outcome, SendOutcome::Accepted { .. }) {
                return unreachable_refusal(req);
            }
            if !self.network.advance_to_next_delivery() {
                return unreachable_refusal(req);
            }
            self.network
                .drain_ready_for(&self.candidate_id)
                .into_iter()
                .find_map(|delivery| match delivery.message {
                    ReplicationControlMessage::ElectionVoteDecision(decision) => Some(decision),
                    _ => None,
                })
                .unwrap_or_else(|| unreachable_refusal(req))
        }

        fn elapsed(&self) -> Duration {
            self.network.clock().elapsed()
        }

        fn bump_term(&mut self, new_term: u64) {
            self.bumped_term = Some(new_term);
        }

        fn promote(&mut self, new_term: u64) {
            self.promoted_term = Some(new_term);
        }
    }

    #[derive(Clone)]
    struct RcStore(Rc<MemoryLastVoteStore>);

    impl LastVoteStore for RcStore {
        fn load(&self) -> Result<LastVote, LastVoteError> {
            self.0.load()
        }

        fn persist(&self, vote: &LastVote) -> Result<(), LastVoteError> {
            self.0.persist(vote)
        }
    }

    fn unreachable_refusal(req: &VoteRequest) -> VoteDecision {
        VoteDecision::Refused(RefusalReason::StaleTerm {
            candidate_term: req.term,
            voter_term: u64::MAX,
        })
    }

    fn five_voters() -> Vec<Member> {
        vec![
            Member::data_voting("a"),
            Member::data_voting("b"),
            Member::data_voting("c"),
            Member::data_voting("d"),
            Member::data_voting("e"),
        ]
    }

    fn shared_vote_stores(members: &[Member]) -> BTreeMap<String, Rc<MemoryLastVoteStore>> {
        members
            .iter()
            .map(|member| (member.id.clone(), Rc::new(MemoryLastVoteStore::new())))
            .collect()
    }

    fn partitioned_network(seed: u64) -> InProcessReplicationNetwork<ReplicationControlMessage> {
        let mut network = InProcessReplicationNetwork::new(
            seed,
            NetworkFaults {
                loss_per_million: 0,
                max_delay_ms: 20,
                reorder: true,
            },
        );
        for left in ["a", "b", "c"] {
            for right in ["d", "e"] {
                network.partition(left, right);
            }
        }
        network
    }

    fn candidate_request(id: &str, current_term: u64, lsn: u64, watermark: u64) -> ElectionRequest {
        ElectionRequest {
            candidate: Member::data_voting(id),
            current_term,
            last_log_lsn: lsn,
            commit_watermark: watermark,
        }
    }

    fn lease_store(tag: &str) -> LeaseStore {
        use crate::storage::backend::LocalBackend;

        LeaseStore::new(Arc::new(LocalBackend)).with_prefix(format!(
            "{}/reddb-{tag}-{}",
            std::env::temp_dir().to_string_lossy(),
            crate::utils::now_unix_nanos(),
        ))
    }
}
