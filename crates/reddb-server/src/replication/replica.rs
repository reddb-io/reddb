//! Replica-side replication: connects to primary, consumes WAL records.

use std::time::Duration;

/// Replica replication state.
pub struct ReplicaReplication {
    pub primary_addr: String,
    pub last_applied_lsn: u64,
    pub poll_interval: Duration,
    pub connected: bool,
}

impl ReplicaReplication {
    pub fn new(primary_addr: String, poll_interval_ms: u64) -> Self {
        Self {
            primary_addr,
            last_applied_lsn: 0,
            poll_interval: Duration::from_millis(poll_interval_ms),
            connected: false,
        }
    }
}
