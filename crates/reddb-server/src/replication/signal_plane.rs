//! Signal-plane dissemination seam (issue #1842, parent #1832, ADR 0073).
//!
//! This module names the non-authoritative peer-to-peer signal layer used by
//! admitted cluster members to share routing and health hints. The vocabulary is
//! closed over observations only: liveness, health inputs, load samples,
//! catalog-version hints, and topology hints. There is no membership admission,
//! ownership transition, vote, or bootstrap-state variant, so those authority
//! facts cannot be represented by this seam.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use super::MemberId;

/// Peer reachability state observed by an admitted member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LivenessStatus {
    Alive,
    Suspect,
    Unreachable,
}

/// Coarse load bucket used by signal-plane load samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoadBucket {
    Low,
    Medium,
    High,
    Saturated,
}

/// A peer reachability observation. This is a hint, not membership authority.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LivenessObservation {
    pub observer: MemberId,
    pub observed: MemberId,
    pub incarnation: u64,
    pub status: LivenessStatus,
}

/// Bounded health inputs that may influence local scoring or refresh timing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemberHealthInput {
    pub member: MemberId,
    pub error_count: u32,
    pub replication_lag_records: u64,
    pub read_only: bool,
    pub self_fenced: bool,
}

/// Coarse capacity and throughput sample for a known member.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoadMetricSample {
    pub member: MemberId,
    pub disk_pressure: LoadBucket,
    pub cpu_pressure: LoadBucket,
    pub range_hotness: LoadBucket,
    pub write_throughput_bucket: u16,
    pub read_throughput_bucket: u16,
}

/// Non-authoritative catalog and topology generation hint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CatalogVersionHint {
    pub member: MemberId,
    pub ownership_catalog_version: u64,
    pub topology_generation: u64,
    pub placement_generation: u64,
}

/// Routing-adjacent topology hint for an already-known member.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct TopologyHint {
    pub member: MemberId,
    pub endpoint: String,
    pub region: String,
    pub failure_domain: String,
}

/// Closed signal-plane vocabulary.
///
/// Deliberately absent: membership admission, ownership transitions, votes, and
/// bootstrap state. Adding any authority-bearing variant is a boundary change,
/// not an implementation detail.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignalPlaneMessage {
    LivenessObservation(LivenessObservation),
    MemberHealthInput(MemberHealthInput),
    LoadMetricSample(LoadMetricSample),
    CatalogVersionHint(CatalogVersionHint),
    TopologyHint(TopologyHint),
}

/// A signal delivered to a local consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedSignal {
    pub from: MemberId,
    pub to: MemberId,
    pub message: SignalPlaneMessage,
}

/// Narrow internal seam for signal dissemination.
///
/// Implementations receive authoritative membership as input and disseminate the
/// closed signal vocabulary. They do not expose join, ownership, voting, or
/// bootstrap APIs.
pub trait SignalPlane {
    /// Replace the admitted-member set this signal plane may talk to.
    fn set_members(&mut self, members: Vec<MemberId>);

    /// Publish a non-authoritative signal from an admitted member.
    fn publish(&mut self, from: MemberId, message: SignalPlaneMessage);

    /// Drain signals delivered to `member`.
    fn drain_received(&mut self, member: &MemberId) -> Vec<ReceivedSignal>;
}

/// Per-round bounds for signal-plane gossip traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalPlaneLimits {
    /// Maximum peers sampled by one member in a round.
    pub fanout: usize,
    /// Maximum closed-vocabulary messages carried in one frame.
    pub max_payload_messages: usize,
}

impl Default for SignalPlaneLimits {
    fn default() -> Self {
        Self {
            fanout: 3,
            max_payload_messages: 16,
        }
    }
}

/// Signal-plane traffic counters exported by the transport and engines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignalPlaneMetrics {
    pub sent_frames_total: u64,
    pub sent_messages_total: u64,
    pub received_frames_total: u64,
    pub received_messages_total: u64,
    pub rejected_frames_total: u64,
    pub dropped_frames_total: u64,
    pub payload_cap_hits_total: u64,
    pub fanout_cap: usize,
    pub payload_cap: usize,
}

