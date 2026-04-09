//! HTTP handlers for replication status and snapshot endpoints.

use super::*;

impl RedDBServer {
    /// GET /replication/status
    ///
    /// Returns the current replication role, WAL position, and replica state.
    pub(crate) fn handle_replication_status(&self) -> HttpResponse {
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));

        match &self.replication {
            Some(replication) => {
                let role_str = match &replication.config.role {
                    crate::replication::ReplicationRole::Standalone => "standalone",
                    crate::replication::ReplicationRole::Primary => "primary",
                    crate::replication::ReplicationRole::Replica { .. } => "replica",
                };
                object.insert(
                    "role".to_string(),
                    JsonValue::String(role_str.to_string()),
                );

                if let Some(ref primary) = replication.primary {
                    let wal_lsn = primary.wal_buffer.current_lsn();
                    object.insert(
                        "wal_lsn".to_string(),
                        JsonValue::Number(wal_lsn as f64),
                    );

                    let oldest = primary.wal_buffer.oldest_lsn();
                    if let Some(oldest_lsn) = oldest {
                        object.insert(
                            "oldest_lsn".to_string(),
                            JsonValue::Number(oldest_lsn as f64),
                        );
                    }

                    let replica_count = primary.replica_count();
                    object.insert(
                        "replica_count".to_string(),
                        JsonValue::Number(replica_count as f64),
                    );
                }

                if let crate::replication::ReplicationRole::Replica { ref primary_addr } =
                    replication.config.role
                {
                    object.insert(
                        "primary_addr".to_string(),
                        JsonValue::String(primary_addr.clone()),
                    );
                }
            }
            None => {
                object.insert(
                    "role".to_string(),
                    JsonValue::String("standalone".to_string()),
                );
                object.insert(
                    "note".to_string(),
                    JsonValue::String("replication is not configured".to_string()),
                );
            }
        }

        json_response(200, JsonValue::Object(object))
    }

    /// POST /replication/snapshot
    ///
    /// Creates a snapshot suitable for bootstrapping a new replica.
    pub(crate) fn handle_replication_snapshot(&self) -> HttpResponse {
        match self.native_use_cases().create_snapshot() {
            Ok(snapshot) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "snapshot".to_string(),
                    crate::presentation::native_json::snapshot_descriptor_json(&snapshot),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(500, err.to_string()),
        }
    }
}
