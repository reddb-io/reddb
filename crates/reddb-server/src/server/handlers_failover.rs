//! Failover and promotion admin HTTP endpoints.

use super::*;

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

impl RedDBServer {
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
            let reason = "promotion only allowed on a replica (current role is not Replica)";
            if let Err(err) = self.runtime.emit_control_event(
                crate::runtime::control_events::EventKind::FailoverPromotion,
                crate::runtime::control_events::Outcome::Denied,
                "failover_promote",
                Some("replication:role".to_string()),
                Some(reason.to_string()),
                Vec::new(),
            ) {
                return json_error(500, err.to_string());
            }
            return json_error(409, reason);
        }

        // Backend guard.
        let Some(backend) = self.runtime.db().options().remote_backend_atomic.clone() else {
            let reason = "promotion requires a CAS-capable remote backend (use s3, fs, or http with RED_HTTP_CONDITIONAL_WRITES=true)";
            if let Err(err) = self.runtime.emit_control_event(
                crate::runtime::control_events::EventKind::FailoverPromotion,
                crate::runtime::control_events::Outcome::Denied,
                "failover_promote",
                Some("replication:backend".to_string()),
                Some(reason.to_string()),
                Vec::new(),
            ) {
                return json_error(500, err.to_string());
            }
            return json_error(412, reason);
        };

        // Apply health guard. Anything other than `ok` / `healthy`
        // / `connecting` indicates the replica isn't current.
        let health = self.runtime.replica_apply_health().unwrap_or_default();
        if matches!(
            health.as_str(),
            "stalled_gap" | "divergence" | "apply_error"
        ) {
            let reason = format!(
                "promotion refused — replica apply state is `{health}`; resolve before promoting"
            );
            if let Err(err) = self.runtime.emit_control_event(
                crate::runtime::control_events::EventKind::ReplicationSafety,
                crate::runtime::control_events::Outcome::Denied,
                "promotion_refused",
                Some("replication:apply_health".to_string()),
                Some(reason.clone()),
                vec![(
                    "apply_health".to_string(),
                    crate::runtime::control_events::Sensitivity::raw(health),
                )],
            ) {
                return json_error(500, err.to_string());
            }
            return json_error(409, reason);
        }

        // Body parsing.
        let (holder_id, ttl_ms) = if body.is_empty() {
            (default_holder_id(), 60_000u64)
        } else {
            match reddb_wire::replication::FailoverPromotionRequest::decode_json(&body) {
                Ok(request) => (
                    request.holder_id.unwrap_or_else(default_holder_id),
                    request.ttl_ms.unwrap_or(60_000),
                ),
                Err(err) => {
                    let reason = err.to_string();
                    if let Err(emit_err) = self.runtime.emit_control_event(
                        crate::runtime::control_events::EventKind::FailoverPromotion,
                        crate::runtime::control_events::Outcome::Error,
                        "failover_promote",
                        Some("replication:request".to_string()),
                        Some(reason.clone()),
                        Vec::new(),
                    ) {
                        return json_error(500, emit_err.to_string());
                    }
                    return json_error(400, reason);
                }
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
                let replica_id = self.runtime.resolve_replica_id();
                let applied_lsn = self
                    .runtime
                    .config_u64("red.replication.last_applied_lsn", 0);
                let timeline = match self
                    .runtime
                    .record_failover_timeline_promotion(&replica_id, applied_lsn)
                {
                    Ok(timeline) => timeline,
                    Err(err) => {
                        let _ = store.release(&lease);
                        let reason = format!(
                            "promotion acquired lease but failed to record timeline history: {err}"
                        );
                        if let Err(emit_err) = self.runtime.emit_control_event(
                            crate::runtime::control_events::EventKind::FailoverPromotion,
                            crate::runtime::control_events::Outcome::Error,
                            "failover_promote",
                            Some(format!("replication:database:{database_key}")),
                            Some(reason.clone()),
                            vec![
                                (
                                    "holder_id".to_string(),
                                    crate::runtime::control_events::Sensitivity::raw(
                                        lease.holder_id.clone(),
                                    ),
                                ),
                                (
                                    "applied_lsn".to_string(),
                                    crate::runtime::control_events::Sensitivity::raw(
                                        applied_lsn.to_string(),
                                    ),
                                ),
                            ],
                        ) {
                            return json_error(500, emit_err.to_string());
                        }
                        return json_error(500, reason);
                    }
                };
                let timeline_id = timeline
                    .current()
                    .unwrap_or_else(reddb_file::TimelineId::initial);
                if let Err(err) = self.runtime.emit_control_event(
                    crate::runtime::control_events::EventKind::FailoverPromotion,
                    crate::runtime::control_events::Outcome::Allowed,
                    "failover_promote",
                    Some(format!("replication:database:{database_key}")),
                    None,
                    vec![
                        (
                            "holder_id".to_string(),
                            crate::runtime::control_events::Sensitivity::raw(&lease.holder_id),
                        ),
                        (
                            "generation".to_string(),
                            crate::runtime::control_events::Sensitivity::raw(
                                lease.generation.to_string(),
                            ),
                        ),
                        (
                            "ttl_ms".to_string(),
                            crate::runtime::control_events::Sensitivity::raw(ttl_ms.to_string()),
                        ),
                        (
                            "timeline".to_string(),
                            crate::runtime::control_events::Sensitivity::raw(
                                timeline_id.0.to_string(),
                            ),
                        ),
                        (
                            "applied_lsn".to_string(),
                            crate::runtime::control_events::Sensitivity::raw(
                                applied_lsn.to_string(),
                            ),
                        ),
                    ],
                ) {
                    return json_error(500, err.to_string());
                }
                let reply = reddb_wire::replication::FailoverPromotionReply::promoted(
                    lease.holder_id,
                    lease.generation,
                    lease.acquired_at_ms,
                    lease.expires_at_ms,
                    timeline_id.0,
                    applied_lsn,
                );
                let value: JsonValue = crate::json::from_slice(&reply.encode_json())
                    .unwrap_or_else(|_| {
                        let mut object = Map::new();
                        object.insert("ok".to_string(), JsonValue::Bool(true));
                        JsonValue::Object(object)
                    });
                json_response(200, value)
            }
            Err(err) => {
                let reason = format!("promotion refused: {err}");
                if let Err(emit_err) = self.runtime.emit_control_event(
                    crate::runtime::control_events::EventKind::FailoverPromotion,
                    crate::runtime::control_events::Outcome::Denied,
                    "failover_promote",
                    Some(format!("replication:database:{database_key}")),
                    Some(reason.clone()),
                    vec![
                        (
                            "holder_id".to_string(),
                            crate::runtime::control_events::Sensitivity::raw(holder_id),
                        ),
                        (
                            "ttl_ms".to_string(),
                            crate::runtime::control_events::Sensitivity::raw(ttl_ms.to_string()),
                        ),
                    ],
                ) {
                    return json_error(500, emit_err.to_string());
                }
                json_error(409, reason)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::ReplicationConfig;
    use crate::runtime::RedDBRuntime;
    use crate::storage::backend::LocalBackend;
    use crate::RedDBOptions;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    const CHILD_ENV: &str = "REDDB_FAILOVER_PROMOTE_CRASH_CHILD";
    const DATA_PATH_ENV: &str = "REDDB_FAILOVER_PROMOTE_CRASH_DATA_PATH";
    const DATABASE_KEY_ENV: &str = "REDDB_FAILOVER_PROMOTE_CRASH_DATABASE_KEY";
    const CRASH_ENV: &str = "REDDB_PRIMARY_REPLICA_CRASH_AT";

    fn temp_data_path(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("reddb_failover_{name}_{suffix}.rdb"))
    }

    fn cleanup(data_path: &Path, database_key: &str) {
        let _ = std::fs::remove_file(data_path);
        let _ = std::fs::remove_dir_all(
            crate::replication::primary::PrimaryReplication::primary_replica_root_for(data_path),
        );
        let lease_key = reddb_file::serverless_writer_lease_key("leases/", database_key);
        let _ = std::fs::remove_file(&lease_key);
        let _ = std::fs::remove_file(crate::storage::backend::local::local_cas_lock_path_for(
            Path::new(&lease_key),
        ));
    }

    fn replica_runtime_for_promote(data_path: &Path, database_key: &str) -> RedDBRuntime {
        let backend = Arc::new(LocalBackend);
        let mut options = RedDBOptions::persistent(data_path)
            .with_replication(ReplicationConfig::replica("http://primary:5050"))
            .with_atomic_remote_backend(backend);
        options.remote_key = Some(database_key.to_string());
        let runtime = RedDBRuntime::with_options(options).expect("runtime boots");
        runtime.db().store().set_config_tree(
            "red.replication",
            &crate::json!({
                "replica_id": "replica-a",
                "state": "healthy",
                "last_applied_lsn": 42,
            }),
        );
        runtime
    }

    #[test]
    fn admin_failover_promote_records_timeline_history_before_success() {
        let data_path = temp_data_path("timeline_history");
        let database_key = format!("admin-promote-{}", crate::utils::now_unix_nanos());
        cleanup(&data_path, &database_key);

        let runtime = replica_runtime_for_promote(&data_path, &database_key);
        let server = RedDBServer::new(runtime.clone());

        let response = server
            .handle_admin_failover_promote(br#"{"holder_id":"replica-a","ttl_ms":30000}"#.to_vec());
        let body = String::from_utf8(response.body).expect("response body");

        assert_eq!(response.status, 200, "{body}");
        assert!(body.contains(r#""timeline":2"#), "{body}");
        assert!(body.contains(r#""applied_lsn":42"#), "{body}");

        let path = runtime
            .primary_replica_timeline_history_path()
            .expect("timeline history path");
        let timeline = reddb_file::TimelineHistory::read_from_path(path).expect("read timeline");
        assert_eq!(timeline.current(), Some(reddb_file::TimelineId(2)));
        assert_eq!(timeline.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));
        assert_eq!(timeline.entries[1].reason, "promote replica-a");

        cleanup(&data_path, &database_key);
    }

    #[test]
    fn admin_failover_promote_timeline_write_survives_atomic_crash_points() {
        if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
            return;
        }

        for point in [
            "atomic_after_tmp_write",
            "atomic_after_tmp_sync",
            "atomic_after_rename",
            "atomic_after_dir_sync",
        ] {
            let data_path = temp_data_path(&format!("timeline_crash_{point}"));
            let database_key = format!("admin-promote-crash-{}", crate::utils::now_unix_nanos());
            cleanup(&data_path, &database_key);

            let root = crate::replication::primary::PrimaryReplication::primary_replica_root_for(
                &data_path,
            );
            let plan =
                reddb_file::PrimaryReplicaFilePlan::new(&root, reddb_file::TimelineId::initial());
            reddb_file::TimelineHistory::new(1)
                .write_to_path(plan.timeline_history_path())
                .expect("write initial timeline history");

            let child = Command::new(std::env::current_exe().expect("current test exe"))
                .arg("admin_failover_promote_timeline_crash_child")
                .arg("--nocapture")
                .env(CHILD_ENV, "1")
                .env(DATA_PATH_ENV, &data_path)
                .env(DATABASE_KEY_ENV, &database_key)
                .env(CRASH_ENV, point)
                .status()
                .expect("run crash child");
            assert_eq!(
                child.code(),
                Some(173),
                "child should crash at {point}, status={child:?}"
            );

            let history = reddb_file::TimelineHistory::read_from_path(plan.timeline_history_path())
                .expect("timeline history remains decodable");
            assert!(
                history.current() == Some(reddb_file::TimelineId(1))
                    || history.current() == Some(reddb_file::TimelineId(2)),
                "timeline must be old or new after {point}, got {:?}",
                history.current()
            );
            if history.current() == Some(reddb_file::TimelineId(2)) {
                assert_eq!(history.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));
                assert_eq!(history.entries[1].reason, "promote replica-a");
            }

            cleanup(&data_path, &database_key);
        }
    }

    #[test]
    fn admin_failover_promote_timeline_crash_child() -> ExitCode {
        if std::env::var(CHILD_ENV).ok().as_deref() != Some("1") {
            return ExitCode::SUCCESS;
        }
        let data_path = PathBuf::from(std::env::var(DATA_PATH_ENV).expect("data path env"));
        let database_key = std::env::var(DATABASE_KEY_ENV).expect("database key env");
        let runtime = replica_runtime_for_promote(&data_path, &database_key);
        let server = RedDBServer::new(runtime);
        let _ = server
            .handle_admin_failover_promote(br#"{"holder_id":"replica-a","ttl_ms":30000}"#.to_vec());
        ExitCode::from(1)
    }
}
