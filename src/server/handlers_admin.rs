//! Lifecycle / admin HTTP endpoints (PLAN.md Phase 1).
//!
//! Universal contract surface consumed by orchestrators (K8s preStop,
//! Fly autostop, ECS drain, systemd, custom).
//!
//! - `POST /admin/shutdown` — flush + checkpoint + optional backup,
//!   200 only when safe to die. Idempotent.
//! - `POST /admin/drain` — stop accepting new writes, in-flight finish,
//!   200 once drain complete. Soft pre-shutdown step.
//! - `GET  /health/live` — process responsive (always cheap).
//! - `GET  /health/ready` — accepts queries (WAL replay + restore done).
//! - `GET  /health/startup` — same logic as ready, K8s-style longer
//!   timeout window.

use super::*;
use crate::runtime::lifecycle::Phase;
use std::path::{Path, PathBuf};

/// Path to the persistent runtime-toggle file kept beside the
/// `.rdb` data file. Operators can prep a fresh deploy by writing
/// `{"read_only": true}` before first boot to come up locked.
pub(crate) fn runtime_state_path(data_path: &Path) -> PathBuf {
    let parent = data_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(".runtime-state.json")
}

/// Atomically persist the read_only toggle. Writes to a sibling
/// `.tmp` file then renames to defeat torn writes — same pattern
/// the snapshot publish path uses.
pub(crate) fn persist_runtime_readonly(state_path: &Path, enabled: bool) -> std::io::Result<()> {
    use std::io::Write;
    let mut object = crate::json::Map::new();
    object.insert("read_only".to_string(), crate::json::Value::Bool(enabled));
    let body = crate::serde_json::to_string_pretty(&crate::json::Value::Object(object))
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::Other, err.to_string()))?;
    if let Some(parent) = state_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = state_path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, state_path)?;
    Ok(())
}

/// Read a previously-persisted read_only toggle. Returns `None`
/// when the file doesn't exist or doesn't parse — boot continues
/// from the env-var / RedDBOptions value in that case.
pub fn load_runtime_readonly(data_path: &Path) -> Option<bool> {
    let state_path = runtime_state_path(data_path);
    let bytes = std::fs::read(&state_path).ok()?;
    let parsed: crate::json::Value = crate::json::from_slice(&bytes).ok()?;
    parsed.get("read_only").and_then(|v| v.as_bool())
}

/// PLAN.md Phase 11.6 — default lease holder id when the operator
/// doesn't pin one in the promotion request body. Mirrors the boot
/// loop's resolution (`RED_LEASE_HOLDER_ID` → `<hostname>-<pid>`).
fn default_holder_id() -> String {
    if let Some(explicit) = crate::utils::env_with_file_fallback("RED_LEASE_HOLDER_ID") {
        return explicit;
    }
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown-host".to_string());
    format!("{host}-{}", std::process::id())
}

