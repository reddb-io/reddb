//! HTTP handlers for replication status and snapshot endpoints.

use super::*;

impl RedDBServer {
    /// GET /replication/status
    ///
    /// Returns the current replication role, WAL position, and replica state.
    pub(crate) fn handle_replication_status(&self) -> HttpResponse {
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        let db = self.runtime.db();
        object.insert(
            "current_term".to_string(),
            JsonValue::Number(db.options().replication.term as f64),
        );
        let profile = db.options().replication.failover_profile;
        let mut profile_json = Map::new();
        profile_json.insert(
            "name".to_string(),
            JsonValue::String(profile.name_str().to_string()),
        );
        profile_json.insert(
            "lease_window_ms".to_string(),
            JsonValue::Number(profile.lease_window_ms as f64),
        );
        profile_json.insert(
            "member_health_score_threshold".to_string(),
            JsonValue::Number(profile.member_health_score_threshold as f64),
        );
        profile_json.insert(
            "promotion_grace_ms".to_string(),
            JsonValue::Number(profile.promotion_grace_ms as f64),
        );
        profile_json.insert(
            "max_clock_drift_ms".to_string(),
            JsonValue::Number(profile.max_clock_drift_ms as f64),
        );
        profile_json.insert(
            "lease_safety_margin_ms".to_string(),
            JsonValue::Number(profile.lease_safety_margin_ms() as f64),
        );
        object.insert(
            "failover_profile".to_string(),
            JsonValue::Object(profile_json),
        );
        match &db.options().replication.role {
            crate::replication::ReplicationRole::Standalone => {
                object.insert(
                    "role".to_string(),
                    JsonValue::String("standalone".to_string()),
                );
            }
            crate::replication::ReplicationRole::Primary => {
                object.insert("role".to_string(), JsonValue::String("primary".to_string()));
                // Leader identity (issue #839). A primary is the leader of
                // its own term, so the leader is this node. Operators read
                // `leader` alongside `current_term` to confirm who carries
                // the term without cross-referencing the election log.
                object.insert("is_leader".to_string(), JsonValue::Bool(true));
                object.insert(
                    "leader".to_string(),
                    JsonValue::String(self.runtime.node_id()),
                );
                // Full-resync / partial-resync counters (issue #839). The
                // full-resync counter is the primary alert signal; the
                // partial counter contextualises it (brief disconnects that
                // recovered incrementally are healthy).
                object.insert(
                    "full_resync_count".to_string(),
                    JsonValue::Number(self.runtime.replication_full_resync_count() as f64),
                );
                object.insert(
                    "partial_resync_count".to_string(),
                    JsonValue::Number(self.runtime.replication_partial_resync_count() as f64),
                );
                if let Some(ref primary) = db.replication {
                    let wal_lsn = primary
                        .logical_wal_spool
                        .as_ref()
                        .map(|spool| spool.current_lsn())
                        .unwrap_or_else(|| primary.wal_buffer.current_lsn());
                    object.insert("wal_lsn".to_string(), JsonValue::Number(wal_lsn as f64));
                    object.insert(
                        "commit_watermark".to_string(),
                        JsonValue::Number(self.runtime.commit_watermark() as f64),
                    );
                    if let Some(oldest_lsn) = primary
                        .logical_wal_spool
                        .as_ref()
                        .and_then(|spool| spool.oldest_lsn().ok().flatten())
                        .or_else(|| primary.wal_buffer.oldest_lsn())
                    {
                        object.insert(
                            "oldest_lsn".to_string(),
                            JsonValue::Number(oldest_lsn as f64),
                        );
                    }
                    object.insert(
                        "replica_count".to_string(),
                        JsonValue::Number(primary.replica_count() as f64),
                    );
                    // Per-replica lag in both LSN-offset and wall-clock
                    // (issue #839). `lag_lsn` is the record distance from
                    // the primary's logical head to the replica's last ack;
                    // `lag_seconds` is the wall-clock age since the primary
                    // last heard from the replica. The two together tell a
                    // replica that is behind-but-pulling (high lag_lsn, low
                    // lag_seconds) apart from one that has gone silent.
                    let head_lsn = primary.current_logical_lsn();
                    let now_ms = crate::utils::now_unix_millis() as u128;
                    let replicas = primary
                        .replica_snapshots()
                        .into_iter()
                        .map(|replica| {
                            let mut replica_json = Map::new();
                            replica_json
                                .insert("id".to_string(), JsonValue::String(replica.id.clone()));
                            replica_json.insert(
                                "last_acked_lsn".to_string(),
                                JsonValue::Number(replica.last_acked_lsn as f64),
                            );
                            replica_json.insert(
                                "last_sent_lsn".to_string(),
                                JsonValue::Number(replica.last_sent_lsn as f64),
                            );
                            replica_json.insert(
                                "lag_lsn".to_string(),
                                JsonValue::Number(
                                    head_lsn.saturating_sub(replica.last_acked_lsn) as f64
                                ),
                            );
                            let lag_ms = now_ms.saturating_sub(replica.last_seen_at_unix_ms);
                            replica_json.insert(
                                "lag_seconds".to_string(),
                                JsonValue::Number((lag_ms as f64) / 1000.0),
                            );
                            replica_json.insert(
                                "rebootstrapping".to_string(),
                                JsonValue::Bool(replica.rebootstrapping),
                            );
                            JsonValue::Object(replica_json)
                        })
                        .collect();
                    object.insert("replicas".to_string(), JsonValue::Array(replicas));
                    if let Some(progress) = primary.replication_progress() {
                        object.insert(
                            "replication_lag_lsn".to_string(),
                            JsonValue::Number(progress.lag_lsn as f64),
                        );
                        object.insert(
                            "safe_replay_lsn".to_string(),
                            JsonValue::Number(progress.safe_replay_lsn as f64),
                        );
                    }
                    if let Some(floor) = primary.retention_floor_lsn() {
                        object.insert(
                            "retention_floor_lsn".to_string(),
                            JsonValue::Number(floor as f64),
                        );
                    }
                    match self.runtime.primary_replica_wal_retention_plan() {
                        Ok(Some(retention)) => {
                            object.insert(
                                "wal_oldest_required_lsn".to_string(),
                                JsonValue::Number(
                                    retention.oldest_required_lsn.unwrap_or(0) as f64,
                                ),
                            );
                            object.insert(
                                "wal_retained_bytes".to_string(),
                                JsonValue::Number(retention.retained_bytes_before_prune as f64),
                            );
                            object.insert(
                                "wal_retained_after_prune_bytes".to_string(),
                                JsonValue::Number(retention.retained_bytes_after_prune as f64),
                            );
                            object.insert(
                                "wal_removable_segment_count".to_string(),
                                JsonValue::Number(retention.removable_segments.len() as f64),
                            );
                            object
                                .insert("wal_retention_error".to_string(), JsonValue::Bool(false));
                        }
                        Ok(None) => {}
                        Err(err) => {
                            object.insert("wal_retention_error".to_string(), JsonValue::Bool(true));
                            object.insert(
                                "wal_retention_error_message".to_string(),
                                JsonValue::String(err.to_string()),
                            );
                        }
                    }
                    let slots = primary
                        .slot_snapshots()
                        .into_iter()
                        .map(|slot| {
                            let mut slot_json = Map::new();
                            slot_json.insert(
                                "id".to_string(),
                                JsonValue::String(slot.replica_id.clone()),
                            );
                            slot_json.insert(
                                "restart_lsn".to_string(),
                                JsonValue::Number(slot.restart_lsn as f64),
                            );
                            slot_json.insert(
                                "confirmed_lsn".to_string(),
                                JsonValue::Number(slot.confirmed_lsn() as f64),
                            );
                            slot_json.insert(
                                "invalidated".to_string(),
                                JsonValue::Bool(slot.invalidation_reason.is_some()),
                            );
                            if let Some(reason) = slot.invalidation_reason {
                                slot_json.insert(
                                    "invalidation_reason".to_string(),
                                    JsonValue::String(reason.as_str().to_string()),
                                );
                            }
                            JsonValue::Object(slot_json)
                        })
                        .collect();
                    object.insert("slots".to_string(), JsonValue::Array(slots));
                }
            }
            crate::replication::ReplicationRole::Replica { primary_addr } => {
                object.insert("role".to_string(), JsonValue::String("replica".to_string()));
                object.insert("is_leader".to_string(), JsonValue::Bool(false));
                // Leader identity (issue #839). From a replica's vantage the
                // leader is the primary it streams from; surfaced under the
                // same `leader` key the primary uses so dashboards read one
                // field regardless of which node they scrape.
                object.insert(
                    "leader".to_string(),
                    JsonValue::String(primary_addr.clone()),
                );
                object.insert(
                    "primary_addr".to_string(),
                    JsonValue::String(primary_addr.clone()),
                );
                object.insert(
                    "last_applied_lsn".to_string(),
                    JsonValue::Number(
                        self.runtime
                            .config_u64("red.replication.last_applied_lsn", 0)
                            as f64,
                    ),
                );
                object.insert(
                    "state".to_string(),
                    JsonValue::String(self.runtime.config_string("red.replication.state", "idle")),
                );
                let last_error = self.runtime.config_string("red.replication.last_error", "");
                if !last_error.is_empty() {
                    object.insert("last_error".to_string(), JsonValue::String(last_error));
                }
                object.insert(
                    "last_seen_primary_lsn".to_string(),
                    JsonValue::Number(
                        self.runtime
                            .config_u64("red.replication.last_seen_primary_lsn", 0)
                            as f64,
                    ),
                );
                object.insert(
                    "last_seen_oldest_lsn".to_string(),
                    JsonValue::Number(
                        self.runtime
                            .config_u64("red.replication.last_seen_oldest_lsn", 0)
                            as f64,
                    ),
                );
                object.insert(
                    "rejoin_target_timeline".to_string(),
                    JsonValue::Number(
                        self.runtime
                            .config_u64("red.replication.rejoin_target_timeline", 0)
                            as f64,
                    ),
                );
                object.insert(
                    "rejoin_rewind_to_lsn".to_string(),
                    JsonValue::Number(
                        self.runtime
                            .config_u64("red.replication.rejoin_rewind_to_lsn", 0)
                            as f64,
                    ),
                );
                object.insert(
                    "rejoin_rewind_confirmed_timeline".to_string(),
                    JsonValue::Number(
                        self.runtime
                            .config_u64("red.replication.rejoin_rewind_confirmed_timeline", 0)
                            as f64,
                    ),
                );
                object.insert(
                    "rejoin_rewind_confirmed_lsn".to_string(),
                    JsonValue::Number(
                        self.runtime
                            .config_u64("red.replication.rejoin_rewind_confirmed_lsn", 0)
                            as f64,
                    ),
                );
            }
        }

        json_response(200, JsonValue::Object(object))
    }

