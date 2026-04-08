use super::*;

impl RedDBRuntime {
    pub fn create_export(&self, name: impl Into<String>) -> RedDBResult<ExportDescriptor> {
        self.inner
            .db
            .create_named_export(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn graph_projections(&self) -> RedDBResult<Vec<PhysicalGraphProjection>> {
        Ok(self.inner.db.declared_graph_projections())
    }

    pub fn operational_graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        self.inner.db.operational_graph_projections()
    }

    pub fn graph_projection_named(&self, name: &str) -> RedDBResult<RuntimeGraphProjection> {
        let status = self
            .graph_projection_statuses()
            .into_iter()
            .find(|status| status.name == name)
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))?;
        if !status.declared {
            return Err(RedDBError::Catalog(format!(
                "graph projection '{name}' is not declared"
            )));
        }
        if !status.operational {
            return Err(RedDBError::Catalog(format!(
                "graph projection '{name}' is declared but not operationally materialized"
            )));
        }
        if status.lifecycle_state == "stale" {
            return Err(RedDBError::Catalog(format!(
                "graph projection '{name}' is stale and must be rematerialized before use"
            )));
        }
        let projection = self
            .operational_graph_projections()
            .into_iter()
            .find(|projection| projection.name == name)
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))?;
        Ok(RuntimeGraphProjection {
            node_labels: (!projection.node_labels.is_empty()).then_some(projection.node_labels),
            node_types: (!projection.node_types.is_empty()).then_some(projection.node_types),
            edge_labels: (!projection.edge_labels.is_empty()).then_some(projection.edge_labels),
        })
    }

    pub fn save_graph_projection(
        &self,
        name: impl Into<String>,
        projection: RuntimeGraphProjection,
        source: Option<String>,
    ) -> RedDBResult<PhysicalGraphProjection> {
        self.inner
            .db
            .save_graph_projection(
                name,
                projection.node_labels.unwrap_or_default(),
                projection.node_types.unwrap_or_default(),
                projection.edge_labels.unwrap_or_default(),
                source.unwrap_or_else(|| "runtime".to_string()),
            )
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn materialize_graph_projection(
        &self,
        name: &str,
    ) -> RedDBResult<PhysicalGraphProjection> {
        self.inner
            .db
            .materialize_graph_projection(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn mark_graph_projection_materializing(
        &self,
        name: &str,
    ) -> RedDBResult<PhysicalGraphProjection> {
        self.inner
            .db
            .mark_graph_projection_materializing(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn fail_graph_projection(
        &self,
        name: &str,
    ) -> RedDBResult<PhysicalGraphProjection> {
        self.inner
            .db
            .fail_graph_projection(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn mark_graph_projection_stale(
        &self,
        name: &str,
    ) -> RedDBResult<PhysicalGraphProjection> {
        self.inner
            .db
            .mark_graph_projection_stale(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn analytics_jobs(&self) -> RedDBResult<Vec<PhysicalAnalyticsJob>> {
        Ok(self.inner.db.declared_analytics_jobs())
    }

    pub fn operational_analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        self.inner.db.operational_analytics_jobs()
    }

    pub fn save_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: std::collections::BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.inner
            .db
            .save_analytics_job(kind, projection_name, metadata)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn start_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: std::collections::BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        if let Some(projection_name) = projection_name.as_deref() {
            let status = self
                .graph_projection_statuses()
                .into_iter()
                .find(|status| status.name == projection_name)
                .ok_or_else(|| RedDBError::NotFound(projection_name.to_string()))?;
            if !status.declared {
                return Err(RedDBError::Catalog(format!(
                    "graph projection '{projection_name}' is not declared"
                )));
            }
            if !status.operational {
                return Err(RedDBError::Catalog(format!(
                    "graph projection '{projection_name}' is declared but not operationally materialized"
                )));
            }
            if status.lifecycle_state == "stale" {
                return Err(RedDBError::Catalog(format!(
                    "graph projection '{projection_name}' is stale and must be rematerialized before analytics jobs can start against it"
                )));
            }
        }
        self.inner
            .db
            .start_analytics_job(kind, projection_name, metadata)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn queue_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: std::collections::BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        if let Some(projection_name) = projection_name.as_deref() {
            let status = self
                .graph_projection_statuses()
                .into_iter()
                .find(|status| status.name == projection_name)
                .ok_or_else(|| RedDBError::NotFound(projection_name.to_string()))?;
            if !status.declared {
                return Err(RedDBError::Catalog(format!(
                    "graph projection '{projection_name}' is not declared"
                )));
            }
            if !status.operational {
                return Err(RedDBError::Catalog(format!(
                    "graph projection '{projection_name}' is declared but not operationally materialized"
                )));
            }
            if status.lifecycle_state == "stale" {
                return Err(RedDBError::Catalog(format!(
                    "graph projection '{projection_name}' is stale and must be rematerialized before analytics jobs can be queued against it"
                )));
            }
        }
        self.inner
            .db
            .queue_analytics_job(kind, projection_name, metadata)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn fail_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: std::collections::BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.inner
            .db
            .fail_analytics_job(kind, projection_name, metadata)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn mark_analytics_job_stale(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: std::collections::BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.inner
            .db
            .mark_analytics_job_stale(kind, projection_name, metadata)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn complete_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: std::collections::BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        self.inner
            .db
            .record_analytics_job(kind, projection_name, metadata)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn record_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: std::collections::BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob> {
        if let Some(projection_name) = projection_name.as_deref() {
            let status = self
                .graph_projection_statuses()
                .into_iter()
                .find(|status| status.name == projection_name)
                .ok_or_else(|| RedDBError::NotFound(projection_name.to_string()))?;
            if !status.declared {
                return Err(RedDBError::Catalog(format!(
                    "graph projection '{projection_name}' is not declared"
                )));
            }
            if !status.operational {
                return Err(RedDBError::Catalog(format!(
                    "graph projection '{projection_name}' is declared but not operationally materialized"
                )));
            }
            if status.lifecycle_state == "stale" {
                return Err(RedDBError::Catalog(format!(
                    "graph projection '{projection_name}' is stale and must be rematerialized before analytics jobs can complete against it"
                )));
            }
        }
        self.inner
            .db
            .record_analytics_job(kind, projection_name, metadata)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn resolve_graph_projection(
        &self,
        projection_name: Option<&str>,
        inline: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<Option<RuntimeGraphProjection>> {
        let named = match projection_name {
            Some(name) => Some(self.graph_projection_named(name)?),
            None => None,
        };
        Ok(merge_runtime_projection(named, inline))
    }

    pub fn apply_retention_policy(&self) -> RedDBResult<()> {
        self.inner
            .db
            .enforce_retention_policy()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn indexes(&self) -> Vec<crate::PhysicalIndexState> {
        self.inner.db.operational_indexes()
    }

    pub fn declared_indexes(&self) -> Vec<crate::PhysicalIndexState> {
        self.inner.db.declared_indexes()
    }

    pub fn declared_indexes_for_collection(&self, collection: &str) -> Vec<crate::PhysicalIndexState> {
        self.inner
            .db
            .declared_indexes()
            .into_iter()
            .filter(|index| index.collection.as_deref() == Some(collection))
            .collect()
    }

    pub fn index_statuses(&self) -> Vec<crate::catalog::CatalogIndexStatus> {
        self.inner.db.index_statuses()
    }

    pub fn graph_projection_statuses(&self) -> Vec<crate::catalog::CatalogGraphProjectionStatus> {
        self.inner.db.catalog_model_snapshot().graph_projection_statuses
    }

    pub fn analytics_job_statuses(&self) -> Vec<crate::catalog::CatalogAnalyticsJobStatus> {
        self.inner.db.catalog_model_snapshot().analytics_job_statuses
    }

    pub fn indexes_for_collection(&self, collection: &str) -> Vec<crate::PhysicalIndexState> {
        self.inner
            .db
            .operational_indexes()
            .into_iter()
            .filter(|index| index.collection.as_deref() == Some(collection))
            .collect()
    }

    pub fn set_index_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> RedDBResult<crate::PhysicalIndexState> {
        self.inner
            .db
            .set_index_enabled(name, enabled)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn mark_index_building(&self, name: &str) -> RedDBResult<crate::PhysicalIndexState> {
        self.inner
            .db
            .mark_index_building(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn fail_index(&self, name: &str) -> RedDBResult<crate::PhysicalIndexState> {
        self.inner
            .db
            .fail_index(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn mark_index_stale(&self, name: &str) -> RedDBResult<crate::PhysicalIndexState> {
        self.inner
            .db
            .mark_index_stale(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn mark_index_ready(&self, name: &str) -> RedDBResult<crate::PhysicalIndexState> {
        self.inner
            .db
            .mark_index_ready(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn warmup_index_with_lifecycle(
        &self,
        name: &str,
    ) -> RedDBResult<crate::PhysicalIndexState> {
        self.mark_index_building(name)?;
        match self.warmup_index(name) {
            Ok(index) => Ok(index),
            Err(err) => {
                let _ = self.fail_index(name);
                Err(err)
            }
        }
    }

    pub fn warmup_index(&self, name: &str) -> RedDBResult<crate::PhysicalIndexState> {
        self.inner
            .db
            .warmup_index(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn rebuild_indexes(
        &self,
        collection: Option<&str>,
    ) -> RedDBResult<Vec<crate::PhysicalIndexState>> {
        self.inner
            .db
            .rebuild_index_registry(collection)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn rebuild_indexes_with_lifecycle(
        &self,
        collection: Option<&str>,
    ) -> RedDBResult<Vec<crate::PhysicalIndexState>> {
        let target_names: Vec<String> = match collection {
            Some(collection) => self
                .declared_indexes_for_collection(collection)
                .into_iter()
                .map(|index| index.name)
                .collect(),
            None => self
                .declared_indexes()
                .into_iter()
                .map(|index| index.name)
                .collect(),
        };

        let mut marked_building = Vec::new();
        for name in target_names {
            if self.mark_index_building(&name).is_ok() {
                marked_building.push(name);
            }
        }

        match self.rebuild_indexes(collection) {
            Ok(indexes) => Ok(indexes),
            Err(err) => {
                for name in marked_building {
                    let _ = self.fail_index(&name);
                }
                Err(err)
            }
        }
    }

}
