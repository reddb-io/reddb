use super::*;

#[derive(Debug, Clone)]
struct MetricsRetentionRollupPolicy {
    target: String,
    aggregation: String,
    bucket_ns: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MetricsRollupKey {
    metric: String,
    bucket_ns: u64,
    tags: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
struct MetricsRollupAccumulator {
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
}

impl MetricsRollupAccumulator {
    fn new(value: f64) -> Self {
        Self {
            count: 1,
            sum: value,
            min: value,
            max: value,
        }
    }

    fn push(&mut self, value: f64) {
        self.count = self.count.saturating_add(1);
        self.sum += value;
        self.min = self.min.min(value);
        self.max = self.max.max(value);
    }

    fn value(&self, aggregation: &str) -> f64 {
        match aggregation {
            "sum" => self.sum,
            "min" => self.min,
            "max" => self.max,
            "count" => self.count as f64,
            _ => self.sum / self.count.max(1) as f64,
        }
    }
}

fn metrics_rollup_policies_for_retention(
    contract: &crate::physical::CollectionContract,
) -> Vec<MetricsRetentionRollupPolicy> {
    contract
        .metrics_rollup_policies
        .iter()
        .filter_map(|spec| {
            let parsed = crate::storage::timeseries::retention::DownsamplePolicy::parse(spec)?;
            if parsed.source != "raw"
                || !matches!(
                    parsed.aggregation.as_str(),
                    "avg" | "sum" | "min" | "max" | "count"
                )
            {
                return None;
            }
            Some(MetricsRetentionRollupPolicy {
                target: parsed.target,
                aggregation: parsed.aggregation,
                bucket_ns: parsed.bucket_ns,
            })
        })
        .collect()
}

fn metrics_rollup_collection_for_retention(raw_collection: &str, target: &str) -> String {
    let sanitized = target
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("red_metrics_rollup_{raw_collection}_{sanitized}")
}

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

    pub fn materialize_graph_projection(&self, name: &str) -> RedDBResult<PhysicalGraphProjection> {
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

    pub fn fail_graph_projection(&self, name: &str) -> RedDBResult<PhysicalGraphProjection> {
        self.inner
            .db
            .fail_graph_projection(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?
            .ok_or_else(|| RedDBError::NotFound(name.to_string()))
    }

    pub fn mark_graph_projection_stale(&self, name: &str) -> RedDBResult<PhysicalGraphProjection> {
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
        self.check_write(crate::runtime::write_gate::WriteKind::Maintenance)?;
        let now_ms = current_unix_ms_u64();
        self.retire_expired_append_only_segments(now_ms)?;
        let expired = self
            .inner
            .db
            .ttl_expired_entities_now()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        let store = self.inner.db.store();
        for (collection, id) in expired {
            let deleted = store
                .delete(&collection, id)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            if deleted {
                store.context_index().remove_entity(id);
                self.cdc_emit(
                    crate::replication::cdc::ChangeOperation::Delete,
                    &collection,
                    id.raw(),
                    "entity",
                );
            }
        }
        self.inner
            .db
            .enforce_retention_policy()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.enforce_metrics_raw_retention()?;
        self.invalidate_result_cache();
        Ok(())
    }

    pub(crate) fn retire_expired_append_only_segments(&self, now_ms: u64) -> RedDBResult<u64> {
        let Some(path) = self.inner.db.path() else {
            return Ok(0);
        };
        let manifest = reddb_file::OperationalManifest::for_db_path(path);
        let segments = manifest
            .append_only_segments()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if segments.is_empty() {
            return Ok(0);
        }

        let mut total_retired = 0u64;
        for contract in self
            .inner
            .db
            .collection_contracts()
            .into_iter()
            .filter(|contract| contract.append_only)
        {
            let Some(retention_ms) = contract.retention_duration_ms else {
                continue;
            };
            let cutoff = (now_ms as i64).saturating_sub(retention_ms as i64);
            let mut retired_for_collection = 0u64;
            for segment in segments
                .iter()
                .filter(|segment| segment.collection == contract.name)
                .filter(|segment| segment.retention_max_ms.is_some_and(|max| max < cutoff))
            {
                manifest
                    .begin_retire_append_only_segment(&segment.collection, segment.segment_id)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                if manifest
                    .finish_retire_append_only_segment(&segment.collection, segment.segment_id)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?
                {
                    retired_for_collection = retired_for_collection.saturating_add(1);
                }
            }
            if retired_for_collection > 0 {
                self.inner
                    .retention_sweeper
                    .write()
                    .record_segments_retired(&contract.name, retired_for_collection, now_ms);
                total_retired = total_retired.saturating_add(retired_for_collection);
            }
        }
        Ok(total_retired)
    }

    fn enforce_metrics_raw_retention(&self) -> RedDBResult<()> {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64;
        let store = self.inner.db.store();

        for contract in self
            .inner
            .db
            .collection_contracts()
            .into_iter()
            .filter(|contract| contract.declared_model == crate::catalog::CollectionModel::Metrics)
        {
            let Some(raw_retention_ms) = contract.metrics_raw_retention_ms else {
                continue;
            };
            let cutoff_ns = now_ns.saturating_sub(raw_retention_ms.saturating_mul(1_000_000));
            let Some(manager) = store.get_collection(&contract.name) else {
                continue;
            };
            let expired = manager.query_all(|entity| match &entity.data {
                crate::storage::EntityData::TimeSeries(point) => point.timestamp_ns < cutoff_ns,
                _ => false,
            });
            self.materialize_metrics_rollups_for_retention(&contract, &expired)?;
            for entity in expired {
                let deleted = store
                    .delete(&contract.name, entity.id)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                if deleted {
                    store.context_index().remove_entity(entity.id);
                    self.cdc_emit(
                        crate::replication::cdc::ChangeOperation::Delete,
                        &contract.name,
                        entity.id.raw(),
                        "entity",
                    );
                }
            }
        }

        Ok(())
    }

    fn materialize_metrics_rollups_for_retention(
        &self,
        contract: &crate::physical::CollectionContract,
        raw_points: &[crate::storage::UnifiedEntity],
    ) -> RedDBResult<()> {
        if raw_points.is_empty() {
            return Ok(());
        }
        let policies = metrics_rollup_policies_for_retention(contract);
        if policies.is_empty() {
            return Ok(());
        }

        let store = self.inner.db.store();
        for policy in policies {
            let rollup_collection =
                metrics_rollup_collection_for_retention(&contract.name, &policy.target);
            if store.get_collection(&rollup_collection).is_none() {
                store
                    .create_collection(&rollup_collection)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
            }

            let mut buckets = BTreeMap::<MetricsRollupKey, MetricsRollupAccumulator>::new();
            for entity in raw_points {
                let crate::storage::EntityData::TimeSeries(point) = &entity.data else {
                    continue;
                };
                let bucket_ns = (point.timestamp_ns / policy.bucket_ns) * policy.bucket_ns;
                let mut tags = point
                    .tags
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>();
                tags.sort();
                let key = MetricsRollupKey {
                    metric: point.metric.clone(),
                    bucket_ns,
                    tags,
                };
                buckets
                    .entry(key)
                    .and_modify(|acc| acc.push(point.value))
                    .or_insert_with(|| MetricsRollupAccumulator::new(point.value));
            }

            for (key, accumulator) in buckets {
                let tags = key.tags.into_iter().collect::<HashMap<_, _>>();
                if let Some(manager) = store.get_collection(&rollup_collection) {
                    for entity in manager.query_all(|entity| match &entity.data {
                        crate::storage::EntityData::TimeSeries(point) => {
                            point.metric == key.metric
                                && point.timestamp_ns == key.bucket_ns
                                && point.tags == tags
                        }
                        _ => false,
                    }) {
                        store
                            .delete(&rollup_collection, entity.id)
                            .map_err(|err| RedDBError::Internal(err.to_string()))?;
                    }
                }

                let entity = crate::storage::UnifiedEntity::new(
                    crate::storage::EntityId::new(0),
                    crate::storage::EntityKind::TimeSeriesPoint(Box::new(
                        crate::storage::TimeSeriesPointKind {
                            series: rollup_collection.clone(),
                            metric: key.metric.clone(),
                        },
                    )),
                    crate::storage::EntityData::TimeSeries(crate::storage::TimeSeriesData {
                        metric: key.metric,
                        timestamp_ns: key.bucket_ns,
                        value: accumulator.value(&policy.aggregation),
                        tags,
                        // Rollup outputs keep tags inline; they do not intern
                        // through the series dictionary.
                        series_id: None,
                    }),
                );
                store
                    .insert_auto(&rollup_collection, entity)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
            }
        }

        Ok(())
    }

    pub fn indexes(&self) -> Vec<crate::PhysicalIndexState> {
        self.inner.db.operational_indexes()
    }

    pub fn declared_indexes(&self) -> Vec<crate::PhysicalIndexState> {
        self.inner.db.declared_indexes()
    }

    pub fn declared_indexes_for_collection(
        &self,
        collection: &str,
    ) -> Vec<crate::PhysicalIndexState> {
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
        self.inner
            .db
            .catalog_model_snapshot()
            .graph_projection_statuses
    }

    pub fn analytics_job_statuses(&self) -> Vec<crate::catalog::CatalogAnalyticsJobStatus> {
        self.inner
            .db
            .catalog_model_snapshot()
            .analytics_job_statuses
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
        self.check_write(crate::runtime::write_gate::WriteKind::Maintenance)?;
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

fn current_unix_ms_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
