//! Time-series DDL execution

use super::*;

impl RedDBRuntime {
    pub fn execute_create_timeseries(
        &self,
        raw_query: &str,
        query: &CreateTimeSeriesQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
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
            .persist_metadata()
            .map_err(|e| RedDBError::Internal(e.to_string()))?;

        let mut msg = format!("timeseries '{}' created", query.name);
        if let Some(ret) = query.retention_ms {
            msg.push_str(&format!(" (retention={}ms)", ret));
        }
        if let Some(cs) = query.chunk_size {
            msg.push_str(&format!(" (chunk_size={})", cs));
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
