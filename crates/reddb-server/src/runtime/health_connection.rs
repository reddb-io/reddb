use super::*;

impl HealthProvider for RedDBRuntime {
    fn health(&self) -> HealthReport {
        self.health_with_runtime_diagnostics(self.inner.db.health())
    }
}

impl RedDBRuntime {
    pub(crate) fn health_from_catalog(
        &self,
        catalog: &crate::catalog::CatalogModelSnapshot,
    ) -> HealthReport {
        self.health_with_runtime_diagnostics(self.inner.db.health_from_catalog(catalog))
    }

    fn health_with_runtime_diagnostics(&self, mut report: HealthReport) -> HealthReport {
        // Pool counters need a short observation, not a lock held across
        // catalog inspection and checkpoint publication waits.
        let (active_connections, idle_connections) = {
            let pool = self
                .inner
                .pool
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (pool.active, pool.idle.len())
        };
        let (readiness_for_query, readiness_for_write, readiness_for_repair) =
            self.inner.db.readiness_flags_from_health(&report);
        report = report.with_diagnostic(
            "runtime.mode",
            if self.inner.layout.is_persistent() {
                "persistent"
            } else {
                "in-memory"
            },
        );
        report =
            report.with_diagnostic("runtime.active_connections", active_connections.to_string());
        report = report.with_diagnostic("runtime.idle_connections", idle_connections.to_string());
        report = report.with_diagnostic("readiness_for_query", readiness_for_query.to_string());
        report = report.with_diagnostic("readiness_for_write", readiness_for_write.to_string());
        report = report.with_diagnostic("readiness_for_repair", readiness_for_repair.to_string());
        report.with_diagnostic(
            "runtime.max_connections",
            self.inner.pool_config.max_connections.to_string(),
        )
    }
}

impl RuntimeConnection {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn db(&self) -> Arc<RedDB> {
        Arc::clone(&self.inner.db)
    }

    pub fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        RedDBRuntime {
            background_tasks: self.background_tasks.clone(),
            inner: Arc::clone(&self.inner),
        }
        .scan_collection(collection, cursor, limit)
    }
}

impl Drop for RuntimeConnection {
    fn drop(&mut self) {
        let runtime = RedDBRuntime {
            background_tasks: self.background_tasks.clone(),
            inner: Arc::clone(&self.inner),
        };
        // Cleanup targets this lease, independently of the dropping thread's
        // current connection. Never recycle a lease whose cleanup failed.
        let reusable = match runtime.rollback_connection(self.id) {
            Ok(_) => true,
            Err(error) => {
                tracing::error!(connection_id = self.id, %error, "connection rollback failed");
                false
            }
        };
        let mut pool = self
            .inner
            .pool
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pool.active = pool.active.saturating_sub(1);
        if self.uses_pool_slot {
            pool.pooled_active = pool.pooled_active.saturating_sub(1);
        }
        if reusable && pool.idle.len() < self.inner.pool_config.max_idle {
            pool.idle.push(self.id);
        }
        drop(pool);
        // Issue #1245 — record the disconnect after releasing the pool lock.
        self.inner.node_load_telemetry.record_disconnect();
    }
}
