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
pub mod primary;
pub mod replica;
pub mod scheduler;

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
}

impl ReplicationConfig {
    pub fn standalone() -> Self {
        Self {
            role: ReplicationRole::Standalone,
            poll_interval_ms: 100,
            max_batch_size: 1000,
        }
    }

    pub fn primary() -> Self {
        Self {
            role: ReplicationRole::Primary,
            poll_interval_ms: 100,
            max_batch_size: 1000,
        }
    }

    pub fn replica(primary_addr: impl Into<String>) -> Self {
        Self {
            role: ReplicationRole::Replica {
                primary_addr: primary_addr.into(),
            },
            poll_interval_ms: 100,
            max_batch_size: 1000,
        }
    }
}
