use super::*;
use crate::application::{OperationContextFactory, OperationContextInput};
use crate::auth::enforcement_mode::{legacy_rbac_decision, PolicyEnforcementMode};
use crate::auth::policies::{self, EvalContext, ResourceRef};
use crate::server::route_catalog::{
    CommandAuthRequirement, CommandPolicyEngine, CommandSpec, RouteMethod,
};
use std::future::Future;
use std::task::{Context, Poll};
use tonic::codegen::{Body, Service, StdError};
use tonic::server::NamedService;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GrpcCommandBinding {
    rpc: &'static str,
    command_id: &'static str,
}

macro_rules! binding {
    ($rpc:literal, $command_id:literal) => {
        GrpcCommandBinding {
            rpc: $rpc,
            command_id: $command_id,
        }
    };
}

const GRPC_COMMAND_BINDINGS: &[GrpcCommandBinding] = &[
    binding!("Health", "ops.health.aggregate"),
    binding!("Ready", "ops.ready.aggregate"),
    binding!("Stats", "physical.stats"),
    binding!("Collections", "collections.list"),
    binding!("CatalogReadiness", "catalog.readiness"),
    binding!("DeploymentProfiles", "ops.deployment.profiles"),
    binding!("CollectionReadiness", "catalog.collections.readiness"),
    binding!(
        "CollectionAttention",
        "catalog.collections.readiness_attention"
    ),
    binding!("CatalogAttentionSummary", "catalog.attention"),
    binding!("CatalogConsistency", "catalog.consistency"),
    binding!("ServerlessAttach", "serverless.attach"),
    binding!("ServerlessWarmup", "serverless.warmup"),
    binding!("ServerlessReclaim", "serverless.reclaim"),
    binding!("DeclaredIndexes", "catalog.indexes.declared"),
    binding!("OperationalIndexes", "catalog.indexes.operational"),
    binding!("IndexStatuses", "catalog.indexes.status"),
    binding!("IndexAttention", "catalog.indexes.attention"),
    binding!(
        "DeclaredGraphProjections",
        "catalog.graph.projections.declared"
    ),
    binding!(
        "OperationalGraphProjections",
        "catalog.graph.projections.operational"
    ),
    binding!(
        "GraphProjectionStatuses",
        "catalog.graph.projections.status"
    ),
    binding!(
        "GraphProjectionAttention",
        "catalog.graph.projections.attention"
    ),
    binding!("DeclaredAnalyticsJobs", "catalog.analytics_jobs.declared"),
    binding!(
        "OperationalAnalyticsJobs",
        "catalog.analytics_jobs.operational"
    ),
    binding!("AnalyticsJobStatuses", "catalog.analytics_jobs.status"),
    binding!("AnalyticsJobAttention", "catalog.analytics_jobs.attention"),
    binding!("PhysicalMetadata", "physical.metadata"),
    binding!("NativeHeader", "physical.native_header"),
    binding!("NativeCollectionRoots", "physical.native_collection_roots"),
    binding!("NativeManifestSummary", "physical.native_manifest"),
    binding!("NativeRegistrySummary", "physical.native_registry"),
    binding!("NativeRecoverySummary", "physical.native_recovery"),
    binding!("NativeCatalogSummary", "physical.native_catalog"),
    binding!(
        "NativeMetadataStateSummary",
        "physical.native_metadata_state"
    ),
    binding!("PhysicalAuthority", "physical.authority"),
    binding!("NativePhysicalState", "physical.native_state"),
    binding!("NativeVectorArtifacts", "physical.native_vector_artifacts"),
    binding!(
        "InspectNativeVectorArtifacts",
        "physical.native_vector_artifacts.inspect"
    ),
    binding!(
        "InspectNativeVectorArtifact",
        "physical.collections.vector_artifacts.inspect"
    ),
    binding!(
        "NativeHeaderRepairPolicy",
        "physical.native_header.repair_policy"
    ),
    binding!("RepairNativeHeader", "physical.native_header.repair"),
    binding!(
        "WarmupNativeVectorArtifacts",
        "physical.native_vector_artifacts.warmup"
    ),
    binding!(
        "WarmupNativeVectorArtifact",
        "physical.collections.vector_artifacts.warmup"
    ),
    binding!("RepairNativePhysicalState", "physical.native_state.repair"),
    binding!("RebuildPhysicalMetadata", "physical.metadata.rebuild"),
    binding!("Manifest", "physical.manifest"),
    binding!("Roots", "physical.roots"),
    binding!("Snapshots", "physical.snapshots"),
    binding!("Exports", "physical.exports"),
    binding!("Indexes", "physical.indexes"),
    binding!("SetIndexEnabled", "indexes.action"),
    binding!("MarkIndexBuilding", "indexes.action"),
    binding!("MarkIndexReady", "indexes.action"),
    binding!("FailIndex", "indexes.action"),
    binding!("MarkIndexStale", "indexes.action"),
    binding!("WarmupIndex", "indexes.action"),
    binding!("RebuildIndexes", "physical.indexes.rebuild"),
    binding!("GraphProjections", "graph.projections.list"),
    binding!("SaveGraphProjection", "graph.projections.upsert"),
    binding!("SaveAnalyticsJob", "graph.jobs.upsert"),
    binding!("QueueAnalyticsJob", "graph.jobs.queue"),
    binding!("StartAnalyticsJob", "graph.jobs.start"),
    binding!("CompleteAnalyticsJob", "graph.jobs.complete"),
    binding!("MarkAnalyticsJobStale", "graph.jobs.stale"),
    binding!("FailAnalyticsJob", "graph.jobs.fail"),
    binding!(
        "MaterializeGraphProjection",
        "graph.projections.materialize"
    ),
    binding!(
        "MarkGraphProjectionMaterializing",
        "graph.projections.materializing"
    ),
    binding!("MarkGraphProjectionStale", "graph.projections.stale"),
    binding!("FailGraphProjection", "graph.projections.fail"),
    binding!("AnalyticsJobs", "graph.jobs.list"),
    binding!("Scan", "collections.scan"),
    binding!("ExplainQuery", "query.explain"),
    binding!("Query", "query.execute"),
    binding!("BatchQuery", "query.execute"),
    binding!("PrepareQuery", "query.execute"),
    binding!("ExecutePrepared", "query.execute"),
    binding!("Search", "query.search"),
    binding!("TextSearch", "query.text_search"),
    binding!("MultimodalSearch", "query.multimodal_search"),
    binding!("HybridSearch", "query.hybrid_search"),
    binding!("ContextSearch", "query.context"),
    binding!("Similar", "collections.similar"),
    binding!("IvfSearch", "collections.ivf.search"),
    binding!("GraphNeighborhood", "graph.neighborhood"),
    binding!("GraphTraverse", "graph.traverse"),
    binding!("GraphShortestPath", "graph.shortest_path"),
    binding!("GraphComponents", "graph.analytics.components"),
    binding!("GraphCentrality", "graph.analytics.centrality"),
    binding!("GraphCommunity", "graph.analytics.community"),
    binding!("GraphClustering", "graph.analytics.clustering"),
    binding!(
        "GraphPersonalizedPagerank",
        "graph.analytics.pagerank_personalized"
    ),
    binding!("GraphHits", "graph.analytics.hits"),
    binding!("GraphCycles", "graph.analytics.cycles"),
    binding!("GraphTopologicalSort", "graph.analytics.topological_sort"),
    binding!("CreateRow", "collections.rows.create"),
    binding!("CreateNode", "collections.nodes.create"),
    binding!("CreateEdge", "collections.edges.create"),
    binding!("CreateVector", "collections.vectors.create"),
    binding!("CreateDocument", "collections.documents.create"),
    binding!("CreateKv", "kv.dynamic.kv"),
    binding!("KvWatch", "collections.entities.get"),
    binding!("BulkCreateRows", "collections.bulk.rows"),
    binding!("BulkInsertBinary", "collections.batch.insert"),
    binding!("BatchInsert", "collections.batch.insert"),
    binding!("BulkCreateNodes", "collections.bulk.nodes"),
    binding!("BulkCreateEdges", "collections.bulk.edges"),
    binding!("BulkCreateVectors", "collections.bulk.vectors"),
    binding!("BulkCreateDocuments", "collections.bulk.documents"),
    binding!("Ask", "ai.ask"),
    binding!("AskStream", "ai.ask"),
    binding!("Embeddings", "ai.embeddings"),
    binding!("AiPrompt", "ai.prompt"),
    binding!("AiCredentials", "ai.credentials"),
    binding!("PatchEntity", "collections.entities.patch"),
    binding!("CreateSnapshot", "physical.snapshot.create"),
    binding!("CreateExport", "physical.export.create"),
    binding!("ApplyRetention", "physical.retention.apply"),
    binding!("DeleteEntity", "collections.entities.delete"),
    binding!("Checkpoint", "physical.checkpoint"),
    binding!("Topology", "ops.topology.graph"),
    binding!("ReplicationStatus", "ops.replication.status"),
    binding!("PullWalRecords", "ops.replication.snapshot"),
    binding!("ReplicationSnapshot", "ops.replication.snapshot"),
    binding!("SubmitAskSideEffects", "ai.ask"),
    binding!("AckReplicaLsn", "ops.replication.snapshot"),
    binding!("CreateCollection", "collections.create"),
    binding!("DropCollection", "collections.drop"),
    binding!("DescribeCollection", "collections.schema"),
    binding!("AuthBootstrap", "auth.bootstrap"),
    binding!("AuthLogin", "auth.login"),
    binding!("AuthCreateUser", "auth.users.create"),
    binding!("AuthDeleteUser", "auth.users.delete"),
    binding!("AuthListUsers", "auth.users.list"),
    binding!("AuthCreateApiKey", "auth.api_keys.create"),
    binding!("AuthRevokeApiKey", "auth.api_keys.delete"),
    binding!("AuthChangePassword", "auth.change_password"),
    binding!("AuthWhoAmI", "auth.whoami"),
];

