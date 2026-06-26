use super::*;

impl RedDBRuntime {
    /// PLAN.md Phase 11.4 — owned snapshot of every registered
    /// replica's state on this primary. Returns empty vec on
    /// non-primary instances or when no replicas are registered yet.
    pub fn primary_replica_snapshots(&self) -> Vec<crate::replication::primary::ReplicaState> {
        self.inner
            .db
            .replication
            .as_ref()
            .map(|repl| repl.replica_snapshots())
            .unwrap_or_default()
    }

    /// Issue #839 — the primary's current logical-WAL head LSN, used as
    /// the reference point for per-replica lag. `0` on non-primary
    /// instances or before the logical spool has any records.
    pub fn primary_logical_head_lsn(&self) -> u64 {
        self.inner
            .db
            .replication
            .as_ref()
            .map(|repl| repl.current_logical_lsn())
            .unwrap_or(0)
    }

    /// Issue #839 — count of pulls that forced a full re-bootstrap since
    /// process start. The primary operator alert signal; always `0` on a
    /// non-primary instance.
    pub fn replication_full_resync_count(&self) -> u64 {
        self.inner
            .db
            .replication
            .as_ref()
            .map(|repl| repl.full_resync_count())
            .unwrap_or(0)
    }

    /// Issue #839 — count of pulls served as a partial (incremental)
    /// resync since process start. Always `0` on a non-primary instance.
    pub fn replication_partial_resync_count(&self) -> u64 {
        self.inner
            .db
            .replication
            .as_ref()
            .map(|repl| repl.partial_resync_count())
            .unwrap_or(0)
    }

    /// Issue #1243 (PRD #1237 Phase B) — count of primary↔replica
    /// reconnects observed by this node's replica loop since process
    /// start. A reconnect is a link that was healthy, dropped (the pull
    /// loop fell back to `connecting`), and recovered. The initial connect
    /// is **not** counted. Always `0` on a node whose replica loop never
    /// ran (e.g. a standalone or primary instance). In-memory only: resets
    /// to `0` on process restart, like the resync counters above.
    pub fn replication_reconnects_count(&self) -> u64 {
        self.inner.replica_link_metrics.reconnects_total()
    }

    /// Issue #1243 — this node's persisted replica identity, read-only.
    /// Unlike [`node_id`](Self::node_id) / `resolve_replica_id` this never
    /// generates or persists a new id; it returns the empty string when one
    /// has not been assigned yet. Used as the bounded `replica_id`
    /// dimension on `reddb_replication_reconnects_total`.
    pub fn replication_replica_id(&self) -> String {
        self.config_string("red.replication.replica_id", "")
    }

    /// Issue #1243 — drive the reconnect tracker from a replica health
    /// state string. Production code reaches this through the replica
    /// loop's health-persist chokepoint; exposed on the runtime so an
    /// integration test can drive a deterministic link drop/restore
    /// without standing up a full gRPC link on a memory-constrained host.
    pub fn observe_replica_link_state(&self, state: &str) {
        self.inner.replica_link_metrics.observe_state(state);
    }

    pub fn enforce_primary_replica_retention_limits(
        &self,
    ) -> Vec<(String, reddb_file::ReplicationSlotInvalidationCause)> {
        self.inner
            .db
            .replication
            .as_ref()
            .map(|repl| repl.enforce_retention_limits(crate::utils::now_unix_millis() as u128))
            .unwrap_or_default()
    }

    /// Issue #839 — this node's stable identity, surfaced as the leader
    /// identity in `/replication/status` when the node is the primary.
    /// Reuses the same persisted id a replica advertises to the primary,
    /// so a cluster has one stable name per node regardless of role.
    pub fn node_id(&self) -> String {
        self.resolve_replica_id()
    }

