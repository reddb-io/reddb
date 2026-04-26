use std::collections::BTreeMap;

use crate::application::entity::{
    apply_patch_operations_to_json, apply_patch_operations_to_storage_map,
    apply_patch_operations_to_vector_fields, json_to_metadata_value, json_to_storage_value,
    metadata_from_json, metadata_to_json, CreateDocumentInput, CreateEdgeInput, CreateEntityOutput,
    CreateKvInput, CreateNodeInput, CreateRowInput, CreateRowsBatchInput,
    CreateTimeSeriesPointInput, CreateVectorInput, DeleteEntityInput, DeleteEntityOutput,
    PatchEntityInput, PatchEntityOperation, PatchEntityOperationType,
};
use crate::application::schema::{
    CreateTableInput, CreateTimeSeriesInput, DropTableInput, DropTimeSeriesInput,
};
use crate::application::tree::{
    CreateTreeInput, DeleteTreeNodeInput, DropTreeInput, InsertTreeNodeInput, MoveTreeNodeInput,
    RebalanceTreeInput, ValidateTreeInput,
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
    RuntimeGraphPropertiesResult, RuntimeGraphTopologicalSortResult, RuntimeGraphTraversalResult,
    RuntimeGraphTraversalStrategy, RuntimeIvfSearchResult, RuntimeQueryExplain, RuntimeQueryResult,
    RuntimeQueryWeights, RuntimeStats, ScanCursor, ScanPage,
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
    fn resolve_semantic_api_key(&self, provider: &crate::ai::AiProvider) -> RedDBResult<String>;
}

pub trait RuntimeEntityPort {
    fn create_row(&self, input: CreateRowInput) -> RedDBResult<CreateEntityOutput>;
    fn create_rows_batch(
        &self,
        input: CreateRowsBatchInput,
    ) -> RedDBResult<Vec<CreateEntityOutput>>;
    /// Pre-validated bulk insert — caller has already checked column
    /// types and uniqueness. Server skips
    /// `normalize_row_fields_for_contract`, `enforce_row_uniqueness`,
    /// and `enforce_row_batch_uniqueness`. Returns the row count.
    /// Used by `MSG_BULK_INSERT_PREVALIDATED`.
    fn create_rows_batch_prevalidated(&self, input: CreateRowsBatchInput) -> RedDBResult<usize>;
    /// Columnar pre-validated bulk insert — the wire handler
    /// decoded straight into `Vec<Vec<Value>>` + a shared column-
    /// name vector, no per-cell `(String, Value)` tuples allocated.
    /// Avoids ~N×ncols String clones vs the tuple path. The schema
    /// is shared across every row as a single `Arc<Vec<String>>`.
    fn create_rows_batch_prevalidated_columnar(
        &self,
        collection: String,
        column_names: std::sync::Arc<Vec<String>>,
        rows: Vec<Vec<crate::storage::schema::Value>>,
    ) -> RedDBResult<usize>;
    fn create_node(&self, input: CreateNodeInput) -> RedDBResult<CreateEntityOutput>;
    fn create_edge(&self, input: CreateEdgeInput) -> RedDBResult<CreateEntityOutput>;
    fn create_vector(&self, input: CreateVectorInput) -> RedDBResult<CreateEntityOutput>;
    fn create_document(&self, input: CreateDocumentInput) -> RedDBResult<CreateEntityOutput>;
    fn create_kv(&self, input: CreateKvInput) -> RedDBResult<CreateEntityOutput>;
    fn create_timeseries_point(
        &self,
        input: CreateTimeSeriesPointInput,
    ) -> RedDBResult<CreateEntityOutput>;
    fn get_kv(
        &self,
        collection: &str,
        key: &str,
    ) -> RedDBResult<Option<(crate::storage::schema::Value, crate::storage::EntityId)>>;
    fn delete_kv(&self, collection: &str, key: &str) -> RedDBResult<bool>;
    fn patch_entity(&self, input: PatchEntityInput) -> RedDBResult<CreateEntityOutput>;
    fn delete_entity(&self, input: DeleteEntityInput) -> RedDBResult<DeleteEntityOutput>;
}

