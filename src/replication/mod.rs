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

pub mod cdc;
pub mod commit_policy;
pub mod commit_waiter;
pub mod lease;
pub mod logical;
pub mod primary;
pub mod quorum;
pub mod replica;
pub mod scheduler;

pub use commit_policy::CommitPolicy;
pub use commit_waiter::{AwaitOutcome, CommitWaiter};
pub use lease::{LeaseError, LeaseStore, WriterLease};
pub use quorum::{QuorumConfig, QuorumCoordinator, QuorumError};

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
}

impl ReplicationConfig {
    pub fn standalone() -> Self {
        Self {
            role: ReplicationRole::Standalone,
            poll_interval_ms: 100,
            max_batch_size: 1000,
            region: "local".to_string(),
            quorum: QuorumConfig::async_commit(),
        }
    }

    pub fn primary() -> Self {
        Self {
            role: ReplicationRole::Primary,
            poll_interval_ms: 100,
            max_batch_size: 1000,
            region: "local".to_string(),
            quorum: QuorumConfig::async_commit(),
        }
    }

    pub fn replica(primary_addr: impl Into<String>) -> Self {
        Self {
            role: ReplicationRole::Replica {
                primary_addr: primary_addr.into(),
            },
            poll_interval_ms: 100,
            max_batch_size: 1000,
            region: "local".to_string(),
            quorum: QuorumConfig::async_commit(),
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
}
