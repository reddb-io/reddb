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
//!     .with_replication(ReplicationConfig::replica("http://primary:50051"));
//! ```

pub mod bookmark;
pub mod cdc;
pub mod commit_policy;
pub mod commit_waiter;
pub mod election;
pub mod failover;
pub mod flow_control;
pub mod lease;
pub mod logical;
pub mod primary;
pub mod quorum;
pub mod replica;
pub mod rollback;
pub mod scheduler;
pub mod swap_db;
pub mod topology_advertiser;

pub use bookmark::{BookmarkDecodeError, CausalBookmark};
pub use commit_policy::CommitPolicy;
pub use commit_waiter::{AwaitOutcome, CommitWaiter};
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

pub const DEFAULT_REPLICATION_TERM: u64 = 1;
pub const DEFAULT_SLOT_RETENTION_MAX_LAG_LSN: u64 = 100_000;
pub const DEFAULT_SLOT_IDLE_TIMEOUT_MS: u64 = 86_400_000;

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
        /// gRPC address of the primary (e.g., "http://primary:50051")
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
}