#[derive(Clone, Debug)]
pub(super) struct GrpcDispatchContext {
    pub(super) principal: Option<String>,
}

#[derive(Clone)]
pub(super) struct GrpcCatalogService {
    inner: RedDbServer<GrpcRuntime>,
    runtime: GrpcRuntime,
}

impl GrpcCatalogService {
    pub(super) fn new(inner: RedDbServer<GrpcRuntime>, runtime: GrpcRuntime) -> Self {
        Self { inner, runtime }
    }
}

impl NamedService for GrpcCatalogService {
    const NAME: &'static str = <RedDbServer<GrpcRuntime> as NamedService>::NAME;
}

impl<B> Service<http::Request<B>> for GrpcCatalogService
where
    B: Body + Send + 'static,
    B::Error: Into<StdError> + Send + 'static,
{
    type Response = http::Response<tonic::body::Body>;
    type Error = std::convert::Infallible;
    type Future = std::pin::Pin<
        Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>,
    >;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        <RedDbServer<GrpcRuntime> as Service<http::Request<B>>>::poll_ready(&mut self.inner, cx)
    }

    fn call(&mut self, mut request: http::Request<B>) -> Self::Future {
        let authorization = resolve_peer_auth(&request).and_then(|peer_auth| {
            authorize_dispatch(
                &self.runtime,
                request.uri().path(),
                &MetadataMap::from_headers(request.headers().clone()),
                peer_auth,
            )
        });
        match authorization {
            Ok(context) => {
                request.extensions_mut().insert(context);
                Box::pin(self.inner.call(request))
            }
            Err(status) => Box::pin(async move { Ok(status.into_http()) }),
        }
    }
}