    /// Issue #826 — re-evaluate write-admission flow control from the
    /// live primary replica registry and return the resulting throttle
    /// state. Computes the max lag across in-quorum replicas (async
    /// read-replicas excluded) against the primary's current LSN and
    /// engages/releases the `WriteGate` throttle accordingly.
    ///
    /// No-op (returns `false`) on non-primary instances or when flow
    /// control is disabled (soft target `0`). Cheap enough to call on
    /// the replica-ack path and from `/metrics` scrapes so the throttle
    /// tracks lag without a dedicated background loop.
    pub fn refresh_replication_flow_control(&self) -> bool {
        let flow = self.inner.write_gate.flow_control();
        if !flow.is_enabled() {
            return false;
        }
        let Some(repl) = self.inner.db.replication.as_ref() else {
            return false;
        };
        let primary_lsn = repl.current_logical_lsn();
        let replicas = repl.replica_snapshots();
        flow.observe(&replicas, primary_lsn)
    }

    /// PLAN.md Phase 11.4 — active commit policy. Reads
    /// `RED_PRIMARY_COMMIT_POLICY` once at runtime construction;
    /// future env reloads will need a reload endpoint. Default is
    /// `Local` — current behavior, no replica blocking.
    pub fn commit_policy(&self) -> crate::replication::CommitPolicy {
        crate::replication::CommitPolicy::from_env()
    }

    /// Issue #1001 — resolve the *effective* commit policy for one collection by
    /// combining the cluster default ([`commit_policy`](Self::commit_policy)),
    /// the collection's declared override, the collection data model, and the
    /// deployment's HA intent (`RED_CLUSTER_HA_INTENT`).
    ///
    /// Both write admission and failover eligibility call this so they read the
    /// same decision: a durable model (transactional/queue/audit/config/vault)
    /// may not silently use local-only acknowledgement under declared HA intent
    /// — that returns [`CommitPolicyViolation`] and the caller must fail closed.
    /// Explicitly ephemeral/cache-like collections may opt into local commit
    /// with the documented failover-eligibility data-loss window.
    pub fn resolve_commit_policy(
        &self,
        model: crate::cluster::CollectionDataModel,
        collection_override: Option<crate::replication::CommitPolicy>,
    ) -> Result<crate::cluster::CommitPolicyResolution, crate::cluster::CommitPolicyViolation> {
        crate::cluster::resolve_commit_policy(
            self.commit_policy(),
            collection_override,
            model,
            crate::cluster::HaIntent::from_env(),
        )
    }

    pub fn primary_replica_durability(&self) -> reddb_file::ReplicationDurability {
        Self::primary_replica_durability_for_policy(self.commit_policy())
    }

    pub(crate) fn primary_replica_durability_for_policy(
        policy: crate::replication::CommitPolicy,
    ) -> reddb_file::ReplicationDurability {
        match policy {
            crate::replication::CommitPolicy::AckN(n) if n > 0 => {
                reddb_file::ReplicationDurability::RemoteFlush {
                    quorum: u16::try_from(n).unwrap_or(u16::MAX),
                }
            }
            _ => reddb_file::ReplicationDurability::Async,
        }
    }

    /// PLAN.md Phase 11.5 — accessor for replica-side apply error
    /// counters (gap / divergence / apply / decode / apply_miss). Returned
    /// snapshot is consistent across the counters; the labels match
    /// `reddb_replica_apply_errors_total{kind}`. Issue #814 adds the
    /// `apply_miss` kind for deletes against a missing target.
    pub fn replica_apply_error_counts(
        &self,
    ) -> [(crate::replication::logical::ApplyErrorKind, u64); 6] {
        self.inner.replica_apply_metrics.snapshot()
    }

    /// PLAN.md Phase 11.4 — observability snapshot of every
    /// replica's durable LSN as known to the commit waiter. Empty
    /// vec on non-primary instances or when no replica has acked.
    pub fn commit_waiter_snapshot(&self) -> Vec<(String, u64)> {
        self.inner
            .db
            .replication
            .as_ref()
            .map(|repl| repl.commit_waiter.snapshot())
            .unwrap_or_default()
    }

