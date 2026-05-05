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