fn authorize_dispatch(
    runtime: &GrpcRuntime,
    path: &str,
    metadata: &MetadataMap,
    peer_auth: Option<AuthResult>,
) -> Result<GrpcDispatchContext, Status> {
    let rpc = path
        .strip_prefix("/reddb.v1.RedDb/")
        .ok_or_else(|| Status::unimplemented("unknown gRPC service"))?;
    let binding = GRPC_COMMAND_BINDINGS
        .iter()
        .find(|binding| binding.rpc == rpc)
        .ok_or_else(|| Status::unimplemented(format!("undeclared gRPC method {rpc}")))?;
    authorize_command(runtime, metadata, binding.command_id, peer_auth)
}

fn authorize_command(
    runtime: &GrpcRuntime,
    metadata: &MetadataMap,
    command_id: &str,
    peer_auth: Option<AuthResult>,
) -> Result<GrpcDispatchContext, Status> {
    let auth = peer_auth.unwrap_or_else(|| runtime.resolve_auth(metadata));
    let principal = match &auth {
        AuthResult::Authenticated { username, .. } => Some(username.clone()),
        AuthResult::Anonymous | AuthResult::Denied(_) => None,
    };
    let (tenant, username) = principal
        .as_deref()
        .and_then(|principal| principal.split_once('/'))
        .map_or((None, principal.as_deref()), |(tenant, username)| {
            (Some(tenant), Some(username))
        });
    let operation_context = OperationContextFactory::build(OperationContextInput {
        request_id: metadata
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
        principal: principal.clone(),
        tenant: tenant.map(str::to_string),
        ..OperationContextInput::default()
    });
    let engine = GrpcCommandPolicyEngine {
        auth_store: &runtime.auth_store,
        auth: &auth,
        tenant,
        username,
    };
    crate::server::route_catalog::CommandAuthorizer::new(crate::server::command_catalog(), &engine)
        .authorize(&operation_context, command_id)
        .map_err(|error| match auth {
            AuthResult::Authenticated { .. } => Status::permission_denied(error.to_string()),
            AuthResult::Anonymous | AuthResult::Denied(_) => {
                Status::unauthenticated(error.to_string())
            }
        })?;

    let command = crate::server::command_catalog()
        .command(command_id)
        .ok_or_else(|| Status::internal(format!("unknown command {command_id}")))?;
    if command.auth == CommandAuthRequirement::UserRequired && command_is_write(command) {
        runtime
            .runtime
            .check_write(crate::runtime::write_gate::WriteKind::Dml)
            .map_err(|error| Status::failed_precondition(error.to_string()))?;
    }

    Ok(GrpcDispatchContext { principal })
}

