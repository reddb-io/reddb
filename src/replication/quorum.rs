//! Quorum-based commit coordination (Phase 2.6 multi-region PG parity).
//!
//! The existing `PrimaryReplication` module streams WAL records to every
//! connected replica but the primary acks the client as soon as the
//! record hits its own WAL — replicas are eventually-consistent. For
//! multi-region deployments that's not enough: a datacenter failure
//! after ack but before replication would drop the write.
//!
//! `QuorumCoordinator` sits between the write path and the client ack.
//! It watches `ReplicaState::last_acked_lsn` on the underlying primary
//! and blocks the caller until the configured quorum of replicas has
//! durably received the record. Three quorum shapes are supported:
//!
//! * **Async** (default, backwards compatible) — ack immediately, don't
//!   wait for replicas. Same semantics as pre-Phase-2.6 RedDB.
//! * **Sync(n)** — wait for N replicas (any region) before acking.
//! * **Regions(set)** — wait for at least one replica from each listed
//!   region. Survives full-region loss as long as the surviving regions
//!   were in the required set at write time.
//!
//! Crash safety: the primary WAL is already durable before quorum wait
//! begins, so a coordinator crash doesn't lose the record — it just
//! means the client never got an ack and must retry idempotently.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::primary::PrimaryReplication;

/// Quorum mode selected for a replication config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuorumMode {
    /// Ack the client immediately, propagate asynchronously.
    /// Loses writes if the primary dies before replicas catch up.
    Async,
    /// Wait for `n` replicas (any region) to ack before returning.
    /// Tolerates `replicas - n` losses.
    Sync { min_replicas: usize },
    /// Wait for at least one replica from *each* listed region.
    /// Survives full-region loss as long as the remaining regions were
    /// in the required set and have acknowledged the write.
    Regions { required: HashSet<String> },
}

/// Quorum configuration stored alongside `ReplicationConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumConfig {
    pub mode: QuorumMode,
    /// How long the coordinator waits for acks before giving up.
    /// `None` = wait forever (use for strong consistency only when
    /// you can tolerate the writer stalling on a partitioned region).
    pub timeout: Option<Duration>,
}

impl QuorumConfig {
    /// Ack immediately. Loss tolerance = 0 under primary failure.
    pub fn async_commit() -> Self {
        Self {
            mode: QuorumMode::Async,
            timeout: None,
        }
    }

    /// Wait for `n` replicas to ack (any region). Typical PG-like
    /// "synchronous_commit = on, synchronous_standby_names = 'ANY n'".
    pub fn sync(min_replicas: usize) -> Self {
        Self {
            mode: QuorumMode::Sync { min_replicas },
            timeout: Some(Duration::from_secs(5)),
        }
    }

    /// Wait for at least one replica from each region in the set. Use
    /// this for disaster-recovery deployments across cloud regions.
    pub fn regions<I, S>(regions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            mode: QuorumMode::Regions {
                required: regions.into_iter().map(|r| r.into()).collect(),
            },
            timeout: Some(Duration::from_secs(10)),
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn without_timeout(mut self) -> Self {
        self.timeout = None;
        self
    }

    /// Is this config ack-first (no wait)?
    pub fn is_async(&self) -> bool {
        matches!(self.mode, QuorumMode::Async)
    }
}

impl Default for QuorumConfig {
    fn default() -> Self {
        Self::async_commit()
    }
}

/// Errors raised by the quorum coordinator. The write itself succeeded
/// on the primary WAL — these errors signal that replica acknowledgement
/// did not reach quorum and the caller must decide whether to surface
/// the failure or continue anyway.
#[derive(Debug, Clone)]
pub enum QuorumError {
    /// Timed out waiting for enough acks. Includes the set of regions
    /// that had replied (for observability / fallback routing).
    Timeout {
        target_lsn: u64,
        elapsed_ms: u128,
        acked_regions: HashSet<String>,
    },
    /// Not enough replicas are currently connected to ever satisfy the
    /// configured quorum. Returned immediately (no wait) so the caller
    /// can fail fast instead of hanging on a timeout.
    InsufficientReplicas { required: usize, connected: usize },
    /// Required-regions mode is configured but one or more regions have
    /// zero connected replicas. Reported up front so the health-check
    /// layer can alert on "regional partition" before writes stall.
    MissingRegions { missing: Vec<String> },
}

impl std::fmt::Display for QuorumError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QuorumError::Timeout {
                target_lsn,
                elapsed_ms,
                acked_regions,
            } => write!(
                f,
                "quorum timeout after {elapsed_ms}ms waiting for lsn {target_lsn} \
                 (acked by regions: {:?})",
                acked_regions
            ),
            QuorumError::InsufficientReplicas {
                required,
                connected,
            } => write!(
                f,
                "quorum requires {required} replicas but only {connected} connected"
            ),
            QuorumError::MissingRegions { missing } => {
                write!(
                    f,
                    "required regions with no connected replicas: {:?}",
                    missing
                )
            }
        }
    }
}

