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
use tokio_stream::wrappers::TcpListenerStream;
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status};

pub mod proto {
    tonic::include_proto!("reddb.v1");
}

use proto::red_db_server::{RedDb, RedDbServer};
use proto::{
    BatchQueryReply, BatchQueryRequest, BulkEntityReply, CollectionRequest, CollectionsReply,
    DeleteEntityRequest, DeploymentProfileRequest, Empty, EntityReply, ExecutePreparedRequest,
    ExportRequest, GraphProjectionUpsertRequest, HealthReply, IndexNameRequest, IndexToggleRequest,
    JsonBulkCreateRequest, JsonCreateRequest, JsonPayloadRequest, ManifestRequest, OperationReply,
    PayloadReply, PrepareQueryReply, PrepareQueryRequest, QueryReply, QueryRequest, ScanEntity,
    ScanReply, ScanRequest, StatsReply, UpdateEntityRequest,
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

    fn grpc_runtime(&self) -> GrpcRuntime {
        GrpcRuntime {
            runtime: self.runtime.clone(),
            auth_store: self.auth_store.clone(),
            prepared_registry: PreparedStatementRegistry::new(),
        }
    }

    pub async fn serve(&self) -> Result<(), Box<dyn std::error::Error>> {
        let addr = self.options.bind_addr.parse()?;
        tonic::transport::Server::builder()
            .add_service(
                RedDbServer::new(self.grpc_runtime())
                    .max_decoding_message_size(256 * 1024 * 1024)
                    .max_encoding_message_size(256 * 1024 * 1024),
            )
            .serve(addr)
            .await?;
        Ok(())
    }

    pub async fn serve_on(
        &self,
        listener: std::net::TcpListener,
    ) -> Result<(), Box<dyn std::error::Error>> {
        listener.set_nonblocking(true)?;
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let incoming = TcpListenerStream::new(listener);
        tonic::transport::Server::builder()
            .add_service(
                RedDbServer::new(self.grpc_runtime())
                    .max_decoding_message_size(256 * 1024 * 1024)
                    .max_encoding_message_size(256 * 1024 * 1024),
            )
            .serve_with_incoming(incoming)
            .await?;
        Ok(())
    }
}

/// Server-side prepared statement — parsed + parameterized once, executed N times.
struct GrpcPreparedStatement {
    shape: std::sync::Arc<crate::storage::query::ast::QueryExpr>,
    parameter_count: usize,
    created_at: std::time::Instant,
}

/// Registry of prepared statements for one server instance.
/// Session-independent: any connection can execute any prepared statement by ID.
struct PreparedStatementRegistry {
    // parking_lot::RwLock never poisons on panic — safe to use without unwrap().
    map: parking_lot::RwLock<std::collections::HashMap<u64, GrpcPreparedStatement>>,
    next_id: std::sync::atomic::AtomicU64,
    get_count: std::sync::atomic::AtomicU64,
}

impl PreparedStatementRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            map: parking_lot::RwLock::new(std::collections::HashMap::new()),
            next_id: std::sync::atomic::AtomicU64::new(1),
            get_count: std::sync::atomic::AtomicU64::new(0),
        })
    }

    fn prepare(&self, shape: crate::storage::query::ast::QueryExpr, parameter_count: usize) -> u64 {
        use std::sync::atomic::Ordering;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut map = self.map.write();
        self.evict_old_locked(&mut map);
        map.insert(
            id,
            GrpcPreparedStatement {
                // Store as Arc to avoid cloning the full AST on every execute.
                shape: std::sync::Arc::new(shape),
                parameter_count,
                created_at: std::time::Instant::now(),
            },
        );
        id
    }

    fn get_shape_and_count(
        &self,
        id: u64,
    ) -> Option<(std::sync::Arc<crate::storage::query::ast::QueryExpr>, usize)> {
        // Periodic eviction on execute/get traffic so long-lived servers that
        // prepare once and execute many times still age out stale statements.
        let get_count = self
            .get_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        if get_count % 256 == 0 {
            let mut map = self.map.write();
            self.evict_old_locked(&mut map);
        }
        let map = self.map.read();
        map.get(&id)
            .map(|s| (std::sync::Arc::clone(&s.shape), s.parameter_count))
    }

    fn evict_old_locked(&self, map: &mut std::collections::HashMap<u64, GrpcPreparedStatement>) {
        let threshold = std::time::Duration::from_secs(3600);
        map.retain(|_, v| v.created_at.elapsed() < threshold);
    }
}

#[derive(Clone)]
struct GrpcRuntime {
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    prepared_registry: Arc<PreparedStatementRegistry>,
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