pub trait RuntimeSchemaPort {
    fn create_table(&self, input: CreateTableInput) -> RedDBResult<RuntimeQueryResult>;
    fn drop_table(&self, input: DropTableInput) -> RedDBResult<RuntimeQueryResult>;
    fn create_timeseries(&self, input: CreateTimeSeriesInput) -> RedDBResult<RuntimeQueryResult>;
    fn drop_timeseries(&self, input: DropTimeSeriesInput) -> RedDBResult<RuntimeQueryResult>;
}

pub trait RuntimeTreePort {
    fn create_tree(&self, input: CreateTreeInput) -> RedDBResult<RuntimeQueryResult>;
    fn drop_tree(&self, input: DropTreeInput) -> RedDBResult<RuntimeQueryResult>;
    fn insert_tree_node(&self, input: InsertTreeNodeInput) -> RedDBResult<RuntimeQueryResult>;
    fn move_tree_node(&self, input: MoveTreeNodeInput) -> RedDBResult<RuntimeQueryResult>;
    fn delete_tree_node(&self, input: DeleteTreeNodeInput) -> RedDBResult<RuntimeQueryResult>;
    fn validate_tree(&self, input: ValidateTreeInput) -> RedDBResult<RuntimeQueryResult>;
    fn rebalance_tree(&self, input: RebalanceTreeInput) -> RedDBResult<RuntimeQueryResult>;
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
    fn graph_properties(
        &self,
        projection: Option<RuntimeGraphProjection>,
    ) -> RedDBResult<RuntimeGraphPropertiesResult>;
}

pub trait RuntimeVcsPort {
    fn vcs_commit(
        &self,
        input: crate::application::vcs::CreateCommitInput,
    ) -> RedDBResult<crate::application::vcs::Commit>;

    fn vcs_branch_create(
        &self,
        input: crate::application::vcs::CreateBranchInput,
    ) -> RedDBResult<crate::application::vcs::Ref>;

    fn vcs_branch_delete(&self, name: &str) -> RedDBResult<()>;

    fn vcs_tag_create(
        &self,
        input: crate::application::vcs::CreateTagInput,
    ) -> RedDBResult<crate::application::vcs::Ref>;

    fn vcs_list_refs(
        &self,
        prefix: Option<&str>,
    ) -> RedDBResult<Vec<crate::application::vcs::Ref>>;

    fn vcs_checkout(
        &self,
        input: crate::application::vcs::CheckoutInput,
    ) -> RedDBResult<crate::application::vcs::Ref>;

    fn vcs_merge(
        &self,
        input: crate::application::vcs::MergeInput,
    ) -> RedDBResult<crate::application::vcs::MergeOutcome>;

    fn vcs_cherry_pick(
        &self,
        connection_id: u64,
        commit: &str,
        author: crate::application::vcs::Author,
    ) -> RedDBResult<crate::application::vcs::MergeOutcome>;

    fn vcs_revert(
        &self,
        connection_id: u64,
        commit: &str,
        author: crate::application::vcs::Author,
    ) -> RedDBResult<crate::application::vcs::Commit>;

    fn vcs_reset(&self, input: crate::application::vcs::ResetInput) -> RedDBResult<()>;

    fn vcs_log(
        &self,
        input: crate::application::vcs::LogInput,
    ) -> RedDBResult<Vec<crate::application::vcs::Commit>>;

    fn vcs_diff(
        &self,
        input: crate::application::vcs::DiffInput,
    ) -> RedDBResult<crate::application::vcs::Diff>;

    fn vcs_status(
        &self,
        input: crate::application::vcs::StatusInput,
    ) -> RedDBResult<crate::application::vcs::Status>;

    fn vcs_lca(
        &self,
        a: &str,
        b: &str,
    ) -> RedDBResult<Option<crate::application::vcs::CommitHash>>;

    fn vcs_conflicts_list(
        &self,
        merge_state_id: &str,
    ) -> RedDBResult<Vec<crate::application::vcs::Conflict>>;

    fn vcs_conflict_resolve(
        &self,
        conflict_id: &str,
        resolved: crate::json::Value,
    ) -> RedDBResult<()>;