/// One authenticated intra-cluster signal frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalFrame {
    pub authenticated_peer: MemberId,
    pub from: MemberId,
    pub to: MemberId,
    pub messages: Vec<SignalPlaneMessage>,
}

/// Rejection from the secured intra-cluster signal transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalTransportError {
    AuthenticatedPeerMismatch {
        authenticated_peer: MemberId,
        declared_sender: MemberId,
    },
    SenderNotAdmitted(MemberId),
    RecipientNotAdmitted(MemberId),
    PayloadTooLarge {
        actual: usize,
        max: usize,
    },
    EndpointUnavailable(MemberId),
}

/// Secured member-to-member signal transport for admitted cluster members.
#[derive(Debug)]
pub struct IntraClusterSignalBus {
    admitted: BTreeSet<MemberId>,
    endpoints: BTreeMap<MemberId, VecDeque<SignalFrame>>,
    limits: SignalPlaneLimits,
    metrics: SignalPlaneMetrics,
}

impl Default for IntraClusterSignalBus {
    fn default() -> Self {
        Self::new(SignalPlaneLimits::default())
    }
}

impl IntraClusterSignalBus {
    pub fn new(limits: SignalPlaneLimits) -> Self {
        let metrics = SignalPlaneMetrics {
            fanout_cap: limits.fanout,
            payload_cap: limits.max_payload_messages,
            ..Default::default()
        };
        Self {
            admitted: BTreeSet::new(),
            endpoints: BTreeMap::new(),
            limits,
            metrics,
        }
    }

    pub fn set_limits(&mut self, limits: SignalPlaneLimits) {
        self.limits = limits;
        self.metrics.fanout_cap = limits.fanout;
        self.metrics.payload_cap = limits.max_payload_messages;
    }

    pub fn set_admitted_members(&mut self, members: Vec<MemberId>) {
        self.admitted = members.into_iter().collect();
        self.endpoints
            .retain(|member, _| self.admitted.contains(member));
    }

    pub fn register_endpoint(&mut self, member: MemberId) {
        if self.admitted.is_empty() || self.admitted.contains(&member) {
            self.endpoints.entry(member).or_default();
        }
    }

    pub fn unregister_endpoint(&mut self, member: &MemberId) {
        self.endpoints.remove(member);
    }

    pub fn send(&mut self, frame: SignalFrame) -> Result<(), SignalTransportError> {
        if frame.authenticated_peer != frame.from {
            self.metrics.rejected_frames_total += 1;
            return Err(SignalTransportError::AuthenticatedPeerMismatch {
                authenticated_peer: frame.authenticated_peer,
                declared_sender: frame.from,
            });
        }
        if !self.admitted.contains(&frame.from) {
            self.metrics.rejected_frames_total += 1;
            return Err(SignalTransportError::SenderNotAdmitted(frame.from));
        }
        if !self.admitted.contains(&frame.to) {
            self.metrics.rejected_frames_total += 1;
            return Err(SignalTransportError::RecipientNotAdmitted(frame.to));
        }
        if frame.messages.len() > self.limits.max_payload_messages {
            self.metrics.rejected_frames_total += 1;
            self.metrics.payload_cap_hits_total += 1;
            return Err(SignalTransportError::PayloadTooLarge {
                actual: frame.messages.len(),
                max: self.limits.max_payload_messages,
            });
        }

        let message_count = frame.messages.len() as u64;
        let Some(endpoint) = self.endpoints.get_mut(&frame.to) else {
            self.metrics.dropped_frames_total += 1;
            return Err(SignalTransportError::EndpointUnavailable(frame.to));
        };
        endpoint.push_back(frame);
        self.metrics.sent_frames_total += 1;
        self.metrics.sent_messages_total += message_count;
        Ok(())
    }

