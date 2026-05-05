use crate::application::ports::RuntimeCatalogPort;
use crate::catalog::{
    CatalogAnalyticsJobStatus, CatalogAttentionSummary, CatalogConsistencyReport,
    CatalogGraphProjectionStatus, CatalogIndexStatus, CatalogModelSnapshot, CollectionDescriptor,
};
use crate::runtime::RuntimeStats;
use crate::{PhysicalAnalyticsJob, PhysicalGraphProjection, PhysicalIndexState, RedDBResult};

pub struct CatalogUseCases<'a, P: ?Sized> {
    runtime: &'a P,
}

impl<'a, P: RuntimeCatalogPort + ?Sized> CatalogUseCases<'a, P> {
    pub fn new(runtime: &'a P) -> Self {
        Self { runtime }
    }

    pub fn collections(&self) -> Vec<String> {
        self.runtime.collections()
    }

    pub fn snapshot(&self) -> CatalogModelSnapshot {
        self.runtime.catalog()
    }

    pub fn consistency_report(&self) -> CatalogConsistencyReport {
        self.runtime.catalog_consistency_report()
    }

    pub fn attention_summary(&self) -> CatalogAttentionSummary {
        self.runtime.catalog_attention_summary()
    }

    pub fn collection_attention(&self) -> Vec<CollectionDescriptor> {
        self.runtime.collection_attention()
    }

    pub fn indexes(&self) -> Vec<PhysicalIndexState> {
        self.runtime.indexes()
    }

    pub fn declared_indexes(&self) -> Vec<PhysicalIndexState> {
        self.runtime.declared_indexes()
    }

    pub fn indexes_for_collection(&self, collection: &str) -> Vec<PhysicalIndexState> {
        self.runtime.indexes_for_collection(collection)
    }

    pub fn declared_indexes_for_collection(&self, collection: &str) -> Vec<PhysicalIndexState> {
        self.runtime.declared_indexes_for_collection(collection)
    }

    pub fn index_statuses(&self) -> Vec<CatalogIndexStatus> {
        self.runtime.index_statuses()
    }

    pub fn index_attention(&self) -> Vec<CatalogIndexStatus> {
        self.runtime.index_attention()
    }

    pub fn graph_projections(&self) -> RedDBResult<Vec<PhysicalGraphProjection>> {
        self.runtime.graph_projections()
    }

    pub fn operational_graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        self.runtime.operational_graph_projections()
    }

    pub fn graph_projection_statuses(&self) -> Vec<CatalogGraphProjectionStatus> {
        self.runtime.graph_projection_statuses()
    }

    pub fn graph_projection_attention(&self) -> Vec<CatalogGraphProjectionStatus> {
        self.runtime.graph_projection_attention()
    }

    pub fn analytics_jobs(&self) -> RedDBResult<Vec<PhysicalAnalyticsJob>> {
        self.runtime.analytics_jobs()
    }

    pub fn operational_analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        self.runtime.operational_analytics_jobs()
    }

    pub fn analytics_job_statuses(&self) -> Vec<CatalogAnalyticsJobStatus> {
        self.runtime.analytics_job_statuses()
    }

    pub fn analytics_job_attention(&self) -> Vec<CatalogAnalyticsJobStatus> {
        self.runtime.analytics_job_attention()
    }

    pub fn stats(&self) -> RuntimeStats {
        self.runtime.stats()
    }
}
