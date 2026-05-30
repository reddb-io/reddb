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
}