    pub fn drain(&mut self, member: MemberId) -> Vec<SignalFrame> {
        let Some(endpoint) = self.endpoints.get_mut(&member) else {
            return Vec::new();
        };
        endpoint.drain(..).collect()
    }

    pub fn metrics(&self) -> SignalPlaneMetrics {
        self.metrics.clone()
    }
}

/// Cloneable handle for the secured intra-cluster signal transport.
#[derive(Debug, Clone, Default)]
pub struct SharedSignalTransport {
    bus: Arc<Mutex<IntraClusterSignalBus>>,
}

impl SharedSignalTransport {
    pub fn set_limits(&self, limits: SignalPlaneLimits) {
        self.bus
            .lock()
            .expect("signal transport mutex poisoned")
            .set_limits(limits);
    }

    pub fn set_admitted_members(&self, members: Vec<MemberId>) {
        self.bus
            .lock()
            .expect("signal transport mutex poisoned")
            .set_admitted_members(members);
    }

    pub fn register_endpoint(&self, member: MemberId) {
        self.bus
            .lock()
            .expect("signal transport mutex poisoned")
            .register_endpoint(member);
    }

    pub fn unregister_endpoint(&self, member: &MemberId) {
        self.bus
            .lock()
            .expect("signal transport mutex poisoned")
            .unregister_endpoint(member);
    }

    pub fn send(&self, frame: SignalFrame) -> Result<(), SignalTransportError> {
        self.bus
            .lock()
            .expect("signal transport mutex poisoned")
            .send(frame)
    }

    pub fn drain(&self, member: MemberId) -> Vec<SignalFrame> {
        self.bus
            .lock()
            .expect("signal transport mutex poisoned")
            .drain(member)
    }

    pub fn metrics(&self) -> SignalPlaneMetrics {
        self.bus
            .lock()
            .expect("signal transport mutex poisoned")
            .metrics()
    }
}

/// SWIM-style signal-plane engine over the secured intra-cluster transport.
#[derive(Debug)]
pub struct TransportSignalPlane {
    local_member: MemberId,
    transport: SharedSignalTransport,
    limits: SignalPlaneLimits,
    members: BTreeSet<MemberId>,
    known: BTreeSet<SignalPlaneMessage>,
    inbox: Vec<ReceivedSignal>,
    round: u64,
    metrics: SignalPlaneMetrics,
}

impl TransportSignalPlane {
    pub fn new(
        local_member: MemberId,
        transport: SharedSignalTransport,
        limits: SignalPlaneLimits,
    ) -> Self {
        transport.set_limits(limits);
        transport.register_endpoint(local_member.clone());
        let metrics = SignalPlaneMetrics {
            fanout_cap: limits.fanout,
            payload_cap: limits.max_payload_messages,
            ..Default::default()
        };
        Self {
            local_member,
            transport,
            limits,
            members: BTreeSet::new(),
            known: BTreeSet::new(),
            inbox: Vec::new(),
            round: 0,
            metrics,
        }
    }

    pub fn advance_round(&mut self) {
        self.receive_frames();
        self.send_round();
        self.round += 1;
    }

    pub fn knows(&self, message: &SignalPlaneMessage) -> bool {
        self.known.contains(message)
    }

    pub fn metrics(&self) -> SignalPlaneMetrics {
        self.metrics.clone()
    }

    fn receive_frames(&mut self) {
        let frames = self.transport.drain(self.local_member.clone());
        for frame in frames {
            if !self.members.contains(&frame.from) || !self.members.contains(&frame.to) {
                self.metrics.rejected_frames_total += 1;
                continue;
            }
            self.metrics.received_frames_total += 1;
            self.metrics.received_messages_total += frame.messages.len() as u64;
            for message in frame.messages {
                self.known.insert(message.clone());
                self.inbox.push(ReceivedSignal {
                    from: frame.from.clone(),
                    to: self.local_member.clone(),
                    message,
                });
            }
        }
    }