    /// PLAN.md Phase 11.4 — `(reached, timed_out, not_required, last_micros)`
    /// counters for /metrics. Always-zero on non-primary instances.
    pub fn commit_waiter_metrics_snapshot(&self) -> (u64, u64, u64, u64) {
        self.inner
            .db
            .replication
            .as_ref()
            .map(|repl| repl.commit_waiter.metrics_snapshot())
            .unwrap_or((0, 0, 0, 0))
    }

    /// Named commit watermark: highest LSN durable on the active
    /// synchronous commit quorum. Returns 0 when the active policy does
    /// not require replica durability.
    pub fn commit_watermark(&self) -> u64 {
        match self.primary_replica_durability() {
            reddb_file::ReplicationDurability::RemoteWrite { quorum }
            | reddb_file::ReplicationDurability::RemoteFlush { quorum }
            | reddb_file::ReplicationDurability::RemoteApply { quorum }
                if quorum > 0 =>
            {
                self.inner
                    .db
                    .replication
                    .as_ref()
                    .map(|repl| repl.commit_waiter.commit_watermark(u32::from(quorum)))
                    .unwrap_or(0)
            }
            _ if matches!(
                self.commit_policy(),
                crate::replication::CommitPolicy::Quorum
            ) =>
            {
                self.inner
                    .db
                    .quorum
                    .as_ref()
                    .map(|q| q.commit_watermark())
                    .unwrap_or(0)
            }
            _ => 0,
        }
    }

    /// PLAN.md Phase 11.4 — block until at least `count` replicas
    /// have durably applied through `target_lsn`, or `timeout`
    /// elapses. Returns the `AwaitOutcome` so the caller can decide
    /// whether to surface a timeout error to the client or continue
    /// (the policy mapping lives in the commit dispatcher).
    ///
    /// Used by the `ack_n` commit policy once the operator flips
    /// `RED_PRIMARY_COMMIT_POLICY` away from `local`.
    pub fn await_replica_acks(
        &self,
        target_lsn: u64,
        count: u32,
        timeout: std::time::Duration,
    ) -> crate::replication::AwaitOutcome {
        match &self.inner.db.replication {
            Some(repl) => repl.commit_waiter.await_acks(target_lsn, count, timeout),
            None => {
                // No replication configured: policy must be `Local`.
                // Treat as immediate `NotRequired` so callers don't
                // block on a degenerate setup.
                crate::replication::AwaitOutcome::NotRequired
            }
        }
    }

