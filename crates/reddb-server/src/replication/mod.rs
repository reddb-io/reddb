//! Replication Module
//!
//! Implements single-primary, multi-replica replication via WAL streaming.
//!
//! # Architecture
//!
//! - Primary: accepts writes and streams WAL records to replicas
//! - Replica: read-only, connects to primary for WAL streaming
//! - Initial sync via snapshot transfer, then incremental WAL
//!
//! # Usage
//!
//! ```ignore
//! // Primary
//! let options = RedDBOptions::persistent("./primary-data")
//!     .with_replication(ReplicationConfig::primary());
//!
//! // Replica
//! let options = RedDBOptions::persistent("./replica-data")
//!     .with_replication(ReplicationConfig::replica("http://primary:55055"));
//! ```

pub mod bookmark;
pub mod cascade;
pub mod cdc;
pub mod commit_policy;
pub mod commit_waiter;
pub mod control_plane;
pub mod dst;
pub mod election;
pub mod failover;
pub mod fence;
pub mod flow_control;
pub mod lease;
pub mod logical;
pub mod primary;
pub mod quorum;
pub mod reconnect;
pub mod replica;
pub mod rollback;
pub mod scheduler;
pub mod swap_db;
pub mod topology_advertiser;
pub mod witness;

pub use bookmark::{BookmarkDecodeError, CausalBookmark};
pub use cascade::{
    plan_upstream, CascadeRefusal, CascadeRelay, CascadeUpstream, DownstreamSlot, ReplicaClass,
    UpstreamChoice,
};
pub use commit_policy::CommitPolicy;
pub use commit_waiter::{AwaitOutcome, CommitWaiter};
pub use control_plane::{
    ControlPlaneConsensus, ControlPlaneEntry, ControlPlaneEntryKind, ControlPlaneLogIndex,
    ControlPlanePayload, ControlPlaneRole, MemberId, ProposeRefusal,
};
pub use election::{
    quorum_threshold, randomized_election_timeout, ElectionCoordinator, ElectionOutcome,
    ElectionRequest, ElectionTransport, FileLastVoteStore, LastVote, LastVoteError, LastVoteStore,
    Member, MemberKind, MemoryLastVoteStore, RefusalReason, VoteDecision, VoteRequest, Voter,
    VotingState,
};
pub use failover::{
    FailoverCoordinator, FailoverError, FailoverMode, FailoverNode, FailoverOutcome,
    FailoverRequest, FailoverTransport, NodeRole, RoleAssignment,
};
pub use fence::{
    term_is_stale, FenceBoundary, FenceVerdict, FileTermStore, MemoryTermStore, StaleTermFenced,
    StaleTermRejection, StreamHandshake, TermFence, TermStore, TermStoreError,
};
pub use flow_control::{Admission, FlowController};
pub use lease::{LeaseError, LeaseStore, WriterLease};
pub use quorum::{QuorumConfig, QuorumCoordinator, QuorumError};
pub use rollback::{
    DivergentTail, RollbackCoordinator, RollbackError, RollbackEvent, RollbackOutcome,
    RollbackPlan, RollbackRequest, RollbackTransport, TailRecord,
};
pub use swap_db::{RebootstrapInProgress, SwapDb};
pub use topology_advertiser::{
    LagConfig, TopologyAdvertiser, TopologyAuthGate, DEFAULT_REPLICA_TIMEOUT_MS,
    TOPOLOGY_READ_CAPABILITY,
};
pub use witness::{RuntimeProfile, WitnessSupervisor};

pub const DEFAULT_REPLICATION_TERM: u64 = reddb_wire::replication::DEFAULT_REPLICATION_TERM;
pub const DEFAULT_SLOT_RETENTION_MAX_LAG_LSN: u64 = 100_000;
pub const DEFAULT_SLOT_IDLE_TIMEOUT_MS: u64 = 86_400_000;
pub const SHIPPED_FAILOVER_PROFILES: [FailoverProfile; 3] = [
    FailoverProfile::CONSERVATIVE,
    FailoverProfile::BALANCED,
    FailoverProfile::AGGRESSIVE,
];