    fn send_round(&mut self) {
        if self.known.is_empty() {
            return;
        }

        let payload = self
            .known
            .iter()
            .take(self.limits.max_payload_messages)
            .cloned()
            .collect::<Vec<_>>();
        if self.known.len() > payload.len() {
            self.metrics.payload_cap_hits_total += 1;
        }

        for peer in self.sample_peers() {
            let message_count = payload.len() as u64;
            let frame = SignalFrame {
                authenticated_peer: self.local_member.clone(),
                from: self.local_member.clone(),
                to: peer,
                messages: payload.clone(),
            };
            match self.transport.send(frame) {
                Ok(()) => {
                    self.metrics.sent_frames_total += 1;
                    self.metrics.sent_messages_total += message_count;
                }
                Err(SignalTransportError::EndpointUnavailable(_)) => {
                    self.metrics.dropped_frames_total += 1;
                }
                Err(_) => {
                    self.metrics.rejected_frames_total += 1;
                }
            }
        }
    }

    fn sample_peers(&self) -> Vec<MemberId> {
        if self.limits.fanout == 0 {
            return Vec::new();
        }

        let peers = self
            .members
            .iter()
            .filter(|member| *member != &self.local_member)
            .cloned()
            .collect::<Vec<_>>();
        if peers.len() <= self.limits.fanout {
            return peers;
        }

        let start = (self.round + stable_member_hash(&self.local_member)) as usize % peers.len();
        (0..self.limits.fanout)
            .map(|offset| peers[(start + offset) % peers.len()].clone())
            .collect()
    }
}

impl SignalPlane for TransportSignalPlane {
    fn set_members(&mut self, members: Vec<MemberId>) {
        self.members = members.iter().cloned().collect();
        self.transport.set_admitted_members(members);
        if self.members.contains(&self.local_member) {
            self.transport.register_endpoint(self.local_member.clone());
        }
    }

    fn publish(&mut self, from: MemberId, message: SignalPlaneMessage) {
        if from == self.local_member && self.members.contains(&from) {
            self.known.insert(message);
        } else {
            self.metrics.rejected_frames_total += 1;
        }
    }

    fn drain_received(&mut self, member: &MemberId) -> Vec<ReceivedSignal> {
        if member == &self.local_member {
            self.receive_frames();
            std::mem::take(&mut self.inbox)
        } else {
            Vec::new()
        }
    }
}

fn stable_member_hash(member: &MemberId) -> u64 {
    member.bytes().fold(0_u64, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u64::from(byte))
    })
}

/// Deterministic fault schedule for the in-process simulated-peer engine.
#[derive(Debug, Clone, Default)]
pub struct SignalPlaneSchedule {
    dropped: BTreeSet<ScheduledLink>,
    delays: BTreeMap<ScheduledLink, u64>,
    partitions: Vec<Partition>,
}

impl SignalPlaneSchedule {
    /// Drop all sends on one directed link during `round`.
    pub fn drop_delivery(&mut self, round: u64, from: MemberId, to: MemberId) {
        self.dropped.insert(ScheduledLink { round, from, to });
    }

    /// Add `extra_rounds` of delay to one directed link during `round`.
    pub fn delay_delivery(&mut self, round: u64, from: MemberId, to: MemberId, extra_rounds: u64) {
        self.delays
            .insert(ScheduledLink { round, from, to }, extra_rounds);
    }

    /// Partition two members from each other for an inclusive round range.
    pub fn partition_between(
        &mut self,
        start_round: u64,
        end_round: u64,
        a: MemberId,
        b: MemberId,
    ) {
        self.partitions.push(Partition {
            start_round,
            end_round,
            a,
            b,
        });
    }

    fn is_dropped(&self, round: u64, from: &MemberId, to: &MemberId) -> bool {
        self.dropped.contains(&ScheduledLink {
            round,
            from: from.clone(),
            to: to.clone(),
        })
    }