    /// PLAN.md Phase 11.4 — enforce the configured commit policy
    /// against `post_lsn` (the LSN of the just-completed write).
    /// Returns `Ok(AwaitOutcome)` on every successful enforcement
    /// (including `Reached` and `TimedOut` when fail-on-timeout is
    /// off). Returns `Err(ReadOnly)` only when a synchronous policy
    /// misses its threshold and `RED_COMMIT_FAIL_ON_TIMEOUT=true` is
    /// set.
    ///
    /// The HTTP / gRPC / wire surfaces map the error to 504 / wire
    /// backoff. Default behaviour (env unset) logs warn and returns
    /// success — matches PLAN.md "default v1 stays local" semantics
    /// while still letting the operator opt into hard-blocking.
    pub fn enforce_commit_policy(
        &self,
        post_lsn: u64,
    ) -> RedDBResult<crate::replication::AwaitOutcome> {
        let policy = self.commit_policy();
        if matches!(policy, crate::replication::CommitPolicy::Quorum) {
            return match self.inner.db.wait_for_replication_quorum(post_lsn) {
                Ok(()) => Ok(crate::replication::AwaitOutcome::Reached(0)),
                Err(err) => {
                    tracing::warn!(
                        target: "reddb::commit",
                        post_lsn,
                        error = %err,
                        "quorum: timed out waiting for commit watermark"
                    );
                    let fail = std::env::var("RED_COMMIT_FAIL_ON_TIMEOUT")
                        .ok()
                        .map(|v| {
                            let t = v.trim();
                            t.eq_ignore_ascii_case("true")
                                || t == "1"
                                || t.eq_ignore_ascii_case("yes")
                        })
                        .unwrap_or(false);
                    if fail {
                        return Err(RedDBError::ReadOnly(format!(
                            "commit policy timed out at lsn {post_lsn}: {err} (RED_COMMIT_FAIL_ON_TIMEOUT=true)"
                        )));
                    }
                    Ok(crate::replication::AwaitOutcome::TimedOut {
                        observed: 0,
                        required: 1,
                    })
                }
            };
        }

        let durability = Self::primary_replica_durability_for_policy(policy);
        let n = match durability {
            reddb_file::ReplicationDurability::RemoteWrite { quorum }
            | reddb_file::ReplicationDurability::RemoteFlush { quorum }
            | reddb_file::ReplicationDurability::RemoteApply { quorum }
                if quorum > 0 =>
            {
                u32::from(quorum)
            }
            _ => return Ok(crate::replication::AwaitOutcome::NotRequired),
        };
        let timeout_ms = std::env::var("RED_REPLICATION_ACK_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5_000);
        let outcome =
            self.await_replica_acks(post_lsn, n, std::time::Duration::from_millis(timeout_ms));
        {
            use crate::runtime::control_events::{EventKind, Outcome, Sensitivity};
            let (event_outcome, fields) = match &outcome {
                crate::replication::AwaitOutcome::Reached(count) => (
                    Outcome::Allowed,
                    vec![
                        (
                            "post_lsn".to_string(),
                            Sensitivity::raw(post_lsn.to_string()),
                        ),
                        ("required".to_string(), Sensitivity::raw(n.to_string())),
                        ("observed".to_string(), Sensitivity::raw(count.to_string())),
                        (
                            "timeout_ms".to_string(),
                            Sensitivity::raw(timeout_ms.to_string()),
                        ),
                    ],
                ),
                crate::replication::AwaitOutcome::TimedOut { observed, required } => (
                    Outcome::Error,
                    vec![
                        (
                            "post_lsn".to_string(),
                            Sensitivity::raw(post_lsn.to_string()),
                        ),
                        (
                            "required".to_string(),
                            Sensitivity::raw(required.to_string()),
                        ),
                        (
                            "observed".to_string(),
                            Sensitivity::raw(observed.to_string()),
                        ),
                        (
                            "timeout_ms".to_string(),
                            Sensitivity::raw(timeout_ms.to_string()),
                        ),
                    ],
                ),
                crate::replication::AwaitOutcome::NotRequired => (Outcome::Allowed, Vec::new()),
            };
            if !fields.is_empty() {
                self.emit_control_event(
                    EventKind::ReplicationSafety,
                    event_outcome,
                    "replication_commit_policy",
                    Some(format!("replication:lsn:{post_lsn}")),
                    None,
                    fields,
                )?;
            }
        }
        if let crate::replication::AwaitOutcome::TimedOut { observed, required } = &outcome {
            tracing::warn!(
                target: "reddb::commit",
                post_lsn,
                observed = *observed,
                required = *required,
                timeout_ms,
                "ack_n: timed out waiting for replicas"
            );
            let fail = std::env::var("RED_COMMIT_FAIL_ON_TIMEOUT")
                .ok()
                .map(|v| {
                    let t = v.trim();
                    t.eq_ignore_ascii_case("true") || t == "1" || t.eq_ignore_ascii_case("yes")
                })
                .unwrap_or(false);
            if fail {
                return Err(RedDBError::ReadOnly(format!(
                    "commit policy timed out at lsn {post_lsn}: observed={observed} required={required} (RED_COMMIT_FAIL_ON_TIMEOUT=true)"
                )));
            }
        }
        Ok(outcome)
    }
}