/// Named failover timing posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailoverProfileName {
    Conservative,
    Balanced,
    Aggressive,
    Custom,
}

impl FailoverProfileName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Conservative => "conservative",
            Self::Balanced => "balanced",
            Self::Aggressive => "aggressive",
            Self::Custom => "custom",
        }
    }
}

/// Tuned failover constants over the existing lease, health, and grace
/// mechanisms.
///
/// Shipped profiles document the intended trade-off:
/// - `conservative`: long lease and high health threshold; favors avoiding
///   false promotion over fast recovery.
/// - `balanced`: default posture; keeps a moderate lease and health threshold
///   for ordinary multi-node deployments.
/// - `aggressive`: short lease and lower health threshold; favors fast recovery
///   when the operator accepts a narrower timing margin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FailoverProfile {
    pub name: FailoverProfileName,
    pub lease_window_ms: u64,
    pub member_health_score_threshold: u8,
    pub promotion_grace_ms: u64,
    pub max_clock_drift_ms: u64,
}

impl FailoverProfile {
    pub const CONSERVATIVE: Self = Self {
        name: FailoverProfileName::Conservative,
        lease_window_ms: 60_000,
        member_health_score_threshold: 90,
        promotion_grace_ms: 30_000,
        max_clock_drift_ms: 5_000,
    };

    pub const BALANCED: Self = Self {
        name: FailoverProfileName::Balanced,
        lease_window_ms: 30_000,
        member_health_score_threshold: 75,
        promotion_grace_ms: 10_000,
        max_clock_drift_ms: 5_000,
    };

    pub const AGGRESSIVE: Self = Self {
        name: FailoverProfileName::Aggressive,
        lease_window_ms: 15_000,
        member_health_score_threshold: 60,
        promotion_grace_ms: 5_000,
        max_clock_drift_ms: 2_000,
    };

    pub const fn shipped() -> &'static [Self] {
        &SHIPPED_FAILOVER_PROFILES
    }

    pub const fn name_str(self) -> &'static str {
        self.name.as_str()
    }

    pub fn validate(self) -> Result<Self, String> {
        if self.lease_window_ms == 0 {
            return Err("failover profile lease_window_ms must be positive".to_string());
        }
        if self.member_health_score_threshold > 100 {
            return Err(
                "failover profile member_health_score_threshold must be <= 100".to_string(),
            );
        }
        if self.promotion_grace_ms >= self.lease_window_ms {
            return Err(
                "failover profile promotion_grace_ms must be smaller than lease_window_ms"
                    .to_string(),
            );
        }
        let safety_margin_ms = self.lease_window_ms - self.promotion_grace_ms;
        if safety_margin_ms < self.max_clock_drift_ms {
            return Err(format!(
                "failover profile lease safety margin {safety_margin_ms}ms is below configured max clock drift {}ms",
                self.max_clock_drift_ms
            ));
        }
        Ok(self)
    }

    pub const fn lease_safety_margin_ms(self) -> u64 {
        self.lease_window_ms - self.promotion_grace_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shipped_failover_profiles_satisfy_lease_safety_validation() {
        for profile in FailoverProfile::shipped() {
            profile.validate().expect(profile.name_str());
            assert!(
                profile.lease_safety_margin_ms() >= profile.max_clock_drift_ms,
                "{} profile violates lease clock-discipline margin",
                profile.name_str()
            );
        }
    }

    #[test]
    fn failover_profile_rejects_margin_below_configured_clock_drift() {
        let invalid = FailoverProfile {
            name: FailoverProfileName::Custom,
            lease_window_ms: 1_000,
            member_health_score_threshold: 80,
            promotion_grace_ms: 900,
            max_clock_drift_ms: 200,
        };

        let err = invalid.validate().expect_err("profile must be rejected");

        assert!(err.contains("lease safety margin"), "{err}");
        assert!(err.contains("max clock drift"), "{err}");
    }

    #[test]
    fn runtime_open_rejects_invalid_failover_profile() {
        let invalid = FailoverProfile {
            name: FailoverProfileName::Custom,
            lease_window_ms: 1_000,
            member_health_score_threshold: 80,
            promotion_grace_ms: 900,
            max_clock_drift_ms: 200,
        };
        let options = crate::RedDBOptions::in_memory()
            .with_replication(ReplicationConfig::primary().with_failover_profile(invalid));

        let err = match crate::RedDBRuntime::with_options(options) {
            Ok(_) => panic!("boot must reject config"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("invalid config"), "{err}");
        assert!(err.to_string().contains("lease safety margin"), "{err}");
    }

    #[test]
    fn changing_failover_profile_updates_active_profile() {
        let mut config = ReplicationConfig::primary();

        config
            .change_failover_profile(FailoverProfile::AGGRESSIVE, "test")
            .expect("valid profile applies");

        assert_eq!(
            config.failover_profile.name,
            FailoverProfileName::Aggressive
        );
    }
}

/// Role of this RedDB instance in a replication cluster.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ReplicationRole {
    /// Standalone instance (default, no replication).
    #[default]
    Standalone,
    /// Primary: accepts reads and writes, streams WAL to replicas.
    Primary,
    /// Replica: read-only, receives WAL from primary.
    Replica {
        /// gRPC address of the primary (e.g., "http://primary:55055")
        primary_addr: String,
    },
}