    fn vcs_resolve_as_of(
        &self,
        spec: crate::application::vcs::AsOfSpec,
    ) -> RedDBResult<crate::storage::transaction::snapshot::Xid>;

    fn vcs_resolve_commitish(
        &self,
        spec: &str,
    ) -> RedDBResult<crate::application::vcs::CommitHash>;

    fn vcs_set_versioned(&self, collection: &str, enabled: bool) -> RedDBResult<()>;
    fn vcs_list_versioned(&self) -> RedDBResult<Vec<String>>;
    fn vcs_is_versioned(&self, collection: &str) -> RedDBResult<bool>;
}

/// Context-aware extension trait that mirrors `RuntimeEntityPort`
/// with `&OperationContext` threaded through every method.
///
/// This is the migration runway for the `OperationContext` deepening
/// (PLAN cluster 6). Each default implementation forwards to the
/// existing context-less method, so today the trait is a pure
/// pass-through — but new callers can already adopt the
/// context-passing surface, and a future PR will replace the
/// defaults with real impls that read `ctx.xid` / `ctx.write_consent`.
///
/// Hidden behind the `ctx-ports` feature flag during the migration
/// window so the impl bloat doesn't burden default builds. Once
/// every port is migrated, the flag goes away and these traits
/// become the only surface.
pub trait RuntimeEntityPortCtx: RuntimeEntityPort {
    fn create_row_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: CreateRowInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let _ = ctx;
        self.create_row(input)
    }
    fn create_node_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: CreateNodeInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let _ = ctx;
        self.create_node(input)
    }
    fn create_edge_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: CreateEdgeInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let _ = ctx;
        self.create_edge(input)
    }
    fn create_vector_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: CreateVectorInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let _ = ctx;
        self.create_vector(input)
    }
    fn create_document_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: CreateDocumentInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let _ = ctx;
        self.create_document(input)
    }
    fn create_kv_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: CreateKvInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let _ = ctx;
        self.create_kv(input)
    }
    fn create_timeseries_point_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: CreateTimeSeriesPointInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let _ = ctx;
        self.create_timeseries_point(input)
    }
    fn get_kv_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        collection: &str,
        key: &str,
    ) -> RedDBResult<Option<(crate::storage::schema::Value, crate::storage::EntityId)>> {
        let _ = ctx;
        self.get_kv(collection, key)
    }
    fn delete_kv_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        collection: &str,
        key: &str,
    ) -> RedDBResult<bool> {
        let _ = ctx;
        self.delete_kv(collection, key)
    }
    fn patch_entity_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: PatchEntityInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let _ = ctx;
        self.patch_entity(input)
    }
    fn delete_entity_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: DeleteEntityInput,
    ) -> RedDBResult<DeleteEntityOutput> {
        let _ = ctx;
        self.delete_entity(input)
    }
}

/// Blanket impl: every concrete `RuntimeEntityPort` automatically
/// gains the context-aware surface via the default forwards above.
impl<T: RuntimeEntityPort + ?Sized> RuntimeEntityPortCtx for T {}

// ─── ctx extension traits for the remaining mutating ports ───
//
// Same pattern as RuntimeEntityPortCtx: methods take
// `&OperationContext` first, default-forward to the existing
// context-less call. Blanket impls give every concrete port
// the new surface for free. Future PRs replace the forwards
// with real `ctx.write_consent` / `ctx.xid` handling.
//
// Read-only ports (RuntimeCatalogPort, RuntimeGraphPort) and the
// read-only methods of mutating ports are intentionally absent —
// `OperationContext` adds no locality there until the snapshot-
// xid migration also lands.

pub trait RuntimeQueryPortCtx: RuntimeQueryPort {
    fn execute_query_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        query: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.execute_query(query)
    }
    fn explain_query_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        query: &str,
    ) -> RedDBResult<RuntimeQueryExplain> {
        let _ = ctx;
        self.explain_query(query)
    }
    fn scan_collection_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        let _ = ctx;
        self.scan_collection(collection, cursor, limit)
    }
}
impl<T: RuntimeQueryPort + ?Sized> RuntimeQueryPortCtx for T {}

