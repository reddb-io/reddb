//! Minimal HTTP server for RedDB management and remote access.

pub(crate) use crate::application::json_input::{
    json_bool_field, json_f32_field, json_string_field, json_usize_field,
};
pub(crate) use crate::application::{
    AdminUseCases, CatalogUseCases, CreateEdgeInput, CreateEntityOutput, CreateKvInput,
    CreateNodeEmbeddingInput, CreateNodeGraphLinkInput, CreateNodeInput, CreateNodeTableLinkInput,
    CreateRowInput, CreateVectorInput, DeleteEntityInput, EntityUseCases, ExecuteQueryInput,
    ExplainQueryInput, GraphCentralityInput, GraphClusteringInput, GraphCommunitiesInput,
    GraphComponentsInput, GraphCyclesInput, GraphHitsInput, GraphNeighborhoodInput,
    GraphPersonalizedPageRankInput, GraphShortestPathInput, GraphTopologicalSortInput,
    GraphTraversalInput, GraphUseCases, InspectNativeArtifactInput, NativeUseCases,
    PatchEntityInput, PatchEntityOperation, PatchEntityOperationType, QueryUseCases,
    SearchHybridInput, SearchIvfInput, SearchMultimodalInput, SearchSimilarInput, SearchTextInput,
};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use std::sync::Arc;

use crate::api::{RedDBError, RedDBOptions, RedDBResult};
use crate::auth::store::AuthStore;
use crate::catalog::{CatalogModelSnapshot, CollectionDescriptor, CollectionModel, SchemaMode};
use crate::health::{HealthProvider, HealthReport, HealthState};
use crate::json::{parse_json, to_vec as json_to_vec, Map, Value as JsonValue};
use crate::runtime::{
    RedDBRuntime, RuntimeFilter, RuntimeFilterValue, RuntimeGraphCentralityAlgorithm,
    RuntimeGraphCentralityResult, RuntimeGraphClusteringResult, RuntimeGraphCommunityAlgorithm,
    RuntimeGraphCommunityResult, RuntimeGraphComponentsMode, RuntimeGraphComponentsResult,
    RuntimeGraphCyclesResult, RuntimeGraphDirection, RuntimeGraphHitsResult,
    RuntimeGraphNeighborhoodResult, RuntimeGraphPathAlgorithm, RuntimeGraphPathResult,
    RuntimeGraphPattern, RuntimeGraphProjection, RuntimeGraphTopologicalSortResult,
    RuntimeGraphTraversalResult, RuntimeGraphTraversalStrategy, RuntimeIvfSearchResult,
    RuntimeQueryWeights, RuntimeStats, ScanCursor, ScanPage,
};
use crate::storage::schema::Value;
use crate::storage::unified::devx::refs::{NodeRef, TableRef, VectorRef};
use crate::storage::unified::dsl::{MatchComponents, QueryResult as DslQueryResult};
use crate::storage::unified::{MetadataValue, RefTarget, SparseVector};
use crate::storage::{CrossRef, EntityData, EntityId, EntityKind, SimilarResult, UnifiedEntity};

fn analytics_job_json(job: &crate::PhysicalAnalyticsJob) -> JsonValue {
    crate::presentation::admin_json::analytics_job_json(job)
}

fn graph_projection_json(projection: &crate::PhysicalGraphProjection) -> JsonValue {
    crate::presentation::admin_json::graph_projection_json(projection)
}

mod handlers_ai;
mod handlers_auth;
mod handlers_entity;
mod handlers_graph;
mod handlers_ops;
mod handlers_query;
mod handlers_replication;
mod input_parsing;
mod patch_support;
mod request_body;
mod routing;
mod serverless_support;
mod transport;

use self::handlers_ai::*;
use self::handlers_entity::*;
use self::handlers_graph::*;
use self::handlers_ops::*;
use self::handlers_query::*;
use self::input_parsing::*;
use self::patch_support::*;
use self::request_body::*;
use self::routing::*;
use self::serverless_support::*;
use self::transport::*;

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub bind_addr: String,
    pub max_body_bytes: usize,
    pub read_timeout_ms: u64,
    pub write_timeout_ms: u64,
    pub max_scan_limit: usize,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:8080".to_string(),
            max_body_bytes: 1024 * 1024,
            read_timeout_ms: 5_000,
            write_timeout_ms: 5_000,
            max_scan_limit: 1_000,
        }
    }
}

