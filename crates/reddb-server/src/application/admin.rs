use std::collections::BTreeMap;

use crate::application::ports::RuntimeAdminPort;
use crate::catalog::{CatalogAnalyticsJobStatus, CatalogGraphProjectionStatus, CatalogIndexStatus};
use crate::runtime::RuntimeGraphProjection;
use crate::{PhysicalAnalyticsJob, PhysicalGraphProjection, PhysicalIndexState, RedDBResult};

#[derive(Debug, Clone, Default)]
pub struct ServerlessAnalyticsWarmupTarget {
    pub kind: String,
    pub projection: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ServerlessWarmupPlan {
    pub indexes: Vec<String>,
    pub graph_projections: Vec<String>,
    pub analytics_jobs: Vec<ServerlessAnalyticsWarmupTarget>,
    pub includes_native_artifacts: bool,
}

pub struct AdminUseCases<'a, P: ?Sized> {
    runtime: &'a P,
}

impl<'a, P: RuntimeAdminPort + ?Sized> AdminUseCases<'a, P> {
    pub fn new(runtime: &'a P) -> Self {
        Self { runtime }
    }

    pub fn set_index_enabled(&self, name: &str, enabled: bool) -> RedDBResult<PhysicalIndexState> {
        self.runtime.set_index_enabled(name, enabled)
    }

    pub fn mark_index_building(&self, name: &str) -> RedDBResult<PhysicalIndexState> {
        self.runtime.mark_index_building(name)
    }

    pub fn fail_index(&self, name: &str) -> RedDBResult<PhysicalIndexState> {
        self.runtime.fail_index(name)
    }

    pub fn mark_index_stale(&self, name: &str) -> RedDBResult<PhysicalIndexState> {
        self.runtime.mark_index_stale(name)
    }

    pub fn mark_index_ready(&self, name: &str) -> RedDBResult<PhysicalIndexState> {
        self.runtime.mark_index_ready(name)
    }

    pub fn warmup_index(&self, name: &str) -> RedDBResult<PhysicalIndexState> {
        self.runtime.warmup_index_with_lifecycle(name)
    }

    pub fn rebuild_indexes(
        &self,
        collection: Option<&str>,
    ) -> RedDBResult<Vec<PhysicalIndexState>> {
        self.runtime.rebuild_indexes_with_lifecycle(collection)
    }

    pub fn save_graph_projection(
        &self,
        name: impl Into<String>,
        projection: RuntimeGraphProjection,
        source: Option<String>,
    ) -> RedDBResult<PhysicalGraphProjection> {
        self.runtime.save_graph_projection(name, projection, source)
    }

    pub fn mark_graph_projection_materializing(
        &self,
        name: &str,
    ) -> RedDBResult<PhysicalGraphProjection> {
        self.runtime.mark_graph_projection_materializing(name)
    }

    pub fn materialize_graph_projection(&self, name: &str) -> RedDBResult<PhysicalGraphProjection> {
        self.runtime.materialize_graph_projection(name)
    }

    pub fn fail_graph_projection(&self, name: &str) -> RedDBResult<PhysicalGraphProjection> {
        self.runtime.fail_graph_projection(name)
    }

    pub fn mark_graph_projection_stale(&self, name: &str) -> RedDBResult<PhysicalGraphProjection> {
        self.runtime.mark_graph_projection_stale(name)
    }

    pub fn save_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.runtime
            .save_analytics_job(kind, projection_name, metadata)
    }

    pub fn start_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.runtime
            .start_analytics_job(kind, projection_name, metadata)
    }

    pub fn queue_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.runtime
            .queue_analytics_job(kind, projection_name, metadata)
    }

    pub fn fail_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.runtime
            .fail_analytics_job(kind, projection_name, metadata)
    }

    pub fn mark_analytics_job_stale(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.runtime
            .mark_analytics_job_stale(kind, projection_name, metadata)
    }

    pub fn complete_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.runtime
            .complete_analytics_job(kind, projection_name, metadata)
    }

    pub fn build_serverless_warmup_plan(
        &self,
        index_statuses: &[CatalogIndexStatus],
        graph_projection_statuses: &[CatalogGraphProjectionStatus],
        analytics_job_statuses: &[CatalogAnalyticsJobStatus],
        force: bool,
        include_indexes: bool,
        include_graph_projections: bool,
        include_analytics_jobs: bool,
        include_native_artifacts: bool,
    ) -> ServerlessWarmupPlan {
        let mut plan = ServerlessWarmupPlan::default();

        if include_indexes {
            for status in index_statuses {
                if !status.declared {
                    continue;
                }
                if force || status.requires_rebuild {
                    plan.indexes.push(status.name.clone());
                }
            }
        }

        if include_graph_projections {
            for status in graph_projection_statuses {
                if !status.declared {
                    continue;
                }
                let should_rematerialize = force
                    || status.requires_rematerialization
                    || status.rerun_required
                    || !status.dependent_jobs_in_sync;
                if should_rematerialize {
                    plan.graph_projections.push(status.name.clone());
                }
            }
        }

        if include_analytics_jobs {
            for status in analytics_job_statuses {
                if !status.declared {
                    continue;
                }
                if force || status.requires_rerun || status.executable {
                    plan.analytics_jobs.push(ServerlessAnalyticsWarmupTarget {
                        kind: status.kind.clone(),
                        projection: status.projection.clone(),
                    });
                }
            }
        }

        plan.includes_native_artifacts = include_native_artifacts;
        plan
    }
}
