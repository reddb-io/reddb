pub(crate) use crate::application::{
    AdminUseCases, CatalogUseCases, ExecuteQueryInput, ExplainQueryInput, GraphCentralityInput,
    GraphClusteringInput, GraphCommunitiesInput, GraphComponentsInput, GraphCyclesInput,
    GraphHitsInput, GraphNeighborhoodInput, GraphPersonalizedPageRankInput,
    GraphShortestPathInput, GraphTopologicalSortInput, GraphTraversalInput, GraphUseCases,
    InspectNativeArtifactInput, NativeUseCases, QueryUseCases, CreateEdgeInput,
    CreateEntityOutput, CreateNodeGraphLinkInput, CreateNodeInput, CreateNodeTableLinkInput,
    CreateRowInput, CreateVectorInput, DeleteEntityInput, EntityUseCases, PatchEntityInput,
    SearchHybridInput, SearchIvfInput, SearchSimilarInput, SearchTextInput,
};
pub(crate) use crate::application::json_input::{
    json_bool_field, json_f32_field, json_string_field, json_usize_field,
};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{RedDBOptions, RedDBResult};
use crate::health::{HealthProvider, HealthState};
use crate::json::{
    from_str as json_from_str, to_string as json_to_string, Map, Value as JsonValue,
};
use crate::runtime::{
    RedDBRuntime, RuntimeFilter, RuntimeFilterValue, RuntimeGraphCentralityAlgorithm,
    RuntimeGraphCentralityResult, RuntimeGraphClusteringResult, RuntimeGraphCommunityAlgorithm,
    RuntimeGraphCommunityResult, RuntimeGraphComponentsMode, RuntimeGraphComponentsResult,
    RuntimeGraphCyclesResult, RuntimeGraphDirection, RuntimeGraphHitsResult,
    RuntimeGraphNeighborhoodResult, RuntimeGraphPathAlgorithm, RuntimeGraphPathResult,
    RuntimeGraphPattern, RuntimeGraphProjection, RuntimeGraphTopologicalSortResult,
    RuntimeGraphTraversalResult, RuntimeGraphTraversalStrategy, RuntimeIvfSearchResult,
    RuntimeQueryResult, RuntimeQueryWeights, RuntimeStats, ScanPage,
};
use crate::storage::schema::Value;
use crate::storage::unified::devx::refs::{NodeRef, TableRef};
use crate::storage::unified::{Metadata, MetadataValue};
use crate::storage::{EntityData, EntityId, UnifiedEntity};
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};

pub mod proto {
    tonic::include_proto!("reddb.v1");
}

use proto::red_db_server::{RedDb, RedDbServer};
use proto::{
    BulkEntityReply, CollectionRequest, CollectionsReply, DeleteEntityRequest,
    DeploymentProfileRequest, Empty, EntityReply, ExportRequest, GraphProjectionUpsertRequest,
    HealthReply, IndexNameRequest, IndexToggleRequest, JsonBulkCreateRequest, JsonCreateRequest,
    JsonPayloadRequest, ManifestRequest, OperationReply, PayloadReply, QueryReply, QueryRequest,
    ScanEntity, ScanReply, ScanRequest, StatsReply, UpdateEntityRequest,
};

mod control_support;
mod entity_ops;
mod input_support;
mod scan_json;

use self::control_support::*;
use self::entity_ops::*;
use self::input_support::*;
use self::scan_json::*;

#[derive(Debug, Clone)]
pub struct GrpcServerOptions {
    pub bind_addr: String,
    pub auth_token: Option<String>,
    pub write_token: Option<String>,
}

impl Default for GrpcServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:50051".to_string(),
            auth_token: None,
            write_token: None,
        }
    }
}

#[derive(Clone)]
pub struct RedDBGrpcServer {
    runtime: RedDBRuntime,
    options: GrpcServerOptions,
}

impl RedDBGrpcServer {
    pub fn new(runtime: RedDBRuntime) -> Self {
        Self::with_options(runtime, GrpcServerOptions::default())
    }

    pub fn from_database_options(
        db_options: RedDBOptions,
        options: GrpcServerOptions,
    ) -> RedDBResult<Self> {
        let runtime = RedDBRuntime::with_options(db_options)?;
        Ok(Self::with_options(runtime, options))
    }

    pub fn with_options(runtime: RedDBRuntime, options: GrpcServerOptions) -> Self {
        Self { runtime, options }
    }

    pub fn runtime(&self) -> &RedDBRuntime {
        &self.runtime
    }

    pub fn options(&self) -> &GrpcServerOptions {
        &self.options
    }

    pub async fn serve(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = self.options.bind_addr.parse()?;
        tonic::transport::Server::builder()
            .add_service(RedDbServer::new(GrpcRuntime {
                runtime: self.runtime.clone(),
                auth_token: self.options.auth_token.clone(),
                write_token: self.options.write_token.clone(),
            }))
            .serve(addr)
            .await?;
        Ok(())
    }
}

#[derive(Clone)]
struct GrpcRuntime {
    runtime: RedDBRuntime,
    auth_token: Option<String>,
    write_token: Option<String>,
}

impl GrpcRuntime {
    fn admin_use_cases(&self) -> AdminUseCases<'_, RedDBRuntime> {
        AdminUseCases::new(&self.runtime)
    }

    fn catalog_use_cases(&self) -> CatalogUseCases<'_, RedDBRuntime> {
        CatalogUseCases::new(&self.runtime)
    }

    fn query_use_cases(&self) -> QueryUseCases<'_, RedDBRuntime> {
        QueryUseCases::new(&self.runtime)
    }

    fn entity_use_cases(&self) -> EntityUseCases<'_, RedDBRuntime> {
        EntityUseCases::new(&self.runtime)
    }

    fn graph_use_cases(&self) -> GraphUseCases<'_, RedDBRuntime> {
        GraphUseCases::new(&self.runtime)
    }

    fn native_use_cases(&self) -> NativeUseCases<'_, RedDBRuntime> {
        NativeUseCases::new(&self.runtime)
    }
}

#[tonic::async_trait]
impl RedDb for GrpcRuntime {
    include!("grpc/service_admin.rs");
    include!("grpc/service_query_a.rs");
    include!("grpc/service_query_b.rs");
}
