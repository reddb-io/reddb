//! Runtime telemetry / gates / limits / shutdown accessors.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 8/10, issue #1629).
//! Houses the mutation-engine handle, the write-gate + resource-limit checks,
//! the queue / claim / query-latency / occupancy / node-load /
//! vector-introspection telemetry readers, the slow-query store, lease-lifecycle
//! accessors, the batch / db size guards, graceful shutdown, quota bucket,
//! encryption-at-rest status, replica-apply health, and the `record_metrics_*`
//! family.
use super::*;

impl RedDBRuntime {
    /// Emit a CDC change event and replicate to WAL buffer.
    /// Create a `MutationEngine` bound to this runtime.
    ///
    /// The engine is cheap to construct (no allocation) and should be
    /// dropped after `apply` returns. Use this from application-layer
    /// `create_row` / `create_rows_batch` instead of calling
    /// `bulk_insert` + `index_entity_insert` + `cdc_emit` separately.
    pub(crate) fn mutation_engine(&self) -> crate::runtime::mutation::MutationEngine<'_> {
        crate::runtime::mutation::MutationEngine::new(self)
    }

    /// Public-mutation gate snapshot (PLAN.md W1).
    ///
    /// Surfaces that accept untrusted client requests (SQL DML/DDL,
    /// gRPC mutating RPCs, HTTP/native wire mutations, admin
    /// maintenance, serverless lifecycle) call `check_write` before
    /// dispatching to storage. Returns `RedDBError::ReadOnly` on any
    /// instance running as a replica or with `options.read_only =
    /// true`. The replica internal logical-WAL apply path reaches into
    /// the store directly and never calls this method, so legitimate
    /// replica catch-up still works.
    pub fn check_write(&self, kind: crate::runtime::write_gate::WriteKind) -> RedDBResult<()> {
        self.inner.write_gate.check(kind)
    }

    /// Read-only handle to the gate, useful for transports that want
    /// to surface the policy in health/status output without taking on
    /// a dependency on the concrete enum.
    pub fn write_gate(&self) -> &crate::runtime::write_gate::WriteGate {
        &self.inner.write_gate
    }

    /// Process lifecycle handle (PLAN.md Phase 1). Health probes,
    /// admin/shutdown, and signal handlers consult this single
    /// state machine.
    pub fn lifecycle(&self) -> &crate::runtime::lifecycle::Lifecycle {
        &self.inner.lifecycle
    }

    /// Operator-imposed resource limits (PLAN.md Phase 4.1).
    pub fn resource_limits(&self) -> &crate::runtime::resource_limits::ResourceLimits {
        &self.inner.resource_limits
    }

    /// Append-only audit log for admin mutations (PLAN.md Phase 6.5).
    pub fn audit_log(&self) -> &crate::runtime::audit_log::AuditLogger {
        &self.inner.audit_log
    }

    /// Shared `Arc` to the audit logger — used by collaborators (the
    /// lease lifecycle, future request-context plumbing) that need to
    /// keep the logger alive past the runtime's stack frame.
    pub fn audit_log_arc(&self) -> Arc<crate::runtime::audit_log::AuditLogger> {
        Arc::clone(&self.inner.audit_log)
    }

    /// Shared queue telemetry counters (delivered/acked/nacked).
    pub(crate) fn queue_telemetry(
        &self,
    ) -> &crate::runtime::queue_telemetry::QueueTelemetryCounters {
        &self.inner.queue_telemetry
    }

    /// Snapshots of the queue telemetry counters in label-deterministic
    /// order for `/metrics` rendering and the integration test.
    pub fn queue_telemetry_snapshot(
        &self,
    ) -> crate::runtime::queue_telemetry::QueueTelemetrySnapshot {
        crate::runtime::queue_telemetry::QueueTelemetrySnapshot {
            delivered: self.inner.queue_telemetry.delivered_snapshot(),
            acked: self.inner.queue_telemetry.acked_snapshot(),
            nacked: self.inner.queue_telemetry.nacked_snapshot(),
            wait_started: self.inner.queue_telemetry.wait_started_snapshot(),
            wait_woken: self.inner.queue_telemetry.wait_woken_snapshot(),
            wait_timed_out: self.inner.queue_telemetry.wait_timed_out_snapshot(),
            wait_cancelled: self.inner.queue_telemetry.wait_cancelled_snapshot(),
            wait_duration: self.inner.queue_telemetry.wait_duration_snapshot(),
        }
    }

    /// Snapshots of Concurrent claim counters in label-deterministic order.
    pub fn claim_telemetry_snapshot(&self) -> crate::runtime::ClaimTelemetrySnapshot {
        self.inner.claim_telemetry.snapshot()
    }

    /// Per-`kind` query latency histograms for `/metrics` (only kinds with
    /// a real sample are present — empty kinds are absent, not zero-filled).
    pub fn query_latency_snapshot(
        &self,
    ) -> Vec<crate::runtime::query_latency_telemetry::QueryLatencyHistogram> {
        self.inner.query_latency_telemetry.snapshot()
    }

    /// Cross-kind query latency rollup for `/cluster/status` and the
    /// red-ui percentile panels. `count == 0` until a real sample exists.
    pub fn query_latency_rollup(
        &self,
    ) -> crate::runtime::query_latency_telemetry::QueryLatencyHistogram {
        self.inner.query_latency_telemetry.rollup()
    }

    /// Issue #1244 — take a fresh node CPU/RAM occupancy reading for
    /// `/cluster/status`. CPU utilisation is measured over the interval
    /// since the previous call (the first call only establishes a baseline
    /// and reports `NotSampled`). See `occupancy_sampler` for overhead.
    pub fn sample_occupancy(&self) -> crate::runtime::occupancy_sampler::OccupancySample {
        self.inner.occupancy_sampler.sample()
    }

    /// Issue #1245 — point-in-time node load snapshot (active queries +
    /// connect/disconnect churn). Feeds `/metrics`, `/cluster/status`, and
    /// the red-ui load panels.
    pub fn node_load_snapshot(&self) -> crate::runtime::node_load_telemetry::NodeLoadSnapshot {
        self.inner.node_load_telemetry.snapshot()
    }

    /// Issue #742 — consumer presence registry. Heartbeats land here
    /// from `QUEUE READ` (and, in a follow-up slice, an explicit
    /// `QUEUE HEARTBEAT` command); Red UI and `red.queue_consumers`
    /// read snapshots through `queue_consumer_presence_snapshot`.
    pub(crate) fn queue_presence(
        &self,
    ) -> &std::sync::Arc<crate::storage::queue::presence::ConsumerPresenceRegistry> {
        &self.inner.queue_presence
    }

    /// Issue #742 — point-in-time presence snapshot, classifying each
    /// `(queue, group, consumer)` as active/stale/expired against the
    /// supplied TTL. Wall-clock is read once here so the lifecycle
    /// flags inside the snapshot are internally consistent.
    pub fn queue_consumer_presence_snapshot(
        &self,
        ttl_ms: u64,
    ) -> Vec<crate::storage::queue::presence::ConsumerPresence> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        self.inner.queue_presence.snapshot(now_ns, ttl_ms)
    }

    /// Issue #742 — active-consumer count per `(queue, group)` for the
    /// queue-metadata surface. Stale/expired entries are excluded by
    /// definition; they are still visible in the per-row snapshot.
    pub fn queue_active_consumer_counts(
        &self,
        ttl_ms: u64,
    ) -> std::collections::HashMap<(String, String), u32> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        self.inner
            .queue_presence
            .count_active_by_group(now_ns, ttl_ms)
    }

    /// Issue #743 — vector + TurboQuant introspection registry. Engine
    /// publish points (collection create, artifact build start /
    /// finish, fallback toggle, drop) update this; Red UI and
    /// `red.*` vector virtual tables read snapshots through
    /// `vector_introspection_snapshot` / `vector_introspection_get`.
    pub(crate) fn vector_introspection_registry(
        &self,
    ) -> &std::sync::Arc<crate::storage::vector::introspection::VectorIntrospectionRegistry> {
        &self.inner.vector_introspection
    }

    /// Issue #743 — full snapshot of every tracked vector collection's
    /// `(VectorMetadata, ArtifactMetadata)`. Deterministically ordered
    /// by collection name so Red UI tables and tests both see a
    /// stable shape.
    pub fn vector_introspection_snapshot(
        &self,
    ) -> Vec<crate::storage::vector::introspection::VectorIntrospection> {
        self.inner.vector_introspection.snapshot()
    }

    /// Issue #743 — single-collection lookup, for the per-collection
    /// metadata endpoint Red UI hits when an operator opens one
    /// vector's toolbar.
    pub fn vector_introspection_get(
        &self,
        collection: &str,
    ) -> Option<crate::storage::vector::introspection::VectorIntrospection> {
        self.inner.vector_introspection.get(collection)
    }

    /// Issue #1238 — ADR 0060 read-model accessor for slow-query telemetry.
    ///
    /// Returns a reference to the bounded ring store so HTTP handlers and
    /// the red-ui read model can call `store.read(filter)` without
    /// touching `red-slow.log` directly.
    pub fn slow_query_store(&self) -> &Arc<crate::telemetry::slow_query_store::SlowQueryStore> {
        &self.inner.slow_query_store
    }

    /// Slice 10 of issue #527 — render-time scan of pending entries
    /// per (queue, group) for the `queue_pending_gauge` exposition.
    /// Walks `red_queue_meta` live so the gauge cannot drift from
    /// the source of truth.
    pub fn queue_pending_counts(&self) -> Vec<((String, String), u64)> {
        let store = self.inner.db.store();
        crate::runtime::impl_queue::pending_counts_by_group(store.as_ref())
            .into_iter()
            .collect()
    }

    /// Shared `Arc` to the write gate. Same rationale as
    /// `audit_log_arc`: collaborators (lease lifecycle, refresh
    /// thread) need a clone-cheap handle they can move into a
    /// background thread.
    pub fn write_gate_arc(&self) -> Arc<crate::runtime::write_gate::WriteGate> {
        Arc::clone(&self.inner.write_gate)
    }

    /// Serverless writer-lease state machine. `None` when the operator
    /// did not opt into lease fencing (`RED_LEASE_REQUIRED` unset).
    pub fn lease_lifecycle(&self) -> Option<&Arc<crate::runtime::lease_lifecycle::LeaseLifecycle>> {
        self.inner.lease_lifecycle.get()
    }

    /// Install the lease lifecycle. Idempotent; subsequent calls
    /// return the previously stored value untouched.
    pub fn set_lease_lifecycle(
        &self,
        lifecycle: Arc<crate::runtime::lease_lifecycle::LeaseLifecycle>,
    ) -> Result<(), Arc<crate::runtime::lease_lifecycle::LeaseLifecycle>> {
        self.inner.lease_lifecycle.set(lifecycle)
    }

    /// Reject the call when the requested batch size exceeds
    /// `RED_MAX_BATCH_SIZE`. Returns `RedDBError::QuotaExceeded`
    /// shaped so the HTTP layer can map it to 413 Payload Too
    /// Large (PLAN.md Phase 4.1).
    pub fn check_batch_size(&self, requested: usize) -> RedDBResult<()> {
        if self.inner.resource_limits.batch_size_exceeded(requested) {
            let max = self.inner.resource_limits.max_batch_size.unwrap_or(0);
            return Err(RedDBError::QuotaExceeded(format!(
                "max_batch_size:{requested}:{max}"
            )));
        }
        Ok(())
    }

    /// Reject the call when the local DB file exceeds
    /// `RED_MAX_DB_SIZE_BYTES`. Reads file metadata once per call —
    /// the cost is a single `stat()` syscall, negligible against the
    /// I/O the caller is about to do. Returns `QuotaExceeded` shaped
    /// for HTTP 507 Insufficient Storage.
    pub fn check_db_size(&self) -> RedDBResult<()> {
        let Some(limit) = self.inner.resource_limits.max_db_size_bytes else {
            return Ok(());
        };
        if limit == 0 {
            return Ok(());
        }
        let Some(path) = self.inner.db.path() else {
            return Ok(());
        };
        let current = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if current > limit {
            return Err(RedDBError::QuotaExceeded(format!(
                "max_db_size_bytes:{current}:{limit}"
            )));
        }
        Ok(())
    }

    /// Graceful shutdown coordinator (PLAN.md Phase 1.1).
    ///
    /// Steps, in order, all idempotent across re-entrant calls:
    ///   1. Move lifecycle into `ShuttingDown` (concurrent callers
    ///      observe `Stopped` after first finishes).
    ///   2. Flush WAL + run final checkpoint via `db.flush()` so
    ///      every acked write is durable on disk.
    ///   3. If `backup_on_shutdown == true` and a remote backend is
    ///      configured, run a synchronous `trigger_backup()` so the
    ///      remote head reflects the final state.
    ///   4. Stamp the report and move to `Stopped`. Subsequent calls
    ///      return the cached report without re-running anything.
    ///
    /// On any error, the runtime is still marked `Stopped` so the
    /// process can exit; the caller logs the error context but does
    /// not retry the same shutdown — the operator can inspect the
    /// report fields to see which step failed.
    pub fn graceful_shutdown(
        &self,
        backup_on_shutdown: bool,
    ) -> RedDBResult<crate::runtime::lifecycle::ShutdownReport> {
        if !self.inner.lifecycle.begin_shutdown() {
            // Someone else already shut down (or is in flight). Return
            // the cached report so the HTTP caller and SIGTERM handler
            // get the same idempotent answer.
            return Ok(self.inner.lifecycle.shutdown_report().unwrap_or_default());
        }

        let started_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let mut report = crate::runtime::lifecycle::ShutdownReport {
            started_at_ms: started_ms,
            ..Default::default()
        };

        // Flush WAL + run any pending checkpoint. Local fsync is
        // unconditional — even a lease-lost replica needs its WAL on
        // disk before exit so a future restore has the latest tail.
        // The remote upload is gated separately so a lost-lease writer
        // doesn't clobber the new holder's state on its way out.
        let flush_res = self.inner.db.flush_local_only();
        report.flushed_wal = flush_res.is_ok();
        report.final_checkpoint = flush_res.is_ok();
        if let Err(err) = &flush_res {
            tracing::error!(
                target: "reddb::lifecycle",
                error = %err,
                "graceful_shutdown: local flush failed"
            );
        } else if let Err(lease_err) =
            self.assert_remote_write_allowed("shutdown/checkpoint_upload")
        {
            tracing::warn!(
                target: "reddb::serverless::lease",
                error = %lease_err,
                "graceful_shutdown: remote upload skipped — lease not held"
            );
        } else if let Err(err) = self.inner.db.upload_to_remote_backend() {
            tracing::error!(
                target: "reddb::lifecycle",
                error = %err,
                "graceful_shutdown: remote upload failed"
            );
        }

        // Optional final backup. Skipped silently when no remote
        // backend is configured — `trigger_backup()` returns Err
        // anyway in that case, but logging it as a shutdown failure
        // would be misleading on a standalone (no-backend) runtime.
        if backup_on_shutdown && self.inner.db.remote_backend.is_some() {
            // The trigger_backup gate now reads `WriteKind::Backup`,
            // which a replica/read_only instance refuses. That's
            // intentional — replicas don't drive backups; only the
            // primary does. We still want shutdown to flush its WAL
            // even if the backup branch is gated off.
            match self.trigger_backup() {
                Ok(result) => {
                    report.backup_uploaded = result.uploaded;
                }
                Err(err) => {
                    tracing::warn!(
                        target: "reddb::lifecycle",
                        error = %err,
                        "graceful_shutdown: final backup skipped"
                    );
                }
            }
        }

        let completed_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(started_ms);
        report.completed_at_ms = completed_ms;
        report.duration_ms = completed_ms.saturating_sub(started_ms);

        self.inner.lifecycle.finish_shutdown(report.clone());
        Ok(report)
    }

    /// PLAN.md Phase 4.4 — per-caller quota bucket. Always
    /// returned; `is_configured()` lets callers short-circuit.
    pub fn quota_bucket(&self) -> &crate::runtime::quota_bucket::QuotaBucket {
        &self.inner.quota_bucket
    }

    /// PLAN.md Phase 6.3 — whether at-rest encryption is configured.
    /// Reads `RED_ENCRYPTION_KEY` / `RED_ENCRYPTION_KEY_FILE` lazily;
    /// returns `("enabled", None)` when a key is loadable, `("error", Some(msg))`
    /// when the operator set the env but it doesn't parse, and
    /// `("disabled", None)` when no key is configured. The pager
    /// hookup is deferred — this accessor surfaces the operator's
    /// intent for /admin/status without yet using the key in writes.
    pub fn encryption_at_rest_status(&self) -> (&'static str, Option<String>) {
        match crate::crypto::page_encryption::key_from_env() {
            Ok(Some(_)) => ("enabled", None),
            Ok(None) => ("disabled", None),
            Err(err) => ("error", Some(err)),
        }
    }

    /// PLAN.md Phase 11.5 — current replica apply health label
    /// (`ok`, `gap`, `divergence`, `apply_error`, `connecting`,
    /// `stalled_gap`). Read from the persisted `red.replication.state`
    /// config key updated by the replica loop. Returns `None` on
    /// non-replica instances or when no apply has run yet.
    pub fn replica_apply_health(&self) -> Option<String> {
        let state = self.config_string("red.replication.state", "");
        if state.is_empty() {
            None
        } else {
            Some(state)
        }
    }

    pub(crate) fn record_metrics_ingest(
        &self,
        accepted_samples: u64,
        accepted_series: u64,
        rejected_samples: u64,
        rejected_series: u64,
    ) {
        self.inner.metrics_ingest_stats.record(
            accepted_samples,
            accepted_series,
            rejected_samples,
            rejected_series,
        );
    }

    pub(crate) fn record_metrics_cardinality_budget_rejections(&self, rejected_series: u64) {
        self.inner
            .metrics_ingest_stats
            .record_cardinality_budget_rejections(rejected_series);
    }

    pub(crate) fn record_metrics_tenant_activity(
        &self,
        tenant: &str,
        namespace: &str,
        operation: &str,
    ) {
        self.inner
            .metrics_tenant_activity_stats
            .record(tenant, namespace, operation);
    }

    pub(crate) fn metrics_tenant_activity_snapshot(
        &self,
    ) -> Vec<crate::runtime::MetricsTenantActivityStats> {
        self.inner.metrics_tenant_activity_stats.snapshot()
    }
}
