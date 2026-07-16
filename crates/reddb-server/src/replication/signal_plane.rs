//! Signal-plane dissemination seam (issue #1842, parent #1832, ADR 0073).
//!
//! This module names the non-authoritative peer-to-peer signal layer used by
//! admitted cluster members to share routing and health hints. The vocabulary is
//! closed over observations only: liveness, health inputs, load samples,
//! catalog-version hints, and topology hints. There is no membership admission,
//! ownership transition, vote, or bootstrap-state variant, so those authority
//! facts cannot be represented by this seam.

use std::collections::{BTreeMap, BTreeSet};

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
}