/// Sanitize replica IDs for use as Prometheus label values.
/// Replaces double quotes, backslashes, and newlines so the resulting
/// metric line stays parseable. Operators picking aggressive replica
/// IDs is rare but malicious input must not break /metrics.
fn sanitize_label(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

impl RedDBServer {
    /// `POST /admin/shutdown` — graceful shutdown coordinator.
    /// Returns 200 with the shutdown report when complete; 200 with
    /// the cached report when already shut down (idempotent); 500
    /// on flush failure (process should still exit afterwards).
    ///
    /// The HTTP layer does not own process exit — that's the
    /// signal-handler / `run_server` driver. This handler reports
    /// state; orchestrators that posted SIGTERM separately will see
    /// the process die when their grace window elapses.
    pub(crate) fn handle_admin_shutdown(&self) -> HttpResponse {
        let backup_on_shutdown = std::env::var("RED_BACKUP_ON_SHUTDOWN")
            .ok()
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(true);

        match self.runtime.graceful_shutdown(backup_on_shutdown) {
            Ok(report) => {
                // PLAN.md Phase 6.5 — audit operator-triggered
                // shutdown. Recorded as "ok" + duration so the log
                // shipper can graph shutdown latency over time.
                let mut details = Map::new();
                details.insert(
                    "backup_uploaded".to_string(),
                    JsonValue::Bool(report.backup_uploaded),
                );
                details.insert(
                    "duration_ms".to_string(),
                    JsonValue::Number(report.duration_ms as f64),
                );
                self.runtime.audit_log().record(
                    "admin/shutdown",
                    "operator",
                    "instance",
                    "ok",
                    JsonValue::Object(details),
                );
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "phase".to_string(),
                    JsonValue::String(self.runtime.lifecycle().phase().as_str().to_string()),
                );
                object.insert(
                    "flushed_wal".to_string(),
                    JsonValue::Bool(report.flushed_wal),
                );
                object.insert(
                    "final_checkpoint".to_string(),
                    JsonValue::Bool(report.final_checkpoint),
                );
                object.insert(
                    "backup_uploaded".to_string(),
                    JsonValue::Bool(report.backup_uploaded),
                );
                object.insert(
                    "duration_ms".to_string(),
                    JsonValue::Number(report.duration_ms as f64),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(500, err.to_string()),
        }
    }

    /// `POST /admin/restore` — operator-triggered restore from the
    /// configured remote backend (PLAN.md Phase 3.2). Refuses unless
    /// the runtime is read_only / replica so live writes can't race
    /// the swap. Body fields are optional:
    /// `{"to_lsn": u64, "to_timestamp_ms": u64, "snapshot_id": str}`.
    /// Empty body restores to latest.
    pub(crate) fn handle_admin_restore(&self, body: Vec<u8>) -> HttpResponse {
        if !self.runtime.write_gate().is_read_only() {
            return json_error(
                409,
                "POST /admin/restore requires the runtime to be read_only or replica-role; \
                 toggle via RED_READONLY=true or POST /admin/readonly first",
            );
        }
        let db = self.runtime.db();
        let Some(backend) = db.options().remote_backend.clone() else {
            return json_error(412, "no remote backend configured (RED_BACKEND=none)");
        };
        let Some(local_path) = db.path().map(|p| p.to_path_buf()) else {
            return json_error(412, "in-memory runtime cannot be restored from remote");
        };
        let snapshot_prefix = db.options().default_snapshot_prefix();
        let wal_prefix = db.options().default_wal_archive_prefix();
        let target_time_ms = if body.is_empty() {
            0u64
        } else {
            match crate::serde_json::from_slice::<crate::serde_json::Value>(&body) {
                Ok(v) => v
                    .get("to_timestamp_ms")
                    .and_then(|n| n.as_u64())
                    .or_else(|| {
                        v.get("to_timestamp")
                            .and_then(|n| n.as_u64())
                            .map(|s| s.saturating_mul(1000))
                    })
                    .unwrap_or(0),
                Err(err) => return json_error(400, format!("invalid JSON body: {err}")),
            }
        };
        let recovery =
            crate::storage::wal::PointInTimeRecovery::new(backend, snapshot_prefix, wal_prefix);
        match recovery.restore_to(target_time_ms, &local_path) {
            Ok(report) => {
                let mut details = Map::new();
                details.insert(
                    "snapshot_used".to_string(),
                    JsonValue::Number(report.snapshot_used as f64),
                );
                details.insert(
                    "wal_segments_replayed".to_string(),
                    JsonValue::Number(report.wal_segments_replayed as f64),
                );
                details.insert(
                    "records_applied".to_string(),
                    JsonValue::Number(report.records_applied as f64),
                );
                details.insert(
                    "recovered_to_lsn".to_string(),
                    JsonValue::Number(report.recovered_to_lsn as f64),
                );
                details.insert(
                    "recovered_to_time".to_string(),
                    JsonValue::Number(report.recovered_to_time as f64),
                );
                self.runtime.audit_log().record(
                    "admin/restore",
                    "operator",
                    "instance",
                    "ok",
                    JsonValue::Object(details.clone()),
                );
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                for (k, v) in details {
                    object.insert(k, v);
                }
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => {
                self.runtime.audit_log().record(
                    "admin/restore",
                    "operator",
                    "instance",
                    &format!("err: {err}"),
                    JsonValue::Null,
                );
                json_error(500, err.to_string())
            }
        }
    }

    /// `POST /admin/backup` — operator-triggered backup, alias of
    /// `/backup/trigger` placed under the universal `/admin/*`
    /// namespace per PLAN.md Phase 3.3.
    pub(crate) fn handle_admin_backup(
        &self,
        _query: &std::collections::BTreeMap<String, String>,
    ) -> HttpResponse {
        match self.runtime.trigger_backup() {
            Ok(result) => {
                let mut details = Map::new();
                details.insert(
                    "snapshot_id".to_string(),
                    JsonValue::Number(result.snapshot_id as f64),
                );
                details.insert("uploaded".to_string(), JsonValue::Bool(result.uploaded));
                details.insert(
                    "duration_ms".to_string(),
                    JsonValue::Number(result.duration_ms as f64),
                );
                self.runtime.audit_log().record(
                    "admin/backup",
                    "operator",
                    "instance",
                    "ok",
                    JsonValue::Object(details.clone()),
                );
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                for (k, v) in details {
                    object.insert(k, v);
                }
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => {
                self.runtime.audit_log().record(
                    "admin/backup",
                    "operator",
                    "instance",
                    &format!("err: {err}"),
                    JsonValue::Null,
                );
                json_error(500, err.to_string())
            }
        }
    }

    /// `POST /admin/readonly` — flip the public-mutation gate
    /// (PLAN.md Phase 4.3).
    ///
    /// Body: `{"enabled": true|false}`. Returns the new state. Useful
    /// for orchestrators that need to suspend writes (maintenance,
    /// billing suspension, hot key rotation) without killing the
    /// process or detaching the volume. Replicas reject writes
    /// regardless of this flag — the replication-role gate fires
    /// first.
    ///
    /// Persistence: the new state is written to
    /// `<data_dir>/.runtime-state.json` so a subsequent restart
    /// re-applies it. Failure to persist returns 500 — the in-memory
    /// flag is reverted so caller and disk stay consistent.
    pub(crate) fn handle_admin_readonly(&self, body: Vec<u8>) -> HttpResponse {
        let enabled = if body.is_empty() {
            true
        } else {
            match crate::serde_json::from_slice::<crate::serde_json::Value>(&body) {
                Ok(v) => v.get("enabled").and_then(|n| n.as_bool()).unwrap_or(true),
                Err(err) => return json_error(400, format!("invalid JSON body: {err}")),
            }
        };

        let previous = self.runtime.write_gate().set_read_only(enabled);

        // Persist the toggle so a subsequent restart re-applies it
        // before any client surface comes online. Best-effort: on
        // failure we revert the in-memory flag so disk and runtime
        // agree (operator can then re-issue once the storage issue
        // is resolved).
        if let Some(data_path) = self.runtime.db().path() {
            let state_path = runtime_state_path(data_path);
            if let Err(err) = persist_runtime_readonly(&state_path, enabled) {
                self.runtime.write_gate().set_read_only(previous);
                return json_error(
                    500,
                    format!("read_only persisted to {state_path:?} failed: {err}"),
                );
            }
        }

        let mut details = Map::new();
        details.insert("enabled".to_string(), JsonValue::Bool(enabled));
        details.insert("previous".to_string(), JsonValue::Bool(previous));
        self.runtime.audit_log().record(
            "admin/readonly",
            "operator",
            "instance",
            "ok",
            JsonValue::Object(details),
        );
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("read_only".to_string(), JsonValue::Bool(enabled));
        object.insert("previous".to_string(), JsonValue::Bool(previous));
        json_response(200, JsonValue::Object(object))
    }

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
                "# HELP reddb_replica_lag_records Distance from primary current LSN to replica acked LSN."
            );
            let _ = writeln!(body, "# TYPE reddb_replica_lag_records gauge");
            for r in &replicas {
                let _ = writeln!(
                    body,
                    "reddb_replica_lag_records{{replica_id=\"{}\"}} {}",
                    sanitize_label(&r.id),
                    current_lsn.saturating_sub(r.last_acked_lsn)
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

        HttpResponse {
            status: 200,
            content_type: "text/plain; version=0.0.4",
            body: body.into_bytes(),
        }
    }

    /// `GET /admin/status` — full structured snapshot of operator-
    /// relevant state (PLAN.md Phase 5.4). One JSON object that
    /// frontend dashboards / control-plane sidecars can poll
    /// without scraping multiple endpoints.
    pub(crate) fn handle_admin_status(&self) -> HttpResponse {
        let lifecycle = self.runtime.lifecycle();
        let phase = lifecycle.phase();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let uptime_secs = (now_ms.saturating_sub(lifecycle.started_at_ms()) as f64) / 1000.0;
        let read_only = self.runtime.write_gate().is_read_only();
        let role = match self.runtime.write_gate().role() {
            crate::replication::ReplicationRole::Standalone => "standalone",
            crate::replication::ReplicationRole::Primary => "primary",
            crate::replication::ReplicationRole::Replica { .. } => "replica",
        };
        let db = self.runtime.db();
        let db_size_bytes = db
            .path()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0);
        let backend_kind = db
            .options()
            .remote_backend
            .as_ref()
            .map(|b| b.name().to_string());

        let mut object = Map::new();
        object.insert(
            "version".to_string(),
            JsonValue::String(env!("CARGO_PKG_VERSION").to_string()),
        );
        object.insert(
            "phase".to_string(),
            JsonValue::String(phase.as_str().to_string()),
        );
        object.insert(
            "uptime_secs".to_string(),
            JsonValue::Number((uptime_secs * 1000.0).round() / 1000.0),
        );
        object.insert(
            "started_at_unix_ms".to_string(),
            JsonValue::Number(lifecycle.started_at_ms() as f64),
        );
        if let Some(ready_at) = lifecycle.ready_at_ms() {
            object.insert(
                "ready_at_unix_ms".to_string(),
                JsonValue::Number(ready_at as f64),
            );
        }
        object.insert(
            "db_size_bytes".to_string(),
            JsonValue::Number(db_size_bytes as f64),
        );
        object.insert("read_only".to_string(), JsonValue::Bool(read_only));
        object.insert(
            "replication_role".to_string(),
            JsonValue::String(role.to_string()),
        );
        object.insert(
            "writer_lease".to_string(),
            JsonValue::String(self.runtime.write_gate().lease_state().label().to_string()),
        );

        // PLAN.md Phase 6.3 — surface encryption-at-rest configuration
        // so dashboards / `red doctor` can flag a misconfigured key
        // (Err on parse) before it silently leaves data plaintext.
        let (enc_state, enc_error) = self.runtime.encryption_at_rest_status();
        let mut enc_obj = Map::new();
        enc_obj.insert(
            "state".to_string(),
            JsonValue::String(enc_state.to_string()),
        );
        if let Some(err) = enc_error {
            enc_obj.insert("error".to_string(), JsonValue::String(err));
        }
        object.insert("encryption_at_rest".to_string(), JsonValue::Object(enc_obj));

        // Backup posture (PLAN.md Phase 5.1). `last_backup` carries
        // the same shape /metrics emits so dashboards and alert rules
        // share a single contract.
        let backup = self.runtime.backup_status();
        let mut backup_obj = Map::new();
        if let Some(last) = backup.last_backup.as_ref() {
            backup_obj.insert(
                "last_success_unix_ms".to_string(),
                JsonValue::Number(last.timestamp as f64),
            );
            backup_obj.insert(
                "last_duration_ms".to_string(),
                JsonValue::Number(last.duration_ms as f64),
            );
            backup_obj.insert(
                "age_seconds".to_string(),
                JsonValue::Number(((now_ms.saturating_sub(last.timestamp)) as f64) / 1000.0),
            );
        }
        backup_obj.insert(
            "total_successes".to_string(),
            JsonValue::Number(backup.total_backups as f64),
        );
        backup_obj.insert(
            "total_failures".to_string(),
            JsonValue::Number(backup.total_failures as f64),
        );
        backup_obj.insert(
            "interval_secs".to_string(),
            JsonValue::Number(backup.interval_secs as f64),
        );
        object.insert("backup".to_string(), JsonValue::Object(backup_obj));

        // WAL archive lag.
        let (current_lsn, last_archived_lsn) = self.runtime.wal_archive_progress();
        let mut wal_obj = Map::new();
        wal_obj.insert(
            "current_lsn".to_string(),
            JsonValue::Number(current_lsn as f64),
        );
        wal_obj.insert(
            "last_archived_lsn".to_string(),
            JsonValue::Number(last_archived_lsn as f64),
        );
        wal_obj.insert(
            "archive_lag_records".to_string(),
            JsonValue::Number(current_lsn.saturating_sub(last_archived_lsn) as f64),
        );
        object.insert("wal".to_string(), JsonValue::Object(wal_obj));

        // PLAN.md Phase 11.5 — replica apply health + counters.
        // Always emit so dashboards have a stable shape; missing
        // health label means this isn't a replica or no apply has
        // happened yet.
        let mut replica_obj = Map::new();
        if let Some(health) = self.runtime.replica_apply_health() {
            replica_obj.insert("apply_health".to_string(), JsonValue::String(health));
        }
        let mut errors_obj = Map::new();
        for (kind, count) in self.runtime.replica_apply_error_counts() {
            errors_obj.insert(kind.label().to_string(), JsonValue::Number(count as f64));
        }
        replica_obj.insert("apply_errors".to_string(), JsonValue::Object(errors_obj));
        // Per-replica array (primary view). Empty on replica/standalone.
        let snaps = self.runtime.primary_replica_snapshots();
        if !snaps.is_empty() {
            let arr: Vec<JsonValue> = snaps
                .iter()
                .map(|r| {
                    let mut o = Map::new();
                    o.insert("id".to_string(), JsonValue::String(r.id.clone()));
                    o.insert(
                        "last_acked_lsn".to_string(),
                        JsonValue::Number(r.last_acked_lsn as f64),
                    );
                    o.insert(
                        "last_sent_lsn".to_string(),
                        JsonValue::Number(r.last_sent_lsn as f64),
                    );
                    o.insert(
                        "last_durable_lsn".to_string(),
                        JsonValue::Number(r.last_durable_lsn as f64),
                    );
                    o.insert(
                        "last_seen_at_unix_ms".to_string(),
                        JsonValue::Number(r.last_seen_at_unix_ms as f64),
                    );
                    o.insert(
                        "lag_records".to_string(),
                        JsonValue::Number(current_lsn.saturating_sub(r.last_acked_lsn) as f64),
                    );
                    if let Some(region) = &r.region {
                        o.insert("region".to_string(), JsonValue::String(region.clone()));
                    }
                    JsonValue::Object(o)
                })
                .collect();
            replica_obj.insert("primary_view".to_string(), JsonValue::Array(arr));
        }
        replica_obj.insert(
            "commit_policy".to_string(),
            JsonValue::String(self.runtime.commit_policy().label().to_string()),
        );
        // PLAN.md Phase 11.4 — durable-LSN map per replica for
        // ack_n debugging. Empty until at least one ack lands.
        let durable = self.runtime.commit_waiter_snapshot();
        if !durable.is_empty() {
            let arr: Vec<JsonValue> = durable
                .into_iter()
                .map(|(id, lsn)| {
                    let mut o = Map::new();
                    o.insert("replica_id".to_string(), JsonValue::String(id));
                    o.insert("durable_lsn".to_string(), JsonValue::Number(lsn as f64));
                    JsonValue::Object(o)
                })
                .collect();
            replica_obj.insert("durable_view".to_string(), JsonValue::Array(arr));
        }
        object.insert("replica".to_string(), JsonValue::Object(replica_obj));
        if let Some(backend) = backend_kind {
            object.insert("remote_backend".to_string(), JsonValue::String(backend));
        }
        // PLAN.md Phase 4.1 — operator-imposed limits surface so
        // external dashboards can show headroom alongside usage.
        let limits = self.runtime.resource_limits();
        let mut limits_obj = Map::new();
        if let Some(v) = limits.max_db_size_bytes {
            limits_obj.insert("max_db_size_bytes".to_string(), JsonValue::Number(v as f64));
        }
        if let Some(v) = limits.max_connections {
            limits_obj.insert("max_connections".to_string(), JsonValue::Number(v as f64));
        }
        if let Some(v) = limits.max_qps {
            limits_obj.insert("max_qps".to_string(), JsonValue::Number(v as f64));
        }
        if let Some(v) = limits.max_batch_size {
            limits_obj.insert("max_batch_size".to_string(), JsonValue::Number(v as f64));
        }
        if let Some(v) = limits.max_memory_bytes {
            limits_obj.insert("max_memory_bytes".to_string(), JsonValue::Number(v as f64));
        }
        if let Some(d) = limits.max_query_duration {
            limits_obj.insert(
                "max_query_duration_ms".to_string(),
                JsonValue::Number(d.as_millis() as f64),
            );
        }
        if let Some(v) = limits.max_result_bytes {
            limits_obj.insert("max_result_bytes".to_string(), JsonValue::Number(v as f64));
        }
        object.insert("limits".to_string(), JsonValue::Object(limits_obj));

        if let Some(report) = lifecycle.shutdown_report() {
            let mut shutdown_obj = Map::new();
            shutdown_obj.insert(
                "duration_ms".to_string(),
                JsonValue::Number(report.duration_ms as f64),
            );
            shutdown_obj.insert(
                "flushed_wal".to_string(),
                JsonValue::Bool(report.flushed_wal),
            );
            shutdown_obj.insert(
                "backup_uploaded".to_string(),
                JsonValue::Bool(report.backup_uploaded),
            );
            object.insert("shutdown".to_string(), JsonValue::Object(shutdown_obj));
        }
        json_response(200, JsonValue::Object(object))
    }

    /// `POST /admin/drain` — flip to Draining phase. Subsequent
    /// `WriteGate`-checked writes will be rejected until shutdown
    /// completes or another phase override re-enables Ready.
    /// Idempotent.
    /// `POST /admin/failover/promote` — manual replica → primary
    /// promotion (PLAN.md Phase 11.6).
    ///
    /// Hard checks before bumping the lease generation:
    ///   * Caller is currently a replica (role guard) — primaries
    ///     don't promote themselves.
    ///   * Remote backend is configured (lease lives there).
    ///   * Replica apply health is `ok` — no unresolved WAL gap or
    ///     divergence. A replica that's behind cannot become the
    ///     authoritative writer.
    ///   * Lease can be acquired — `try_acquire` returns success.
    ///     Failure surfaces the existing holder so the operator
    ///     understands why.
    ///
    /// Body: `{"holder_id": "...", "ttl_ms": <u64>}`. `holder_id`
    /// defaults to `RED_LEASE_HOLDER_ID` env / `<hostname>-<pid>`.
    /// `ttl_ms` defaults to 60_000.
    ///
    /// On success the response includes the new lease's generation
    /// and acquired_at. **Promotion does NOT flip the running role
    /// to primary** — the operator's runbook is to restart the
    /// process with `RED_REPLICATION_MODE=primary` after a
    /// successful promotion. Auto-role-flip is a Phase 11.6 follow-
    /// up that requires draining live read traffic safely.
    pub(crate) fn handle_admin_failover_promote(&self, body: Vec<u8>) -> HttpResponse {
        // Role guard.
        if !matches!(
            self.runtime.write_gate().role(),
            crate::replication::ReplicationRole::Replica { .. }
        ) {
            return json_error(
                409,
                "promotion only allowed on a replica (current role is not Replica)",
            );
        }

        // Backend guard.
        let Some(backend) = self.runtime.db().options().remote_backend_atomic.clone() else {
            return json_error(
                412,
                "promotion requires a CAS-capable remote backend (use s3, fs, or http with RED_HTTP_CONDITIONAL_WRITES=true)",
            );
        };

        // Apply health guard. Anything other than `ok` / `healthy`
        // / `connecting` indicates the replica isn't current.
        let health = self.runtime.replica_apply_health().unwrap_or_default();
        if matches!(
            health.as_str(),
            "stalled_gap" | "divergence" | "apply_error"
        ) {
            return json_error(
                409,
                format!(
                    "promotion refused — replica apply state is `{health}`; resolve before promoting"
                ),
            );
        }

        // Body parsing.
        let (holder_id, ttl_ms) = if body.is_empty() {
            (default_holder_id(), 60_000u64)
        } else {
            match crate::serde_json::from_slice::<crate::serde_json::Value>(&body) {
                Ok(v) => {
                    let holder = v
                        .get("holder_id")
                        .and_then(|n| n.as_str())
                        .map(|s| s.to_string())
                        .unwrap_or_else(default_holder_id);
                    let ttl = v
                        .get("ttl_ms")
                        .and_then(|n| n.as_u64())
                        .filter(|t| *t > 0)
                        .unwrap_or(60_000);
                    (holder, ttl)
                }
                Err(err) => return json_error(400, format!("invalid JSON body: {err}")),
            }
        };

        let database_key = self
            .runtime
            .db()
            .options()
            .remote_key
            .clone()
            .unwrap_or_else(|| "main".to_string());
        let store = crate::replication::LeaseStore::new(backend);

        match crate::runtime::lease_lifecycle::admin_promote_lease(
            &store,
            self.runtime.audit_log(),
            &database_key,
            &holder_id,
            ttl_ms,
        ) {
            Ok(lease) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("holder_id".to_string(), JsonValue::String(lease.holder_id));
                object.insert(
                    "generation".to_string(),
                    JsonValue::Number(lease.generation as f64),
                );
                object.insert(
                    "acquired_at_ms".to_string(),
                    JsonValue::Number(lease.acquired_at_ms as f64),
                );
                object.insert(
                    "expires_at_ms".to_string(),
                    JsonValue::Number(lease.expires_at_ms as f64),
                );
                object.insert(
                    "next_step".to_string(),
                    JsonValue::String(
                        "restart with RED_REPLICATION_MODE=primary to start accepting writes"
                            .to_string(),
                    ),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(409, format!("promotion refused: {err}")),
        }
    }

    /// `GET /admin/openapi(.yaml)` — serve the public admin API
    /// spec (PLAN.md Phase 10.3). The YAML file is embedded at
    /// build time so the running binary always serves the spec
    /// matching its own surface, even if the deploy target lacks
    /// access to the source repo.
    pub(crate) fn handle_admin_openapi(&self) -> HttpResponse {
        const SPEC: &str = include_str!("../../docs/spec/admin-api.openapi.yaml");
        HttpResponse {
            status: 200,
            content_type: "application/yaml",
            body: SPEC.as_bytes().to_vec(),
        }
    }

    pub(crate) fn handle_admin_drain(&self) -> HttpResponse {
        self.runtime.lifecycle().mark_draining();
        self.runtime.audit_log().record(
            "admin/drain",
            "operator",
            "instance",
            "ok",
            JsonValue::Null,
        );
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "phase".to_string(),
            JsonValue::String(self.runtime.lifecycle().phase().as_str().to_string()),
        );
        json_response(200, JsonValue::Object(object))
    }

    /// `GET /health/live` — process is alive and responsive. Always
    /// 200 once the runtime is constructed; 503 only after Stopped.
    /// Never touches I/O.
    pub(crate) fn handle_health_live(&self) -> HttpResponse {
        let phase = self.runtime.lifecycle().phase();
        let alive = !matches!(phase, Phase::Stopped);
        let status = if alive { 200 } else { 503 };
        let mut object = Map::new();
        object.insert(
            "status".to_string(),
            JsonValue::String(if alive { "alive" } else { "stopped" }.to_string()),
        );
        object.insert(
            "phase".to_string(),
            JsonValue::String(phase.as_str().to_string()),
        );
        json_response(status, JsonValue::Object(object))
    }

    /// `GET /health/ready` — runtime is fully past WAL replay /
    /// restore-from-remote and accepts queries.
    pub(crate) fn handle_health_ready(&self) -> HttpResponse {
        self.health_ready_response("ready")
    }

    /// `GET /health/startup` — Kubernetes startup probe variant.
    /// Same readiness logic as `/health/ready`; orchestrator gives
    /// it a longer grace window before failing the pod.
    pub(crate) fn handle_health_startup(&self) -> HttpResponse {
        self.health_ready_response("startup")
    }

    fn health_ready_response(&self, probe: &str) -> HttpResponse {
        let lifecycle = self.runtime.lifecycle();
        let phase = lifecycle.phase();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let started_at = lifecycle.started_at_ms();
        let since_secs = (now.saturating_sub(started_at) as f64) / 1000.0;
        let mut object = Map::new();
        object.insert("probe".to_string(), JsonValue::String(probe.to_string()));
        object.insert(
            "phase".to_string(),
            JsonValue::String(phase.as_str().to_string()),
        );
        object.insert(
            "since_secs".to_string(),
            JsonValue::Number((since_secs * 1000.0).round() / 1000.0),
        );
        if let Some(ready_at) = lifecycle.ready_at_ms() {
            object.insert(
                "ready_at_unix_ms".to_string(),
                JsonValue::Number(ready_at as f64),
            );
        }

        if phase.accepts_queries() {
            object.insert("status".to_string(), JsonValue::String("ready".to_string()));
            json_response(200, JsonValue::Object(object))
        } else {
            object.insert(
                "status".to_string(),
                JsonValue::String(phase.as_str().to_string()),
            );
            if let Some(reason) = lifecycle.not_ready_reason() {
                object.insert("reason".to_string(), JsonValue::String(reason));
            } else {
                object.insert(
                    "reason".to_string(),
                    JsonValue::String(match phase {
                        Phase::Starting => "starting".to_string(),
                        Phase::ShuttingDown => "shutting_down".to_string(),
                        Phase::Stopped => "stopped".to_string(),
                        Phase::Draining => "draining".to_string(),
                        Phase::Ready => "ready".to_string(),
                    }),
                );
            }
            json_response(503, JsonValue::Object(object))
        }
    }
}