pub trait RuntimeSchemaPortCtx: RuntimeSchemaPort {
    fn create_table_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: CreateTableInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.create_table(input)
    }
    fn drop_table_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: DropTableInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.drop_table(input)
    }
    fn create_timeseries_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: CreateTimeSeriesInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.create_timeseries(input)
    }
    fn drop_timeseries_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: DropTimeSeriesInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.drop_timeseries(input)
    }
}
impl<T: RuntimeSchemaPort + ?Sized> RuntimeSchemaPortCtx for T {}

pub trait RuntimeTreePortCtx: RuntimeTreePort {
    fn create_tree_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: CreateTreeInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.create_tree(input)
    }
    fn drop_tree_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: DropTreeInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.drop_tree(input)
    }
    fn insert_tree_node_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: InsertTreeNodeInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.insert_tree_node(input)
    }
    fn move_tree_node_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: MoveTreeNodeInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.move_tree_node(input)
    }
    fn delete_tree_node_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: DeleteTreeNodeInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.delete_tree_node(input)
    }
    fn rebalance_tree_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: RebalanceTreeInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        let _ = ctx;
        self.rebalance_tree(input)
    }
}
impl<T: RuntimeTreePort + ?Sized> RuntimeTreePortCtx for T {}

pub trait RuntimeNativePortCtx: RuntimeNativePort {
    fn create_snapshot_ctx(
        &self,
        ctx: &crate::application::OperationContext,
    ) -> RedDBResult<SnapshotDescriptor> {
        let _ = ctx;
        self.create_snapshot()
    }
    fn create_export_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        name: String,
    ) -> RedDBResult<ExportDescriptor> {
        let _ = ctx;
        self.create_export(name)
    }
    fn checkpoint_ctx(
        &self,
        ctx: &crate::application::OperationContext,
    ) -> RedDBResult<()> {
        let _ = ctx;
        self.checkpoint()
    }
    fn apply_retention_policy_ctx(
        &self,
        ctx: &crate::application::OperationContext,
    ) -> RedDBResult<()> {
        let _ = ctx;
        self.apply_retention_policy()
    }
    fn run_maintenance_ctx(
        &self,
        ctx: &crate::application::OperationContext,
    ) -> RedDBResult<()> {
        let _ = ctx;
        self.run_maintenance()
    }
    fn repair_native_header_from_metadata_ctx(
        &self,
        ctx: &crate::application::OperationContext,
    ) -> RedDBResult<String> {
        let _ = ctx;
        self.repair_native_header_from_metadata()
    }
    fn rebuild_physical_metadata_from_native_state_ctx(
        &self,
        ctx: &crate::application::OperationContext,
    ) -> RedDBResult<bool> {
        let _ = ctx;
        self.rebuild_physical_metadata_from_native_state()
    }
}
impl<T: RuntimeNativePort + ?Sized> RuntimeNativePortCtx for T {}

pub trait RuntimeVcsPortCtx: RuntimeVcsPort {
    fn vcs_branch_delete_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        name: &str,
    ) -> RedDBResult<()> {
        let _ = ctx;
        self.vcs_branch_delete(name)
    }
    fn vcs_reset_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        input: crate::application::vcs::ResetInput,
    ) -> RedDBResult<()> {
        let _ = ctx;
        self.vcs_reset(input)
    }
    fn vcs_set_versioned_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        collection: &str,
        enabled: bool,
    ) -> RedDBResult<()> {
        let _ = ctx;
        self.vcs_set_versioned(collection, enabled)
    }
    fn vcs_conflict_resolve_ctx(
        &self,
        ctx: &crate::application::OperationContext,
        conflict_id: &str,
        resolved: crate::json::Value,
    ) -> RedDBResult<()> {
        let _ = ctx;
        self.vcs_conflict_resolve(conflict_id, resolved)
    }
}
impl<T: RuntimeVcsPort + ?Sized> RuntimeVcsPortCtx for T {}

#[path = "ports_impls.rs"]
mod ports_impls;
pub(crate) use ports_impls::build_row_update_contract_plan;
pub(crate) use ports_impls::normalize_row_update_assignment_with_plan;
pub(crate) use ports_impls::normalize_row_update_value_for_rule;
