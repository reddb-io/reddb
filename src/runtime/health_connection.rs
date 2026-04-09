use super::*;

impl HealthProvider for RedDBRuntime {
    fn health(&self) -> HealthReport {
        let pool = self.inner.pool.lock().expect("health: connection pool lock poisoned");
        let mut report = self.inner.db.health();
        let (readiness_for_query, readiness_for_write, readiness_for_repair) =
            self.inner.db.readiness_flags_from_health(&report);
        report = report.with_diagnostic("runtime.mode", if self.inner.layout.is_persistent() {
            "persistent"
        } else {
            "in-memory"
        });
        report = report.with_diagnostic("runtime.active_connections", pool.active.to_string());
        report = report.with_diagnostic("runtime.idle_connections", pool.idle.len().to_string());
        report = report.with_diagnostic(
            "readiness_for_query",
            readiness_for_query.to_string(),
        );
        report = report.with_diagnostic(
            "readiness_for_write",
            readiness_for_write.to_string(),
        );
        report = report.with_diagnostic(
            "readiness_for_repair",
            readiness_for_repair.to_string(),
        );
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
            inner: Arc::clone(&self.inner),
        }
        .scan_collection(collection, cursor, limit)
    }
}

impl Drop for RuntimeConnection {
    fn drop(&mut self) {
        let mut pool = self.inner.pool.lock().expect("drop RuntimeConnection: connection pool lock poisoned");
        pool.active = pool.active.saturating_sub(1);
        if pool.idle.len() < self.inner.pool_config.max_idle {
            pool.idle.push(self.id);
        }
    }
}
