//! Deterministic in-process transport for replication control-plane tests.
//!
//! This is a fault-injection seam, not a runtime socket replacement. Tests can
//! drive election, lease, quorum, CDC, and logical-replication state machines
//! through one seed-controlled queue with an explicit simulation clock.

use std::collections::BTreeSet;
use std::time::Duration;

use super::election::{VoteDecision, VoteRequest};

/// Control-plane messages the simulator can carry without opening sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicationControlMessage {
    PrimaryHeartbeat {
        term: u64,
        lsn: u64,
    },
    ReplicaFrontier {
        replica_id: String,
        term: u64,
        lsn: u64,
    },
    ElectionVoteRequest(VoteRequest),
    ElectionVoteDecision(VoteDecision),
    FailoverCommit {
        new_primary: String,
        term: u64,
    },
    LeaseFence {
        holder_id: String,
        term: u64,
    },
    QuorumAck {
        replica_id: String,
        term: u64,
        lsn: u64,
    },
    CdcRecord {
        term: u64,
        lsn: u64,
    },
    LogicalApply {
        term: u64,
        lsn: u64,
    },
}

/// What happened when a message was offered to the in-process transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Queued,
    Partitioned,
    Lost,
}

/// A delivered message with the simulator metadata retained for assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimulatedMessage<M> {
    pub from: String,
    pub to: String,
    pub sequence: u64,
    pub sent_at: Duration,
    pub deliver_at: Duration,
    pub payload: M,
}

/// Manual simulation clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SimulationClock {
    now: Duration,
}

impl SimulationClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn now(&self) -> Duration {
        self.now
    }

    pub fn advance(&mut self, delta: Duration) {
        self.now = self.now.saturating_add(delta);
    }
}

/// Small deterministic RNG so simulator tests do not depend on OS entropy.
#[derive(Debug, Clone)]
pub struct SeededRng {
    state: u64,
}

impl SeededRng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    pub fn gen_below(&mut self, upper: u64) -> u64 {
        if upper == 0 {
            return 0;
        }
        self.next_u64() % upper
    }

    fn gen_index(&mut self, upper: usize) -> usize {
        if upper == 0 {
            return 0;
        }
        let bound = u64::try_from(upper).expect("usize fits into u64 on supported targets");
        let value = self.gen_below(bound);
        usize::try_from(value).expect("value is below original usize bound")
    }
}

/// Seed-controlled in-process transport for replication control-plane traffic.
#[derive(Debug, Clone)]
pub struct InProcessReplicationTransport<M> {
    clock: SimulationClock,
    rng: SeededRng,
    partitions: BTreeSet<(String, String)>,
    queue: Vec<SimulatedMessage<M>>,
    max_delay: Duration,
    loss_per_million: u32,
    reorder_window: usize,
    next_sequence: u64,
    last_sender: Option<String>,
}

impl<M> InProcessReplicationTransport<M> {
    pub fn new(seed: u64) -> Self {
        Self {
            clock: SimulationClock::new(),
            rng: SeededRng::new(seed),
            partitions: BTreeSet::new(),
            queue: Vec::new(),
            max_delay: Duration::ZERO,
            loss_per_million: 0,
            reorder_window: 0,
            next_sequence: 0,
            last_sender: None,
        }
    }

    pub fn now(&self) -> Duration {
        self.clock.now()
    }

    pub fn advance(&mut self, delta: Duration) {
        self.clock.advance(delta);
    }

    pub fn set_max_delay(&mut self, delay: Duration) {
        self.max_delay = delay;
    }

    pub fn set_loss_per_million(&mut self, loss_per_million: u32) {
        self.loss_per_million = loss_per_million.min(1_000_000);
    }

    pub fn set_reorder_window(&mut self, reorder_window: usize) {
        self.reorder_window = reorder_window;
    }

    pub fn partition(&mut self, a: &str, b: &str) {
        self.partitions.insert(Self::edge(a, b));
    }

    pub fn heal(&mut self, a: &str, b: &str) {
        self.partitions.remove(&Self::edge(a, b));
    }

    pub fn last_sender(&self) -> Option<&str> {
        self.last_sender.as_deref()
    }

    pub fn send(&mut self, from: &str, to: &str, payload: M) -> DeliveryOutcome {
        self.last_sender = Some(from.to_string());
        if self.partitions.contains(&Self::edge(from, to)) {
            return DeliveryOutcome::Partitioned;
        }
        if self.message_is_lost() {
            return DeliveryOutcome::Lost;
        }

        let delay = self.next_delay();
        let message = SimulatedMessage {
            from: from.to_string(),
            to: to.to_string(),
            sequence: self.next_sequence,
            sent_at: self.clock.now(),
            deliver_at: self.clock.now().saturating_add(delay),
            payload,
        };
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.insert_with_reorder(message);
        DeliveryOutcome::Queued
    }

    pub fn drain_ready(&mut self, to: &str) -> Vec<SimulatedMessage<M>> {
        let now = self.clock.now();
        let mut ready = Vec::new();
        let mut pending = Vec::new();
        for message in self.queue.drain(..) {
            if message.to == to && message.deliver_at <= now {
                ready.push(message);
            } else {
                pending.push(message);
            }
        }
        self.queue = pending;
        ready
    }

    fn edge(a: &str, b: &str) -> (String, String) {
        if a <= b {
            (a.to_string(), b.to_string())
        } else {
            (b.to_string(), a.to_string())
        }
    }

    fn message_is_lost(&mut self) -> bool {
        self.loss_per_million > 0
            && self.rng.gen_below(1_000_000) < u64::from(self.loss_per_million)
    }

    fn next_delay(&mut self) -> Duration {
        let max_millis = u64::try_from(self.max_delay.as_millis()).unwrap_or(u64::MAX);
        if max_millis == 0 {
            Duration::ZERO
        } else {
            Duration::from_millis(self.rng.gen_below(max_millis).saturating_add(1))
        }
    }

    fn insert_with_reorder(&mut self, message: SimulatedMessage<M>) {
        if self.reorder_window == 0 || self.queue.is_empty() {
            self.queue.push(message);
            return;
        }
        let max_back = self.reorder_window.min(self.queue.len());
        let back = self.rng.gen_index(max_back.saturating_add(1));
        let index = self.queue.len().saturating_sub(back);
        self.queue.insert(index, message);
    }
}
