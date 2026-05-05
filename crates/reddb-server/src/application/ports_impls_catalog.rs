use super::*;
impl RuntimeCatalogPort for RedDBRuntime {
    fn collections(&self) -> Vec<String> {
        self.db().collections()
    }

    fn catalog(&self) -> CatalogModelSnapshot {
        RedDBRuntime::catalog(self)
    }

    fn catalog_consistency_report(&self) -> CatalogConsistencyReport {
        RedDBRuntime::catalog_consistency_report(self)
    }

    fn catalog_attention_summary(&self) -> CatalogAttentionSummary {
        RedDBRuntime::catalog_attention_summary(self)
    }

    fn collection_attention(&self) -> Vec<CollectionDescriptor> {
        RedDBRuntime::collection_attention(self)
    }

    fn indexes(&self) -> Vec<PhysicalIndexState> {
        RedDBRuntime::indexes(self)
    }

    fn declared_indexes(&self) -> Vec<PhysicalIndexState> {
        RedDBRuntime::declared_indexes(self)
    }

    fn indexes_for_collection(&self, collection: &str) -> Vec<PhysicalIndexState> {
        RedDBRuntime::indexes_for_collection(self, collection)
    }

    fn declared_indexes_for_collection(&self, collection: &str) -> Vec<PhysicalIndexState> {
        RedDBRuntime::declared_indexes_for_collection(self, collection)
    }

    fn index_statuses(&self) -> Vec<CatalogIndexStatus> {
        RedDBRuntime::index_statuses(self)
    }

    fn index_attention(&self) -> Vec<CatalogIndexStatus> {
        RedDBRuntime::index_attention(self)
    }

    fn graph_projections(&self) -> RedDBResult<Vec<PhysicalGraphProjection>> {
        RedDBRuntime::graph_projections(self)
    }

    fn operational_graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        RedDBRuntime::operational_graph_projections(self)
    }

    fn graph_projection_statuses(&self) -> Vec<CatalogGraphProjectionStatus> {
        RedDBRuntime::graph_projection_statuses(self)
    }

    fn graph_projection_attention(&self) -> Vec<CatalogGraphProjectionStatus> {
        RedDBRuntime::graph_projection_attention(self)
    }

    fn analytics_jobs(&self) -> RedDBResult<Vec<PhysicalAnalyticsJob>> {
        RedDBRuntime::analytics_jobs(self)
    }

    fn operational_analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        RedDBRuntime::operational_analytics_jobs(self)
    }

    fn analytics_job_statuses(&self) -> Vec<CatalogAnalyticsJobStatus> {
        RedDBRuntime::analytics_job_statuses(self)
    }

    fn analytics_job_attention(&self) -> Vec<CatalogAnalyticsJobStatus> {
        RedDBRuntime::analytics_job_attention(self)
    }

    fn stats(&self) -> RuntimeStats {
        RedDBRuntime::stats(self)
    }
}