/// Configuration for replication.
#[derive(Debug, Clone)]
pub struct ReplicationConfig {
    pub role: ReplicationRole,
    /// Current replication term/epoch stamped into WAL-derived records.
    pub term: u64,
    /// How often replica polls for new WAL records (milliseconds).
    pub poll_interval_ms: u64,
    /// Maximum batch size for WAL record transfer.
    pub max_batch_size: usize,
    /// Region identifier for this instance (Phase 2.6 multi-region).
    ///
    /// Used by the quorum coordinator to spread write acks across
    /// fault domains: `QuorumConfig::required_regions` forces a commit
    /// to wait until at least one replica in each listed region has
    /// acked. Defaults to `"local"` for single-region deployments.
    pub region: String,
    /// Quorum configuration (Phase 2.6 multi-region).
    pub quorum: QuorumConfig,
    /// Maximum LSN lag a replication slot may pin before the primary
    /// invalidates it and allows WAL pruning to continue.
    pub slot_retention_max_lag_lsn: u64,
    /// Maximum wall-clock idle age for a slot before invalidation.
    pub slot_idle_timeout_ms: u64,
    /// Streaming class (issue #838). A [`ReplicaClass::Voting`] node is on the
    /// durability/election path and always streams directly from the primary;
    /// a [`ReplicaClass::AsyncReadReplica`] may cascade from an intermediate.
    /// Defaults to `Voting` — a node only cascades when explicitly declared a
    /// read-replica.
    pub replica_class: ReplicaClass,
    /// Optional intermediate replica to cascade from (issue #838). Honoured
    /// only for an async read-replica; a voting member refuses it and streams
    /// directly from the primary. See [`ReplicationConfig::resolved_upstream`].
    pub cascade_from: Option<CascadeUpstream>,
    /// Active named failover profile. The profile is a bundle of tuned
    /// lease/health/grace constants; it does not add new failover machinery.
    pub failover_profile: FailoverProfile,
}

impl ReplicationConfig {
    pub fn standalone() -> Self {
        Self {
            role: ReplicationRole::Standalone,
            term: DEFAULT_REPLICATION_TERM,
            poll_interval_ms: 100,
            max_batch_size: 1000,
            region: "local".to_string(),
            quorum: QuorumConfig::async_commit(),
            slot_retention_max_lag_lsn: DEFAULT_SLOT_RETENTION_MAX_LAG_LSN,
            slot_idle_timeout_ms: DEFAULT_SLOT_IDLE_TIMEOUT_MS,
            replica_class: ReplicaClass::Voting,
            cascade_from: None,
            failover_profile: FailoverProfile::BALANCED,
        }
    }

