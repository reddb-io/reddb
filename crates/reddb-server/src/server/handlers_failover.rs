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
    ///   * Replica apply health is `ok` / `healthy`, or it is reconnecting
    ///     after observing a primary frontier; its applied LSN must cover
    ///     that latest observed primary LSN.
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
    /// process with `REDDB_NODE_ROLE=primary` and the returned
    /// `RED_REPLICATION_TERM` after a successful promotion. Auto-
    /// role-flip is a Phase 11.6 follow-up that requires draining
    /// live read traffic safely.
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

        let applied_lsn = self.runtime.config_u64("replication.last_applied_lsn", 0);
        let last_seen_primary_lsn = self
            .runtime
            .config_u64("replication.last_seen_primary_lsn", 0);

        // A caught-up replica transitions from `healthy` to `connecting`
        // when its primary disappears. That state is promotion-ready only
        // when a non-zero observed frontier proves it previously synced.
        let health = self.runtime.replica_apply_health().unwrap_or_default();
        let reconnecting_after_sync = health == "connecting" && last_seen_primary_lsn > 0;
        if !matches!(health.as_str(), "ok" | "healthy") && !reconnecting_after_sync {
            let reason = format!(
                "promotion refused — replica apply state `{health}` is not promotion-ready"
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

        if applied_lsn < last_seen_primary_lsn {
            let reason = format!(
                "promotion refused — replica applied LSN {applied_lsn} is behind last seen primary LSN {last_seen_primary_lsn}"
            );
            if let Err(err) = self.runtime.emit_control_event(
                crate::runtime::control_events::EventKind::ReplicationSafety,
                crate::runtime::control_events::Outcome::Denied,
                "promotion_refused",
                Some("replication:apply_lag".to_string()),
                Some(reason.clone()),
                vec![
                    (
                        "applied_lsn".to_string(),
                        crate::runtime::control_events::Sensitivity::raw(applied_lsn.to_string()),
                    ),
                    (
                        "last_seen_primary_lsn".to_string(),
                        crate::runtime::control_events::Sensitivity::raw(
                            last_seen_primary_lsn.to_string(),
                        ),
                    ),
                ],
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
        let current_term = self.runtime.current_replication_term();
        let Some(promotion_term) = current_term.checked_add(1) else {
            return json_error(409, "promotion refused — replication term exhausted");
        };

        match crate::runtime::lease_lifecycle::admin_promote_lease(
            &store,
            self.runtime.audit_log(),
            &database_key,
            &holder_id,
            ttl_ms,
            promotion_term,
        ) {
            Ok(lease) => {
                let replica_id = self.runtime.resolve_replica_id();
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
                            "term".to_string(),
                            crate::runtime::control_events::Sensitivity::raw(
                                promotion_term.to_string(),
                            ),
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
                    promotion_term,
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
    use crate::{RedDBOptions, StorageDeployPreset};
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn promote_request() -> Vec<u8> {
        reddb_wire::replication::FailoverPromotionRequest {
            holder_id: Some("replica-a".to_string()),
            ttl_ms: Some(30_000),
        }
        .encode_json()
    }

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
            // The handler fixture owns replication state; a background pull
            // loop can race its configured replica identity and applied LSN.
            .with_replica_loop_enabled(false)
            .with_atomic_remote_backend(backend);
        options.remote_key = Some(database_key.to_string());
        let runtime = RedDBRuntime::with_options(options).expect("runtime boots");
        runtime.db().store().set_config_tree(
            "replication",
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

        let response = server.handle_admin_failover_promote(promote_request());
        let body = String::from_utf8(response.body.to_vec()).expect("response body");

        assert_eq!(response.status, 200, "{body}");
        assert!(body.contains(r#""term":2"#), "{body}");
        assert!(body.contains(r#""timeline":2"#), "{body}");
        assert!(body.contains(r#""applied_lsn":42"#), "{body}");

        let path = runtime
            .primary_replica_timeline_history_path()
            .expect("timeline history path");
        assert!(!path.exists(), "timeline metadata stays inside database");
        let timeline = runtime
            .primary_replica_timeline_history()
            .expect("read timeline")
            .expect("history");
        assert_eq!(timeline.current(), Some(reddb_file::TimelineId(2)));
        assert_eq!(timeline.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));
        assert_eq!(timeline.entries[1].reason, "promote replica-a");

        cleanup(&data_path, &database_key);
    }

    #[test]
    fn admin_failover_promote_refuses_connecting_replica_without_observed_frontier() {
        let data_path = temp_data_path("connecting");
        let database_key = format!("admin-promote-{}", crate::utils::now_unix_nanos());
        cleanup(&data_path, &database_key);

        let runtime = replica_runtime_for_promote(&data_path, &database_key);
        runtime
            .db()
            .store()
            .set_config_tree("replication", &crate::json!({ "state": "connecting" }));
        let server = RedDBServer::new(runtime);

        let response = server.handle_admin_failover_promote(promote_request());
        let body = String::from_utf8(response.body.to_vec()).expect("response body");

        assert_eq!(response.status, 409, "{body}");
        assert!(body.contains("not promotion-ready"), "{body}");
        cleanup(&data_path, &database_key);
    }

    #[test]
    fn admin_failover_promote_refuses_replica_behind_seen_primary() {
        let data_path = temp_data_path("behind_primary");
        let database_key = format!("admin-promote-{}", crate::utils::now_unix_nanos());
        cleanup(&data_path, &database_key);

        let runtime = replica_runtime_for_promote(&data_path, &database_key);
        runtime.db().store().set_config_tree(
            "replication",
            &crate::json!({
                "state": "healthy",
                "last_applied_lsn": 41,
                "last_seen_primary_lsn": 42,
            }),
        );
        let server = RedDBServer::new(runtime);

        let response = server.handle_admin_failover_promote(promote_request());
        let body = String::from_utf8(response.body.to_vec()).expect("response body");

        assert_eq!(response.status, 409, "{body}");
        assert!(body.contains("applied LSN 41"), "{body}");
        assert!(body.contains("primary LSN 42"), "{body}");
        cleanup(&data_path, &database_key);
    }

    #[test]
    fn admin_failover_promotion_waits_for_an_in_flight_checkpoint() {
        let data_path = temp_data_path("checkpoint_race");
        let database_key = format!("admin-promote-{}", crate::utils::now_unix_nanos());
        cleanup(&data_path, &database_key);

        {
            let seed = RedDBRuntime::with_options(
                RedDBOptions::persistent(&data_path)
                    .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
                    .expect("native replication profile"),
            )
            .expect("seed runtime");
            let payload = "x".repeat(128 * 1024);
            for table in 0..8 {
                seed.execute_query(&format!(
                    "CREATE TABLE checkpoint_pad_{table} (id INTEGER, payload TEXT)"
                ))
                .expect("create checkpoint padding table");
                for id in 0..16 {
                    seed.execute_query(&format!(
                        "INSERT INTO checkpoint_pad_{table} (id, payload) VALUES ({id}, '{payload}')"
                    ))
                    .expect("insert checkpoint padding row");
                }
            }
            seed.checkpoint().expect("seed checkpoint");
        }

        let backend = Arc::new(LocalBackend);
        let mut options = RedDBOptions::persistent(&data_path)
            .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
            .expect("native replica profile")
            .with_replication(ReplicationConfig::replica("http://primary:5050"))
            .with_replica_loop_enabled(false)
            .with_atomic_remote_backend(backend);
        options.remote_key = Some(database_key.clone());
        let runtime = Arc::new(RedDBRuntime::with_options(options).expect("replica runtime"));
        runtime.db().store().set_config_tree(
            "replication",
            &crate::json!({
                "replica_id": "replica-a",
                "state": "healthy",
                "last_applied_lsn": 42,
            }),
        );
        for table in 0..8 {
            let manager = runtime
                .db()
                .store()
                .get_collection(&format!("checkpoint_pad_{table}"))
                .expect("checkpoint padding collection");
            for mut entity in manager.query_all(|_| true) {
                entity.updated_at = entity.updated_at.saturating_add(1);
                manager.update(entity).expect("dirty checkpoint page");
            }
        }

        let pager = Arc::clone(runtime.db().store().pager().expect("paged store"));
        let pages_before = pager.page_count().expect("page count before checkpoint");
        let free_before = pager
            .free_page_count()
            .expect("free pages before checkpoint");
        let checkpoint_finished = Arc::new(AtomicBool::new(false));
        let completion_order = Arc::new(AtomicUsize::new(0));
        let checkpoint = {
            let runtime = Arc::clone(&runtime);
            let checkpoint_finished = Arc::clone(&checkpoint_finished);
            let completion_order = Arc::clone(&completion_order);
            thread::spawn(move || {
                runtime.checkpoint().expect("concurrent checkpoint");
                completion_order
                    .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                    .expect("checkpoint completes first");
                checkpoint_finished.store(true, Ordering::SeqCst);
            })
        };

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while pager.page_count().expect("checkpoint page progress") <= pages_before
            && pager
                .free_page_count()
                .expect("checkpoint free-page progress")
                == free_before
        {
            assert!(
                !checkpoint_finished.load(Ordering::SeqCst),
                "checkpoint finished before exposing an in-flight publication window"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "checkpoint did not reach page publication"
            );
            thread::yield_now();
        }

        let (promotion_tx, promotion_rx) = mpsc::channel();
        let promotion = {
            let runtime = Arc::clone(&runtime);
            let completion_order = Arc::clone(&completion_order);
            thread::spawn(move || {
                let response = RedDBServer::new((*runtime).clone())
                    .handle_admin_failover_promote(promote_request());
                let order = completion_order.fetch_add(1, Ordering::SeqCst) + 1;
                promotion_tx
                    .send((response.status, response.body.to_vec(), order))
                    .expect("report promotion result");
            })
        };

        assert!(
            promotion_rx
                .recv_timeout(Duration::from_millis(10))
                .is_err(),
            "promotion returned while checkpoint publication was still in flight"
        );
        checkpoint.join().expect("checkpoint thread");
        let (status, body, promotion_order) = promotion_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("promotion completes after checkpoint");
        promotion.join().expect("promotion thread");
        let body = String::from_utf8(body).expect("promotion response body");
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            promotion_order, 2,
            "checkpoint must publish before promotion"
        );

        let history = runtime
            .primary_replica_timeline_history()
            .expect("read promoted timeline")
            .expect("promoted timeline");
        assert_eq!(history.current(), Some(reddb_file::TimelineId(2)));
        assert_eq!(history.ancestor_lsn(reddb_file::TimelineId(2)), Some(42));
        drop(pager);
        drop(runtime);

        let reopened = RedDBRuntime::with_options(
            RedDBOptions::persistent(&data_path)
                .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
                .expect("cold native profile")
                .with_read_only(true),
        )
        .expect("cold open after checkpoint and promotion");
        for table in 0..8 {
            let rows = reopened
                .execute_query(&format!(
                    "SELECT id FROM checkpoint_pad_{table} ORDER BY id"
                ))
                .expect("cold checkpoint rows");
            assert_eq!(rows.result.records.len(), 16, "checkpoint_pad_{table}");
        }
        let history = reopened
            .primary_replica_timeline_history()
            .expect("cold promoted timeline")
            .expect("cold timeline");
        assert_eq!(history.current(), Some(reddb_file::TimelineId(2)));
        drop(reopened);
        cleanup(&data_path, &database_key);
    }

    #[test]
    fn admin_failover_promote_timeline_write_survives_atomic_crash_points() {
        if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
            return;
        }

        for point in [
            "timeline_before_wal",
            "timeline_after_wal",
            "timeline_after_sync",
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

            let recovered = replica_runtime_for_promote(&data_path, &database_key);
            let history = recovered
                .primary_replica_timeline_history()
                .expect("read timeline")
                .expect("history");
            if point == "timeline_after_sync" {
                assert_eq!(history.current(), Some(reddb_file::TimelineId(2)));
            }
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

            drop(recovered);
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
        let _ = server.handle_admin_failover_promote(promote_request());
        ExitCode::from(1)
    }
}
