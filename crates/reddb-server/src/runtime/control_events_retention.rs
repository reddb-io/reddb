//! Issue #2900 — native retention for `red.control_events`.
//!
//! The ledger appends one row per `emit()` and nothing removed them. This
//! module applies the declarative retention policy of issue #580/#584 to the
//! system collection instead of adding a dedicated deleter:
//!
//! - the policy lives on the collection contract (`retention_duration_ms`,
//!   anchored on the automatic `created_at` timestamp, which the ledger
//!   stamps at emit time);
//! - reads of `red.control_events` hide expired rows immediately
//!   (`red_schema::control_event_reads`);
//! - the background `retention_sweeper` deletes them in bounded batches
//!   through the regular `DELETE` path (WAL and replicas included);
//! - `red.retention` surfaces the policy and the sweeper counters.
//!
//! The window comes from `control_events.retention` (env
//! `REDDB_CONTROL_EVENTS_RETENTION`), e.g. `48h` or `7d`; `off` disables it.
//! It is applied at boot and again after `SET CONFIG control_events.retention`.
//! SQL DDL on `red.*` names stays refused.

use super::control_events::CONTROL_EVENTS_COLLECTION;
use super::RedDBRuntime;
use crate::{RedDBError, RedDBResult};

/// `red.config` key holding the retention window.
pub const CONFIG_KEY: &str = "control_events.retention";

/// Engine default when neither env nor `red.config` sets the key.
pub const DEFAULT_RETENTION: &str = "7d";

/// Parse a retention setting into milliseconds. `Ok(None)` disables the
/// policy. Accepts the duration forms of `SET RETENTION` (`48h`, `7d`,
/// `48 hours`, ...). A zero window is rejected because it would hide every
/// event.
pub(crate) fn parse_retention(raw: &str) -> Result<Option<u64>, String> {
    let value = raw.trim();
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "off" | "none" | "disabled" | "false"
    ) {
        return Ok(None);
    }
    let invalid = || {
        format!("invalid {CONFIG_KEY} `{raw}`: expected a duration such as `48h` or `7d`, or `off`")
    };
    let nanos = reddb_types::duration::parse_duration_ns(value).ok_or_else(invalid)?;
    let millis = nanos / 1_000_000;
    if millis == 0 {
        return Err(invalid());
    }
    Ok(Some(millis))
}

/// Whether a ledger row falls outside the window. Mirrors the sweeper's
/// conservative reading of the second-resolution `created_at` (end of the
/// stored second), so reads never hide a row the sweeper would keep.
pub(crate) fn is_expired(entity: &crate::storage::UnifiedEntity, cutoff_ms: i64) -> bool {
    i64::try_from(entity.created_at)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000))
        .and_then(|millis| millis.checked_add(999))
        .is_some_and(|millis| millis < cutoff_ms)
}

impl RedDBRuntime {
    /// Effective retention window for `red.control_events` in milliseconds
    /// (env > `red.config` > default). An invalid stored value falls back to
    /// the default so a bad setting can never disable retention silently.
    pub(crate) fn control_events_retention_ms(&self) -> Option<u64> {
        let raw = self.config_string(CONFIG_KEY, DEFAULT_RETENTION);
        match parse_retention(&raw) {
            Ok(window) => window,
            Err(err) => {
                tracing::warn!(
                    target: "reddb::control_events",
                    "{err}; using the default {DEFAULT_RETENTION}"
                );
                parse_retention(DEFAULT_RETENTION).expect("default retention parses")
            }
        }
    }