/// Replication state exposed to the HTTP server.
pub struct ServerReplicationState {
    pub config: crate::replication::ReplicationConfig,
    pub primary: Option<crate::replication::primary::PrimaryReplication>,
}

#[derive(Clone)]
pub struct RedDBServer {
    runtime: RedDBRuntime,
    options: ServerOptions,
    auth_store: Option<Arc<AuthStore>>,
    replication: Option<Arc<ServerReplicationState>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerlessWarmupScope {
    Indexes,
    GraphProjections,
    AnalyticsJobs,
    NativeArtifacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeploymentProfile {
    Embedded,
    Server,
    Serverless,
}

#[derive(Debug, Default)]
struct ServerlessAnalyticsWarmupTarget {
    kind: String,
    projection: Option<String>,
}

#[derive(Debug, Default)]
struct ServerlessWarmupPlan {
    indexes: Vec<String>,
    graph_projections: Vec<String>,
    analytics_jobs: Vec<ServerlessAnalyticsWarmupTarget>,
    includes_native_artifacts: bool,
}

#[derive(Debug, Clone)]
struct ParsedQueryRequest {
    query: String,
    entity_types: Option<Vec<String>>,
    capabilities: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy)]
enum PatchOperationType {
    Set,
    Replace,
    Unset,
}

#[derive(Debug, Clone)]
struct PatchOperation {
    op: PatchOperationType,
    path: Vec<String>,
    value: Option<JsonValue>,
}

impl RedDBServer {
    pub fn new(runtime: RedDBRuntime) -> Self {
        Self::with_options(runtime, ServerOptions::default())
    }

    pub fn from_database_options(
        db_options: RedDBOptions,
        server_options: ServerOptions,
    ) -> RedDBResult<Self> {
        let runtime = RedDBRuntime::with_options(db_options)?;
        Ok(Self::with_options(runtime, server_options))
    }

    pub fn with_options(runtime: RedDBRuntime, options: ServerOptions) -> Self {
        Self {
            runtime,
            options,
            auth_store: None,
            replication: None,
        }
    }

    /// Attach an `AuthStore` for HTTP-layer authentication.
    pub fn with_auth(mut self, auth_store: Arc<AuthStore>) -> Self {
        self.auth_store = Some(auth_store);
        self
    }

    /// Attach replication state for status and snapshot endpoints.
    pub fn with_replication(mut self, state: Arc<ServerReplicationState>) -> Self {
        self.replication = Some(state);
        self
    }

    pub fn runtime(&self) -> &RedDBRuntime {
        &self.runtime
    }

    pub fn options(&self) -> &ServerOptions {
        &self.options
    }

    fn query_use_cases(&self) -> QueryUseCases<'_, RedDBRuntime> {
        QueryUseCases::new(&self.runtime)
    }

    fn admin_use_cases(&self) -> AdminUseCases<'_, RedDBRuntime> {
        AdminUseCases::new(&self.runtime)
    }

    fn entity_use_cases(&self) -> EntityUseCases<'_, RedDBRuntime> {
        EntityUseCases::new(&self.runtime)
    }

    fn catalog_use_cases(&self) -> CatalogUseCases<'_, RedDBRuntime> {
        CatalogUseCases::new(&self.runtime)
    }

    fn graph_use_cases(&self) -> GraphUseCases<'_, RedDBRuntime> {
        GraphUseCases::new(&self.runtime)
    }

    fn native_use_cases(&self) -> NativeUseCases<'_, RedDBRuntime> {
        NativeUseCases::new(&self.runtime)
    }

    pub fn serve(&self) -> io::Result<()> {
        let listener = TcpListener::bind(&self.options.bind_addr)?;
        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let _ = self.handle_connection(stream);
                }
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    pub fn serve_in_background(&self) -> thread::JoinHandle<io::Result<()>> {
        let server = self.clone();
        thread::spawn(move || server.serve())
    }

    fn handle_connection(&self, mut stream: TcpStream) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_millis(self.options.read_timeout_ms)))?;
        stream.set_write_timeout(Some(Duration::from_millis(self.options.write_timeout_ms)))?;

        let request = HttpRequest::read_from(&mut stream, self.options.max_body_bytes)?;
        let response = self.route(request);
        stream.write_all(&response.to_http_bytes())?;
        stream.flush()?;
        Ok(())
    }
}
