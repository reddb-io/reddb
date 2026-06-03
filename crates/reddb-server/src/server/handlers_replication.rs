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
                    let slots = primary
                        .slot_snapshots()
                        .into_iter()
                        .map(|slot| {
                            let mut slot_json = Map::new();
                            slot_json.insert("id".to_string(), JsonValue::String(slot.id));
                            slot_json.insert(
                                "restart_lsn".to_string(),
                                JsonValue::Number(slot.restart_lsn as f64),
                            );
                            slot_json.insert(
                                "confirmed_lsn".to_string(),
                                JsonValue::Number(slot.confirmed_lsn as f64),
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
            }
        }

        json_response(200, JsonValue::Object(object))
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
}
