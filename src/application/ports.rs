use std::collections::BTreeMap;

use crate::application::entity::{
    apply_patch_operations_to_json, apply_patch_operations_to_storage_map,
    apply_patch_operations_to_vector_fields, json_to_metadata_value, json_to_storage_value,
    metadata_from_json, metadata_to_json, CreateEdgeInput, CreateEntityOutput, CreateNodeInput,
    CreateRowInput, CreateVectorInput, DeleteEntityInput, DeleteEntityOutput, PatchEntityInput,
    PatchEntityOperationType,
};
use crate::catalog::{
    CatalogAnalyticsJobStatus, CatalogAttentionSummary, CatalogConsistencyReport,
    CatalogGraphProjectionStatus, CatalogIndexStatus, CatalogModelSnapshot, CollectionDescriptor,
};
use crate::health::HealthProvider;
use crate::physical::{ExportDescriptor, ManifestEvent, PhysicalMetadataFile, SnapshotDescriptor};
use crate::runtime::{
    RedDBRuntime, RuntimeFilter, RuntimeGraphCentralityAlgorithm, RuntimeGraphCentralityResult,
    RuntimeGraphClusteringResult, RuntimeGraphCommunityResult, RuntimeGraphComponentsMode,
    RuntimeGraphComponentsResult, RuntimeGraphCyclesResult, RuntimeGraphDirection,
    RuntimeGraphHitsResult, RuntimeGraphNeighborhoodResult, RuntimeGraphPathAlgorithm,
    RuntimeGraphPathResult, RuntimeGraphPattern, RuntimeGraphProjection,
    RuntimeGraphTopologicalSortResult, RuntimeGraphTraversalResult,
    RuntimeGraphTraversalStrategy, RuntimeIvfSearchResult, RuntimeQueryExplain,
    RuntimeQueryResult, RuntimeQueryWeights, RuntimeStats, ScanCursor, ScanPage,
};
use crate::storage::engine::PhysicalFileHeader;
use crate::storage::unified::devx::refs::{NodeRef, TableRef, VectorRef};
use crate::storage::unified::devx::{
    NativeVectorArtifactBatchInspection, NativeVectorArtifactInspection, PhysicalAuthorityStatus,
    SimilarResult,
};
use crate::storage::unified::dsl::QueryResult as DslQueryResult;
use crate::storage::unified::store::{
    NativeCatalogSummary, NativeManifestSummary, NativeMetadataStateSummary,
    NativePhysicalState, NativeRecoverySummary, NativeRegistrySummary,
    NativeVectorArtifactPageSummary,
};
use crate::{PhysicalAnalyticsJob, PhysicalGraphProjection, PhysicalIndexState};
use crate::RedDBResult;