    pub fn primary() -> Self {
        Self {
            role: ReplicationRole::Primary,
            term: DEFAULT_REPLICATION_TERM,
            poll_interval_ms: 100,
            max_batch_size: 1000,
            region: "local".to_string(),
            quorum: QuorumConfig::async_commit(),
            slot_retention_max_lag_lsn: DEFAULT_SLOT_RETENTION_MAX_LAG_LSN,
            slot_idle_timeout_ms: DEFAULT_SLOT_IDLE_TIMEOUT_MS,
            replica_class: ReplicaClass::Voting,
            cascade_from: None,
            failover_profile: FailoverProfile::BALANCED,
        }
    }

    pub fn replica(primary_addr: impl Into<String>) -> Self {
        Self {
            role: ReplicationRole::Replica {
                primary_addr: primary_addr.into(),
            },
            term: DEFAULT_REPLICATION_TERM,
            poll_interval_ms: 100,
            max_batch_size: 1000,
            region: "local".to_string(),
            quorum: QuorumConfig::async_commit(),
            slot_retention_max_lag_lsn: DEFAULT_SLOT_RETENTION_MAX_LAG_LSN,
            slot_idle_timeout_ms: DEFAULT_SLOT_IDLE_TIMEOUT_MS,
            replica_class: ReplicaClass::Voting,
            cascade_from: None,
            failover_profile: FailoverProfile::BALANCED,
        }
    }

    /// Attach a quorum configuration (fluent setter).
    pub fn with_quorum(mut self, quorum: QuorumConfig) -> Self {
        self.quorum = quorum;
        self
    }

    /// Set the region identifier (fluent setter).
    pub fn with_region(mut self, region: impl Into<String>) -> Self {
        self.region = region.into();
        self
    }

    /// Set the replication term stamped into newly produced records.
    pub fn with_term(mut self, term: u64) -> Self {
        self.term = term;
        self
    }

    pub fn with_slot_retention_max_lag_lsn(mut self, max_lag_lsn: u64) -> Self {
        self.slot_retention_max_lag_lsn = max_lag_lsn;
        self
    }

    pub fn with_slot_idle_timeout_ms(mut self, timeout_ms: u64) -> Self {
        self.slot_idle_timeout_ms = timeout_ms;
        self
    }

    pub fn with_failover_profile(mut self, profile: FailoverProfile) -> Self {
        self.failover_profile = profile;
        self
    }

    pub fn validate_failover_profile(&self) -> Result<(), String> {
        self.failover_profile.validate().map(|_| ())
    }

    pub fn change_failover_profile(
        &mut self,
        profile: FailoverProfile,
        changed_by: impl Into<String>,
    ) -> Result<(), String> {
        let profile = profile.validate()?;
        let old_profile = self.failover_profile.name_str().to_string();
        self.failover_profile = profile;
        crate::telemetry::operator_event::OperatorEvent::FailoverProfileChanged {
            old_profile,
            new_profile: profile.name_str().to_string(),
            changed_by: changed_by.into(),
        }
        .emit_global();
        Ok(())
    }

    /// Set the streaming class explicitly (issue #838).
    pub fn with_replica_class(mut self, class: ReplicaClass) -> Self {
        self.replica_class = class;
        self
    }

    /// Declare this node an async read-replica that cascades from `intermediate`
    /// (issue #838). Sets [`ReplicaClass::AsyncReadReplica`] and the cascade
    /// source together — the only combination that actually streams from an
    /// intermediate. A node left at the default `Voting` class ignores any
    /// cascade source and streams directly from the primary.
    pub fn cascading_from(mut self, node_id: impl Into<String>, addr: impl Into<String>) -> Self {
        self.replica_class = ReplicaClass::AsyncReadReplica;
        self.cascade_from = Some(CascadeUpstream::new(node_id, addr));
        self
    }

    /// Resolve where this node should open its WAL stream, applying the
    /// cascade policy (issue #838). A voting member always resolves to the
    /// primary even when a cascade source is configured; the returned
    /// [`CascadeRefusal`] explains any fallback so it is observable rather
    /// than silent. `self_node_id` guards against a node cascading from its
    /// own slot.
    pub fn resolved_upstream(
        &self,
        self_node_id: &str,
    ) -> (UpstreamChoice, Option<CascadeRefusal>) {
        plan_upstream(self_node_id, self.replica_class, self.cascade_from.as_ref())
    }
}