    /// Publish the configured window as the collection's retention policy.
    /// Idempotent; returns the applied window. Replicas keep the primary's
    /// contract and receive its sweeper deletes through the WAL.
    pub(crate) fn apply_control_events_retention(&self) -> RedDBResult<Option<u64>> {
        let window = self.control_events_retention_ms();
        let db = &self.inner.db;
        if db.is_replica_role()
            || db
                .store()
                .get_collection(CONTROL_EVENTS_COLLECTION)
                .is_none()
        {
            return Ok(window);
        }
        let mut contract = match db.collection_contract(CONTROL_EVENTS_COLLECTION) {
            Some(contract) => contract,
            None if window.is_none() => return Ok(window),
            None => super::impl_lifecycle::system_keyed_collection_contract(
                CONTROL_EVENTS_COLLECTION,
                crate::catalog::CollectionModel::Table,
            ),
        };
        if contract.timestamps_enabled && contract.retention_duration_ms == window {
            return Ok(window);
        }
        // The ledger never writes a timestamp column; `created_at` resolves to
        // the entity's insertion time, which is the event time.
        contract.timestamps_enabled = true;
        contract.retention_duration_ms = window;
        contract.updated_at_unix_ms = crate::utils::now_unix_millis() as u128;
        if db.options().read_only {
            db.cache_read_only_bootstrap_contract(contract);
            return Ok(window);
        }
        db.save_collection_contract(contract)
            .and_then(|_| db.persist_metadata())
            .map_err(|err| RedDBError::Internal(format!("{CONFIG_KEY}: {err}")))?;
        self.invalidate_result_cache();
        Ok(window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::unified::{EntityData, EntityId, EntityKind, RowData};
    use crate::storage::UnifiedEntity;
    use reddb_types::Value;
    use std::sync::Arc;

    const DAY_MS: u64 = 86_400_000;

    #[test]
    fn parses_durations_and_off() {
        assert_eq!(parse_retention("7d"), Ok(Some(7 * DAY_MS)));
        assert_eq!(parse_retention("48h"), Ok(Some(2 * DAY_MS)));
        assert_eq!(parse_retention(" 48 hours "), Ok(Some(2 * DAY_MS)));
        assert_eq!(parse_retention("OFF"), Ok(None));
        assert!(parse_retention("0d").is_err());
        assert!(parse_retention("forever").is_err());
    }

    #[test]
    fn env_override_is_the_matrix_env_name() {
        assert_eq!(
            crate::runtime::config_overlay::env_name_for(CONFIG_KEY),
            "REDDB_CONTROL_EVENTS_RETENTION"
        );
        assert_eq!(
            crate::runtime::config_overlay::coerce_env_value(CONFIG_KEY, "48h"),
            Some(Value::text("48h"))
        );
    }

    fn insert_event(runtime: &RedDBRuntime, id: &str, age_secs: u64) {
        let mut named = std::collections::HashMap::new();
        named.insert("id".to_string(), Value::text(id.to_string()));
        named.insert("kind".to_string(), Value::text("test.retention"));
        let mut entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::from(CONTROL_EVENTS_COLLECTION),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        );
        let now_secs = crate::utils::now_unix_millis() / 1_000;
        entity.created_at = now_secs - age_secs;
        entity.updated_at = entity.created_at;
        runtime
            .db()
            .store()
            .insert_auto(CONTROL_EVENTS_COLLECTION, entity)
            .expect("insert control event");
    }

    fn visible_ids(runtime: &RedDBRuntime) -> Vec<String> {
        let result = runtime
            .execute_query("SELECT id FROM red.control_events WHERE kind = 'test.retention'")
            .expect("read control events");
        let mut ids: Vec<String> = result
            .result
            .records
            .iter()
            .filter_map(|record| match record.get("id") {
                Some(Value::Text(id)) => Some(id.to_string()),
                _ => None,
            })
            .collect();
        ids.sort();
        ids
    }

    /// Live physical versions (a sweeper DELETE stamps `xmax`).
    fn physical_ids(runtime: &RedDBRuntime) -> usize {
        let manager = runtime
            .db()
            .store()
            .get_collection(CONTROL_EVENTS_COLLECTION)
            .expect("ledger collection");
        let mut count = 0;
        manager.for_each_entity(|entity| {
            if entity.xmax == 0
                && entity
                    .data
                    .as_row()
                    .and_then(|row| row.get_field("kind"))
                    .is_some_and(|kind| kind == &Value::text("test.retention"))
            {
                count += 1;
            }
            true
        });
        count
    }

    fn retention_row(runtime: &RedDBRuntime) -> Option<Value> {
        let result = runtime
            .execute_query(
                "SELECT retention_duration FROM red.retention WHERE name = 'red.control_events'",
            )
            .expect("red.retention");
        result
            .result
            .records
            .first()
            .and_then(|record| record.get("retention_duration").cloned())
    }

    fn runtime() -> RedDBRuntime {
        let mut runtime = RedDBRuntime::in_memory().expect("runtime");
        drop(runtime.background_tasks.take());
        runtime
    }

    #[test]
    fn default_window_is_seven_days_and_sweeper_removes_expired_events() {
        let runtime = runtime();
        assert_eq!(
            runtime
                .apply_control_events_retention()
                .expect("boot policy"),
            Some(7 * DAY_MS)
        );
        assert_eq!(
            retention_row(&runtime),
            Some(Value::UnsignedInteger(7 * DAY_MS))
        );

        insert_event(&runtime, "fresh", 3_600);
        insert_event(&runtime, "stale", 8 * 86_400);
        assert_eq!(visible_ids(&runtime), vec!["fresh".to_string()]);
        assert_eq!(physical_ids(&runtime), 2);

        for _ in 0..8 {
            runtime.sweep_retention_tick(1_000);
        }
        assert_eq!(physical_ids(&runtime), 1, "sweeper deletes the stale event");
        assert_eq!(visible_ids(&runtime), vec!["fresh".to_string()]);
        let swept = runtime
            .execute_query(
                "SELECT rows_swept_total FROM red.retention WHERE name = 'red.control_events'",
            )
            .expect("sweeper counters");
        assert_eq!(
            swept.result.records[0].get("rows_swept_total"),
            Some(&Value::UnsignedInteger(1))
        );
    }

    #[test]
    fn set_config_override_narrows_the_window_at_runtime() {
        let runtime = runtime();
        runtime
            .apply_control_events_retention()
            .expect("boot policy");
        insert_event(&runtime, "fresh", 3_600);
        insert_event(&runtime, "three_days", 3 * 86_400);
        assert_eq!(visible_ids(&runtime).len(), 2);

        runtime
            .execute_query("SET CONFIG control_events.retention = '48h'")
            .expect("set retention");
        assert_eq!(
            retention_row(&runtime),
            Some(Value::UnsignedInteger(2 * DAY_MS))
        );
        assert_eq!(visible_ids(&runtime), vec!["fresh".to_string()]);

        let err = runtime
            .execute_query("SET CONFIG control_events.retention = 'forever'")
            .expect_err("invalid retention is rejected");
        assert!(err.to_string().contains(CONFIG_KEY), "{err}");
        assert_eq!(
            retention_row(&runtime),
            Some(Value::UnsignedInteger(2 * DAY_MS))
        );
    }

    #[test]
    fn off_keeps_every_event() {
        let runtime = runtime();
        runtime
            .execute_query("SET CONFIG control_events.retention = 'off'")
            .expect("disable retention");
        assert_eq!(
            runtime.apply_control_events_retention().expect("policy"),
            None
        );
        assert_eq!(retention_row(&runtime), Some(Value::Null));

        insert_event(&runtime, "fresh", 3_600);
        insert_event(&runtime, "ancient", 365 * 86_400);
        for _ in 0..8 {
            runtime.sweep_retention_tick(1_000);
        }
        assert_eq!(
            visible_ids(&runtime),
            vec!["ancient".to_string(), "fresh".to_string()]
        );
        assert_eq!(physical_ids(&runtime), 2);
    }
}
