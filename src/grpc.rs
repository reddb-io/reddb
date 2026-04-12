pub(crate) use crate::application::json_input::{
    json_bool_field, json_f32_field, json_string_field, json_usize_field,
};
pub(crate) use crate::application::{
    AdminUseCases, CatalogUseCases, CreateEdgeInput, CreateEntityOutput, CreateNodeGraphLinkInput,
    CreateNodeInput, CreateNodeTableLinkInput, CreateRowInput, CreateVectorInput,
    DeleteEntityInput, EntityUseCases, ExecuteQueryInput, ExplainQueryInput, GraphCentralityInput,
    GraphClusteringInput, GraphCommunitiesInput, GraphComponentsInput, GraphCyclesInput,
    GraphHitsInput, GraphNeighborhoodInput, GraphPersonalizedPageRankInput, GraphShortestPathInput,
    GraphTopologicalSortInput, GraphTraversalInput, GraphUseCases, InspectNativeArtifactInput,
    NativeUseCases, PatchEntityInput, QueryUseCases, SearchHybridInput, SearchIvfInput,
    SearchSimilarInput, SearchTextInput,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::{RedDBOptions, RedDBResult};
use crate::auth::middleware::{check_permission, AuthResult};
use crate::auth::store::AuthStore;
use crate::auth::Role;
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
    BatchQueryReply, BatchQueryRequest, BulkEntityReply, CollectionRequest, CollectionsReply,
    DeleteEntityRequest, DeploymentProfileRequest, Empty, EntityReply, ExportRequest,
    GraphProjectionUpsertRequest, HealthReply, IndexNameRequest, IndexToggleRequest,
    JsonBulkCreateRequest, JsonCreateRequest, JsonPayloadRequest, ManifestRequest, OperationReply,
    PayloadReply, QueryReply, QueryRequest, ScanEntity, ScanReply, ScanRequest, StatsReply,
    UpdateEntityRequest,
};

mod control_support;
mod entity_ops;
mod input_support;
pub(crate) mod scan_json;

use self::control_support::*;
use self::entity_ops::*;
use self::input_support::*;
use self::scan_json::*;

#[derive(Debug, Clone)]
pub struct GrpcServerOptions {
    pub bind_addr: String,
}

impl Default for GrpcServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:50051".to_string(),
        }
    }
}

#[derive(Clone)]
pub struct RedDBGrpcServer {
    runtime: RedDBRuntime,
    options: GrpcServerOptions,
    auth_store: Arc<AuthStore>,
}

impl RedDBGrpcServer {
    pub fn new(runtime: RedDBRuntime) -> Self {
        let auth_config = crate::auth::AuthConfig::default();
        let auth_store = Arc::new(AuthStore::new(auth_config));
        Self::with_options(runtime, GrpcServerOptions::default(), auth_store)
    }

    pub fn from_database_options(
        db_options: RedDBOptions,
        options: GrpcServerOptions,
    ) -> RedDBResult<Self> {
        // Create runtime first so we can access the pager for vault pages.
        let runtime = RedDBRuntime::with_options(db_options.clone())?;

        let auth_store = if db_options.auth.vault_enabled {
            // The vault stores its encrypted state in reserved pages inside
            // the main .rdb file.  Extract the pager reference from the
            // runtime's underlying store.
            let pager = runtime.db().store().pager().cloned().ok_or_else(|| {
                crate::api::RedDBError::Internal(
                    "vault requires a paged database (persistent mode)".into(),
                )
            })?;
            let store = AuthStore::with_vault(db_options.auth.clone(), pager, None)
                .map_err(|e| crate::api::RedDBError::Internal(e.to_string()))?;
            Arc::new(store)
        } else {
            Arc::new(AuthStore::new(db_options.auth.clone()))
        };
        auth_store.bootstrap_from_env();
        Ok(Self::with_options(runtime, options, auth_store))
    }

    pub fn with_options(
        runtime: RedDBRuntime,
        options: GrpcServerOptions,
        auth_store: Arc<AuthStore>,
    ) -> Self {
        // Inject the auth store into the runtime so that Value::Secret
        // auto-encrypt/decrypt can read the vault AES key.
        runtime.set_auth_store(Arc::clone(&auth_store));
        Self {
            runtime,
            options,
            auth_store,
        }
    }

    pub fn runtime(&self) -> &RedDBRuntime {
        &self.runtime
    }

    pub fn options(&self) -> &GrpcServerOptions {
        &self.options
    }

    pub fn auth_store(&self) -> &Arc<AuthStore> {
        &self.auth_store
    }

    pub async fn serve(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = self.options.bind_addr.parse()?;
        tonic::transport::Server::builder()
            .add_service(
                RedDbServer::new(GrpcRuntime {
                    runtime: self.runtime.clone(),
                    auth_store: self.auth_store.clone(),
                })
                .max_decoding_message_size(256 * 1024 * 1024)
                .max_encoding_message_size(256 * 1024 * 1024),
            )
            .serve(addr)
            .await?;
        Ok(())
    }
}

#[derive(Clone)]
struct GrpcRuntime {
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
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

include!("grpc/service_impl.rs");
