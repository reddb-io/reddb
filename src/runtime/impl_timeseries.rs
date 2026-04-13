//! Time-series DDL execution

use std::collections::HashMap;
use std::sync::Arc;

use super::*;

const TIMESERIES_META_COLLECTION: &str = "red_timeseries_meta";

impl RedDBRuntime {
    pub fn execute_create_timeseries(
        &self,
        raw_query: &str,
        query: &CreateTimeSeriesQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        for spec in &query.downsample_policies {
            crate::storage::timeseries::retention::DownsamplePolicy::parse(spec).ok_or_else(
                || RedDBError::Query(format!("invalid downsample policy '{}'", spec)),
            )?;
        }

        let store = self.inner.db.store();
        let exists = store.get_collection(&query.name).is_some();
        if exists {
            if query.if_not_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("timeseries '{}' already exists", query.name),
                    "create",
                ));
            }
            return Err(RedDBError::Query(format!(
                "timeseries '{}' already exists",
                query.name
            )));
        }
        store
            .create_collection(&query.name)
            .map_err(|e| RedDBError::Internal(e.to_string()))?;
        if let Some(ttl_ms) = query.retention_ms {
            self.inner
                .db
                .set_collection_default_ttl_ms(&query.name, ttl_ms);
        }
        self.inner
            .db
            .save_collection_contract(timeseries_collection_contract(query))
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        save_timeseries_metadata(store.as_ref(), query)?;
        self.invalidate_result_cache();
        self.inner
            .db
            .persist_metadata()
            .map_err(|e| RedDBError::Internal(e.to_string()))?;

        let mut msg = format!("timeseries '{}' created", query.name);
        if let Some(ret) = query.retention_ms {
            msg.push_str(&format!(" (retention={}ms)", ret));
        }
        if let Some(cs) = query.chunk_size {
            msg.push_str(&format!(" (chunk_size={})", cs));
        }
        if !query.downsample_policies.is_empty() {
            msg.push_str(&format!(
                " (downsample_policies={})",
                query.downsample_policies.len()
            ));
        }
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &msg,
            "create",
        ))
    }

    pub fn execute_drop_timeseries(
        &self,
        raw_query: &str,
        query: &DropTimeSeriesQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();
        if store.get_collection(&query.name).is_none() {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("timeseries '{}' does not exist", query.name),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "timeseries '{}' not found",
                query.name
            )));
        }
        store
            .drop_collection(&query.name)
            .map_err(|e| RedDBError::Internal(e.to_string()))?;
        self.inner.db.clear_collection_default_ttl_ms(&query.name);
        self.inner
            .db
            .remove_collection_contract(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        remove_timeseries_metadata(store.as_ref(), &query.name);
        self.invalidate_result_cache();
        self.inner
            .db
            .persist_metadata()
            .map_err(|e| RedDBError::Internal(e.to_string()))?;
        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("timeseries '{}' dropped", query.name),
            "drop",
        ))
    }
}

fn save_timeseries_metadata(
    store: &crate::storage::unified::UnifiedStore,
    query: &CreateTimeSeriesQuery,
) -> RedDBResult<()> {
    remove_timeseries_metadata(store, &query.name);
    let _ = store.get_or_create_collection(TIMESERIES_META_COLLECTION);

    let mut fields = HashMap::new();
    fields.insert(
        "kind".to_string(),
        Value::Text("timeseries_config".to_string()),
    );
    fields.insert("series".to_string(), Value::Text(query.name.clone()));
    fields.insert(
        "retention_ms".to_string(),
        query
            .retention_ms
            .map(Value::UnsignedInteger)
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "chunk_size".to_string(),
        query
            .chunk_size
            .map(|value| Value::UnsignedInteger(value as u64))
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "downsample_policies".to_string(),
        Value::Array(
            query
                .downsample_policies
                .iter()
                .cloned()
                .map(Value::Text)
                .collect(),
        ),
    );

    store
        .insert_auto(
            TIMESERIES_META_COLLECTION,
            UnifiedEntity::new(
                EntityId::new(0),
                EntityKind::TableRow {
                    table: Arc::from(TIMESERIES_META_COLLECTION),
                    row_id: 0,
                },
                EntityData::Row(crate::storage::RowData {
                    columns: Vec::new(),
                    named: Some(fields),
                    schema: None,
                }),
            ),
        )
        .map_err(|err| RedDBError::Internal(err.to_string()))?;

    Ok(())
}

fn remove_timeseries_metadata(store: &crate::storage::unified::UnifiedStore, series: &str) {
    let Some(manager) = store.get_collection(TIMESERIES_META_COLLECTION) else {
        return;
    };
    let rows = manager.query_all(|entity| {
        entity.data.as_row().is_some_and(|row| {
            row.get_field("series")
                .is_some_and(|value| matches!(value, Value::Text(candidate) if candidate == series))
        })
    });
    for row in rows {
        let _ = store.delete(TIMESERIES_META_COLLECTION, row.id);
    }
}

fn timeseries_collection_contract(
    query: &CreateTimeSeriesQuery,
) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    crate::physical::CollectionContract {
        name: query.name.clone(),
        declared_model: crate::catalog::CollectionModel::TimeSeries,
        schema_mode: crate::catalog::SchemaMode::SemiStructured,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: query.retention_ms,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
    }
}

fn current_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
