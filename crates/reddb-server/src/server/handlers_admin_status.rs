//! Structured admin status HTTP endpoint.

use super::*;

impl RedDBServer {
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

    /// `GET /cluster/status` — Red UI cluster status snapshot (#738).
    ///
    /// Aggregates deployment shape, runtime health, storage, WAL, system
    /// resources, and replication facts behind one stable contract so
    /// the UI can render a cluster status page without stitching
    /// multiple endpoints. Fields the engine cannot measure reliably
    /// today are returned as a structured
    /// `{ "available": false, "reason": "..." }` envelope rather than
    /// fabricated — see the #738 thread-discussion decision.
    pub(crate) fn handle_cluster_status(&self) -> HttpResponse {
        use crate::presentation::cluster_status_json::{
            cluster_status_json, ClusterStatusInputs, ConnectionSnapshot, DeploymentShapeView,
            LatencySample, ProcessRoleView, ReplicaView, ReplicationSnapshot, StorageSnapshot,
            SystemSnapshot, TransportListenerView, TransportSnapshot, WalSnapshot,
        };

        let lifecycle = self.runtime.lifecycle();
        let phase = lifecycle.phase();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let uptime_secs = (now_ms.saturating_sub(lifecycle.started_at_ms()) as f64) / 1000.0;

        let role_view = match self.runtime.write_gate().role() {
            crate::replication::ReplicationRole::Standalone => ProcessRoleView::Standalone,
            crate::replication::ReplicationRole::Primary => ProcessRoleView::Primary,
            crate::replication::ReplicationRole::Replica { .. } => ProcessRoleView::Replica,
        };

        let db = self.runtime.db();
        let db_size_bytes = db
            .path()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len());
        let remote_backend = db
            .options()
            .remote_backend
            .as_ref()
            .map(|b| b.name().to_string());

        let (enc_state, enc_error) = self.runtime.encryption_at_rest_status();

        let active = self
            .options
            .transport_readiness
            .active
            .iter()
            .map(|l| TransportListenerView {
                transport: l.transport.clone(),
                bind_addr: l.bind_addr.clone(),
                explicit: l.explicit,
                reason: None,
            })
            .collect();
        let failed = self
            .options
            .transport_readiness
            .failed
            .iter()
            .map(|l| TransportListenerView {
                transport: l.transport.clone(),
                bind_addr: l.bind_addr.clone(),
                explicit: l.explicit,
                reason: Some(l.reason.clone()),
            })
            .collect();

        let runtime_stats = self.runtime.stats();
        let limits = self.runtime.resource_limits();

        let (current_lsn, last_archived_lsn) = self.runtime.wal_archive_progress();

        let system = &runtime_stats.system;
        let system_view = SystemSnapshot {
            pid: system.pid,
            cpu_cores: system.cpu_cores,
            os: system.os.clone(),
            arch: system.arch.clone(),
            hostname: system.hostname.clone(),
            // `SystemInfo` returns 0 on platforms where the engine
            // cannot probe memory (currently anything non-Linux). Map
            // that to `None` so the JSON envelope is honest about
            // measurement absence rather than reporting `0`.
            total_memory_bytes: if system.total_memory_bytes == 0 {
                None
            } else {
                Some(system.total_memory_bytes)
            },
            available_memory_bytes: if system.available_memory_bytes == 0 {
                None
            } else {
                Some(system.available_memory_bytes)
            },
        };

        let replicas = self
            .runtime
            .primary_replica_snapshots()
            .into_iter()
            .map(|r| ReplicaView {
                id: r.id,
                last_acked_lsn: r.last_acked_lsn,
                last_sent_lsn: r.last_sent_lsn,
                last_durable_lsn: r.last_durable_lsn,
                last_seen_at_unix_ms: r.last_seen_at_unix_ms,
                region: r.region,
            })
            .collect();
        let apply_errors = self
            .runtime
            .replica_apply_error_counts()
            .iter()
            .map(|(kind, count)| (kind.label().to_string(), *count))
            .collect();

        // Issue #1241 — derive overall latency percentiles from the
        // recorded histogram rollup. Stays `None` (honest unavailable
        // envelope) until at least one query has been sampled.
        let latency_rollup = self.runtime.query_latency_rollup();
        let latency = match (
            latency_rollup.quantile(0.50),
            latency_rollup.quantile(0.95),
            latency_rollup.quantile(0.99),
        ) {
            (Some(p50), Some(p95), Some(p99)) => Some(LatencySample {
                p50_seconds: p50,
                p95_seconds: p95,
                p99_seconds: p99,
                sample_count: latency_rollup.count,
            }),
            _ => None,
        };

        // Issue #1245 — node load snapshot (active queries + churn counters).
        let node_load = self.runtime.node_load_snapshot();
        let load = if node_load.has_activity() {
            Some(node_load)
        } else {
            None
        };

        let inputs = ClusterStatusInputs {
            snapshot_at_unix_ms: now_ms,
            version: env!("CARGO_PKG_VERSION").to_string(),
            phase: phase.as_str().to_string(),
            uptime_secs,
            started_at_unix_ms: lifecycle.started_at_ms(),
            ready_at_unix_ms: lifecycle.ready_at_ms(),
            read_only: self.runtime.write_gate().is_read_only(),
            // This handler is only reachable from the network-serving
            // runtime (this crate's HTTP listener). Per the #738
            // thread-discussion we report `server` rather than
            // fabricating a richer classification we cannot prove.
            deployment_shape: DeploymentShapeView::Server,
            process_role: role_view,
            transport: TransportSnapshot { active, failed },
            connections: ConnectionSnapshot {
                active: runtime_stats.active_connections as u64,
                idle: runtime_stats.idle_connections as u64,
                total_checkouts: runtime_stats.total_checkouts,
                max: limits.max_connections,
            },
            storage: StorageSnapshot {
                db_size_bytes,
                remote_backend,
                encryption_state: enc_state.to_string(),
                encryption_error: enc_error,
                paged_mode: runtime_stats.paged_mode,
            },
            wal: WalSnapshot {
                current_lsn,
                last_archived_lsn,
            },
            system: system_view,
            replication: ReplicationSnapshot {
                role: role_view,
                commit_policy: self.runtime.commit_policy().label().to_string(),
                replicas,
                apply_health: self.runtime.replica_apply_health(),
                apply_errors,
            },
            latency,
            load,
        };

        json_response(200, cluster_status_json(&inputs))
    }
}
