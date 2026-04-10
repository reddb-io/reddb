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
        object.insert("restore_points".to_string(), JsonValue::Array(Vec::new()));
        json_response(200, JsonValue::Object(object))
    }
}
