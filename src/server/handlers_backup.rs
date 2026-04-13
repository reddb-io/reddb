use super::*;

impl RedDBServer {
    /// GET /changes?since_lsn=0&limit=100 — poll CDC events.
    pub(crate) fn handle_cdc_poll(&self, query: &BTreeMap<String, String>) -> HttpResponse {
        let since_lsn = query
            .get("since_lsn")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let limit = query
            .get("limit")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(100)
            .min(10_000);

        let events = self.runtime.cdc_poll(since_lsn, limit);

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "events".to_string(),
            JsonValue::Array(
                events
                    .iter()
                    .map(|e| {
                        let mut ev = Map::new();
                        ev.insert("lsn".to_string(), JsonValue::Number(e.lsn as f64));
                        ev.insert(
                            "timestamp".to_string(),
                            JsonValue::Number(e.timestamp as f64),
                        );
                        ev.insert(
                            "operation".to_string(),
                            JsonValue::String(e.operation.as_str().to_string()),
                        );
                        ev.insert(
                            "collection".to_string(),
                            JsonValue::String(e.collection.clone()),
                        );
                        ev.insert(
                            "entity_id".to_string(),
                            JsonValue::Number(e.entity_id as f64),
                        );
                        ev.insert(
                            "entity_kind".to_string(),
                            JsonValue::String(e.entity_kind.clone()),
                        );
                        JsonValue::Object(ev)
                    })
                    .collect(),
            ),
        );
        let next_lsn = events.last().map(|e| e.lsn).unwrap_or(since_lsn);
        object.insert("next_lsn".to_string(), JsonValue::Number(next_lsn as f64));
        json_response(200, JsonValue::Object(object))
    }

    /// GET /backup/status — backup scheduler status.
    pub(crate) fn handle_backup_status(&self) -> HttpResponse {
        let status = self.runtime.backup_status();
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("running".to_string(), JsonValue::Bool(status.running));
        object.insert(
            "interval_secs".to_string(),
            JsonValue::Number(status.interval_secs as f64),
        );
        object.insert(
            "total_backups".to_string(),
            JsonValue::Number(status.total_backups as f64),
        );
        object.insert(
            "total_failures".to_string(),
            JsonValue::Number(status.total_failures as f64),
        );
        if let Some(ref last) = status.last_backup {
            let mut lb = Map::new();
            lb.insert(
                "snapshot_id".to_string(),
                JsonValue::Number(last.snapshot_id as f64),
            );
            lb.insert("uploaded".to_string(), JsonValue::Bool(last.uploaded));
            lb.insert(
                "duration_ms".to_string(),
                JsonValue::Number(last.duration_ms as f64),
            );
            lb.insert(
                "timestamp".to_string(),
                JsonValue::Number(last.timestamp as f64),
            );
            object.insert("last_backup".to_string(), JsonValue::Object(lb));
        }
        json_response(200, JsonValue::Object(object))
    }

    /// POST /backup/trigger — force an immediate backup.
    pub(crate) fn handle_backup_trigger(&self) -> HttpResponse {
        match self.runtime.trigger_backup() {
            Ok(result) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "snapshot_id".to_string(),
                    JsonValue::Number(result.snapshot_id as f64),
                );
                object.insert("uploaded".to_string(), JsonValue::Bool(result.uploaded));
                object.insert(
                    "duration_ms".to_string(),
                    JsonValue::Number(result.duration_ms as f64),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(500, err.to_string()),
        }
    }

    /// GET /recovery/restore-points — list available restore points.
    pub(crate) fn handle_restore_points(&self) -> HttpResponse {
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        let db = self.runtime.db();
        let options = db.options();

        let Some(backend) = &options.remote_backend else {
            object.insert("restore_points".to_string(), JsonValue::Array(Vec::new()));
            return json_response(200, JsonValue::Object(object));
        };

        let head_key = options.default_backup_head_key();
        let backup_head = match crate::storage::wal::load_backup_head(backend.as_ref(), &head_key) {
            Ok(head) => head,
            Err(err) => return json_error(500, err.to_string()),
        };

        let default_snapshot_prefix = options.default_snapshot_prefix();
        let snapshot_prefix = backup_head
            .as_ref()
            .and_then(|head| {
                std::path::Path::new(&head.snapshot_key)
                    .parent()
                    .map(|parent| parent.to_string_lossy().trim_end_matches('/').to_string())
            })
            .filter(|prefix| !prefix.is_empty())
            .map(|prefix| format!("{prefix}/"))
            .unwrap_or(default_snapshot_prefix);
        let wal_prefix = backup_head
            .as_ref()
            .map(|head| head.wal_prefix.clone())
            .unwrap_or_else(|| options.default_wal_archive_prefix());

        let recovery = crate::storage::wal::PointInTimeRecovery::new(
            backend.clone(),
            snapshot_prefix,
            wal_prefix,
        );
        let restore_points = match recovery.list_restore_points() {
            Ok(points) => points,
            Err(err) => return json_error(500, err.to_string()),
        };

        object.insert(
            "restore_points".to_string(),
            JsonValue::Array(
                restore_points
                    .into_iter()
                    .map(|point| {
                        let mut restore_point = Map::new();
                        restore_point.insert(
                            "snapshot_id".to_string(),
                            JsonValue::Number(point.snapshot_id as f64),
                        );
                        restore_point.insert(
                            "snapshot_time".to_string(),
                            JsonValue::Number(point.snapshot_time as f64),
                        );
                        restore_point.insert(
                            "wal_segment_count".to_string(),
                            JsonValue::Number(point.wal_segment_count as f64),
                        );
                        restore_point.insert(
                            "latest_recoverable_time".to_string(),
                            JsonValue::Number(point.latest_recoverable_time as f64),
                        );
                        JsonValue::Object(restore_point)
                    })
                    .collect(),
            ),
        );
        if let Some(head) = backup_head {
            let mut head_object = Map::new();
            head_object.insert(
                "timeline_id".to_string(),
                JsonValue::String(head.timeline_id),
            );
            head_object.insert(
                "snapshot_key".to_string(),
                JsonValue::String(head.snapshot_key),
            );
            head_object.insert(
                "snapshot_id".to_string(),
                JsonValue::Number(head.snapshot_id as f64),
            );
            head_object.insert(
                "snapshot_time".to_string(),
                JsonValue::Number(head.snapshot_time as f64),
            );
            head_object.insert(
                "current_lsn".to_string(),
                JsonValue::Number(head.current_lsn as f64),
            );
            head_object.insert(
                "last_archived_lsn".to_string(),
                JsonValue::Number(head.last_archived_lsn as f64),
            );
            head_object.insert("wal_prefix".to_string(), JsonValue::String(head.wal_prefix));
            object.insert("head".to_string(), JsonValue::Object(head_object));
        }
        json_response(200, JsonValue::Object(object))
    }
}