    /// `POST /admin/replication/rejoin/confirm-rewind`
    ///
    /// Records that an external operator/system already performed the physical
    /// rewind required by the current rejoin plan. This does not rewind data by
    /// itself; it only lets startup continue when the confirmation exactly
    /// matches the plan stored on the replica.
    pub(crate) fn handle_admin_replication_confirm_rewind(&self, body: Vec<u8>) -> HttpResponse {
        if !matches!(
            self.runtime.write_gate().role(),
            crate::replication::ReplicationRole::Replica { .. }
        ) {
            return json_error(409, "rejoin rewind confirmation only allowed on a replica");
        }

        if self.runtime.config_string("red.replication.state", "") != "rejoin_rewind_required" {
            return json_error(409, "replica is not waiting for a confirmed rejoin rewind");
        }

        let confirmation =
            match reddb_wire::replication::RejoinRewindConfirmation::decode_json(&body) {
                Ok(confirmation) => confirmation,
                Err(err) => return json_error(400, err.to_string()),
            };
        let target_timeline = confirmation.target_timeline;
        let rewind_to_lsn = confirmation.rewind_to_lsn;
        if target_timeline == 0 {
            return json_error(400, "target_timeline must be a positive integer");
        }
        if rewind_to_lsn == 0 {
            return json_error(400, "rewind_to_lsn must be a positive integer");
        }

        let planned_timeline = self
            .runtime
            .config_u64("red.replication.rejoin_target_timeline", 0);
        let planned_lsn = self
            .runtime
            .config_u64("red.replication.rejoin_rewind_to_lsn", 0);
        if target_timeline != planned_timeline || rewind_to_lsn != planned_lsn {
            return json_error(
                409,
                format!(
                    "rewind confirmation does not match current plan: expected timeline {planned_timeline} at LSN {planned_lsn}"
                ),
            );
        }

        self.runtime
            .mark_replica_rejoin_rewind_confirmed(target_timeline, rewind_to_lsn);

        let reply = reddb_wire::replication::RejoinRewindConfirmationReply::confirmed(
            target_timeline,
            rewind_to_lsn,
        );
        let value: JsonValue = crate::json::from_slice(&reply.encode_json()).unwrap_or_else(|_| {
            let mut object = Map::new();
            object.insert("ok".to_string(), JsonValue::Bool(true));
            JsonValue::Object(object)
        });
        json_response(200, value)
    }

