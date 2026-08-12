pub mod admin;
pub(crate) mod admin_payload;
pub(crate) mod collection_contract_enforcer;
pub mod entity;
pub(crate) mod entity_payload;
pub mod graph;
pub(crate) mod graph_payload;
pub(crate) mod json_input;
pub mod merge_json;
pub mod migration_collections;
pub mod migration_graph;
pub mod migration_inference;
pub mod native;
pub mod operation_context;
pub mod ports;
pub mod query;
pub(crate) mod query_payload;
pub mod schema;
pub(crate) mod serverless_payload;
pub mod topology_collections;
pub mod tree;
pub(crate) mod ttl_payload;
pub mod vcs;
pub mod vcs_collections;

pub use admin::{AdminUseCases, ServerlessAnalyticsWarmupTarget, ServerlessWarmupPlan};
pub use entity::{
    CreateDocumentInput, CreateEdgeInput, CreateEntityOutput, CreateKvInput,
    CreateNodeEmbeddingInput, CreateNodeGraphLinkInput, CreateNodeInput, CreateNodeTableLinkInput,
    CreateRowInput, CreateRowsBatchInput, CreateTimeSeriesPointInput, CreateVectorInput,
    DeleteEntityInput, DeleteEntityOutput, EntityUseCases, PatchEntityInput, PatchEntityOperation,
    PatchEntityOperationType,
};
pub use graph::{
    GraphCentralityInput, GraphClusteringInput, GraphCommunitiesInput, GraphComponentsInput,
    GraphCyclesInput, GraphHitsInput, GraphNeighborhoodInput, GraphPersonalizedPageRankInput,
    GraphPropertiesInput, GraphShortestPathInput, GraphTopologicalSortInput, GraphTraversalInput,
    GraphUseCases,
};
pub use native::{InspectNativeArtifactInput, NativeUseCases, RuntimeReadiness};
pub use operation_context::{
    OperationContext, OperationContextFactory, OperationContextInput, WriteConsent,
    WriteConsentSeal, Xid,
};
pub use ports::{
    RuntimeAdminPort, RuntimeEntityPort, RuntimeEntityPortCtx, RuntimeGraphPort, RuntimeNativePort,
    RuntimeNativePortCtx, RuntimeSchemaPort, RuntimeSchemaPortCtx, RuntimeTreePort,
    RuntimeTreePortCtx, RuntimeVcsPort, RuntimeVcsPortCtx,
};
pub use query::{
    ExecuteQueryInput, ExplainQueryInput, ScanCollectionInput, SearchContextInput,
    SearchHybridInput, SearchIndexInput, SearchIvfInput, SearchMultimodalInput, SearchSimilarInput,
    SearchTextInput,
};
pub use schema::{
    CreateTableColumnInput, CreateTableInput, CreateTablePartitionKind, CreateTablePartitionSpec,
    CreateTimeSeriesInput, DropTableInput, DropTimeSeriesInput, SchemaUseCases,
};
pub use tree::{
    CreateTreeInput, DeleteTreeNodeInput, DropTreeInput, InsertTreeNodeInput, MoveTreeNodeInput,
    RebalanceTreeInput, TreeNodeInput, TreePositionInput, TreeUseCases, ValidateTreeInput,
};
pub use vcs::{
    AsOfSpec, Author, CheckoutInput, CheckoutTarget, Commit, CommitHash, Conflict,
    CreateBranchInput, CreateCommitInput, CreateTagInput, Diff, DiffChange, DiffEntry, DiffInput,
    LogInput, LogRange, MergeInput, MergeOpts, MergeOutcome, MergeStrategy, Ref, RefKind, RefName,
    ResetInput, ResetMode, Status, StatusInput, VcsUseCases,
};

#[cfg(test)]
mod architecture_tests {
    const MODULE: &str = include_str!("mod.rs");
    const PORTS: &str = include_str!("ports.rs");
    const PORT_IMPLS: &str = include_str!("ports_impls.rs");
    const QUERY: &str = include_str!("query.rs");

    #[test]
    fn query_and_catalog_pass_through_ports_stay_deleted() {
        for retired_name in [
            concat!("Runtime", "Query", "Port"),
            concat!("Runtime", "Catalog", "Port"),
            concat!("Query", "UseCases"),
            concat!("Catalog", "UseCases"),
            concat!("ports_impls_", "query.rs"),
            concat!("ports_impls_", "catalog.rs"),
        ] {
            assert!(
                !MODULE.contains(retired_name)
                    && !PORTS.contains(retired_name)
                    && !PORT_IMPLS.contains(retired_name)
                    && !QUERY.contains(retired_name),
                "retired pass-through surface reappeared: {retired_name}"
            );
        }
    }
}
