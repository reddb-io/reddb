use super::*;
impl RuntimeAdminPort for RedDBRuntime {
    fn set_index_enabled(&self, name: &str, enabled: bool) -> RedDBResult<PhysicalIndexState> {
        RedDBRuntime::set_index_enabled(self, name, enabled)
    }

    fn mark_index_building(&self, name: &str) -> RedDBResult<PhysicalIndexState> {
        RedDBRuntime::mark_index_building(self, name)
    }

    fn fail_index(&self, name: &str) -> RedDBResult<PhysicalIndexState> {
        RedDBRuntime::fail_index(self, name)
    }

    fn mark_index_stale(&self, name: &str) -> RedDBResult<PhysicalIndexState> {
        RedDBRuntime::mark_index_stale(self, name)
    }

    fn mark_index_ready(&self, name: &str) -> RedDBResult<PhysicalIndexState> {
        RedDBRuntime::mark_index_ready(self, name)
    }

    fn warmup_index_with_lifecycle(&self, name: &str) -> RedDBResult<PhysicalIndexState> {
        RedDBRuntime::warmup_index_with_lifecycle(self, name)
    }

    fn rebuild_indexes_with_lifecycle(
        &self,
        collection: Option<&str>,
    ) -> RedDBResult<Vec<PhysicalIndexState>> {
        RedDBRuntime::rebuild_indexes_with_lifecycle(self, collection)
    }

    fn save_graph_projection(
        &self,
        name: impl Into<String>,
        projection: RuntimeGraphProjection,
        source: Option<String>,
    ) -> RedDBResult<PhysicalGraphProjection> {
        RedDBRuntime::save_graph_projection(self, name, projection, source)
    }

    fn mark_graph_projection_materializing(
        &self,
        name: &str,
    ) -> RedDBResult<PhysicalGraphProjection> {
        RedDBRuntime::mark_graph_projection_materializing(self, name)
    }

    fn materialize_graph_projection(&self, name: &str) -> RedDBResult<PhysicalGraphProjection> {
        RedDBRuntime::materialize_graph_projection(self, name)
    }

    fn fail_graph_projection(&self, name: &str) -> RedDBResult<PhysicalGraphProjection> {
        RedDBRuntime::fail_graph_projection(self, name)
    }

    fn mark_graph_projection_stale(&self, name: &str) -> RedDBResult<PhysicalGraphProjection> {
        RedDBRuntime::mark_graph_projection_stale(self, name)
    }

    fn save_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        RedDBRuntime::save_analytics_job(self, kind, projection_name, metadata)
    }

    fn start_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        RedDBRuntime::start_analytics_job(self, kind, projection_name, metadata)
    }

    fn queue_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        RedDBRuntime::queue_analytics_job(self, kind, projection_name, metadata)
    }

    fn fail_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        RedDBRuntime::fail_analytics_job(self, kind, projection_name, metadata)
    }

    fn mark_analytics_job_stale(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        RedDBRuntime::mark_analytics_job_stale(self, kind, projection_name, metadata)
    }

    fn complete_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        RedDBRuntime::complete_analytics_job(self, kind, projection_name, metadata)
    }
}
