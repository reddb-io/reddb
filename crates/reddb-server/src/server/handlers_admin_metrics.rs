//! Prometheus / OpenMetrics HTTP endpoint.

use super::*;
use crate::runtime::lifecycle::Phase;
use crate::server::handlers_admin::sanitize_label;

impl RedDBServer {
    /// `GET /metrics` — Prometheus / OpenMetrics exposition.
    ///
    /// Initial metric set (PLAN.md Phase 5.1) covers the
    /// orchestrator-relevant signals: uptime, health phase, read-
    /// only state, replication role, last-backup outcome, on-disk
    /// size when known. Counters that need request-path
    /// instrumentation (ops_total, query_duration_seconds_bucket)
    /// land in a follow-up commit so this endpoint can ship today
    /// against the existing data sources.
    pub(crate) fn handle_metrics(&self) -> HttpResponse {
        use std::fmt::Write;
        let lifecycle = self.runtime.lifecycle();
        let phase = lifecycle.phase();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let uptime_secs = (now_ms.saturating_sub(lifecycle.started_at_ms()) as f64) / 1000.0;
        let cold_start_secs = lifecycle
            .ready_at_ms()
            .map(|ready_ms| (ready_ms.saturating_sub(lifecycle.started_at_ms()) as f64) / 1000.0);
        let health_status: u8 = match phase {
            Phase::Stopped => 0,
            Phase::Starting | Phase::ShuttingDown => 0,
            Phase::Draining => 1,
            Phase::Ready => 2,
        };
        let read_only = self.runtime.write_gate().is_read_only();
        let role = match self.runtime.write_gate().role() {
            crate::replication::ReplicationRole::Standalone => "standalone",
            crate::replication::ReplicationRole::Primary => "primary",
            crate::replication::ReplicationRole::Replica { .. } => "replica",
        };
        let db_size_bytes = self
            .runtime
            .db()
            .path()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        let runtime_stats = self.runtime.stats();
        let result_blob_stats = runtime_stats.result_blob_cache;
        let kv_stats = runtime_stats.kv;
        let metrics_ingest = runtime_stats.metrics_ingest;

        let mut body = String::with_capacity(1024);
        let _ = writeln!(
            body,
            "# HELP reddb_uptime_seconds Seconds since the runtime was constructed."
        );
        let _ = writeln!(body, "# TYPE reddb_uptime_seconds gauge");
        let _ = writeln!(body, "reddb_uptime_seconds {}", uptime_secs);

        let _ = writeln!(
            body,
            "# HELP reddb_health_status 0=down/starting, 1=degraded/draining, 2=ready."
        );
        let _ = writeln!(body, "# TYPE reddb_health_status gauge");
        let _ = writeln!(body, "reddb_health_status {}", health_status);

        let _ = writeln!(
            body,
            "# HELP reddb_phase Lifecycle phase as a labeled gauge (always 1; phase in label)."
        );
        let _ = writeln!(body, "# TYPE reddb_phase gauge");
        let _ = writeln!(body, "reddb_phase{{phase=\"{}\"}} 1", phase.as_str());

        let _ = writeln!(
            body,
            "# HELP reddb_read_only 1 when public mutations are gated, 0 otherwise."
        );
        let _ = writeln!(body, "# TYPE reddb_read_only gauge");
        let _ = writeln!(body, "reddb_read_only {}", if read_only { 1 } else { 0 });

        let _ = writeln!(
            body,
            "# HELP reddb_replication_role Replication role of this instance."
        );
        let _ = writeln!(body, "# TYPE reddb_replication_role gauge");
        let _ = writeln!(body, "reddb_replication_role{{role=\"{}\"}} 1", role);

        // PLAN.md Phase 5 / W6 — serverless writer lease state.
        // `not_required` for instances that opted out of lease fencing;
        // `held` / `not_held` for instances behind the fence so dashboards
        // can alert on lease loss without scraping logs.
        let lease_state = self.runtime.write_gate().lease_state();
        let _ = writeln!(
            body,
            "# HELP reddb_writer_lease_state Serverless writer-lease gate state (label)."
        );
        let _ = writeln!(body, "# TYPE reddb_writer_lease_state gauge");
        let _ = writeln!(
            body,
            "reddb_writer_lease_state{{state=\"{}\"}} 1",
            lease_state.label()
        );

        // PLAN.md Phase 5.1 — backup + WAL archive lag.
        // These are the SRE signals an orchestrator alerts on when a
        // serverless instance is healthy on the surface but its DR
        // posture has degraded silently.
        let backup_status = self.runtime.backup_status();
        if let Some(last) = backup_status.last_backup.as_ref() {
            let last_ts_secs = (last.timestamp as f64) / 1000.0;
            let _ = writeln!(
                body,
                "# HELP reddb_backup_last_success_timestamp_seconds Unix ts (s) of the most recent successful backup."
            );
            let _ = writeln!(
                body,
                "# TYPE reddb_backup_last_success_timestamp_seconds gauge"
            );
            let _ = writeln!(
                body,
                "reddb_backup_last_success_timestamp_seconds {}",
                last_ts_secs
            );
            let age_secs = ((now_ms.saturating_sub(last.timestamp)) as f64) / 1000.0;
            let _ = writeln!(
                body,
                "# HELP reddb_backup_age_seconds Seconds since last successful backup."
            );
            let _ = writeln!(body, "# TYPE reddb_backup_age_seconds gauge");
            let _ = writeln!(body, "reddb_backup_age_seconds {}", age_secs);
            let _ = writeln!(
                body,
                "# HELP reddb_backup_last_duration_seconds Wall-clock duration of the most recent backup."
            );
            let _ = writeln!(body, "# TYPE reddb_backup_last_duration_seconds gauge");
            let _ = writeln!(
                body,
                "reddb_backup_last_duration_seconds {}",
                (last.duration_ms as f64) / 1000.0
            );
        }
        let _ = writeln!(
            body,
            "# HELP reddb_backup_failures_total Total backup failures since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_backup_failures_total counter");
        let _ = writeln!(
            body,
            "reddb_backup_failures_total {}",
            backup_status.total_failures
        );
        let _ = writeln!(
            body,
            "# HELP reddb_backup_total_total Total successful backups since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_backup_total_total counter");
        let _ = writeln!(
            body,
            "reddb_backup_total_total {}",
            backup_status.total_backups
        );

        // WAL archive lag — distance between the engine's current LSN
        // and the last archived LSN. Operators alert when this grows
        // unbounded; it means archive uploads are failing or paused
        // (e.g. backend unreachable, lease lost).
        let (current_lsn, last_archived_lsn) = self.runtime.wal_archive_progress();
        let lag = current_lsn.saturating_sub(last_archived_lsn);
        let _ = writeln!(
            body,
            "# HELP reddb_wal_current_lsn Current local LSN (most recent record visible to writers)."
        );
        let _ = writeln!(body, "# TYPE reddb_wal_current_lsn gauge");
        let _ = writeln!(body, "reddb_wal_current_lsn {}", current_lsn);
        let _ = writeln!(
            body,
            "# HELP reddb_wal_last_archived_lsn LSN of the most recently archived WAL segment."
        );
        let _ = writeln!(body, "# TYPE reddb_wal_last_archived_lsn gauge");
        let _ = writeln!(body, "reddb_wal_last_archived_lsn {}", last_archived_lsn);
        let _ = writeln!(
            body,
            "# HELP reddb_wal_archive_lag_records Records between current LSN and last archived LSN."
        );
        let _ = writeln!(body, "# TYPE reddb_wal_archive_lag_records gauge");
        let _ = writeln!(body, "reddb_wal_archive_lag_records {}", lag);
        let primary_replica_retention_result = self.runtime.primary_replica_wal_retention_plan();
        let primary_replica_retention_error = u8::from(primary_replica_retention_result.is_err());
        let primary_replica_retention = primary_replica_retention_result.ok().flatten();
        let retained_bytes = primary_replica_retention
            .as_ref()
            .map(|plan| plan.retained_bytes_before_prune)
            .unwrap_or(0);
        let retained_bytes_after_prune = primary_replica_retention
            .as_ref()
            .map(|plan| plan.retained_bytes_after_prune)
            .unwrap_or(0);
        let oldest_required_lsn = primary_replica_retention
            .as_ref()
            .and_then(|plan| plan.oldest_required_lsn)
            .unwrap_or(0);
        let removable_segments = primary_replica_retention
            .as_ref()
            .map(|plan| plan.removable_segments.len())
            .unwrap_or(0);
        let _ = writeln!(
            body,
            "# HELP reddb_primary_replica_wal_retained_bytes Bytes currently retained in primary-replica WAL segments before pruning."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_primary_replica_wal_retained_bytes gauge"
        );
        let _ = writeln!(
            body,
            "reddb_primary_replica_wal_retained_bytes {}",
            retained_bytes
        );
        let _ = writeln!(
            body,
            "# HELP reddb_primary_replica_wal_retained_after_prune_bytes Bytes expected to remain in primary-replica WAL segments after pruning eligible files."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_primary_replica_wal_retained_after_prune_bytes gauge"
        );
        let _ = writeln!(
            body,
            "reddb_primary_replica_wal_retained_after_prune_bytes {}",
            retained_bytes_after_prune
        );
        let _ = writeln!(
            body,
            "# HELP reddb_primary_replica_wal_oldest_required_lsn Oldest LSN still required by replication slots; zero means unavailable."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_primary_replica_wal_oldest_required_lsn gauge"
        );
        let _ = writeln!(
            body,
            "reddb_primary_replica_wal_oldest_required_lsn {}",
            oldest_required_lsn
        );
        let _ = writeln!(
            body,
            "# HELP reddb_primary_replica_wal_removable_segments Primary-replica WAL segment files eligible for pruning."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_primary_replica_wal_removable_segments gauge"
        );
        let _ = writeln!(
            body,
            "reddb_primary_replica_wal_removable_segments {}",
            removable_segments
        );
        let _ = writeln!(
            body,
            "# HELP reddb_primary_replica_wal_retention_error 1 when primary-replica WAL retention metrics could not be computed."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_primary_replica_wal_retention_error gauge"
        );
        let _ = writeln!(
            body,
            "reddb_primary_replica_wal_retention_error {}",
            primary_replica_retention_error
        );

        let _ = writeln!(
            body,
            "# HELP reddb_metrics_remote_write_samples_accepted_total Metrics remote-write samples accepted since process start."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_metrics_remote_write_samples_accepted_total counter"
        );
        let _ = writeln!(
            body,
            "reddb_metrics_remote_write_samples_accepted_total {}",
            metrics_ingest.samples_accepted
        );
        let _ = writeln!(
            body,
            "# HELP reddb_metrics_remote_write_series_accepted_total Metrics remote-write series accepted since process start."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_metrics_remote_write_series_accepted_total counter"
        );
        let _ = writeln!(
            body,
            "reddb_metrics_remote_write_series_accepted_total {}",
            metrics_ingest.series_accepted
        );
        let _ = writeln!(
            body,
            "# HELP reddb_metrics_remote_write_samples_rejected_total Metrics remote-write samples rejected since process start."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_metrics_remote_write_samples_rejected_total counter"
        );
        let _ = writeln!(
            body,
            "reddb_metrics_remote_write_samples_rejected_total {}",
            metrics_ingest.samples_rejected
        );
        let _ = writeln!(
            body,
            "# HELP reddb_metrics_remote_write_series_rejected_total Metrics remote-write series rejected since process start."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_metrics_remote_write_series_rejected_total counter"
        );
        let _ = writeln!(
            body,
            "reddb_metrics_remote_write_series_rejected_total {}",
            metrics_ingest.series_rejected
        );
        let _ = writeln!(
            body,
            "# HELP reddb_metrics_remote_write_series_rejected_by_reason_total Metrics remote-write series rejected since process start by reason."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_metrics_remote_write_series_rejected_by_reason_total counter"
        );
        let _ = writeln!(
            body,
            "reddb_metrics_remote_write_series_rejected_by_reason_total{{reason=\"cardinality_budget\"}} {}",
            metrics_ingest.series_rejected_cardinality_budget
        );
        let _ = writeln!(
            body,
            "# HELP reddb_metrics_tenant_activity_total Metrics adapter requests by tenant, namespace, and operation since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_metrics_tenant_activity_total counter");
        for activity in self.runtime.metrics_tenant_activity_snapshot() {
            let _ = writeln!(
                body,
                "reddb_metrics_tenant_activity_total{{tenant=\"{}\",namespace=\"{}\",operation=\"{}\"}} {}",
                sanitize_label(&activity.tenant),
                sanitize_label(&activity.namespace),
                sanitize_label(&activity.operation),
                activity.count
            );
        }

        // PLAN.md Phase 11.4 — per-replica lag visibility. Emitted
        // when this primary has registered replicas; replicas that
        // haven't ack'd anything yet (`last_acked_lsn == 0`) still
        // show up so dashboards can detect "registered but stuck".
        let replicas = self.runtime.primary_replica_snapshots();
        let _ = writeln!(
            body,
            "# HELP reddb_replica_count Currently registered replicas."
        );
        let _ = writeln!(body, "# TYPE reddb_replica_count gauge");
        let _ = writeln!(body, "reddb_replica_count {}", replicas.len());
        if !replicas.is_empty() {
            let replica_lag_budget_secs = std::env::var("RED_SLO_REPLICA_LAG_BUDGET_SECONDS")
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite() && *value >= 0.0)
                .unwrap_or(60.0);
            let _ = writeln!(
                body,
                "# HELP reddb_replica_ack_lsn Most recent LSN acked by each replica."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_ack_lsn gauge");
            for r in &replicas {
                let _ = writeln!(
                    body,
                    "reddb_replica_ack_lsn{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    r.last_acked_lsn
                );
            }
            let _ = writeln!(
                body,
                "# HELP reddb_replica_applied_lsn Most recent LSN applied by each replica."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_applied_lsn gauge");
            for r in &replicas {
                let _ = writeln!(
                    body,
                    "reddb_replica_applied_lsn{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    r.last_acked_lsn
                );
            }
            let _ = writeln!(
                body,
                "# HELP reddb_replica_durable_lsn Most recent LSN durably persisted by each replica."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_durable_lsn gauge");
            for r in &replicas {
                let _ = writeln!(
                    body,
                    "reddb_replica_durable_lsn{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    r.last_durable_lsn
                );
            }
            let _ = writeln!(
                body,
                "# HELP reddb_replica_lag_records Real LSN distance from last sent LSN to applied LSN."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_lag_records gauge");
            for r in &replicas {
                let _ = writeln!(
                    body,
                    "reddb_replica_lag_records{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    r.last_sent_lsn.saturating_sub(r.last_acked_lsn)
                );
            }
            let _ = writeln!(
                body,
                "# HELP reddb_replica_apply_errors_total Replica-reported WAL apply errors since process start."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_apply_errors_total counter");
            for r in &replicas {
                let _ = writeln!(
                    body,
                    "reddb_replica_apply_errors_total{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    r.apply_error_count
                );
            }
            let _ = writeln!(
                body,
                "# HELP reddb_replica_divergence_total Replica-reported WAL divergence errors since process start."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_divergence_total counter");
            for r in &replicas {
                let _ = writeln!(
                    body,
                    "reddb_replica_divergence_total{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    r.divergence_count
                );
            }
            let _ = writeln!(
                body,
                "# HELP reddb_replica_lag_seconds Wall-clock seconds since the replica was last seen."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_lag_seconds gauge");
            let _ = writeln!(
                body,
                "# HELP reddb_slo_lag_budget_remaining_seconds Remaining per-replica lag budget; negative means SLO breach."
            );
            let _ = writeln!(body, "# TYPE reddb_slo_lag_budget_remaining_seconds gauge");
            for r in &replicas {
                let lag_ms = (now_ms as u128).saturating_sub(r.last_seen_at_unix_ms);
                let lag_secs = (lag_ms as f64) / 1000.0;
                let _ = writeln!(
                    body,
                    "reddb_replica_lag_seconds{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    lag_secs
                );
                let _ = writeln!(
                    body,
                    "reddb_slo_lag_budget_remaining_seconds{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    replica_lag_budget_secs - lag_secs
                );
            }
        }

        // Issue #826 — write-admission flow control on in-quorum replica
        // lag. Refresh from the live registry so the scrape reflects the
        // current throttle decision, then export the gate state, the soft
        // target, and the observed in-quorum lag. Emitted on every primary
        // scrape (even when disabled) so dashboards can chart engage/release.
        self.runtime.refresh_replication_flow_control();
        let flow = self.runtime.write_gate().flow_control();
        let _ = writeln!(
            body,
            "# HELP reddb_replication_flow_control_throttled 1 when write admission is throttled by in-quorum replica lag."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_replication_flow_control_throttled gauge"
        );
        let _ = writeln!(
            body,
            "reddb_replication_flow_control_throttled {}",
            u8::from(flow.is_throttled())
        );
        let _ = writeln!(
            body,
            "# HELP reddb_replication_flow_control_soft_target_lsn Soft target lag (LSN records) above which writes throttle; 0 disables."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_replication_flow_control_soft_target_lsn gauge"
        );
        let _ = writeln!(
            body,
            "reddb_replication_flow_control_soft_target_lsn {}",
            flow.soft_target_lsn()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_replication_flow_control_in_quorum_lag_lsn Most recent max lag (LSN records) across in-quorum replicas; excludes async read-replicas."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_replication_flow_control_in_quorum_lag_lsn gauge"
        );
        let _ = writeln!(
            body,
            "reddb_replication_flow_control_in_quorum_lag_lsn {}",
            flow.observed_lag_lsn()
        );

        // Issue #839 — full-resync / re-bootstrap counter is the primary
        // operator alert signal. A healthy cluster re-bootstraps rarely;
        // any sustained rise means slots are invalidated faster than
        // replicas keep up. The partial-resync counter is emitted beside
        // it so an alert can distinguish a benign reconnect storm (partial
        // climbing, full flat) from genuine slot loss.
        let _ = writeln!(
            body,
            "# HELP reddb_replication_full_resync_total Pulls that forced a replica full re-bootstrap since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_replication_full_resync_total counter");
        let _ = writeln!(
            body,
            "reddb_replication_full_resync_total {}",
            self.runtime.replication_full_resync_count()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_replication_partial_resync_total Pulls served as an incremental partial resync since process start."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_replication_partial_resync_total counter"
        );
        let _ = writeln!(
            body,
            "reddb_replication_partial_resync_total {}",
            self.runtime.replication_partial_resync_count()
        );

        // PLAN.md Phase 11.5 — replica apply error counters and
        // current health label. Counters are global across the
        // instance lifetime; the health label reflects whatever the
        // replica loop last persisted (`ok`, `connecting`, `gap`,
        // `divergence`, `apply_error`, `stalled_gap`).
        let _ = writeln!(
            body,
            "# HELP reddb_replica_apply_errors_total Replica WAL apply errors since process start, by kind."
        );
        let _ = writeln!(body, "# TYPE reddb_replica_apply_errors_total counter");
        for (kind, count) in self.runtime.replica_apply_error_counts() {
            let _ = writeln!(
                body,
                "reddb_replica_apply_errors_total{{kind=\"{}\"}} {}",
                kind.label(),
                count
            );
        }
        if let Some(health) = self.runtime.replica_apply_health() {
            let _ = writeln!(
                body,
                "# HELP reddb_replica_apply_health Replica apply state (label, value=1)."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_apply_health gauge");
            let _ = writeln!(
                body,
                "reddb_replica_apply_health{{state=\"{}\"}} 1",
                sanitize_label(&health)
            );
        }

        // PLAN.md Phase 4.4 — per-caller quota rejections. Empty
        // when the quota is unconfigured or no caller has been
        // throttled yet. Opportunistic eviction here keeps the
        // rejection map bounded on long-lived processes.
        self.runtime.quota_bucket().evict_idle();
        let rejections = self.runtime.quota_bucket().rejection_snapshot();
        if !rejections.is_empty() {
            let _ = writeln!(
                body,
                "# HELP reddb_quota_rejected_total Requests rejected by per-caller QPS quota."
            );
            let _ = writeln!(body, "# TYPE reddb_quota_rejected_total counter");
            for (principal, count) in &rejections {
                let _ = writeln!(
                    body,
                    "reddb_quota_rejected_total{{principal=\"{}\"}} {}",
                    sanitize_label(principal),
                    count
                );
            }
        }

        // PLAN.md Phase 11.4 — commit waiter outcome counters and
        // last-wait gauge. Operators alert when `timed_out` rises
        // (policy too tight or replicas stalled) and watch the
        // last-wait gauge for p95 trends.
        let (reached, timed_out, not_required, last_micros) =
            self.runtime.commit_waiter_metrics_snapshot();
        let _ = writeln!(
            body,
            "# HELP reddb_commit_wait_total Commit-wait outcomes by kind."
        );
        let _ = writeln!(body, "# TYPE reddb_commit_wait_total counter");
        let _ = writeln!(
            body,
            "reddb_commit_wait_total{{outcome=\"reached\"}} {}",
            reached
        );
        let _ = writeln!(
            body,
            "reddb_commit_wait_total{{outcome=\"timed_out\"}} {}",
            timed_out
        );
        let _ = writeln!(
            body,
            "reddb_commit_wait_total{{outcome=\"not_required\"}} {}",
            not_required
        );
        let _ = writeln!(
            body,
            "# HELP reddb_commit_wait_last_seconds Wall-clock seconds of the most recent commit wait."
        );
        let _ = writeln!(body, "# TYPE reddb_commit_wait_last_seconds gauge");
        let _ = writeln!(
            body,
            "reddb_commit_wait_last_seconds {}",
            (last_micros as f64) / 1_000_000.0
        );
        let _ = writeln!(
            body,
            "# HELP reddb_commit_watermark_lsn Highest LSN durable on the active synchronous commit quorum."
        );
        let _ = writeln!(body, "# TYPE reddb_commit_watermark_lsn gauge");
        let _ = writeln!(
            body,
            "reddb_commit_watermark_lsn {}",
            self.runtime.commit_watermark()
        );

        // PLAN.md Phase 11.4 — declared commit policy as a labeled
        // gauge so dashboards can confirm what the operator pinned.
        // The default `local` is emitted even when no replication is
        // configured, so the metric is always present.
        let policy = self.runtime.commit_policy();
        let _ = writeln!(
            body,
            "# HELP reddb_primary_commit_policy Active commit policy on the primary."
        );
        let _ = writeln!(body, "# TYPE reddb_primary_commit_policy gauge");
        let _ = writeln!(
            body,
            "reddb_primary_commit_policy{{policy=\"{}\"}} 1",
            policy.label()
        );

        // Blob Cache observability for the SQL result-cache adapter.
        // Per-namespace label cardinality is acceptable while the MVP namespace
        // cap stays near 256; raising that cap should move per-namespace detail
        // to an on-demand admin query and keep scrape metrics rolled up.
        let blob_ns = "runtime.result_cache";
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_get_total Blob Cache get outcomes by namespace."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_get_total counter");
        let _ = writeln!(
            body,
            "reddb_cache_blob_get_total{{namespace=\"{}\",result=\"hit_l1\"}} {}",
            blob_ns,
            result_blob_stats.hits()
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_get_total{{namespace=\"{}\",result=\"hit_l2\"}} 0",
            blob_ns
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_get_total{{namespace=\"{}\",result=\"miss\"}} {}",
            blob_ns,
            result_blob_stats.misses()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_put_total Blob Cache put outcomes by namespace."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_put_total counter");
        let _ = writeln!(
            body,
            "reddb_cache_blob_put_total{{namespace=\"{}\",outcome=\"ok\"}} {}",
            blob_ns,
            result_blob_stats.insertions()
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_put_total{{namespace=\"{}\",outcome=\"version_mismatch\"}} {}",
            blob_ns,
            result_blob_stats.version_mismatches()
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_put_total{{namespace=\"{}\",outcome=\"too_large\"}} 0",
            blob_ns
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_put_total{{namespace=\"{}\",outcome=\"metadata_too_large\"}} 0",
            blob_ns
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_invalidate_total Blob Cache invalidations by namespace and kind."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_invalidate_total counter");
        for (kind, count) in [
            ("key", 0),
            ("prefix", 0),
            ("tag", 0),
            ("dependency", result_blob_stats.invalidations()),
            ("namespace", result_blob_stats.namespace_flushes()),
        ] {
            let _ = writeln!(
                body,
                "reddb_cache_blob_invalidate_total{{namespace=\"{}\",kind=\"{}\"}} {}",
                blob_ns, kind, count
            );
        }
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_evict_total Blob Cache evictions by namespace and reason."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_evict_total counter");
        for (reason, count) in [
            ("capacity", result_blob_stats.evictions()),
            ("expiry", result_blob_stats.expirations()),
            ("policy", 0),
        ] {
            let _ = writeln!(
                body,
                "reddb_cache_blob_evict_total{{namespace=\"{}\",reason=\"{}\"}} {}",
                blob_ns, reason, count
            );
        }
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_l1_bytes_in_use L1 bytes currently used by Blob Cache namespace."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_l1_bytes_in_use gauge");
        let _ = writeln!(
            body,
            "reddb_cache_blob_l1_bytes_in_use{{namespace=\"{}\"}} {}",
            blob_ns,
            result_blob_stats.bytes_in_use()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_l1_entries L1 entries currently held by Blob Cache namespace."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_l1_entries gauge");
        let _ = writeln!(
            body,
            "reddb_cache_blob_l1_entries{{namespace=\"{}\"}} {}",
            blob_ns,
            result_blob_stats.entries()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_l2_bytes_in_use L2 bytes currently used by Blob Cache namespace."
        );
        let _ = writeln!(body, "# TYPE reddb_cache_blob_l2_bytes_in_use gauge");
        let _ = writeln!(
            body,
            "reddb_cache_blob_l2_bytes_in_use{{namespace=\"{}\"}} {}",
            blob_ns,
            result_blob_stats.l2_bytes_in_use()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_l2_full_rejections_total Blob Cache puts rejected because L2 is full."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_cache_blob_l2_full_rejections_total counter"
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_l2_full_rejections_total{{namespace=\"{}\"}} {}",
            blob_ns,
            result_blob_stats.l2_full_rejections()
        );
        let _ = writeln!(
            body,
            "# HELP reddb_cache_blob_version_mismatch_total Blob Cache CAS version mismatches by namespace."
        );
        let _ = writeln!(
            body,
            "# TYPE reddb_cache_blob_version_mismatch_total counter"
        );
        let _ = writeln!(
            body,
            "reddb_cache_blob_version_mismatch_total{{namespace=\"{}\"}} {}",
            blob_ns,
            result_blob_stats.version_mismatches()
        );

        let _ = writeln!(
            body,
            "# HELP reddb_kv_ops_total Normal-KV operations since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_kv_ops_total counter");
        for (verb, count) in [
            ("put", kv_stats.puts),
            ("get", kv_stats.gets),
            ("delete", kv_stats.deletes),
            ("incr", kv_stats.incrs),
        ] {
            let _ = writeln!(body, "reddb_kv_ops_total{{verb=\"{}\"}} {}", verb, count);
        }
        let _ = writeln!(
            body,
            "# HELP reddb_kv_cas_total Normal-KV CAS outcomes since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_kv_cas_total counter");
        let _ = writeln!(
            body,
            "reddb_kv_cas_total{{outcome=\"success\"}} {}",
            kv_stats.cas_success
        );
        let _ = writeln!(
            body,
            "reddb_kv_cas_total{{outcome=\"conflict\"}} {}",
            kv_stats.cas_conflict
        );
        let _ = writeln!(
            body,
            "# HELP reddb_kv_watch_streams_active Active normal-KV WATCH streams."
        );
        let _ = writeln!(body, "# TYPE reddb_kv_watch_streams_active gauge");
        let _ = writeln!(
            body,
            "reddb_kv_watch_streams_active {}",
            kv_stats.watch_streams_active
        );
        let _ = writeln!(
            body,
            "# HELP reddb_kv_watch_events_emitted_total Normal-KV WATCH events emitted since process start."
        );
        let _ = writeln!(body, "# TYPE reddb_kv_watch_events_emitted_total counter");
        let _ = writeln!(
            body,
            "reddb_kv_watch_events_emitted_total {}",
            kv_stats.watch_events_emitted
        );
        let _ = writeln!(
            body,
            "# HELP reddb_kv_watch_drops_total Normal-KV WATCH events dropped by bounded subscriber buffers."
        );
        let _ = writeln!(body, "# TYPE reddb_kv_watch_drops_total counter");
        let _ = writeln!(body, "reddb_kv_watch_drops_total {}", kv_stats.watch_drops);

        let _ = writeln!(
            body,
            "# HELP reddb_db_size_bytes On-disk size of the primary database file."
        );
        let _ = writeln!(body, "# TYPE reddb_db_size_bytes gauge");
        let _ = writeln!(body, "reddb_db_size_bytes {}", db_size_bytes);

        if let Some(secs) = cold_start_secs {
            let _ = writeln!(
                body,
                "# HELP reddb_cold_start_duration_seconds Seconds from process start to /health/ready 200."
            );
            let _ = writeln!(body, "# TYPE reddb_cold_start_duration_seconds gauge");
            let _ = writeln!(body, "reddb_cold_start_duration_seconds {}", secs);
        }

        // PLAN.md Phase 9.1 — per-phase cold-start breakdown.
        // Operators use this to identify which phase dominates the
        // cold-start budget (restore, WAL replay, index warmup).
        // Phases that haven't fired yet are simply absent — no zero
        // entries to confuse alert rules.
        let phases = lifecycle.cold_start_phases().durations_ms();
        if !phases.is_empty() {
            let _ = writeln!(
                body,
                "# HELP reddb_cold_start_phase_seconds Per-phase cold-start duration."
            );
            let _ = writeln!(body, "# TYPE reddb_cold_start_phase_seconds gauge");
            for (name, dur_ms) in phases {
                let _ = writeln!(
                    body,
                    "reddb_cold_start_phase_seconds{{phase=\"{}\"}} {}",
                    name,
                    (dur_ms as f64) / 1000.0
                );
            }
        }

        // Operator-imposed limits (PLAN.md Phase 4.1). Emitted as
        // gauges so external dashboards can graph headroom against
        // current usage. `0` means "no cap pinned at boot"; we
        // still emit it so absence vs presence is unambiguous.
        let limits = self.runtime.resource_limits();
        if let Some(v) = limits.max_db_size_bytes {
            let _ = writeln!(
                body,
                "# HELP reddb_limit_db_size_bytes Operator-pinned cap on the primary DB file size."
            );
            let _ = writeln!(body, "# TYPE reddb_limit_db_size_bytes gauge");
            let _ = writeln!(body, "reddb_limit_db_size_bytes {}", v);
        }
        if let Some(v) = limits.max_connections {
            let _ = writeln!(body, "# TYPE reddb_limit_connections gauge");
            let _ = writeln!(body, "reddb_limit_connections {}", v);
        }
        if let Some(v) = limits.max_qps {
            let _ = writeln!(body, "# TYPE reddb_limit_qps gauge");
            let _ = writeln!(body, "reddb_limit_qps {}", v);
        }
        if let Some(v) = limits.max_batch_size {
            let _ = writeln!(body, "# TYPE reddb_limit_batch_size gauge");
            let _ = writeln!(body, "reddb_limit_batch_size {}", v);
        }
        if let Some(v) = limits.max_memory_bytes {
            let _ = writeln!(body, "# TYPE reddb_limit_memory_bytes gauge");
            let _ = writeln!(body, "reddb_limit_memory_bytes {}", v);
        }

        // Queue lifecycle counters — slice 10 of issue #527 / ADR-0017.
        // Process-local counters per (queue, group, mode); the
        // pending gauge is scraped live from `red_queue_meta` so it
        // cannot drift from the source of truth. Cardinality is
        // bounded by the catalog: only queues/groups the operator
        // already created appear here.
        {
            let queue_telemetry = self.runtime.queue_telemetry_snapshot();
            let _ = writeln!(
                body,
                "# HELP queue_delivered_total Messages handed to a consumer (per queue/group/mode)."
            );
            let _ = writeln!(body, "# TYPE queue_delivered_total counter");
            for ((queue, group, mode), n) in &queue_telemetry.delivered {
                let _ = writeln!(
                    body,
                    "queue_delivered_total{{queue=\"{}\",group=\"{}\",mode=\"{}\"}} {}",
                    sanitize_label(queue),
                    sanitize_label(group),
                    sanitize_label(mode),
                    n
                );
            }
            let _ = writeln!(
                body,
                "# HELP queue_acked_total Messages acknowledged (per queue/group/mode)."
            );
            let _ = writeln!(body, "# TYPE queue_acked_total counter");
            for ((queue, group, mode), n) in &queue_telemetry.acked {
                let _ = writeln!(
                    body,
                    "queue_acked_total{{queue=\"{}\",group=\"{}\",mode=\"{}\"}} {}",
                    sanitize_label(queue),
                    sanitize_label(group),
                    sanitize_label(mode),
                    n
                );
            }
            let _ = writeln!(
                body,
                "# HELP queue_nacked_total Messages negatively-acknowledged (per queue/group/mode/outcome)."
            );
            let _ = writeln!(body, "# TYPE queue_nacked_total counter");
            for ((queue, group, mode, outcome), n) in &queue_telemetry.nacked {
                let _ = writeln!(
                    body,
                    "queue_nacked_total{{queue=\"{}\",group=\"{}\",mode=\"{}\",outcome=\"{}\"}} {}",
                    sanitize_label(queue),
                    sanitize_label(group),
                    sanitize_label(mode),
                    outcome,
                    n
                );
            }
            let pending = self.runtime.queue_pending_counts();
            let _ = writeln!(
                body,
                "# HELP queue_pending_gauge In-flight (delivered, not yet acked) messages per queue/group."
            );
            let _ = writeln!(body, "# TYPE queue_pending_gauge gauge");
            for ((queue, group), n) in &pending {
                let _ = writeln!(
                    body,
                    "queue_pending_gauge{{queue=\"{}\",group=\"{}\"}} {}",
                    sanitize_label(queue),
                    sanitize_label(group),
                    n
                );
            }

            // QUEUE READ … WAIT telemetry — slice D of PRD #718 (#729).
            // One started increment per park lifecycle; exactly one
            // terminal outcome (woken/timed_out/cancelled) per started.
            // Labels are (queue, scope) where scope is the registry
            // scope key — today the tenant id, empty in single-tenant.
            let render_wait_counter =
                |body: &mut String, name: &str, help: &str, samples: &[((String, String), u64)]| {
                    let _ = writeln!(body, "# HELP {} {}", name, help);
                    let _ = writeln!(body, "# TYPE {} counter", name);
                    for ((scope, queue), n) in samples {
                        let _ = writeln!(
                            body,
                            "{}{{queue=\"{}\",scope=\"{}\"}} {}",
                            name,
                            sanitize_label(queue),
                            sanitize_label(scope),
                            n
                        );
                    }
                };
            render_wait_counter(
                &mut body,
                "queue_wait_started_total",
                "QUEUE READ ... WAIT lifecycles that entered the park loop.",
                &queue_telemetry.wait_started,
            );
            render_wait_counter(
                &mut body,
                "queue_wait_woken_total",
                "QUEUE READ ... WAIT lifecycles that resolved by wake + delivery.",
                &queue_telemetry.wait_woken,
            );
            render_wait_counter(
                &mut body,
                "queue_wait_timed_out_total",
                "QUEUE READ ... WAIT lifecycles that resolved by WAIT budget expiry.",
                &queue_telemetry.wait_timed_out,
            );
            render_wait_counter(
                &mut body,
                "queue_wait_cancelled_total",
                "QUEUE READ ... WAIT lifecycles that resolved by registry cancellation.",
                &queue_telemetry.wait_cancelled,
            );

            let _ = writeln!(
                body,
                "# HELP queue_wait_duration_ms Wall-clock duration of QUEUE READ ... WAIT park lifecycles, milliseconds."
            );
            let _ = writeln!(body, "# TYPE queue_wait_duration_ms histogram");
            for ((scope, queue), hist) in &queue_telemetry.wait_duration {
                for (i, upper) in crate::runtime::queue_telemetry::WAIT_DURATION_BUCKETS_MS
                    .iter()
                    .enumerate()
                {
                    let count = hist.bucket_counts.get(i).copied().unwrap_or(0);
                    let _ = writeln!(
                        body,
                        "queue_wait_duration_ms_bucket{{queue=\"{}\",scope=\"{}\",le=\"{}\"}} {}",
                        sanitize_label(queue),
                        sanitize_label(scope),
                        upper,
                        count
                    );
                }
                let _ = writeln!(
                    body,
                    "queue_wait_duration_ms_bucket{{queue=\"{}\",scope=\"{}\",le=\"+Inf\"}} {}",
                    sanitize_label(queue),
                    sanitize_label(scope),
                    hist.count
                );
                let _ = writeln!(
                    body,
                    "queue_wait_duration_ms_sum{{queue=\"{}\",scope=\"{}\"}} {}",
                    sanitize_label(queue),
                    sanitize_label(scope),
                    hist.sum_ms
                );
                let _ = writeln!(
                    body,
                    "queue_wait_duration_ms_count{{queue=\"{}\",scope=\"{}\"}} {}",
                    sanitize_label(queue),
                    sanitize_label(scope),
                    hist.count
                );
            }
        }

        // Query latency histogram — issue #1241. Bounded to the `kind`
        // dimension (ADR 0060 §4); no SQL/collection/tenant/user labels.
        // Only kinds with a real sample are emitted (honesty rule §6).
        {
            let latency = self.runtime.query_latency_snapshot();
            if !latency.is_empty() {
                let _ = writeln!(
                    body,
                    "# HELP reddb_query_duration_seconds Query execution latency by kind, seconds."
                );
                let _ = writeln!(body, "# TYPE reddb_query_duration_seconds histogram");
                for hist in &latency {
                    for (i, le) in
                        crate::runtime::query_latency_telemetry::QUERY_DURATION_BUCKETS_SECONDS
                            .iter()
                            .enumerate()
                    {
                        let count = hist.bucket_counts.get(i).copied().unwrap_or(0);
                        let _ = writeln!(
                            body,
                            "reddb_query_duration_seconds_bucket{{kind=\"{}\",le=\"{}\"}} {}",
                            sanitize_label(hist.kind),
                            le,
                            count
                        );
                    }
                    let _ = writeln!(
                        body,
                        "reddb_query_duration_seconds_bucket{{kind=\"{}\",le=\"+Inf\"}} {}",
                        sanitize_label(hist.kind),
                        hist.count
                    );
                    let _ = writeln!(
                        body,
                        "reddb_query_duration_seconds_sum{{kind=\"{}\"}} {}",
                        sanitize_label(hist.kind),
                        hist.sum_seconds
                    );
                    let _ = writeln!(
                        body,
                        "reddb_query_duration_seconds_count{{kind=\"{}\"}} {}",
                        sanitize_label(hist.kind),
                        hist.count
                    );
                }
            }
        }

        // Events outbox metrics — issue #299
        {
            use crate::runtime::impl_queue::{
                EVENTS_DLQ_TOTAL, EVENTS_DRAIN_RETRIES_TOTAL, EVENTS_ENQUEUED_TOTAL,
            };
            let enqueued = EVENTS_ENQUEUED_TOTAL.load(std::sync::atomic::Ordering::Relaxed);
            let retries = EVENTS_DRAIN_RETRIES_TOTAL.load(std::sync::atomic::Ordering::Relaxed);
            let dlq_total = EVENTS_DLQ_TOTAL.load(std::sync::atomic::Ordering::Relaxed);

            let _ = writeln!(
                body,
                "# HELP reddb_events_enqueued_total Total events successfully pushed to target queues."
            );
            let _ = writeln!(body, "# TYPE reddb_events_enqueued_total counter");
            let _ = writeln!(body, "reddb_events_enqueued_total {enqueued}");

            let _ = writeln!(
                body,
                "# HELP reddb_events_drain_retries_total Total event push failures that triggered DLQ routing."
            );
            let _ = writeln!(body, "# TYPE reddb_events_drain_retries_total counter");
            let _ = writeln!(
                body,
                "reddb_events_drain_retries_total{{reason=\"queue_full\"}} {retries}"
            );

            let _ = writeln!(
                body,
                "# HELP reddb_events_dlq_total Total events routed to dead-letter queues."
            );
            let _ = writeln!(body, "# TYPE reddb_events_dlq_total counter");
            let _ = writeln!(body, "reddb_events_dlq_total {dlq_total}");
        }

        // AI provider and embedding metrics — issue #280.
        crate::runtime::ai::metrics::render_ai_metrics(&mut body);

        // Result / graph-analytics cache counters — issue #802. Stable
        // names match the METRIC_RESULT_CACHE_* constants in `runtime`.
        {
            let (hits, misses, evicts) = self.runtime.result_cache_metrics();
            let _ = writeln!(
                body,
                "# HELP reddb_result_cache_hit_total Result cache hits (incl. graph-analytics TVFs)."
            );
            let _ = writeln!(body, "# TYPE reddb_result_cache_hit_total counter");
            let _ = writeln!(body, "reddb_result_cache_hit_total {hits}");
            let _ = writeln!(
                body,
                "# HELP reddb_result_cache_miss_total Result cache misses (cold computes)."
            );
            let _ = writeln!(body, "# TYPE reddb_result_cache_miss_total counter");
            let _ = writeln!(body, "reddb_result_cache_miss_total {misses}");
            let _ = writeln!(
                body,
                "# HELP reddb_result_cache_evict_total Result cache entries evicted by capacity."
            );
            let _ = writeln!(body, "# TYPE reddb_result_cache_evict_total counter");
            let _ = writeln!(body, "reddb_result_cache_evict_total {evicts}");
        }

        // HTTP handler-thread pool metrics — issue #573 slice 4.
        // Renders four series (`http_active_handler_threads`,
        // `http_handler_cap`, `http_handler_rejected_total`,
        // `http_handler_duration_seconds`) so operators can observe
        // saturation against the bounded handler-thread cap.
        self.http_metrics().render(&mut body, self.http_limiter());

        // Issue #1239 — HTTP request/error volume by method, matched route
        // template, and status class. Read from the operational telemetry
        // substrate; this surface only shapes it (ADR 0017 boundary).
        self.http_request_metrics().render(&mut body);

        HttpResponse {
            status: 200,
            content_type: "text/plain; version=0.0.4",
            body: body.into_bytes(),
            extra_headers: Vec::new(),
        }
    }
}