    fn delay(&self, round: u64, from: &MemberId, to: &MemberId) -> u64 {
        self.delays
            .get(&ScheduledLink {
                round,
                from: from.clone(),
                to: to.clone(),
            })
            .copied()
            .unwrap_or(0)
    }

    fn is_partitioned(&self, round: u64, from: &MemberId, to: &MemberId) -> bool {
        self.partitions
            .iter()
            .any(|partition| partition.contains(round) && partition.matches_members(from, to))
    }
}

/// Deterministic in-process signal plane for seam tests and future consumers.
#[derive(Debug, Default)]
pub struct SimulatedSignalPlane {
    members: BTreeMap<MemberId, SimulatedPeer>,
    pending: Vec<PendingSignal>,
    schedule: SignalPlaneSchedule,
    round: u64,
}

impl SimulatedSignalPlane {
    pub fn new(members: Vec<MemberId>) -> Self {
        let mut plane = Self::default();
        plane.set_members(members);
        plane
    }

    pub fn schedule_mut(&mut self) -> &mut SignalPlaneSchedule {
        &mut self.schedule
    }

    pub fn round(&self) -> u64 {
        self.round
    }

    /// Advance one deterministic gossip round.
    pub fn advance_round(&mut self) {
        self.queue_known_signals();
        self.deliver_due_signals();
        self.round += 1;
    }

    /// Members whose local simulated peer has learned `message`.
    pub fn members_with_signal(&self, message: &SignalPlaneMessage) -> Vec<MemberId> {
        self.members
            .iter()
            .filter(|(_, peer)| peer.known.contains(message))
            .map(|(member, _)| member.clone())
            .collect()
    }

    fn queue_known_signals(&mut self) {
        let members = self.members.keys().cloned().collect::<Vec<_>>();
        let mut queued = Vec::new();

        for from in &members {
            let Some(peer) = self.members.get(from) else {
                continue;
            };
            for to in &members {
                if from == to {
                    continue;
                }
                if self.schedule.is_partitioned(self.round, from, to)
                    || self.schedule.is_dropped(self.round, from, to)
                {
                    continue;
                }
                let deliver_at = self.round + self.schedule.delay(self.round, from, to);
                for message in &peer.known {
                    queued.push(PendingSignal {
                        deliver_at,
                        signal: ReceivedSignal {
                            from: from.clone(),
                            to: to.clone(),
                            message: message.clone(),
                        },
                    });
                }
            }
        }

        self.pending.extend(queued);
    }

    fn deliver_due_signals(&mut self) {
        let mut pending = Vec::new();
        for signal in self.pending.drain(..) {
            if signal.deliver_at <= self.round {
                if let Some(peer) = self.members.get_mut(&signal.signal.to) {
                    peer.known.insert(signal.signal.message.clone());
                    peer.inbox.push(signal.signal);
                }
            } else {
                pending.push(signal);
            }
        }
        self.pending = pending;
    }
}

impl SignalPlane for SimulatedSignalPlane {
    fn set_members(&mut self, members: Vec<MemberId>) {
        let admitted = members.into_iter().collect::<BTreeSet<_>>();
        self.members.retain(|member, _| admitted.contains(member));
        for member in admitted {
            self.members.entry(member).or_default();
        }
    }

    fn publish(&mut self, from: MemberId, message: SignalPlaneMessage) {
        if let Some(peer) = self.members.get_mut(&from) {
            peer.known.insert(message);
        }
    }