    /// POST /replication/snapshot
    ///
    /// Creates a snapshot suitable for bootstrapping a new replica.
    pub(crate) fn handle_replication_snapshot(&self) -> HttpResponse {
        crate::server::transport::run_use_case(
            || self.native_use_cases().create_snapshot(),
            |snapshot| {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "snapshot".to_string(),
                    crate::presentation::native_json::snapshot_descriptor_json(snapshot),
                );
                JsonValue::Object(object)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::ReplicationConfig;
    use crate::runtime::RedDBRuntime;
    use crate::RedDBOptions;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_data_path(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("reddb_handlers_replication_{name}_{suffix}.rdb"))
    }

    fn cleanup_data_path(data_path: &Path) {
        let _ = std::fs::remove_file(data_path);
        let _ = std::fs::remove_file(
            crate::replication::primary::PrimaryReplication::slot_path_for(data_path),
        );
        let _ = std::fs::remove_file(crate::replication::primary::LogicalWalSpool::path_for(
            data_path,
        ));
        let _ = std::fs::remove_dir_all(
            crate::replication::primary::PrimaryReplication::primary_replica_root_for(data_path),
        );
    }

    fn replica_waiting_for_rejoin_rewind() -> RedDBRuntime {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory()
                .with_replication(ReplicationConfig::replica("http://primary:5050")),
        )
        .expect("runtime");
        runtime.db().store().set_config_tree(
            "red.replication",
            &crate::json!({
                "state": "rejoin_rewind_required",
                "rejoin_target_timeline": 3,
                "rejoin_rewind_to_lsn": 42,
                "rejoin_rewind_confirmed_timeline": 0,
                "rejoin_rewind_confirmed_lsn": 0,
                "last_applied_lsn": 100,
            }),
        );
        runtime
    }