pub trait RuntimeQueryPort {
    fn execute_query(&self, query: &str) -> RedDBResult<RuntimeQueryResult>;
    fn explain_query(&self, query: &str) -> RedDBResult<RuntimeQueryExplain>;
    fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage>;
    fn search_similar(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
        min_score: f32,
    ) -> RedDBResult<Vec<SimilarResult>>;
    fn search_ivf(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
        n_lists: usize,
        n_probes: Option<usize>,
    ) -> RedDBResult<RuntimeIvfSearchResult>;
    fn search_hybrid(
        &self,
        vector: Option<Vec<f32>>,
        query: Option<String>,
        k: Option<usize>,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        graph_pattern: Option<RuntimeGraphPattern>,
        filters: Vec<RuntimeFilter>,
        weights: Option<RuntimeQueryWeights>,
        min_score: Option<f32>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult>;
    fn search_text(
        &self,
        query: String,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        fields: Option<Vec<String>>,
        limit: Option<usize>,
        fuzzy: bool,
    ) -> RedDBResult<DslQueryResult>;
}

pub trait RuntimeEntityPort {
    fn create_row(&self, input: CreateRowInput) -> RedDBResult<CreateEntityOutput>;
    fn create_node(&self, input: CreateNodeInput) -> RedDBResult<CreateEntityOutput>;
    fn create_edge(&self, input: CreateEdgeInput) -> RedDBResult<CreateEntityOutput>;
    fn create_vector(&self, input: CreateVectorInput) -> RedDBResult<CreateEntityOutput>;
    fn patch_entity(&self, input: PatchEntityInput) -> RedDBResult<CreateEntityOutput>;
    fn delete_entity(&self, input: DeleteEntityInput) -> RedDBResult<DeleteEntityOutput>;
}

pub trait RuntimeAdminPort {
    fn set_index_enabled(&self, name: &str, enabled: bool) -> RedDBResult<PhysicalIndexState>;
    fn mark_index_building(&self, name: &str) -> RedDBResult<PhysicalIndexState>;
    fn fail_index(&self, name: &str) -> RedDBResult<PhysicalIndexState>;
    fn mark_index_stale(&self, name: &str) -> RedDBResult<PhysicalIndexState>;
    fn mark_index_ready(&self, name: &str) -> RedDBResult<PhysicalIndexState>;
    fn warmup_index_with_lifecycle(&self, name: &str) -> RedDBResult<PhysicalIndexState>;
    fn rebuild_indexes_with_lifecycle(
        &self,
        collection: Option<&str>,
    ) -> RedDBResult<Vec<PhysicalIndexState>>;
    fn save_graph_projection(
        &self,
        name: impl Into<String>,
        projection: RuntimeGraphProjection,
        source: Option<String>,
    ) -> RedDBResult<PhysicalGraphProjection>;
    fn mark_graph_projection_materializing(
        &self,
        name: &str,
    ) -> RedDBResult<PhysicalGraphProjection>;
    fn materialize_graph_projection(&self, name: &str) -> RedDBResult<PhysicalGraphProjection>;
    fn fail_graph_projection(&self, name: &str) -> RedDBResult<PhysicalGraphProjection>;
    fn mark_graph_projection_stale(&self, name: &str) -> RedDBResult<PhysicalGraphProjection>;
    fn save_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob>;
    fn start_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob>;
    fn queue_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob>;
    fn fail_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob>;
    fn mark_analytics_job_stale(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob>;
    fn complete_analytics_job(
        &self,
        kind: impl Into<String>,
        projection_name: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> RedDBResult<PhysicalAnalyticsJob>;
}

pub trait RuntimeCatalogPort {
    fn collections(&self) -> Vec<String>;
    fn catalog(&self) -> CatalogModelSnapshot;
    fn catalog_consistency_report(&self) -> CatalogConsistencyReport;
    fn catalog_attention_summary(&self) -> CatalogAttentionSummary;
    fn collection_attention(&self) -> Vec<CollectionDescriptor>;
    fn indexes(&self) -> Vec<PhysicalIndexState>;
    fn declared_indexes(&self) -> Vec<PhysicalIndexState>;
    fn indexes_for_collection(&self, collection: &str) -> Vec<PhysicalIndexState>;
    fn declared_indexes_for_collection(&self, collection: &str) -> Vec<PhysicalIndexState>;
    fn index_statuses(&self) -> Vec<CatalogIndexStatus>;
    fn index_attention(&self) -> Vec<CatalogIndexStatus>;
    fn graph_projections(&self) -> RedDBResult<Vec<PhysicalGraphProjection>>;
    fn operational_graph_projections(&self) -> Vec<PhysicalGraphProjection>;
    fn graph_projection_statuses(&self) -> Vec<CatalogGraphProjectionStatus>;
    fn graph_projection_attention(&self) -> Vec<CatalogGraphProjectionStatus>;
    fn analytics_jobs(&self) -> RedDBResult<Vec<PhysicalAnalyticsJob>>;
    fn operational_analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob>;
    fn analytics_job_statuses(&self) -> Vec<CatalogAnalyticsJobStatus>;
    fn analytics_job_attention(&self) -> Vec<CatalogAnalyticsJobStatus>;
    fn stats(&self) -> RuntimeStats;
}

pub trait RuntimeNativePort {
    fn health_report(&self) -> crate::health::HealthReport;
    fn collection_roots(&self) -> RedDBResult<BTreeMap<String, u64>>;
    fn snapshots(&self) -> RedDBResult<Vec<SnapshotDescriptor>>;
    fn exports(&self) -> RedDBResult<Vec<ExportDescriptor>>;
    fn physical_metadata(&self) -> RedDBResult<PhysicalMetadataFile>;
    fn manifest_events_filtered(
        &self,
        collection: Option<&str>,
        kind: Option<&str>,
        since_snapshot: Option<u64>,
    ) -> RedDBResult<Vec<ManifestEvent>>;
    fn create_snapshot(&self) -> RedDBResult<SnapshotDescriptor>;
    fn create_export(&self, name: String) -> RedDBResult<ExportDescriptor>;
    fn checkpoint(&self) -> RedDBResult<()>;
    fn apply_retention_policy(&self) -> RedDBResult<()>;
    fn run_maintenance(&self) -> RedDBResult<()>;
    fn native_header(&self) -> RedDBResult<PhysicalFileHeader>;
    fn native_collection_roots(&self) -> RedDBResult<BTreeMap<String, u64>>;
    fn native_manifest_summary(&self) -> RedDBResult<NativeManifestSummary>;
    fn native_registry_summary(&self) -> RedDBResult<NativeRegistrySummary>;
    fn native_recovery_summary(&self) -> RedDBResult<NativeRecoverySummary>;
    fn native_catalog_summary(&self) -> RedDBResult<NativeCatalogSummary>;
    fn native_physical_state(&self) -> RedDBResult<NativePhysicalState>;
    fn native_vector_artifact_pages(&self) -> RedDBResult<Vec<NativeVectorArtifactPageSummary>>;
    fn inspect_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> RedDBResult<NativeVectorArtifactInspection>;
    fn warmup_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> RedDBResult<NativeVectorArtifactInspection>;
    fn inspect_native_vector_artifacts(
        &self,
    ) -> RedDBResult<NativeVectorArtifactBatchInspection>;
    fn warmup_native_vector_artifacts(
        &self,
    ) -> RedDBResult<NativeVectorArtifactBatchInspection>;
    fn native_header_repair_policy(&self) -> RedDBResult<String>;
    fn repair_native_header_from_metadata(&self) -> RedDBResult<String>;
    fn rebuild_physical_metadata_from_native_state(&self) -> RedDBResult<bool>;
    fn repair_native_physical_state_from_metadata(&self) -> RedDBResult<bool>;
    fn native_metadata_state_summary(&self) -> RedDBResult<NativeMetadataStateSummary>;
    fn physical_authority_status(&self) -> PhysicalAuthorityStatus;
    fn readiness_for_query(&self) -> bool;
    fn readiness_for_query_serverless(&self) -> bool;
    fn readiness_for_write(&self) -> bool;
    fn readiness_for_write_serverless(&self) -> bool;
    fn readiness_for_repair(&self) -> bool;
    fn readiness_for_repair_serverless(&self) -> bool;
}

pub trait RuntimeGraphPort {
    fn resolve_graph_projection(
        &self,
        name: Option<&str>,
        inline: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<Option<RuntimeGraphProjection>>;
    fn graph_neighborhood(
        &self,
        node: &str,
        direction: RuntimeGraphDirection,
        max_depth: usize,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphNeighborhoodResult>;
    fn graph_traverse(
        &self,
        source: &str,
        direction: RuntimeGraphDirection,
        max_depth: usize,
        strategy: RuntimeGraphTraversalStrategy,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphTraversalResult>;
    fn graph_shortest_path(
        &self,
        source: &str,
        target: &str,
        direction: RuntimeGraphDirection,
        algorithm: RuntimeGraphPathAlgorithm,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphPathResult>;
    fn graph_components(
        &self,
        mode: RuntimeGraphComponentsMode,
        min_size: usize,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphComponentsResult>;
    fn graph_centrality(
        &self,
        algorithm: RuntimeGraphCentralityAlgorithm,
        top_k: usize,
        normalize: bool,
        max_iterations: Option<usize>,
        epsilon: Option<f64>,
        alpha: Option<f64>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCentralityResult>;
    fn graph_communities(
        &self,
        algorithm: crate::runtime::RuntimeGraphCommunityAlgorithm,
        min_size: usize,
        max_iterations: Option<usize>,
        resolution: Option<f64>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCommunityResult>;
    fn graph_clustering(
        &self,
        top_k: usize,
        include_triangles: bool,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphClusteringResult>;
    fn graph_personalized_pagerank(
        &self,
        seeds: Vec<String>,
        top_k: usize,
        alpha: Option<f64>,
        epsilon: Option<f64>,
        max_iterations: Option<usize>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCentralityResult>;
    fn graph_hits(
        &self,
        top_k: usize,
        epsilon: Option<f64>,
        max_iterations: Option<usize>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphHitsResult>;
    fn graph_cycles(
        &self,
        max_length: usize,
        max_cycles: usize,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCyclesResult>;
    fn graph_topological_sort(
        &self,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphTopologicalSortResult>;
}

impl RuntimeQueryPort for RedDBRuntime {
    fn execute_query(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        RedDBRuntime::execute_query(self, query)
    }

    fn explain_query(&self, query: &str) -> RedDBResult<RuntimeQueryExplain> {
        RedDBRuntime::explain_query(self, query)
    }

    fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        RedDBRuntime::scan_collection(self, collection, cursor, limit)
    }

    fn search_similar(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
        min_score: f32,
    ) -> RedDBResult<Vec<SimilarResult>> {
        RedDBRuntime::search_similar(self, collection, vector, k, min_score)
    }

    fn search_ivf(
        &self,
        collection: &str,
        vector: &[f32],
        k: usize,
        n_lists: usize,
        n_probes: Option<usize>,
    ) -> RedDBResult<RuntimeIvfSearchResult> {
        RedDBRuntime::search_ivf(self, collection, vector, k, n_lists, n_probes)
    }

    fn search_hybrid(
        &self,
        vector: Option<Vec<f32>>,
        query: Option<String>,
        k: Option<usize>,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        graph_pattern: Option<RuntimeGraphPattern>,
        filters: Vec<RuntimeFilter>,
        weights: Option<RuntimeQueryWeights>,
        min_score: Option<f32>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult> {
        RedDBRuntime::search_hybrid(
            self,
            vector,
            query,
            k,
            collections,
            entity_types,
            capabilities,
            graph_pattern,
            filters,
            weights,
            min_score,
            limit,
        )
    }

    fn search_text(
        &self,
        query: String,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        fields: Option<Vec<String>>,
        limit: Option<usize>,
        fuzzy: bool,
    ) -> RedDBResult<DslQueryResult> {
        RedDBRuntime::search_text(
            self,
            query,
            collections,
            entity_types,
            capabilities,
            fields,
            limit,
            fuzzy,
        )
    }
}

impl RuntimeEntityPort for RedDBRuntime {
    fn create_row(&self, input: CreateRowInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        let columns: Vec<(&str, crate::storage::schema::Value)> = input
            .fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();
        let mut builder = db.row(&input.collection, columns);

        for (key, value) in input.metadata {
            builder = builder.metadata(key, value);
        }

        for node in input.node_links {
            builder = builder.link_to_node(node);
        }

        for vector in input.vector_links {
            builder = builder.link_to_vector(vector);
        }

        let id = builder.save()?;
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn create_node(&self, input: CreateNodeInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        let mut builder = db.node(&input.collection, &input.label);

        if let Some(node_type) = input.node_type {
            builder = builder.node_type(node_type);
        }

        for (key, value) in input.properties {
            builder = builder.property(key, value);
        }

        for (key, value) in input.metadata {
            builder = builder.metadata(key, value);
        }

        for embedding in input.embeddings {
            if let Some(model) = embedding.model {
                builder = builder.embedding_with_model(embedding.name, embedding.vector, model);
            } else {
                builder = builder.embedding(embedding.name, embedding.vector);
            }
        }

        for link in input.table_links {
            builder = builder.link_to_table(link.key, link.table);
        }

        for link in input.node_links {
            builder = builder.link_to_weighted(link.target, link.edge_label, link.weight);
        }

        let id = builder.save()?;
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn create_edge(&self, input: CreateEdgeInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        let mut builder = db
            .edge(&input.collection, &input.label)
            .from(input.from)
            .to(input.to);

        if let Some(weight) = input.weight {
            builder = builder.weight(weight);
        }

        for (key, value) in input.properties {
            builder = builder.property(key, value);
        }

        for (key, value) in input.metadata {
            builder = builder.metadata(key, value);
        }

        let id = builder.save()?;
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn create_vector(&self, input: CreateVectorInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        let mut builder = db.vector(&input.collection).dense(input.dense);

        if let Some(content) = input.content {
            builder = builder.content(content);
        }

        for (key, value) in input.metadata {
            builder = builder.metadata(key, value);
        }

        if let Some(link_row) = input.link_row {
            builder = builder.link_to_table(link_row);
        }

        if let Some(link_node) = input.link_node {
            builder = builder.link_to_node(link_node);
        }

        let id = builder.save()?;
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn patch_entity(&self, input: PatchEntityInput) -> RedDBResult<CreateEntityOutput> {
        let PatchEntityInput {
            collection,
            id,
            payload,
            operations,
        } = input;

        let db = self.db();
        let store = db.store();
        let Some(manager) = store.get_collection(&collection) else {
            return Err(crate::RedDBError::NotFound(format!(
                "collection not found: {collection}"
            )));
        };
        let Some(mut entity) = manager.get(id) else {
            return Err(crate::RedDBError::NotFound(format!(
                "entity not found: {}",
                id.raw()
            )));
        };

        let mut patch_metadata = store
            .get_metadata(&collection, id)
            .unwrap_or_else(crate::storage::unified::Metadata::new);
        let mut metadata_changed = false;

        match &mut entity.data {
            crate::storage::EntityData::Row(row) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" | "named" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            field_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for table rows. Use fields/*, metadata/*, or weight"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    let named = row.named.get_or_insert_with(Default::default);
                    apply_patch_operations_to_storage_map(named, &field_ops)?;
                }

                if let Some(fields) = payload.get("fields").and_then(crate::json::Value::as_object)
                {
                    let named = row.named.get_or_insert_with(Default::default);
                    for (key, value) in fields {
                        named.insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
            crate::storage::EntityData::Node(node) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" | "properties" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            field_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for graph nodes. Use fields/*, properties/*, or metadata/*"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    apply_patch_operations_to_storage_map(&mut node.properties, &field_ops)?;
                }

                if let Some(fields) = payload.get("fields").and_then(crate::json::Value::as_object)
                {
                    for (key, value) in fields {
                        node.properties
                            .insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
            crate::storage::EntityData::Edge(edge) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();
                let mut weight_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" | "properties" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            field_ops.push(op);
                        }
                        "weight" => {
                            if op.path.len() != 1 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'weight' does not allow nested keys".to_string(),
                                ));
                            }
                            op.path.clear();
                            weight_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for graph edges. Use fields/*, weight, metadata/*"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    apply_patch_operations_to_storage_map(&mut edge.properties, &field_ops)?;
                }

                for op in weight_ops {
                    let value = op.value.ok_or_else(|| {
                        crate::RedDBError::Query("weight operations require a value".to_string())
                    })?;

                    match op.op {
                        PatchEntityOperationType::Unset => {
                            return Err(crate::RedDBError::Query(
                                "weight cannot be unset through patch operations".to_string(),
                            ));
                        }
                        PatchEntityOperationType::Set | PatchEntityOperationType::Replace => {
                            let Some(weight) = value.as_f64() else {
                                return Err(crate::RedDBError::Query(
                                    "weight operation requires a numeric value".to_string(),
                                ));
                            };
                            edge.weight = weight as f32;
                        }
                    }
                }

                if let Some(fields) = payload.get("fields").and_then(crate::json::Value::as_object)
                {
                    for (key, value) in fields {
                        edge.properties
                            .insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
            crate::storage::EntityData::Vector(vector) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            let Some(target) = op.path.first().map(String::as_str) else {
                                return Err(crate::RedDBError::Query(
                                    "patch path requires a target under fields".to_string(),
                                ));
                            };
                            if !matches!(target, "dense" | "content" | "sparse") {
                                return Err(crate::RedDBError::Query(format!(
                                    "unsupported vector patch target '{target}'"
                                )));
                            }
                            field_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for vectors. Use fields/* or metadata/*"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    apply_patch_operations_to_vector_fields(vector, &field_ops)?;
                }

                if let Some(fields) = payload.get("fields").and_then(crate::json::Value::as_object)
                {
                    if let Some(content) = fields.get("content").and_then(crate::json::Value::as_str)
                    {
                        vector.content = Some(content.to_string());
                    }
                    if let Some(dense) = fields.get("dense") {
                        vector.dense = dense
                            .as_array()
                            .ok_or_else(|| {
                                crate::RedDBError::Query(
                                    "field 'dense' must be an array".to_string(),
                                )
                            })?
                            .iter()
                            .map(|value| {
                                value.as_f64().map(|value| value as f32).ok_or_else(|| {
                                    crate::RedDBError::Query(
                                        "field 'dense' must contain only numbers".to_string(),
                                    )
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
        }

        if let Some(metadata) = payload.get("metadata").and_then(crate::json::Value::as_object) {
            for (key, value) in metadata {
                patch_metadata.set(key.clone(), json_to_metadata_value(value)?);
            }
            metadata_changed = true;
        }

        if metadata_changed {
            store
                .set_metadata(&collection, id, patch_metadata)
                .map_err(|err| crate::RedDBError::Query(err.to_string()))?;
        }

        entity.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        manager
            .update(entity)
            .map_err(|err| crate::RedDBError::Query(err.to_string()))?;

        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn delete_entity(&self, input: DeleteEntityInput) -> RedDBResult<DeleteEntityOutput> {
        let deleted = self
            .db()
            .store()
            .delete(&input.collection, input.id)
            .map_err(|err| crate::RedDBError::Internal(err.to_string()))?;
        Ok(DeleteEntityOutput {
            deleted,
            id: input.id,
        })
    }
}

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

impl RuntimeNativePort for RedDBRuntime {
    fn health_report(&self) -> crate::health::HealthReport {
        self.health()
    }

    fn collection_roots(&self) -> RedDBResult<BTreeMap<String, u64>> {
        RedDBRuntime::collection_roots(self)
    }

    fn snapshots(&self) -> RedDBResult<Vec<SnapshotDescriptor>> {
        RedDBRuntime::snapshots(self)
    }

    fn exports(&self) -> RedDBResult<Vec<ExportDescriptor>> {
        RedDBRuntime::exports(self)
    }

    fn physical_metadata(&self) -> RedDBResult<PhysicalMetadataFile> {
        self.db()
            .physical_metadata()
            .ok_or_else(|| RedDBError::NotFound("physical metadata".to_string()))
    }

    fn manifest_events_filtered(
        &self,
        collection: Option<&str>,
        kind: Option<&str>,
        since_snapshot: Option<u64>,
    ) -> RedDBResult<Vec<ManifestEvent>> {
        RedDBRuntime::manifest_events_filtered(self, collection, kind, since_snapshot)
    }

    fn create_snapshot(&self) -> RedDBResult<SnapshotDescriptor> {
        RedDBRuntime::create_snapshot(self)
    }

    fn create_export(&self, name: String) -> RedDBResult<ExportDescriptor> {
        RedDBRuntime::create_export(self, name)
    }

    fn checkpoint(&self) -> RedDBResult<()> {
        RedDBRuntime::checkpoint(self)
    }

    fn apply_retention_policy(&self) -> RedDBResult<()> {
        RedDBRuntime::apply_retention_policy(self)
    }

    fn run_maintenance(&self) -> RedDBResult<()> {
        RedDBRuntime::run_maintenance(self)
    }

    fn native_header(&self) -> RedDBResult<PhysicalFileHeader> {
        RedDBRuntime::native_header(self)
    }

    fn native_collection_roots(&self) -> RedDBResult<BTreeMap<String, u64>> {
        RedDBRuntime::native_collection_roots(self)
    }

    fn native_manifest_summary(&self) -> RedDBResult<NativeManifestSummary> {
        RedDBRuntime::native_manifest_summary(self)
    }

    fn native_registry_summary(&self) -> RedDBResult<NativeRegistrySummary> {
        RedDBRuntime::native_registry_summary(self)
    }

    fn native_recovery_summary(&self) -> RedDBResult<NativeRecoverySummary> {
        RedDBRuntime::native_recovery_summary(self)
    }

    fn native_catalog_summary(&self) -> RedDBResult<NativeCatalogSummary> {
        RedDBRuntime::native_catalog_summary(self)
    }

    fn native_physical_state(&self) -> RedDBResult<NativePhysicalState> {
        RedDBRuntime::native_physical_state(self)
    }

    fn native_vector_artifact_pages(&self) -> RedDBResult<Vec<NativeVectorArtifactPageSummary>> {
        RedDBRuntime::native_vector_artifact_pages(self)
    }

    fn inspect_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> RedDBResult<NativeVectorArtifactInspection> {
        RedDBRuntime::inspect_native_vector_artifact(self, collection, artifact_kind)
    }

    fn warmup_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> RedDBResult<NativeVectorArtifactInspection> {
        RedDBRuntime::warmup_native_vector_artifact(self, collection, artifact_kind)
    }

    fn inspect_native_vector_artifacts(
        &self,
    ) -> RedDBResult<NativeVectorArtifactBatchInspection> {
        RedDBRuntime::inspect_native_vector_artifacts(self)
    }

    fn warmup_native_vector_artifacts(
        &self,
    ) -> RedDBResult<NativeVectorArtifactBatchInspection> {
        RedDBRuntime::warmup_native_vector_artifacts(self)
    }

    fn native_header_repair_policy(&self) -> RedDBResult<String> {
        RedDBRuntime::native_header_repair_policy(self)
    }

    fn repair_native_header_from_metadata(&self) -> RedDBResult<String> {
        RedDBRuntime::repair_native_header_from_metadata(self)
    }

    fn rebuild_physical_metadata_from_native_state(&self) -> RedDBResult<bool> {
        RedDBRuntime::rebuild_physical_metadata_from_native_state(self)
    }

    fn repair_native_physical_state_from_metadata(&self) -> RedDBResult<bool> {
        RedDBRuntime::repair_native_physical_state_from_metadata(self)
    }

    fn native_metadata_state_summary(&self) -> RedDBResult<NativeMetadataStateSummary> {
        RedDBRuntime::native_metadata_state_summary(self)
    }

    fn physical_authority_status(&self) -> PhysicalAuthorityStatus {
        RedDBRuntime::physical_authority_status(self)
    }

    fn readiness_for_query(&self) -> bool {
        RedDBRuntime::readiness_for_query(self)
    }

    fn readiness_for_query_serverless(&self) -> bool {
        RedDBRuntime::readiness_for_query_serverless(self)
    }

    fn readiness_for_write(&self) -> bool {
        RedDBRuntime::readiness_for_write(self)
    }

    fn readiness_for_write_serverless(&self) -> bool {
        RedDBRuntime::readiness_for_write_serverless(self)
    }

    fn readiness_for_repair(&self) -> bool {
        RedDBRuntime::readiness_for_repair(self)
    }

    fn readiness_for_repair_serverless(&self) -> bool {
        RedDBRuntime::readiness_for_repair_serverless(self)
    }
}

impl RuntimeGraphPort for RedDBRuntime {
    fn resolve_graph_projection(
        &self,
        name: Option<&str>,
        inline: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<Option<RuntimeGraphProjection>> {
        RedDBRuntime::resolve_graph_projection(self, name, inline)
    }

    fn graph_neighborhood(
        &self,
        node: &str,
        direction: RuntimeGraphDirection,
        max_depth: usize,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphNeighborhoodResult> {
        RedDBRuntime::graph_neighborhood(self, node, direction, max_depth, edge_labels, projection)
    }

    fn graph_traverse(
        &self,
        source: &str,
        direction: RuntimeGraphDirection,
        max_depth: usize,
        strategy: RuntimeGraphTraversalStrategy,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphTraversalResult> {
        RedDBRuntime::graph_traverse(
            self,
            source,
            direction,
            max_depth,
            strategy,
            edge_labels,
            projection,
        )
    }

    fn graph_shortest_path(
        &self,
        source: &str,
        target: &str,
        direction: RuntimeGraphDirection,
        algorithm: RuntimeGraphPathAlgorithm,
        edge_labels: Option<Vec<String>>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphPathResult> {
        RedDBRuntime::graph_shortest_path(
            self,
            source,
            target,
            direction,
            algorithm,
            edge_labels,
            projection,
        )
    }

    fn graph_components(
        &self,
        mode: RuntimeGraphComponentsMode,
        min_size: usize,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphComponentsResult> {
        RedDBRuntime::graph_components(self, mode, min_size, projection)
    }

    fn graph_centrality(
        &self,
        algorithm: RuntimeGraphCentralityAlgorithm,
        top_k: usize,
        normalize: bool,
        max_iterations: Option<usize>,
        epsilon: Option<f64>,
        alpha: Option<f64>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCentralityResult> {
        RedDBRuntime::graph_centrality(
            self,
            algorithm,
            top_k,
            normalize,
            max_iterations,
            epsilon,
            alpha,
            projection,
        )
    }

    fn graph_communities(
        &self,
        algorithm: crate::runtime::RuntimeGraphCommunityAlgorithm,
        min_size: usize,
        max_iterations: Option<usize>,
        resolution: Option<f64>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCommunityResult> {
        RedDBRuntime::graph_communities(
            self,
            algorithm,
            min_size,
            max_iterations,
            resolution,
            projection,
        )
    }

    fn graph_clustering(
        &self,
        top_k: usize,
        include_triangles: bool,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphClusteringResult> {
        RedDBRuntime::graph_clustering(self, top_k, include_triangles, projection)
    }

    fn graph_personalized_pagerank(
        &self,
        seeds: Vec<String>,
        top_k: usize,
        alpha: Option<f64>,
        epsilon: Option<f64>,
        max_iterations: Option<usize>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCentralityResult> {
        RedDBRuntime::graph_personalized_pagerank(
            self,
            seeds,
            top_k,
            alpha,
            epsilon,
            max_iterations,
            projection,
        )
    }

    fn graph_hits(
        &self,
        top_k: usize,
        epsilon: Option<f64>,
        max_iterations: Option<usize>,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphHitsResult> {
        RedDBRuntime::graph_hits(self, top_k, epsilon, max_iterations, projection)
    }

    fn graph_cycles(
        &self,
        max_length: usize,
        max_cycles: usize,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphCyclesResult> {
        RedDBRuntime::graph_cycles(self, max_length, max_cycles, projection)
    }

    fn graph_topological_sort(
        &self,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphTopologicalSortResult> {
        RedDBRuntime::graph_topological_sort(self, projection)
    }
}