fn resolve_peer_auth<B>(request: &http::Request<B>) -> Result<Option<AuthResult>, Status> {
    use tonic::transport::server::{TcpConnectInfo, TlsConnectInfo};

    let Some(certs) = request
        .extensions()
        .get::<TlsConnectInfo<TcpConnectInfo>>()
        .and_then(TlsConnectInfo::peer_certs)
    else {
        return Ok(None);
    };
    let cert = certs
        .first()
        .ok_or_else(|| Status::unauthenticated("mTLS peer certificate missing"))?;
    let identity = crate::cluster::NodeIdentity::from_peer_certificate_der(cert)
        .map_err(|error| Status::unauthenticated(format!("mTLS peer identity: {error}")))?;
    Ok(Some(AuthResult::Authenticated {
        username: identity.to_string(),
        role: Role::Read,
        source: AuthSource::ClientCert,
    }))
}

struct GrpcCommandPolicyEngine<'a> {
    auth_store: &'a AuthStore,
    auth: &'a AuthResult,
    tenant: Option<&'a str>,
    username: Option<&'a str>,
}

impl CommandPolicyEngine for GrpcCommandPolicyEngine<'_> {
    fn allows(&self, _ctx: &crate::application::OperationContext, command: &CommandSpec) -> bool {
        let default_allow = matches!(
            command.auth,
            CommandAuthRequirement::Public | CommandAuthRequirement::OptionalUser
        );
        let AuthResult::Authenticated { role, .. } = self.auth else {
            return default_allow
                || (matches!(self.auth, AuthResult::Anonymous)
                    && command.auth == CommandAuthRequirement::UserRequired
                    && !self.auth_store.config().require_auth);
        };
        let Some(username) = self.username else {
            return default_allow;
        };

        let action = command_action(command);
        let principal = crate::auth::UserId::from_parts(self.tenant, username);
        let mut resource = ResourceRef::new(
            "command",
            format!("{}:{}", command_audience(command), command.id),
        );
        if let Some(tenant) = self.tenant {
            resource = resource.with_tenant(tenant);
        }
        let context = EvalContext {
            principal_tenant: self.tenant.map(str::to_string),
            current_tenant: self.tenant.map(str::to_string),
            now_ms: crate::auth::now_ms(),
            principal_is_admin_role: role.can_admin(),
            principal_is_platform_scoped: self.tenant.is_none(),
            ..EvalContext::default()
        };
        let policies = self.auth_store.effective_policies(&principal);
        let policy_refs: Vec<_> = policies.iter().map(Arc::as_ref).collect();

        match policies::evaluate(&policy_refs, action, &resource, &context) {
            policies::Decision::Deny { .. } => false,
            policies::Decision::Allow { .. } => true,
            policies::Decision::DefaultDeny | policies::Decision::AdminBypass => {
                default_allow
                    || (self.auth_store.enforcement_mode() == PolicyEnforcementMode::LegacyRbac
                        && legacy_rbac_decision(*role, action))
            }
        }
    }
}