    #[test]
    fn replication_status_surfaces_slot_invalidation_reason() {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory()
                .with_replication(ReplicationConfig::primary().with_slot_retention_max_lag_lsn(3)),
        )
        .expect("runtime");
        let db = runtime.db();
        let primary = db.replication.as_ref().expect("primary replication");
        primary.register_replica("slow".to_string());
        let spool = primary
            .logical_wal_spool
            .as_ref()
            .expect("logical WAL spool");
        for lsn in 1..=4 {
            spool
                .append_with_term_and_timestamp(1, lsn, lsn, &[lsn as u8])
                .expect("append logical WAL");
        }
        primary.enforce_retention_limits(0);

        let server = RedDBServer::new(runtime);
        let response = server.handle_replication_status();
        let body = String::from_utf8(response.body).expect("status body is utf8");

        assert_eq!(response.status, 200);
        assert!(body.contains(r#""invalidated":true"#), "{body}");
        assert!(
            body.contains(r#""invalidation_reason":"horizon""#),
            "{body}"
        );
    }

    #[test]
    fn replication_status_surfaces_term_leader_watermark_and_resync_counters() {
        // Issue #839 acceptance: the operator-facing surface carries the
        // current term, a leader identity, the commit watermark, and the
        // full-resync alert counter.
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_replication(ReplicationConfig::primary().with_term(7)),
        )
        .expect("runtime");

        let server = RedDBServer::new(runtime);
        let response = server.handle_replication_status();
        let body = String::from_utf8(response.body).expect("status body is utf8");

        assert_eq!(response.status, 200);
        assert!(body.contains(r#""role":"primary""#), "{body}");
        assert!(body.contains(r#""current_term":7"#), "{body}");
        assert!(body.contains(r#""is_leader":true"#), "{body}");
        // A primary is the leader of its own term, so `leader` is this
        // node's stable id — present and non-empty.
        assert!(body.contains(r#""leader":""#), "{body}");
        assert!(!body.contains(r#""leader":"""#), "{body}");
        assert!(body.contains(r#""commit_watermark":"#), "{body}");
        assert!(body.contains(r#""full_resync_count":0"#), "{body}");
        assert!(body.contains(r#""partial_resync_count":0"#), "{body}");
    }

    #[test]
    fn replication_status_surfaces_active_failover_profile() {
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_replication(
                ReplicationConfig::primary()
                    .with_failover_profile(crate::replication::FailoverProfile::CONSERVATIVE),
            ),
        )
        .expect("runtime");

        let server = RedDBServer::new(runtime);
        let response = server.handle_replication_status();
        let body = String::from_utf8(response.body).expect("status body is utf8");

        assert_eq!(response.status, 200);
        assert!(body.contains(r#""failover_profile":{"#), "{body}");
        assert!(body.contains(r#""name":"conservative""#), "{body}");
        assert!(body.contains(r#""lease_window_ms":60000"#), "{body}");
        assert!(
            body.contains(r#""member_health_score_threshold":90"#),
            "{body}"
        );
        assert!(body.contains(r#""promotion_grace_ms":30000"#), "{body}");
        assert!(body.contains(r#""max_clock_drift_ms":5000"#), "{body}");
        assert!(body.contains(r#""lease_safety_margin_ms":30000"#), "{body}");
    }

    #[test]
    fn replication_status_surfaces_primary_replica_wal_retention_metrics() {
        let data_path = temp_data_path("wal_retention_status");
        cleanup_data_path(&data_path);

        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(&data_path).with_replication(ReplicationConfig::primary()),
        )
        .expect("runtime");
        let plan = runtime
            .primary_replica_file_plan()
            .expect("primary-replica file plan");
        let mut catalog =
            reddb_file::ReplicationSlotCatalog::new(reddb_file::TimelineId::initial());
        catalog
            .upsert(reddb_file::ReplicationSlot::new(
                "replica-a",
                reddb_file::TimelineId::initial(),
                0,
            ))
            .expect("upsert slot");
        catalog
            .write_to_path(plan.slots_path())
            .expect("write slot catalog");
        let wal_path = plan.wal_segment_path(0);
        std::fs::create_dir_all(wal_path.parent().expect("wal parent")).expect("create wal dir");
        std::fs::write(&wal_path, b"segment").expect("write fake wal segment");

        let server = RedDBServer::new(runtime);
        let response = server.handle_replication_status();
        let body = String::from_utf8(response.body).expect("status body is utf8");

        assert_eq!(response.status, 200);
        assert!(body.contains(r#""wal_oldest_required_lsn":0"#), "{body}");
        assert!(body.contains(r#""wal_retained_bytes":7"#), "{body}");
        assert!(
            body.contains(r#""wal_retained_after_prune_bytes":7"#),
            "{body}"
        );
        assert!(
            body.contains(r#""wal_removable_segment_count":0"#),
            "{body}"
        );
        assert!(body.contains(r#""wal_retention_error":false"#), "{body}");

        cleanup_data_path(&data_path);
    }

    #[test]
    fn replication_status_surfaces_per_replica_lag_offset_and_wall_clock() {
        // Issue #839 acceptance: per-replica lag in both LSN-offset and
        // wall-clock appears in the status payload.
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::in_memory().with_replication(ReplicationConfig::primary()),
        )
        .expect("runtime");
        let db = runtime.db();
        let primary = db.replication.as_ref().expect("primary replication");
        primary.register_replica("r1".to_string());
        // Advance the primary head to LSN 5, tell it the replica was sent
        // through 5 but has only acked through 2 — a 3-record offset lag.
        let spool = primary
            .logical_wal_spool
            .as_ref()
            .expect("logical WAL spool");
        for lsn in 1..=5 {
            spool
                .append_with_term_and_timestamp(1, lsn, lsn, &[lsn as u8])
                .expect("append logical WAL");
        }
        primary.note_replica_pull("r1", 5);
        primary.ack_replica("r1", 2);

        let server = RedDBServer::new(runtime);
        let response = server.handle_replication_status();
        let body = String::from_utf8(response.body).expect("status body is utf8");

        assert_eq!(response.status, 200);
        assert!(body.contains(r#""replicas":["#), "{body}");
        assert!(body.contains(r#""id":"r1""#), "{body}");
        assert!(body.contains(r#""last_acked_lsn":2"#), "{body}");
        assert!(body.contains(r#""lag_lsn":3"#), "{body}");
        // Wall-clock lag is surfaced as `lag_seconds`; the replica was just
        // acked, so the value is small but the field must be present.
        assert!(body.contains(r#""lag_seconds":"#), "{body}");
    }

    #[test]
    fn replication_status_surfaces_rejoin_rewind_plan_and_confirmation() {
        let runtime = replica_waiting_for_rejoin_rewind();
        let server = RedDBServer::new(runtime);

        let response = server.handle_replication_status();
        let body = String::from_utf8(response.body).expect("status body is utf8");

        assert_eq!(response.status, 200);
        assert!(body.contains(r#""role":"replica""#), "{body}");
        assert!(
            body.contains(r#""state":"rejoin_rewind_required""#),
            "{body}"
        );
        assert!(body.contains(r#""rejoin_target_timeline":3"#), "{body}");
        assert!(body.contains(r#""rejoin_rewind_to_lsn":42"#), "{body}");
        assert!(
            body.contains(r#""rejoin_rewind_confirmed_timeline":0"#),
            "{body}"
        );
        assert!(
            body.contains(r#""rejoin_rewind_confirmed_lsn":0"#),
            "{body}"
        );
    }

    #[test]
    fn admin_replication_confirm_rewind_records_exact_current_plan() {
        let runtime = replica_waiting_for_rejoin_rewind();
        let server = RedDBServer::new(runtime);

        let response = server.handle_admin_replication_confirm_rewind(
            reddb_wire::replication::RejoinRewindConfirmation {
                target_timeline: 3,
                rewind_to_lsn: 42,
            }
            .encode_json(),
        );
        let body = String::from_utf8(response.body).expect("confirm body is utf8");

        assert_eq!(response.status, 200);
        assert!(body.contains(r#""ok":true"#), "{body}");
        assert_eq!(
            server
                .runtime
                .config_u64("red.replication.rejoin_rewind_confirmed_timeline", 0),
            3
        );
        assert_eq!(
            server
                .runtime
                .config_u64("red.replication.rejoin_rewind_confirmed_lsn", 0),
            42
        );
        assert_eq!(
            server
                .runtime
                .config_u64("red.replication.last_applied_lsn", 0),
            42
        );
    }

    #[test]
    fn admin_replication_confirm_rewind_rejects_mismatched_plan_without_writing() {
        let runtime = replica_waiting_for_rejoin_rewind();
        let server = RedDBServer::new(runtime);

        let response = server.handle_admin_replication_confirm_rewind(
            reddb_wire::replication::RejoinRewindConfirmation {
                target_timeline: 3,
                rewind_to_lsn: 41,
            }
            .encode_json(),
        );
        let body = String::from_utf8(response.body).expect("confirm body is utf8");

        assert_eq!(response.status, 409);
        assert!(body.contains("does not match current plan"), "{body}");
        assert_eq!(
            server
                .runtime
                .config_u64("red.replication.rejoin_rewind_confirmed_timeline", 0),
            0
        );
        assert_eq!(
            server
                .runtime
                .config_u64("red.replication.rejoin_rewind_confirmed_lsn", 0),
            0
        );
        assert_eq!(
            server
                .runtime
                .config_u64("red.replication.last_applied_lsn", 0),
            100
        );
    }
}
