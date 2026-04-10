use std::collections::BTreeMap;

use crate::application::entity::{
    apply_patch_operations_to_json, apply_patch_operations_to_storage_map,
    apply_patch_operations_to_vector_fields, json_to_metadata_value, json_to_storage_value,
    metadata_from_json, metadata_to_json, CreateDocumentInput, CreateEdgeInput, CreateEntityOutput,
    CreateKvInput, CreateNodeInput, CreateRowInput, CreateVectorInput, DeleteEntityInput,
    DeleteEntityOutput, PatchEntityInput, PatchEntityOperation, PatchEntityOperationType,
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
    RuntimeGraphTopologicalSortResult, RuntimeGraphTraversalResult, RuntimeGraphTraversalStrategy,
    RuntimeIvfSearchResult, RuntimeQueryExplain, RuntimeQueryResult, RuntimeQueryWeights,
    RuntimeStats, ScanCursor, ScanPage,
};
use crate::storage::engine::PhysicalFileHeader;
use crate::storage::unified::devx::refs::{NodeRef, TableRef, VectorRef};
use crate::storage::unified::devx::{
    NativeVectorArtifactBatchInspection, NativeVectorArtifactInspection, PhysicalAuthorityStatus,
    SimilarResult,
};
use crate::storage::unified::dsl::QueryResult as DslQueryResult;
use crate::storage::unified::store::{
    NativeCatalogSummary, NativeManifestSummary, NativeMetadataStateSummary, NativePhysicalState,
    NativeRecoverySummary, NativeRegistrySummary, NativeVectorArtifactPageSummary,
};
use crate::RedDBResult;
use crate::{PhysicalAnalyticsJob, PhysicalGraphProjection, PhysicalIndexState};

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
    fn search_multimodal(
        &self,
        query: String,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult>;
    fn search_index(
        &self,
        index: String,
        value: String,
        exact: bool,
        collections: Option<Vec<String>>,
        entity_types: Option<Vec<String>>,
        capabilities: Option<Vec<String>>,
        limit: Option<usize>,
    ) -> RedDBResult<DslQueryResult>;
    fn search_context(
        &self,
        input: crate::application::SearchContextInput,
    ) -> RedDBResult<crate::runtime::ContextSearchResult>;
}

pub trait RuntimeEntityPort {
    fn create_row(&self, input: CreateRowInput) -> RedDBResult<CreateEntityOutput>;
    fn create_node(&self, input: CreateNodeInput) -> RedDBResult<CreateEntityOutput>;
    fn create_edge(&self, input: CreateEdgeInput) -> RedDBResult<CreateEntityOutput>;
    fn create_vector(&self, input: CreateVectorInput) -> RedDBResult<CreateEntityOutput>;
    fn create_document(&self, input: CreateDocumentInput) -> RedDBResult<CreateEntityOutput>;
    fn create_kv(&self, input: CreateKvInput) -> RedDBResult<CreateEntityOutput>;
    fn get_kv(
        &self,
        collection: &str,
        key: &str,
    ) -> RedDBResult<Option<(crate::storage::schema::Value, crate::storage::EntityId)>>;
    fn delete_kv(&self, collection: &str, key: &str) -> RedDBResult<bool>;
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
    fn inspect_native_vector_artifacts(&self) -> RedDBResult<NativeVectorArtifactBatchInspection>;
    fn warmup_native_vector_artifacts(&self) -> RedDBResult<NativeVectorArtifactBatchInspection>;
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

#[path = "ports_impls.rs"]
mod ports_impls;