impl std::error::Error for QuorumError {}

/// Tracks per-replica region bindings and pairs them with the primary's
/// ack map. `PrimaryReplication` owns the WAL buffer + `ReplicaState`
/// list; this coordinator adds the region dimension and the wait-for-
/// quorum logic without duplicating the ack table.
pub struct QuorumCoordinator {
    primary: Arc<PrimaryReplication>,
    config: QuorumConfig,
    /// Map replica_id → region. Populated by `bind_replica_region` when
    /// a replica connects; queried during quorum evaluation.
    regions: parking_lot::RwLock<std::collections::HashMap<String, String>>,
}

impl QuorumCoordinator {
    pub fn new(primary: Arc<PrimaryReplication>, config: QuorumConfig) -> Self {
        Self {
            primary,
            config,
            regions: parking_lot::RwLock::new(std::collections::HashMap::new()),
        }
    }

    /// Associate a replica with its region. Called by the primary's
    /// handshake handler when a replica connects — the replica declares
    /// its region in the handshake payload.
    pub fn bind_replica_region(&self, replica_id: &str, region: &str) {
        self.regions
            .write()
            .insert(replica_id.to_string(), region.to_string());
    }

    /// Forget a replica's region binding on disconnect. Safe to call
    /// repeatedly; no-op if the binding doesn't exist.
    pub fn unbind_replica(&self, replica_id: &str) {
        self.regions.write().remove(replica_id);
    }

    /// Which regions currently have at least one connected replica?
    pub fn connected_regions(&self) -> HashSet<String> {
        self.regions.read().values().cloned().collect()
    }

