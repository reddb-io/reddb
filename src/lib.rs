#![allow(dead_code, unused_imports, unused_variables)]
// Structural lints we accept for API design reasons:
#![allow(
    clippy::too_many_arguments,   // complex DB operations legitimately need many params
    clippy::type_complexity,      // internal types with nested generics
    clippy::result_large_err,     // tonic::Status is 176 bytes, can't box it
    clippy::should_implement_trait, // from_str() returns Option, not Result — different semantics
    clippy::new_without_default,  // some constructors have side effects
    clippy::enum_variant_names,   // JoinPhase variants all end in Start by design
    clippy::wrong_self_convention, // to_bytes on Copy types in our serialization
    clippy::len_without_is_empty  // segment structs don't need is_empty
)]

pub mod ai;
pub mod api;
pub mod application;
pub mod auth;
pub mod catalog;
pub mod cli;
pub mod client;
pub mod config;
pub mod crypto;
pub mod ec;
pub mod engine;
pub mod geo;
pub mod grpc;
pub mod health;
pub mod index;
pub mod json;
pub mod log;
pub mod mcp;
pub mod modules;
pub mod physical;
pub(crate) mod presentation;
pub mod regress;
pub mod replication;
pub mod rpc_stdio;
pub mod runtime;
pub mod serde_json;
pub mod server;
pub mod service_cli;
mod service_router;
pub mod sqlstate;
pub mod storage;
pub mod telemetry;
pub mod utils;
pub mod wire;

pub mod prelude {
    pub use crate::api::{
        Capability, CapabilitySet, CatalogService, CatalogSnapshot, CollectionStats, DataOps,
        QueryPlanner, RedDBError, RedDBOptions, RedDBResult, SchemaManifest, StorageMode,
        DEFAULT_EXPORT_RETENTION, DEFAULT_SNAPSHOT_RETENTION, REDDB_FORMAT_VERSION,
        REDDB_PROTOCOL_VERSION,
    };
    pub use crate::application::{
        AdminUseCases, CatalogUseCases, EntityUseCases, GraphUseCases, NativeUseCases,
        QueryUseCases, RuntimeAdminPort, RuntimeCatalogPort, RuntimeEntityPort, RuntimeGraphPort,
        RuntimeNativePort, RuntimeQueryPort, RuntimeSchemaPort, SchemaUseCases,
    };
    pub use crate::auth::store::AuthStore;
    pub use crate::auth::{AuthConfig, AuthError, Role as AuthRole};
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
        ArtifactState, BlockReference, CompactionPolicy, ExportDescriptor, GridLayout,
        ManifestEvent, ManifestEventKind, ManifestPointers, PhysicalAnalyticsJob,
        PhysicalGraphProjection, PhysicalIndexState, PhysicalLayout, PhysicalMetadataFile,
        SnapshotDescriptor, SuperblockHeader, WalPolicy, DEFAULT_MANIFEST_EVENT_HISTORY,
        PHYSICAL_METADATA_PROTOCOL_VERSION,
    };
    pub use crate::runtime::{
        ConnectionPoolConfig, RedDBRuntime, RuntimeConnection, RuntimeFilter, RuntimeFilterValue,
        RuntimeGraphCentralityAlgorithm, RuntimeGraphCentralityResult, RuntimeGraphCentralityScore,
        RuntimeGraphClusteringResult, RuntimeGraphCommunity, RuntimeGraphCommunityAlgorithm,
        RuntimeGraphCommunityResult, RuntimeGraphComponent, RuntimeGraphComponentsMode,
        RuntimeGraphComponentsResult, RuntimeGraphCyclesResult, RuntimeGraphDegreeScore,
        RuntimeGraphDirection, RuntimeGraphEdge, RuntimeGraphHitsResult,
        RuntimeGraphNeighborhoodResult, RuntimeGraphNode, RuntimeGraphPath,
        RuntimeGraphPathAlgorithm, RuntimeGraphPathResult, RuntimeGraphPattern,
        RuntimeGraphProjection, RuntimeGraphTopologicalSortResult, RuntimeGraphTraversalResult,
        RuntimeGraphTraversalStrategy, RuntimeGraphVisit, RuntimeIvfMatch, RuntimeIvfSearchResult,
        RuntimeQueryResult, RuntimeQueryWeights, RuntimeStats, ScanCursor, ScanPage,
    };
    pub use crate::server::{RedDBServer, ServerOptions, ServerReplicationState};
}

pub use crate::api::{
    Capability, CapabilitySet, CatalogService, CatalogSnapshot, CollectionStats, DataOps,
    QueryPlanner, RedDBError, RedDBOptions, RedDBResult, SchemaManifest, StorageMode,
    DEFAULT_EXPORT_RETENTION, DEFAULT_SNAPSHOT_RETENTION, REDDB_FORMAT_VERSION,
    REDDB_PROTOCOL_VERSION,
};
pub use crate::application::{
    AdminUseCases, CatalogUseCases, EntityUseCases, GraphUseCases, NativeUseCases, QueryUseCases,
    RuntimeAdminPort, RuntimeCatalogPort, RuntimeEntityPort, RuntimeGraphPort, RuntimeNativePort,
    RuntimeQueryPort, RuntimeSchemaPort, SchemaUseCases,
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
    ArtifactState, BlockReference, CompactionPolicy, ExportDescriptor, GridLayout, ManifestEvent,
    ManifestEventKind, ManifestPointers, PhysicalAnalyticsJob, PhysicalGraphProjection,
    PhysicalIndexState, PhysicalLayout, PhysicalMetadataFile, SnapshotDescriptor, SuperblockHeader,
    WalPolicy, DEFAULT_MANIFEST_EVENT_HISTORY, PHYSICAL_METADATA_PROTOCOL_VERSION,
};
pub use crate::replication::{ReplicationConfig, ReplicationRole};
pub use crate::runtime::{
    ConnectionPoolConfig, RedDBRuntime, RuntimeConnection, RuntimeFilter, RuntimeFilterValue,
    RuntimeGraphCentralityAlgorithm, RuntimeGraphCentralityResult, RuntimeGraphCentralityScore,
    RuntimeGraphClusteringResult, RuntimeGraphCommunity, RuntimeGraphCommunityAlgorithm,
    RuntimeGraphCommunityResult, RuntimeGraphComponent, RuntimeGraphComponentsMode,
    RuntimeGraphComponentsResult, RuntimeGraphCyclesResult, RuntimeGraphDegreeScore,
    RuntimeGraphDirection, RuntimeGraphEdge, RuntimeGraphHitsResult,
    RuntimeGraphNeighborhoodResult, RuntimeGraphNode, RuntimeGraphPath, RuntimeGraphPathAlgorithm,
    RuntimeGraphPathResult, RuntimeGraphPattern, RuntimeGraphProjection,
    RuntimeGraphTopologicalSortResult, RuntimeGraphTraversalResult, RuntimeGraphTraversalStrategy,
    RuntimeGraphVisit, RuntimeIvfMatch, RuntimeIvfSearchResult, RuntimeQueryResult,
    RuntimeQueryWeights, RuntimeStats, ScanCursor, ScanPage,
};
pub use crate::server::{RedDBServer, ServerOptions, ServerReplicationState};

pub use crate::storage::*;