    fn drain_received(&mut self, member: &MemberId) -> Vec<ReceivedSignal> {
        self.members
            .get_mut(member)
            .map(|peer| std::mem::take(&mut peer.inbox))
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScheduledLink {
    round: u64,
    from: MemberId,
    to: MemberId,
}

#[derive(Debug, Clone)]
struct Partition {
    start_round: u64,
    end_round: u64,
    a: MemberId,
    b: MemberId,
}

impl Partition {
    fn contains(&self, round: u64) -> bool {
        self.start_round <= round && round <= self.end_round
    }

    fn matches_members(&self, from: &MemberId, to: &MemberId) -> bool {
        (&self.a == from && &self.b == to) || (&self.a == to && &self.b == from)
    }
}

#[derive(Debug, Default)]
struct SimulatedPeer {
    known: BTreeSet<SignalPlaneMessage>,
    inbox: Vec<ReceivedSignal>,
}

#[derive(Debug, Clone)]
struct PendingSignal {
    deliver_at: u64,
    signal: ReceivedSignal,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str) -> MemberId {
        id.to_string()
    }

    fn liveness(observer: &str, observed: &str) -> SignalPlaneMessage {
        SignalPlaneMessage::LivenessObservation(LivenessObservation {
            observer: member(observer),
            observed: member(observed),
            incarnation: 1,
            status: LivenessStatus::Alive,
        })
    }

    #[test]
    fn message_set_is_closed_over_signal_plane_only() {
        let messages = [
            SignalPlaneMessage::LivenessObservation(LivenessObservation {
                observer: member("a"),
                observed: member("b"),
                incarnation: 1,
                status: LivenessStatus::Alive,
            }),
            SignalPlaneMessage::MemberHealthInput(MemberHealthInput {
                member: member("a"),
                error_count: 0,
                replication_lag_records: 0,
                read_only: false,
                self_fenced: false,
            }),
            SignalPlaneMessage::LoadMetricSample(LoadMetricSample {
                member: member("a"),
                disk_pressure: LoadBucket::Low,
                cpu_pressure: LoadBucket::Medium,
                range_hotness: LoadBucket::High,
                write_throughput_bucket: 2,
                read_throughput_bucket: 3,
            }),
            SignalPlaneMessage::CatalogVersionHint(CatalogVersionHint {
                member: member("a"),
                ownership_catalog_version: 7,
                topology_generation: 8,
                placement_generation: 9,
            }),
            SignalPlaneMessage::TopologyHint(TopologyHint {
                member: member("a"),
                endpoint: "redb://a".to_string(),
                region: "local".to_string(),
                failure_domain: "rack-1".to_string(),
            }),
        ];

        for message in messages {
            match message {
                SignalPlaneMessage::LivenessObservation(_)
                | SignalPlaneMessage::MemberHealthInput(_)
                | SignalPlaneMessage::LoadMetricSample(_)
                | SignalPlaneMessage::CatalogVersionHint(_)
                | SignalPlaneMessage::TopologyHint(_) => {}
            }
        }
    }

    struct FakeSignalPlane {
        members: Vec<MemberId>,
        inbox: Vec<ReceivedSignal>,
    }

    impl SignalPlane for FakeSignalPlane {
        fn set_members(&mut self, members: Vec<MemberId>) {
            self.members = members;
        }

        fn publish(&mut self, from: MemberId, message: SignalPlaneMessage) {
            for member in &self.members {
                if member != &from {
                    self.inbox.push(ReceivedSignal {
                        from: from.clone(),
                        to: member.clone(),
                        message: message.clone(),
                    });
                }
            }
        }

        fn drain_received(&mut self, member: &MemberId) -> Vec<ReceivedSignal> {
            let mut drained = Vec::new();
            self.inbox.retain(|signal| {
                if &signal.to == member {
                    drained.push(signal.clone());
                    false
                } else {
                    true
                }
            });
            drained
        }
    }

    #[test]
    fn fake_signal_plane_exercises_the_narrow_trait() {
        let mut plane = FakeSignalPlane {
            members: Vec::new(),
            inbox: Vec::new(),
        };
        plane.set_members(vec![member("a"), member("b")]);

        plane.publish(member("a"), liveness("a", "b"));

        assert!(plane.drain_received(&member("a")).is_empty());
        assert_eq!(plane.drain_received(&member("b")).len(), 1);
    }

    #[test]
    fn simulated_peers_converge_with_loss_delay_and_temporary_partition() {
        let mut plane =
            SimulatedSignalPlane::new(vec![member("a"), member("b"), member("c"), member("d")]);
        plane
            .schedule_mut()
            .drop_delivery(0, member("a"), member("c"));
        plane
            .schedule_mut()
            .delay_delivery(1, member("b"), member("d"), 1);
        plane
            .schedule_mut()
            .partition_between(0, 1, member("a"), member("d"));

        let observation = liveness("a", "b");
        plane.publish(member("a"), observation.clone());

        let bound = 6;
        for _ in 0..bound {
            if plane.members_with_signal(&observation).len() == 4 {
                break;
            }
            plane.advance_round();
        }

        assert_eq!(plane.members_with_signal(&observation).len(), 4);
        assert!(
            plane.round() <= bound,
            "liveness observation did not converge within {bound} rounds"
        );
    }

    #[test]
    fn simulated_partition_limits_convergence_to_connected_members() {
        let mut plane =
            SimulatedSignalPlane::new(vec![member("a"), member("b"), member("c"), member("d")]);
        plane
            .schedule_mut()
            .partition_between(0, 10, member("c"), member("d"));
        plane
            .schedule_mut()
            .partition_between(0, 10, member("a"), member("d"));
        plane
            .schedule_mut()
            .partition_between(0, 10, member("b"), member("d"));

        let observation = liveness("a", "b");
        plane.publish(member("a"), observation.clone());

        for _ in 0..4 {
            plane.advance_round();
        }

        assert_eq!(
            plane.members_with_signal(&observation),
            vec![member("a"), member("b"), member("c")]
        );
    }

    #[test]
    fn secured_transport_rejects_signal_from_unadmitted_peer_before_delivery() {
        let mut bus = IntraClusterSignalBus::new(SignalPlaneLimits::default());
        bus.set_admitted_members(vec![member("a"), member("b")]);
        bus.register_endpoint(member("a"));
        bus.register_endpoint(member("b"));

        let err = bus
            .send(SignalFrame {
                authenticated_peer: member("stranger"),
                from: member("stranger"),
                to: member("a"),
                messages: vec![liveness("stranger", "a")],
            })
            .expect_err("unadmitted sender must be rejected");

        assert_eq!(
            err,
            SignalTransportError::SenderNotAdmitted(member("stranger"))
        );
        assert_eq!(bus.drain(member("a")).len(), 0);
        assert_eq!(bus.metrics().rejected_frames_total, 1);
    }

    #[test]
    fn three_node_gossip_converges_over_secured_transport_with_one_member_down() {
        let transport = SharedSignalTransport::default();
        let limits = SignalPlaneLimits {
            fanout: 1,
            max_payload_messages: 2,
        };
        let members = vec![member("a"), member("b"), member("c")];

        let mut node_a = TransportSignalPlane::new(member("a"), transport.clone(), limits);
        let mut node_b = TransportSignalPlane::new(member("b"), transport.clone(), limits);
        let mut node_c = TransportSignalPlane::new(member("c"), transport.clone(), limits);

        node_a.set_members(members.clone());
        node_b.set_members(members.clone());
        node_c.set_members(members);
        transport.unregister_endpoint(&member("c"));

        let observation = SignalPlaneMessage::LivenessObservation(LivenessObservation {
            observer: member("a"),
            observed: member("c"),
            incarnation: 2,
            status: LivenessStatus::Unreachable,
        });
        node_a.publish(member("a"), observation.clone());

        let bound = 6;
        for _ in 0..bound {
            node_a.advance_round();
            node_b.advance_round();
            node_c.advance_round();
            if node_a.knows(&observation) && node_b.knows(&observation) {
                break;
            }
        }

        assert!(node_a.knows(&observation));
        assert!(node_b.knows(&observation));
        assert!(!node_c.knows(&observation));

        let metrics = node_a.metrics();
        assert!(metrics.sent_frames_total > 0);
        assert!(metrics.sent_messages_total > 0);
        assert!(metrics.fanout_cap >= 1);
        assert!(metrics.payload_cap >= 2);
    }
}
