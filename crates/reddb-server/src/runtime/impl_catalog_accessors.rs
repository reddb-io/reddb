//! Runtime catalog / stats / maintenance accessors.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 8/10, issue #1629).
//! Houses connection `acquire`, `checkpoint`, the remote-write assertion,
//! `run_maintenance`, `scan_collection`, and the catalog /
//! attention-summary / `stats` readers.
use super::*;

fn runtime_pool_lock(runtime: &RedDBRuntime) -> std::sync::MutexGuard<'_, PoolState> {
    runtime
        .inner
        .pool
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl RedDBRuntime {
    pub fn acquire(&self) -> RedDBResult<RuntimeConnection> {
        let mut pool = self
            .inner
            .pool
            .lock()
            .map_err(|e| RedDBError::Internal(format!("connection pool lock poisoned: {e}")))?;
        if pool.active >= self.inner.pool_config.max_connections {
            return Err(RedDBError::Internal(
                "connection pool exhausted".to_string(),
            ));
        }

        let id = if let Some(id) = pool.idle.pop() {
            id
        } else {
            let id = pool.next_id;
            pool.next_id += 1;
            id
        };
        pool.active += 1;
        pool.total_checkouts += 1;
        drop(pool);

        // Issue #1245 — record the connection acquisition after releasing
        // the pool lock so the lock hold time is unchanged.
        self.inner.node_load_telemetry.record_connect();

        Ok(RuntimeConnection {
            id,
            inner: Arc::clone(&self.inner),
        })
    }

    pub fn checkpoint(&self) -> RedDBResult<()> {
        // Local fsync always allowed — losing the lease shouldn't
        // prevent us from durably persisting what's already in memory.
        // The remote upload is the side-effect that risks clobbering a
        // peer's state, so it's behind the lease gate.
        self.inner.db.flush_local_only().map_err(|err| {
            // Issue #205 — local flush failure is a CheckpointFailed
            // operator-grade event. The local-flush path also covers
            // the WAL fsync we depend on, so a failure here doubles as
            // the WalFsyncFailed signal for the runtime entry point.
            let msg = err.to_string();
            crate::telemetry::operator_event::OperatorEvent::CheckpointFailed {
                lsn: 0,
                error: msg.clone(),
            }
            .emit_global();
            crate::telemetry::operator_event::OperatorEvent::WalFsyncFailed {
                path: "<flush_local_only>".to_string(),
                error: msg.clone(),
            }
            .emit_global();
            RedDBError::Engine(msg)
        })?;
        if let Err(err) = self.assert_remote_write_allowed("checkpoint") {
            tracing::warn!(
                target: "reddb::serverless::lease",
                error = %err,
                "checkpoint: skipping remote upload — lease not held"
            );
            return Ok(());
        }
        self.inner
            .db
            .upload_to_remote_backend()
            .map_err(|err| RedDBError::Engine(err.to_string()))
    }

    /// Guard remote-mutating operations on the writer lease.
    /// Returns `Ok(())` when no remote backend is configured (the
    /// lease is irrelevant) or the lease state is `NotRequired` /
    /// `Held`. Returns `RedDBError::ReadOnly` when the lease is
    /// `NotHeld`, with an audit-friendly action label so the caller
    /// can record the rejection.
    pub(crate) fn assert_remote_write_allowed(&self, action: &str) -> RedDBResult<()> {
        if self.inner.db.remote_backend.is_none() {
            return Ok(());
        }
        match self.inner.write_gate.lease_state() {
            crate::runtime::write_gate::LeaseGateState::NotHeld => {
                self.inner.audit_log.record(
                    action,
                    "system",
                    "remote_backend",
                    "err: writer lease not held",
                    crate::json::Value::Null,
                );
                Err(RedDBError::ReadOnly(format!(
                    "writer lease not held — {action} blocked (serverless fence)"
                )))
            }
            _ => Ok(()),
        }
    }

    pub fn run_maintenance(&self) -> RedDBResult<()> {
        self.inner
            .db
            .run_maintenance()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        let store = self.inner.db.store();
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| RedDBError::NotFound(collection.to_string()))?;

        let mut entities = manager.query_all(|_| true);
        entities.sort_by_key(|entity| entity.id.raw());

        let offset = cursor.map(|cursor| cursor.offset).unwrap_or(0);
        let total = entities.len();
        let end = total.min(offset.saturating_add(limit.max(1)));
        let items = if offset >= total {
            Vec::new()
        } else {
            entities[offset..end].to_vec()
        };
        let next = (end < total).then_some(ScanCursor { offset: end });

        Ok(ScanPage {
            collection: collection.to_string(),
            items,
            next,
            total,
        })
    }

    pub fn catalog(&self) -> CatalogModelSnapshot {
        self.inner.db.catalog_model_snapshot()
    }

    pub fn catalog_consistency_report(&self) -> crate::catalog::CatalogConsistencyReport {
        self.inner.db.catalog_consistency_report()
    }

    pub fn catalog_attention_summary(&self) -> CatalogAttentionSummary {
        crate::catalog::attention_summary(&self.catalog())
    }

    pub fn collection_attention(&self) -> Vec<CollectionDescriptor> {
        crate::catalog::collection_attention(&self.catalog())
    }

    pub fn index_attention(&self) -> Vec<CatalogIndexStatus> {
        crate::catalog::index_attention(&self.catalog())
    }

    pub fn graph_projection_attention(&self) -> Vec<CatalogGraphProjectionStatus> {
        crate::catalog::graph_projection_attention(&self.catalog())
    }

    pub fn analytics_job_attention(&self) -> Vec<CatalogAnalyticsJobStatus> {
        crate::catalog::analytics_job_attention(&self.catalog())
    }

    pub fn stats(&self) -> RuntimeStats {
        let pool = runtime_pool_lock(self);
        RuntimeStats {
            active_connections: pool.active,
            idle_connections: pool.idle.len(),
            total_checkouts: pool.total_checkouts,
            paged_mode: self.inner.db.is_paged(),
            started_at_unix_ms: self.inner.started_at_unix_ms,
            store: self.inner.db.stats(),
            system: SystemInfo::collect(),
            result_blob_cache: self.inner.result_blob_cache.stats(),
            kv: self.inner.kv_stats.snapshot(),
            metrics_ingest: self.inner.metrics_ingest_stats.snapshot(),
        }
    }
}