fn command_action(command: &CommandSpec) -> &'static str {
    match command.auth {
        CommandAuthRequirement::Public
        | CommandAuthRequirement::OptionalUser
        | CommandAuthRequirement::UserRequired => {
            if command_is_write(command) {
                "write"
            } else {
                "select"
            }
        }
        CommandAuthRequirement::AdminToken => "admin:*",
        CommandAuthRequirement::OpsCapability(action) => action,
        CommandAuthRequirement::StreamLease => "stream",
    }
}

fn command_is_write(command: &CommandSpec) -> bool {
    matches!(
        command.method,
        RouteMethod::Post
            | RouteMethod::Put
            | RouteMethod::Patch
            | RouteMethod::Delete
            | RouteMethod::Any
    )
}

fn command_audience(command: &CommandSpec) -> &'static str {
    use crate::server::route_catalog::CommandAudience;
    match command.audience {
        CommandAudience::Client => "client",
        CommandAudience::Operator => "operator",
        CommandAudience::Infra => "infra",
        CommandAudience::CompatibilityAdapter => "compatibility-adapter",
        CommandAudience::Internal => "internal",
    }
}

pub(super) fn authorized_principal<T>(request: &Request<T>) -> Result<&str, Status> {
    request
        .extensions()
        .get::<GrpcDispatchContext>()
        .and_then(|context| context.principal.as_deref())
        .ok_or_else(|| Status::unauthenticated("authenticated principal missing from dispatch"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exposed_rpc_names() -> Vec<&'static str> {
        include_str!("../../../reddb-grpc-proto/proto/reddb.proto")
            .lines()
            .filter_map(|line| line.trim().strip_prefix("rpc "))
            .filter_map(|line| line.split_once('(').map(|(name, _)| name))
            .collect()
    }

    #[test]
    fn grpc_catalog_bindings_cover_every_exposed_rpc() {
        let exposed = exposed_rpc_names();
        let bound: Vec<&str> = GRPC_COMMAND_BINDINGS
            .iter()
            .map(|binding| binding.rpc)
            .collect();

        assert_eq!(bound, exposed);
        for binding in GRPC_COMMAND_BINDINGS {
            assert!(
                crate::server::command_catalog()
                    .command(binding.command_id)
                    .is_some(),
                "gRPC method {} maps to unknown command {}",
                binding.rpc,
                binding.command_id
            );
        }
    }

    #[test]
    fn every_protected_grpc_command_rejects_anonymous_dispatch() {
        let runtime =
            RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory()).expect("runtime");
        let grpc = GrpcRuntime {
            runtime,
            auth_store: Arc::new(AuthStore::new(crate::auth::AuthConfig {
                enabled: true,
                require_auth: true,
                ..crate::auth::AuthConfig::default()
            })),
            prepared_registry: Arc::new(PreparedRegistry::new()),
            oauth_validator: None,
        };
        let metadata = MetadataMap::new();

        for binding in GRPC_COMMAND_BINDINGS {
            let command = crate::server::command_catalog()
                .command(binding.command_id)
                .expect("bound command");
            if matches!(
                command.auth,
                CommandAuthRequirement::Public | CommandAuthRequirement::OptionalUser
            ) {
                continue;
            }

            let error = authorize_command(&grpc, &metadata, binding.command_id, None)
                .expect_err(binding.rpc);
            assert_eq!(
                error.code(),
                tonic::Code::Unauthenticated,
                "{}",
                binding.rpc
            );
        }
    }
}
