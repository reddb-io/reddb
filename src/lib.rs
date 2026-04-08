pub mod application;
pub mod config;
pub mod crypto;
pub mod api;
pub mod catalog;
pub mod engine;
pub mod health;
pub mod grpc;
pub mod index;
pub mod json;
pub mod modules;
pub mod physical;
pub(crate) mod presentation;
pub mod runtime;
pub mod server;
pub mod serde_json;
pub mod storage;
pub mod utils;

pub mod prelude {
    pub use crate::application::{
        AdminUseCases, CatalogUseCases, EntityUseCases, GraphUseCases, NativeUseCases,
        QueryUseCases, RuntimeAdminPort, RuntimeCatalogPort, RuntimeEntityPort,
        RuntimeGraphPort, RuntimeNativePort, RuntimeQueryPort,
    };
    pub use crate::api::{
        CatalogService, CatalogSnapshot, Capability, CapabilitySet, CollectionStats, DataOps,
        QueryPlanner, RedDBError, RedDBOptions, RedDBResult, DEFAULT_EXPORT_RETENTION,
        DEFAULT_SNAPSHOT_RETENTION, REDDB_FORMAT_VERSION, REDDB_PROTOCOL_VERSION,
        SchemaManifest, StorageMode,
    };
    pub use crate::catalog::{
        snapshot_store, CatalogModelSnapshot, CollectionDescriptor, CollectionModel, SchemaMode,
    };
    pub use crate::engine::{EngineInfo, EngineStats, RedDBEngine};
    pub use crate::grpc::{GrpcServerOptions, RedDBGrpcServer};
    pub use crate::health::{HealthIssue, HealthProvider, HealthReport, HealthState};
    pub use crate::index::{
        IndexCatalog, IndexCatalogSnapshot, IndexConfig, IndexKind, IndexMetric, IndexRuntime,
        IndexStats,
    };
    pub use crate::physical::{
        BlockReference, CompactionPolicy, GridLayout, ManifestEvent, ManifestEventKind,
        ExportDescriptor, ManifestPointers, PhysicalAnalyticsJob, PhysicalGraphProjection,
        PhysicalIndexState, PhysicalLayout, PhysicalMetadataFile, SnapshotDescriptor,
        SuperblockHeader, WalPolicy, DEFAULT_MANIFEST_EVENT_HISTORY,
        PHYSICAL_METADATA_PROTOCOL_VERSION,
    };
    pub use crate::runtime::{
        ConnectionPoolConfig, RedDBRuntime, RuntimeConnection, RuntimeIvfMatch,
        RuntimeIvfSearchResult, RuntimeFilter, RuntimeFilterValue,
        RuntimeGraphCentralityAlgorithm, RuntimeGraphCentralityResult,
        RuntimeGraphCentralityScore, RuntimeGraphClusteringResult,
        RuntimeGraphCommunity, RuntimeGraphCommunityAlgorithm, RuntimeGraphCommunityResult,
        RuntimeGraphComponentsMode, RuntimeGraphComponentsResult, RuntimeGraphComponent,
        RuntimeGraphDegreeScore, RuntimeGraphDirection, RuntimeGraphEdge,
        RuntimeGraphHitsResult, RuntimeGraphNeighborhoodResult, RuntimeGraphNode, RuntimeGraphPath,
        RuntimeGraphPathAlgorithm, RuntimeGraphPathResult, RuntimeGraphPattern,
        RuntimeGraphProjection,
        RuntimeGraphTopologicalSortResult, RuntimeGraphTraversalResult,
        RuntimeGraphTraversalStrategy, RuntimeGraphVisit, RuntimeGraphCyclesResult,
        RuntimeQueryResult, RuntimeQueryWeights, RuntimeStats, ScanCursor, ScanPage,
    };
    pub use crate::server::{RedDBServer, ServerOptions};
}

pub use crate::application::{
    AdminUseCases, CatalogUseCases, EntityUseCases, GraphUseCases, NativeUseCases,
    QueryUseCases, RuntimeAdminPort, RuntimeCatalogPort, RuntimeEntityPort,
    RuntimeGraphPort, RuntimeNativePort, RuntimeQueryPort,
};
pub use crate::api::{
    CatalogService, CatalogSnapshot, Capability, CapabilitySet, CollectionStats, DataOps,
    QueryPlanner, RedDBError, RedDBOptions, RedDBResult, DEFAULT_EXPORT_RETENTION,
    DEFAULT_SNAPSHOT_RETENTION, REDDB_FORMAT_VERSION, REDDB_PROTOCOL_VERSION,
    SchemaManifest, StorageMode,
};
pub use crate::catalog::{
    snapshot_store, CatalogModelSnapshot, CollectionDescriptor, CollectionModel, SchemaMode,
};
pub use crate::engine::{EngineInfo, EngineStats, RedDBEngine};
pub use crate::grpc::{GrpcServerOptions, RedDBGrpcServer};
pub use crate::health::{HealthIssue, HealthProvider, HealthReport, HealthState};
pub use crate::index::{
    IndexCatalog, IndexCatalogSnapshot, IndexConfig, IndexKind, IndexMetric, IndexRuntime,
    IndexStats,
};
pub use crate::physical::{
    BlockReference, CompactionPolicy, GridLayout, ManifestEvent, ManifestEventKind,
    ExportDescriptor, ManifestPointers, PhysicalAnalyticsJob, PhysicalGraphProjection,
    PhysicalIndexState, PhysicalLayout, PhysicalMetadataFile, SnapshotDescriptor,
    SuperblockHeader, WalPolicy, DEFAULT_MANIFEST_EVENT_HISTORY,
    PHYSICAL_METADATA_PROTOCOL_VERSION,
};
pub use crate::runtime::{
    ConnectionPoolConfig, RedDBRuntime, RuntimeConnection, RuntimeIvfMatch,
    RuntimeIvfSearchResult, RuntimeFilter, RuntimeFilterValue,
    RuntimeGraphCentralityAlgorithm, RuntimeGraphCentralityResult,
    RuntimeGraphCentralityScore, RuntimeGraphClusteringResult,
    RuntimeGraphCommunity, RuntimeGraphCommunityAlgorithm, RuntimeGraphCommunityResult,
    RuntimeGraphComponentsMode, RuntimeGraphComponentsResult, RuntimeGraphComponent,
    RuntimeGraphDegreeScore, RuntimeGraphDirection, RuntimeGraphEdge,
    RuntimeGraphHitsResult, RuntimeGraphNeighborhoodResult, RuntimeGraphNode, RuntimeGraphPath,
    RuntimeGraphPathAlgorithm, RuntimeGraphPathResult, RuntimeGraphPattern,
    RuntimeGraphProjection,
    RuntimeGraphTopologicalSortResult, RuntimeGraphTraversalResult,
    RuntimeGraphTraversalStrategy, RuntimeGraphVisit, RuntimeGraphCyclesResult,
    RuntimeQueryResult, RuntimeQueryWeights, RuntimeStats, ScanCursor, ScanPage,
};
pub use crate::server::{RedDBServer, ServerOptions};

pub use crate::storage::*;