    /// Wait until the configured quorum has acked `target_lsn`.
    ///
    /// Returns `Ok(())` on successful quorum, `Err(QuorumError)` on
    /// timeout or early-exit validation failures. `Async` mode returns
    /// immediately — the caller already has the primary WAL confirmation.
    pub fn wait_for_quorum(&self, target_lsn: u64) -> Result<(), QuorumError> {
        if self.config.is_async() {
            return Ok(());
        }

        // Early validation: can we ever satisfy this quorum?
        self.validate_preconditions()?;

        let start = Instant::now();
        let timeout = self.config.timeout;
        loop {
            if self.has_quorum(target_lsn) {
                return Ok(());
            }
            if let Some(limit) = timeout {
                if start.elapsed() >= limit {
                    return Err(QuorumError::Timeout {
                        target_lsn,
                        elapsed_ms: start.elapsed().as_millis(),
                        acked_regions: self.acked_regions(target_lsn),
                    });
                }
            }
            // Poll interval matches ReplicationConfig::poll_interval_ms
            // default (100ms). The coordinator doesn't get woken by
            // ack_replica directly today — a future revision adds a
            // condvar on ReplicaState. For Phase 2.6 polling is fine.
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Fast-check the quorum predicate without waiting. Returns true
    /// when the current ack map already satisfies the quorum for
    /// `target_lsn`.
    pub fn has_quorum(&self, target_lsn: u64) -> bool {
        match &self.config.mode {
            QuorumMode::Async => true,
            QuorumMode::Sync { min_replicas } => self.count_acked(target_lsn) >= *min_replicas,
            QuorumMode::Regions { required } => {
                let acked = self.acked_regions(target_lsn);
                required.iter().all(|r| acked.contains(r))
            }
        }
    }

    fn validate_preconditions(&self) -> Result<(), QuorumError> {
        match &self.config.mode {
            QuorumMode::Async => Ok(()),
            QuorumMode::Sync { min_replicas } => {
                let connected = self.primary.replica_count();
                if connected < *min_replicas {
                    return Err(QuorumError::InsufficientReplicas {
                        required: *min_replicas,
                        connected,
                    });
                }
                Ok(())
            }
            QuorumMode::Regions { required } => {
                let connected = self.connected_regions();
                let missing: Vec<String> = required
                    .iter()
                    .filter(|r| !connected.contains(*r))
                    .cloned()
                    .collect();
                if missing.is_empty() {
                    Ok(())
                } else {
                    Err(QuorumError::MissingRegions { missing })
                }
            }
        }
    }

    fn count_acked(&self, target_lsn: u64) -> usize {
        let replicas = self
            .primary
            .replicas
            .read()
            .unwrap_or_else(|e| e.into_inner());
        replicas
            .iter()
            .filter(|r| r.last_acked_lsn >= target_lsn)
            .count()
    }

    fn acked_regions(&self, target_lsn: u64) -> HashSet<String> {
        let replicas = self
            .primary
            .replicas
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let regions = self.regions.read();
        replicas
            .iter()
            .filter(|r| r.last_acked_lsn >= target_lsn)
            .filter_map(|r| regions.get(&r.id).cloned())
            .collect()
    }

    /// Minimum LSN across all connected replicas — the "safe replay"
    /// watermark. Any WAL segment whose records are all `<= this` can
    /// be pruned from the primary's spool without losing any replica's
    /// ability to catch up.
    pub fn safe_replay_lsn(&self) -> Option<u64> {
        let replicas = self
            .primary
            .replicas
            .read()
            .unwrap_or_else(|e| e.into_inner());
        replicas.iter().map(|r| r.last_acked_lsn).min()
    }

    /// Config accessor.
    pub fn config(&self) -> &QuorumConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary() -> Arc<PrimaryReplication> {
        Arc::new(PrimaryReplication::new(None))
    }

    #[test]
    fn async_mode_returns_immediately() {
        let p = primary();
        let q = QuorumCoordinator::new(Arc::clone(&p), QuorumConfig::async_commit());
        assert!(q.wait_for_quorum(42).is_ok());
    }

    #[test]
    fn sync_mode_fails_when_too_few_replicas() {
        let p = primary();
        let q = QuorumCoordinator::new(Arc::clone(&p), QuorumConfig::sync(2));
        // No replicas connected → InsufficientReplicas immediately.
        match q.wait_for_quorum(1) {
            Err(QuorumError::InsufficientReplicas {
                required,
                connected,
            }) => {
                assert_eq!(required, 2);
                assert_eq!(connected, 0);
            }
            other => panic!("expected InsufficientReplicas, got {:?}", other),
        }
    }

    #[test]
    fn sync_mode_returns_when_enough_acks() {
        let p = primary();
        p.register_replica("r1".to_string());
        p.register_replica("r2".to_string());
        p.ack_replica("r1", 10);
        p.ack_replica("r2", 10);
        let q = QuorumCoordinator::new(
            Arc::clone(&p),
            QuorumConfig::sync(2).with_timeout(Duration::from_millis(500)),
        );
        assert!(q.wait_for_quorum(10).is_ok());
    }

    #[test]
    fn region_mode_needs_all_regions_acked() {
        let p = primary();
        p.register_replica("us_a".to_string());
        p.register_replica("eu_a".to_string());
        let q = QuorumCoordinator::new(
            Arc::clone(&p),
            QuorumConfig::regions(["us", "eu"]).with_timeout(Duration::from_millis(500)),
        );
        q.bind_replica_region("us_a", "us");
        q.bind_replica_region("eu_a", "eu");

        // Only us has acked → not enough.
        p.ack_replica("us_a", 50);
        assert!(!q.has_quorum(50));

        // Both acked → quorum satisfied.
        p.ack_replica("eu_a", 50);
        assert!(q.has_quorum(50));
    }

    #[test]
    fn region_mode_rejects_missing_regions_upfront() {
        let p = primary();
        p.register_replica("us_a".to_string());
        let q = QuorumCoordinator::new(
            Arc::clone(&p),
            QuorumConfig::regions(["us", "eu"]).with_timeout(Duration::from_millis(500)),
        );
        q.bind_replica_region("us_a", "us");
        // No replica in "eu" → MissingRegions at validate time.
        match q.wait_for_quorum(1) {
            Err(QuorumError::MissingRegions { missing }) => {
                assert_eq!(missing, vec!["eu".to_string()]);
            }
            other => panic!("expected MissingRegions, got {:?}", other),
        }
    }

    #[test]
    fn safe_replay_lsn_is_min_across_replicas() {
        let p = primary();
        p.register_replica("r1".to_string());
        p.register_replica("r2".to_string());
        p.ack_replica("r1", 100);
        p.ack_replica("r2", 50);
        let q = QuorumCoordinator::new(Arc::clone(&p), QuorumConfig::async_commit());
        assert_eq!(q.safe_replay_lsn(), Some(50));
    }
}
