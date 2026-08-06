use super::*;
use crate::application::{OperationContextFactory, OperationContextInput};
use crate::auth::enforcement_mode::{legacy_rbac_decision, PolicyEnforcementMode};
use crate::auth::policies::{self, EvalContext, ResourceRef};
use crate::server::route_catalog::{CommandPolicyEngine, CommandSpec};
use std::future::Future;
use std::task::{Context, Poll};
use tonic::codegen::{Body, Service, StdError};
use tonic::server::NamedService;

/// Replication capabilities (issue #820). Stream and ack stay separate so a
/// replica that may read the WAL cannot also forge acks, and vice versa.
const REPLICATION_STREAM_ACTION: &str = "cluster:replication:stream";
const REPLICATION_ACK_ACTION: &str = "cluster:replication:ack";

/// The authorization class a gRPC method is bound to.
///
/// This is the declarative transcription of the per-method guard each RPC
/// used to call by hand. It lives on the binding rather than being derived
/// from [`CommandSpec::method`] because a command's canonical HTTP method
/// describes its HTTP shape, not the RPC's effect: `Query`, `Search` and
/// `Scan` are POST commands that only read, and `AuthLogin` is a POST
/// command on the authentication plane rather than a DML write. Deriving
/// the required action from the method would deny read-role principals the
/// query RPCs and would run the replica write gate on logins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GrpcAuthClass {
    /// No dispatch-level principal requirement: the RPC is a probe, part of
    /// the unauthenticated auth plane, or owns its check inside the handler
    /// (`AuthChangePassword`). Explicit Deny policies still apply.
    Open,
    /// Legacy `authorize_read`.
    Read,
    /// Legacy `authorize_write`. Also runs the replica write gate.
    Write,
    /// Legacy `authorize_admin`: an admin role, denied with
    /// `PERMISSION_DENIED` for everyone else.
    Admin,
    /// Legacy `authorize_replication_capability`: an explicit policy Allow on
    /// the named capability for `cluster:replication`, with no legacy-RBAC
    /// fallback, and mTLS peer identity accepted in place of metadata
    /// credentials.
    Replication(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GrpcCommandBinding {
    rpc: &'static str,
    command_id: &'static str,
    auth: GrpcAuthClass,
}

macro_rules! binding {
    ($rpc:literal, $command_id:literal, $auth:expr) => {
        GrpcCommandBinding {
            rpc: $rpc,
            command_id: $command_id,
            auth: $auth,
        }
    };
}

const GRPC_COMMAND_BINDINGS: &[GrpcCommandBinding] = &[
    binding!("Health", "ops.health.aggregate", GrpcAuthClass::Open),
    binding!("Ready", "ops.ready.aggregate", GrpcAuthClass::Open),
    binding!("Stats", "physical.stats", GrpcAuthClass::Read),
    binding!("Collections", "collections.list", GrpcAuthClass::Read),
    binding!("CatalogReadiness", "catalog.readiness", GrpcAuthClass::Read),
    binding!(
        "DeploymentProfiles",
        "ops.deployment.profiles",
        GrpcAuthClass::Read
    ),
    binding!(
        "CollectionReadiness",
        "catalog.collections.readiness",
        GrpcAuthClass::Read
    ),
    binding!(
        "CollectionAttention",
        "catalog.collections.readiness_attention",
        GrpcAuthClass::Read
    ),
    binding!(
        "CatalogAttentionSummary",
        "catalog.attention",
        GrpcAuthClass::Read
    ),
    binding!(
        "CatalogConsistency",
        "catalog.consistency",
        GrpcAuthClass::Read
    ),
    binding!(
        "ServerlessAttach",
        "serverless.attach",
        GrpcAuthClass::Write
    ),
    binding!(
        "ServerlessWarmup",
        "serverless.warmup",
        GrpcAuthClass::Write
    ),
    binding!(
        "ServerlessReclaim",
        "serverless.reclaim",
        GrpcAuthClass::Write
    ),
    binding!(
        "DeclaredIndexes",
        "catalog.indexes.declared",
        GrpcAuthClass::Read
    ),
    binding!(
        "OperationalIndexes",
        "catalog.indexes.operational",
        GrpcAuthClass::Read
    ),
    binding!(
        "IndexStatuses",
        "catalog.indexes.status",
        GrpcAuthClass::Read
    ),
    binding!(
        "IndexAttention",
        "catalog.indexes.attention",
        GrpcAuthClass::Read
    ),
    binding!(
        "DeclaredGraphProjections",
        "catalog.graph.projections.declared",
        GrpcAuthClass::Read
    ),
    binding!(
        "OperationalGraphProjections",
        "catalog.graph.projections.operational",
        GrpcAuthClass::Read
    ),
    binding!(
        "GraphProjectionStatuses",
        "catalog.graph.projections.status",
        GrpcAuthClass::Read
    ),
    binding!(
        "GraphProjectionAttention",
        "catalog.graph.projections.attention",
        GrpcAuthClass::Read
    ),
    binding!(
        "DeclaredAnalyticsJobs",
        "catalog.analytics_jobs.declared",
        GrpcAuthClass::Read
    ),
    binding!(
        "OperationalAnalyticsJobs",
        "catalog.analytics_jobs.operational",
        GrpcAuthClass::Read
    ),
    binding!(
        "AnalyticsJobStatuses",
        "catalog.analytics_jobs.status",
        GrpcAuthClass::Read
    ),
    binding!(
        "AnalyticsJobAttention",
        "catalog.analytics_jobs.attention",
        GrpcAuthClass::Read
    ),
    binding!("PhysicalMetadata", "physical.metadata", GrpcAuthClass::Read),
    binding!(
        "NativeHeader",
        "physical.native_header",
        GrpcAuthClass::Read
    ),
    binding!(
        "NativeCollectionRoots",
        "physical.native_collection_roots",
        GrpcAuthClass::Read
    ),
    binding!(
        "NativeManifestSummary",
        "physical.native_manifest",
        GrpcAuthClass::Read
    ),
    binding!(
        "NativeRegistrySummary",
        "physical.native_registry",
        GrpcAuthClass::Read
    ),
    binding!(
        "NativeRecoverySummary",
        "physical.native_recovery",
        GrpcAuthClass::Read
    ),
    binding!(
        "NativeCatalogSummary",
        "physical.native_catalog",
        GrpcAuthClass::Read
    ),
    binding!(
        "NativeMetadataStateSummary",
        "physical.native_metadata_state",
        GrpcAuthClass::Read
    ),
    binding!(
        "PhysicalAuthority",
        "physical.authority",
        GrpcAuthClass::Read
    ),
    binding!(
        "NativePhysicalState",
        "physical.native_state",
        GrpcAuthClass::Read
    ),
    binding!(
        "NativeVectorArtifacts",
        "physical.native_vector_artifacts",
        GrpcAuthClass::Read
    ),
    binding!(
        "InspectNativeVectorArtifacts",
        "physical.native_vector_artifacts.inspect",
        GrpcAuthClass::Read
    ),
    binding!(
        "InspectNativeVectorArtifact",
        "physical.collections.vector_artifacts.inspect",
        GrpcAuthClass::Read
    ),
    binding!(
        "NativeHeaderRepairPolicy",
        "physical.native_header.repair_policy",
        GrpcAuthClass::Read
    ),
    binding!(
        "RepairNativeHeader",
        "physical.native_header.repair",
        GrpcAuthClass::Write
    ),
    binding!(
        "WarmupNativeVectorArtifacts",
        "physical.native_vector_artifacts.warmup",
        GrpcAuthClass::Write
    ),
    binding!(
        "WarmupNativeVectorArtifact",
        "physical.collections.vector_artifacts.warmup",
        GrpcAuthClass::Write
    ),
    binding!(
        "RepairNativePhysicalState",
        "physical.native_state.repair",
        GrpcAuthClass::Write
    ),
    binding!(
        "RebuildPhysicalMetadata",
        "physical.metadata.rebuild",
        GrpcAuthClass::Write
    ),
    binding!("Manifest", "physical.manifest", GrpcAuthClass::Read),
    binding!("Roots", "physical.roots", GrpcAuthClass::Read),
    binding!("Snapshots", "physical.snapshots", GrpcAuthClass::Read),
    binding!("Exports", "physical.exports", GrpcAuthClass::Read),
    binding!("Indexes", "physical.indexes", GrpcAuthClass::Read),
    binding!("SetIndexEnabled", "indexes.action", GrpcAuthClass::Write),
    binding!("MarkIndexBuilding", "indexes.action", GrpcAuthClass::Write),
    binding!("MarkIndexReady", "indexes.action", GrpcAuthClass::Write),
    binding!("FailIndex", "indexes.action", GrpcAuthClass::Write),
    binding!("MarkIndexStale", "indexes.action", GrpcAuthClass::Write),
    binding!("WarmupIndex", "indexes.action", GrpcAuthClass::Write),
    binding!(
        "RebuildIndexes",
        "physical.indexes.rebuild",
        GrpcAuthClass::Write
    ),
    binding!(
        "GraphProjections",
        "graph.projections.list",
        GrpcAuthClass::Read
    ),
    binding!(
        "SaveGraphProjection",
        "graph.projections.upsert",
        GrpcAuthClass::Write
    ),
    binding!(
        "SaveAnalyticsJob",
        "graph.jobs.upsert",
        GrpcAuthClass::Write
    ),
    binding!(
        "QueueAnalyticsJob",
        "graph.jobs.queue",
        GrpcAuthClass::Write
    ),
    binding!(
        "StartAnalyticsJob",
        "graph.jobs.start",
        GrpcAuthClass::Write
    ),
    binding!(
        "CompleteAnalyticsJob",
        "graph.jobs.complete",
        GrpcAuthClass::Write
    ),
    binding!(
        "MarkAnalyticsJobStale",
        "graph.jobs.stale",
        GrpcAuthClass::Write
    ),
    binding!("FailAnalyticsJob", "graph.jobs.fail", GrpcAuthClass::Write),
    binding!(
        "MaterializeGraphProjection",
        "graph.projections.materialize",
        GrpcAuthClass::Write
    ),
    binding!(
        "MarkGraphProjectionMaterializing",
        "graph.projections.materializing",
        GrpcAuthClass::Write
    ),
    binding!(
        "MarkGraphProjectionStale",
        "graph.projections.stale",
        GrpcAuthClass::Write
    ),
    binding!(
        "FailGraphProjection",
        "graph.projections.fail",
        GrpcAuthClass::Write
    ),
    binding!("AnalyticsJobs", "graph.jobs.list", GrpcAuthClass::Read),
    binding!("Scan", "collections.scan", GrpcAuthClass::Read),
    binding!("ExplainQuery", "query.explain", GrpcAuthClass::Read),
    binding!("Query", "query.execute", GrpcAuthClass::Read),
    binding!("BatchQuery", "query.execute", GrpcAuthClass::Read),
    binding!("PrepareQuery", "query.execute", GrpcAuthClass::Read),
    binding!("ExecutePrepared", "query.execute", GrpcAuthClass::Read),
    binding!("Search", "query.search", GrpcAuthClass::Read),
    binding!("TextSearch", "query.text_search", GrpcAuthClass::Read),
    binding!(
        "MultimodalSearch",
        "query.multimodal_search",
        GrpcAuthClass::Read
    ),
    binding!("HybridSearch", "query.hybrid_search", GrpcAuthClass::Read),
    binding!("ContextSearch", "query.context", GrpcAuthClass::Read),
    binding!("Similar", "collections.similar", GrpcAuthClass::Read),
    binding!("IvfSearch", "collections.ivf.search", GrpcAuthClass::Read),
    binding!(
        "GraphNeighborhood",
        "graph.neighborhood",
        GrpcAuthClass::Read
    ),
    binding!("GraphTraverse", "graph.traverse", GrpcAuthClass::Read),
    binding!(
        "GraphShortestPath",
        "graph.shortest_path",
        GrpcAuthClass::Read
    ),
    binding!(
        "GraphComponents",
        "graph.analytics.components",
        GrpcAuthClass::Read
    ),
    binding!(
        "GraphCentrality",
        "graph.analytics.centrality",
        GrpcAuthClass::Read
    ),
    binding!(
        "GraphCommunity",
        "graph.analytics.community",
        GrpcAuthClass::Read
    ),
    binding!(
        "GraphClustering",
        "graph.analytics.clustering",
        GrpcAuthClass::Read
    ),
    binding!(
        "GraphPersonalizedPagerank",
        "graph.analytics.pagerank_personalized",
        GrpcAuthClass::Read
    ),
    binding!("GraphHits", "graph.analytics.hits", GrpcAuthClass::Read),
    binding!("GraphCycles", "graph.analytics.cycles", GrpcAuthClass::Read),
    binding!(
        "GraphTopologicalSort",
        "graph.analytics.topological_sort",
        GrpcAuthClass::Read
    ),
    binding!("CreateRow", "collections.rows.create", GrpcAuthClass::Write),
    binding!(
        "CreateNode",
        "collections.nodes.create",
        GrpcAuthClass::Write
    ),
    binding!(
        "CreateEdge",
        "collections.edges.create",
        GrpcAuthClass::Write
    ),
    binding!(
        "CreateVector",
        "collections.vectors.create",
        GrpcAuthClass::Write
    ),
    binding!(
        "CreateDocument",
        "collections.documents.create",
        GrpcAuthClass::Write
    ),
    binding!("CreateKv", "kv.dynamic.kv", GrpcAuthClass::Write),
    binding!("KvWatch", "collections.entities.get", GrpcAuthClass::Read),
    binding!(
        "BulkCreateRows",
        "collections.bulk.rows",
        GrpcAuthClass::Write
    ),
    binding!(
        "BulkInsertBinary",
        "collections.batch.insert",
        GrpcAuthClass::Write
    ),
    binding!(
        "BatchInsert",
        "collections.batch.insert",
        GrpcAuthClass::Write
    ),
    binding!(
        "BulkCreateNodes",
        "collections.bulk.nodes",
        GrpcAuthClass::Write
    ),
    binding!(
        "BulkCreateEdges",
        "collections.bulk.edges",
        GrpcAuthClass::Write
    ),
    binding!(
        "BulkCreateVectors",
        "collections.bulk.vectors",
        GrpcAuthClass::Write
    ),
    binding!(
        "BulkCreateDocuments",
        "collections.bulk.documents",
        GrpcAuthClass::Write
    ),
    binding!("Ask", "ai.ask", GrpcAuthClass::Read),
    binding!("AskStream", "ai.ask", GrpcAuthClass::Read),
    binding!("Embeddings", "ai.embeddings", GrpcAuthClass::Write),
    binding!("AiPrompt", "ai.prompt", GrpcAuthClass::Write),
    binding!("AiCredentials", "ai.credentials", GrpcAuthClass::Write),
    binding!(
        "PatchEntity",
        "collections.entities.patch",
        GrpcAuthClass::Write
    ),
    binding!(
        "CreateSnapshot",
        "physical.snapshot.create",
        GrpcAuthClass::Write
    ),
    binding!(
        "CreateExport",
        "physical.export.create",
        GrpcAuthClass::Write
    ),
    binding!(
        "ApplyRetention",
        "physical.retention.apply",
        GrpcAuthClass::Write
    ),
    binding!(
        "DeleteEntity",
        "collections.entities.delete",
        GrpcAuthClass::Write
    ),
    binding!("Checkpoint", "physical.checkpoint", GrpcAuthClass::Write),
    binding!("Topology", "ops.topology.graph", GrpcAuthClass::Open),
    binding!(
        "ReplicationStatus",
        "ops.replication.status",
        GrpcAuthClass::Read
    ),
    binding!(
        "PullWalRecords",
        "ops.replication.snapshot",
        GrpcAuthClass::Replication(REPLICATION_STREAM_ACTION)
    ),
    binding!(
        "ReplicationSnapshot",
        "ops.replication.snapshot",
        GrpcAuthClass::Replication(REPLICATION_STREAM_ACTION)
    ),
    binding!("SubmitAskSideEffects", "ai.ask", GrpcAuthClass::Read),
    binding!(
        "AckReplicaLsn",
        "ops.replication.snapshot",
        GrpcAuthClass::Replication(REPLICATION_ACK_ACTION)
    ),
    binding!(
        "CreateCollection",
        "collections.create",
        GrpcAuthClass::Write
    ),
    binding!("DropCollection", "collections.drop", GrpcAuthClass::Admin),
    binding!(
        "DescribeCollection",
        "collections.schema",
        GrpcAuthClass::Read
    ),
    binding!("AuthBootstrap", "auth.bootstrap", GrpcAuthClass::Open),
    binding!("AuthLogin", "auth.login", GrpcAuthClass::Open),
    binding!("AuthCreateUser", "auth.users.create", GrpcAuthClass::Admin),
    binding!("AuthDeleteUser", "auth.users.delete", GrpcAuthClass::Admin),
    binding!("AuthListUsers", "auth.users.list", GrpcAuthClass::Admin),
    binding!(
        "AuthCreateApiKey",
        "auth.api_keys.create",
        GrpcAuthClass::Admin
    ),
    binding!(
        "AuthRevokeApiKey",
        "auth.api_keys.delete",
        GrpcAuthClass::Admin
    ),
    binding!(
        "AuthChangePassword",
        "auth.change_password",
        GrpcAuthClass::Open
    ),
    binding!("AuthWhoAmI", "auth.whoami", GrpcAuthClass::Open),
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
        let authorization = binding_for_path(request.uri().path()).and_then(|binding| {
            // Only the replication bindings accept an mTLS peer certificate as
            // the principal; every other RPC authenticates from metadata, so a
            // cert-bearing client neither gains access without an API key nor
            // gets its metadata role downgraded.
            let peer_auth = match binding.auth {
                GrpcAuthClass::Replication(_) => resolve_peer_auth(&request)?,
                _ => None,
            };
            authorize_binding(
                &self.runtime,
                binding,
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

/// The catalog commands gRPC serves. `grpc_catalog_bindings_cover_every_exposed_rpc`
/// pins this table against `service RedDb`, so the bindings are the gRPC surface
/// and the command-coverage matrix reads them instead of re-parsing the proto.
pub(crate) fn bound_command_ids() -> impl Iterator<Item = &'static str> {
    GRPC_COMMAND_BINDINGS
        .iter()
        .map(|binding| binding.command_id)
}

fn binding_for_path(path: &str) -> Result<&'static GrpcCommandBinding, Status> {
    let rpc = path
        .strip_prefix("/reddb.v1.RedDb/")
        .ok_or_else(|| Status::unimplemented("unknown gRPC service"))?;
    GRPC_COMMAND_BINDINGS
        .iter()
        .find(|binding| binding.rpc == rpc)
        .ok_or_else(|| Status::unimplemented(format!("undeclared gRPC method {rpc}")))
}

fn authorize_binding(
    runtime: &GrpcRuntime,
    binding: &GrpcCommandBinding,
    metadata: &MetadataMap,
    peer_auth: Option<AuthResult>,
) -> Result<GrpcDispatchContext, Status> {
    if let GrpcAuthClass::Replication(action) = binding.auth {
        let auth = peer_auth.unwrap_or_else(|| runtime.resolve_auth(metadata));
        let username = authorize_replication_capability(runtime, &auth, action)?;
        return Ok(GrpcDispatchContext {
            principal: Some(username),
        });
    }

    let auth = runtime.resolve_auth(metadata);
    let principal = match &auth {
        AuthResult::Authenticated { username, .. } => Some(username.clone()),
        AuthResult::Anonymous | AuthResult::Denied(_) => None,
    };

    // The admin gate runs before the catalog so its denial code stays
    // PERMISSION_DENIED for anonymous callers too, matching `authorize_admin`.
    if binding.auth == GrpcAuthClass::Admin {
        crate::auth::middleware::check_permission(&auth, false, true)
            .map_err(Status::permission_denied)?;
    }

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
        class: binding.auth,
        tenant,
        username,
    };
    // `authorize_read`/`authorize_write` both reported denials as
    // UNAUTHENTICATED; keep that mapping so clients see no status change.
    crate::server::route_catalog::CommandAuthorizer::new(crate::server::command_catalog(), &engine)
        .authorize(&operation_context, binding.command_id)
        .map_err(|error| Status::unauthenticated(error.to_string()))?;

    // Replica write gate. Every RPC that used to call `authorize_write` is
    // bound to `GrpcAuthClass::Write`, so the gate covers the same set
    // regardless of the command's own auth requirement.
    if binding.auth == GrpcAuthClass::Write {
        runtime
            .runtime
            .check_write(crate::runtime::write_gate::WriteKind::Dml)
            .map_err(|error| Status::failed_precondition(error.to_string()))?;
    }

    Ok(GrpcDispatchContext { principal })
}

/// Issue #820: replication capabilities are policy-only. A `DefaultDeny`
/// never falls through to the legacy RBAC posture, so a read-role key cannot
/// stream the WAL or ack an LSN without an explicit policy grant.
fn authorize_replication_capability(
    runtime: &GrpcRuntime,
    auth: &AuthResult,
    action: &str,
) -> Result<String, Status> {
    let username = match auth {
        AuthResult::Authenticated { username, .. } => username.clone(),
        AuthResult::Denied(reason) => return Err(Status::unauthenticated(reason.clone())),
        AuthResult::Anonymous => return Err(Status::unauthenticated("authentication required")),
    };

    let principal = crate::auth::UserId::platform(username.clone());
    let resource = ResourceRef::new("cluster", "replication");
    let outcome = runtime.auth_store.simulate(
        &principal,
        action,
        &resource,
        crate::auth::store::SimCtx::default(),
    );
    match outcome.decision {
        policies::Decision::Allow { .. } | policies::Decision::AdminBypass => Ok(username),
        _ => Err(Status::permission_denied(format!(
            "policy: principal '{username}' is not allowed to perform '{action}'"
        ))),
    }
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
    class: GrpcAuthClass,
    tenant: Option<&'a str>,
    username: Option<&'a str>,
}

impl CommandPolicyEngine for GrpcCommandPolicyEngine<'_> {
    fn allows(&self, _ctx: &crate::application::OperationContext, command: &CommandSpec) -> bool {
        let (action, default_allow) = class_policy(self.class);
        let AuthResult::Authenticated { role, .. } = self.auth else {
            // `check_permission` let anonymous callers through whenever the
            // deployment did not require authentication; a `Denied` result
            // (bad or missing token under `require_auth`) never passed.
            return default_allow || matches!(self.auth, AuthResult::Anonymous);
        };
        let Some(username) = self.username else {
            return default_allow;
        };

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

/// The IAM action a binding class evaluates, plus whether a principal with no
/// matching policy statement is allowed through.
fn class_policy(class: GrpcAuthClass) -> (&'static str, bool) {
    match class {
        GrpcAuthClass::Open => ("select", true),
        GrpcAuthClass::Read => ("select", false),
        GrpcAuthClass::Write => ("write", false),
        GrpcAuthClass::Admin => ("admin:*", false),
        // Handled by `authorize_replication_capability`, which never reaches
        // this engine.
        GrpcAuthClass::Replication(action) => (action, false),
    }
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

    /// The guard every gRPC method called by hand before dispatch consumed
    /// the catalog, recovered from `origin/main`'s
    /// `crates/reddb-server/src/grpc/service_impl.rs`. This table is the
    /// independent record of the pre-catalog behaviour: the matrix test
    /// below derives its expectations from here and its actuals from
    /// [`GRPC_COMMAND_BINDINGS`], so a binding that downgrades a method
    /// fails CI.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum PrePrGuard {
        /// No inline guard, or the handler owns its own check.
        NoGuard,
        Read,
        Write,
        Admin,
        ReplicationStream,
        ReplicationAck,
    }

    macro_rules! guard {
        ($rpc:literal, $guard:ident) => {
            ($rpc, PrePrGuard::$guard)
        };
    }

    const PRE_PR_GRPC_GUARDS: &[(&str, PrePrGuard)] = &[
        guard!("Health", NoGuard),
        guard!("Ready", NoGuard),
        guard!("Stats", Read),
        guard!("Collections", Read),
        guard!("CatalogReadiness", Read),
        guard!("DeploymentProfiles", Read),
        guard!("CollectionReadiness", Read),
        guard!("CollectionAttention", Read),
        guard!("CatalogAttentionSummary", Read),
        guard!("CatalogConsistency", Read),
        guard!("ServerlessAttach", Write),
        guard!("ServerlessWarmup", Write),
        guard!("ServerlessReclaim", Write),
        guard!("DeclaredIndexes", Read),
        guard!("OperationalIndexes", Read),
        guard!("IndexStatuses", Read),
        guard!("IndexAttention", Read),
        guard!("DeclaredGraphProjections", Read),
        guard!("OperationalGraphProjections", Read),
        guard!("GraphProjectionStatuses", Read),
        guard!("GraphProjectionAttention", Read),
        guard!("DeclaredAnalyticsJobs", Read),
        guard!("OperationalAnalyticsJobs", Read),
        guard!("AnalyticsJobStatuses", Read),
        guard!("AnalyticsJobAttention", Read),
        guard!("PhysicalMetadata", Read),
        guard!("NativeHeader", Read),
        guard!("NativeCollectionRoots", Read),
        guard!("NativeManifestSummary", Read),
        guard!("NativeRegistrySummary", Read),
        guard!("NativeRecoverySummary", Read),
        guard!("NativeCatalogSummary", Read),
        guard!("NativeMetadataStateSummary", Read),
        guard!("PhysicalAuthority", Read),
        guard!("NativePhysicalState", Read),
        guard!("NativeVectorArtifacts", Read),
        guard!("InspectNativeVectorArtifacts", Read),
        guard!("InspectNativeVectorArtifact", Read),
        guard!("NativeHeaderRepairPolicy", Read),
        guard!("RepairNativeHeader", Write),
        guard!("WarmupNativeVectorArtifacts", Write),
        guard!("WarmupNativeVectorArtifact", Write),
        guard!("RepairNativePhysicalState", Write),
        guard!("RebuildPhysicalMetadata", Write),
        guard!("Manifest", Read),
        guard!("Roots", Read),
        guard!("Snapshots", Read),
        guard!("Exports", Read),
        guard!("Indexes", Read),
        guard!("SetIndexEnabled", Write),
        guard!("MarkIndexBuilding", Write),
        guard!("MarkIndexReady", Write),
        guard!("FailIndex", Write),
        guard!("MarkIndexStale", Write),
        guard!("WarmupIndex", Write),
        guard!("RebuildIndexes", Write),
        guard!("GraphProjections", Read),
        guard!("SaveGraphProjection", Write),
        guard!("SaveAnalyticsJob", Write),
        guard!("QueueAnalyticsJob", Write),
        guard!("StartAnalyticsJob", Write),
        guard!("CompleteAnalyticsJob", Write),
        guard!("MarkAnalyticsJobStale", Write),
        guard!("FailAnalyticsJob", Write),
        guard!("MaterializeGraphProjection", Write),
        guard!("MarkGraphProjectionMaterializing", Write),
        guard!("MarkGraphProjectionStale", Write),
        guard!("FailGraphProjection", Write),
        guard!("AnalyticsJobs", Read),
        guard!("Scan", Read),
        guard!("ExplainQuery", Read),
        guard!("Query", Read),
        guard!("BatchQuery", Read),
        guard!("PrepareQuery", Read),
        guard!("ExecutePrepared", Read),
        guard!("Search", Read),
        guard!("TextSearch", Read),
        guard!("MultimodalSearch", Read),
        guard!("HybridSearch", Read),
        guard!("ContextSearch", Read),
        guard!("Similar", Read),
        guard!("IvfSearch", Read),
        guard!("GraphNeighborhood", Read),
        guard!("GraphTraverse", Read),
        guard!("GraphShortestPath", Read),
        guard!("GraphComponents", Read),
        guard!("GraphCentrality", Read),
        guard!("GraphCommunity", Read),
        guard!("GraphClustering", Read),
        guard!("GraphPersonalizedPagerank", Read),
        guard!("GraphHits", Read),
        guard!("GraphCycles", Read),
        guard!("GraphTopologicalSort", Read),
        guard!("CreateRow", Write),
        guard!("CreateNode", Write),
        guard!("CreateEdge", Write),
        guard!("CreateVector", Write),
        guard!("CreateDocument", Write),
        guard!("CreateKv", Write),
        guard!("KvWatch", Read),
        guard!("BulkCreateRows", Write),
        guard!("BulkInsertBinary", Write),
        guard!("BatchInsert", Write),
        guard!("BulkCreateNodes", Write),
        guard!("BulkCreateEdges", Write),
        guard!("BulkCreateVectors", Write),
        guard!("BulkCreateDocuments", Write),
        guard!("Ask", Read),
        guard!("AskStream", Read),
        guard!("Embeddings", Write),
        guard!("AiPrompt", Write),
        guard!("AiCredentials", Write),
        guard!("PatchEntity", Write),
        guard!("CreateSnapshot", Write),
        guard!("CreateExport", Write),
        guard!("ApplyRetention", Write),
        guard!("DeleteEntity", Write),
        guard!("Checkpoint", Write),
        guard!("Topology", NoGuard),
        guard!("ReplicationStatus", Read),
        guard!("PullWalRecords", ReplicationStream),
        guard!("ReplicationSnapshot", ReplicationStream),
        guard!("SubmitAskSideEffects", Read),
        guard!("AckReplicaLsn", ReplicationAck),
        guard!("CreateCollection", Write),
        guard!("DropCollection", Admin),
        guard!("DescribeCollection", Read),
        guard!("AuthBootstrap", NoGuard),
        guard!("AuthLogin", NoGuard),
        guard!("AuthCreateUser", Admin),
        guard!("AuthDeleteUser", Admin),
        guard!("AuthListUsers", Admin),
        guard!("AuthCreateApiKey", Admin),
        guard!("AuthRevokeApiKey", Admin),
        guard!("AuthChangePassword", NoGuard),
        guard!("AuthWhoAmI", NoGuard),
    ];

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Principal {
        Anonymous,
        Read,
        Write,
        Admin,
    }

    const PRINCIPALS: &[Principal] = &[
        Principal::Anonymous,
        Principal::Read,
        Principal::Write,
        Principal::Admin,
    ];

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Outcome {
        Allow,
        Unauthenticated,
        PermissionDenied,
        Other,
    }

    /// The decision the pre-catalog guard produced for `principal`, on a
    /// deployment with auth enabled and `require_auth` on and no IAM policies
    /// attached (the legacy-RBAC posture).
    fn expected_outcome(guard: PrePrGuard, principal: Principal) -> Outcome {
        match (guard, principal) {
            (PrePrGuard::NoGuard, _) => Outcome::Allow,
            // `check_permission` mapped every read/write denial to
            // UNAUTHENTICATED, including an authenticated-but-insufficient
            // role.
            (PrePrGuard::Read, Principal::Anonymous) => Outcome::Unauthenticated,
            (PrePrGuard::Read, _) => Outcome::Allow,
            (PrePrGuard::Write, Principal::Anonymous | Principal::Read) => Outcome::Unauthenticated,
            (PrePrGuard::Write, _) => Outcome::Allow,
            // `authorize_admin` mapped every denial to PERMISSION_DENIED.
            (PrePrGuard::Admin, Principal::Admin) => Outcome::Allow,
            (PrePrGuard::Admin, _) => Outcome::PermissionDenied,
            // Replication capabilities are policy-only: with no policy
            // attached every authenticated role is denied, and there is no
            // legacy-RBAC fallback that a read key could ride.
            (PrePrGuard::ReplicationStream | PrePrGuard::ReplicationAck, Principal::Anonymous) => {
                Outcome::Unauthenticated
            }
            (PrePrGuard::ReplicationStream | PrePrGuard::ReplicationAck, _) => {
                Outcome::PermissionDenied
            }
        }
    }

    struct Harness {
        grpc: GrpcRuntime,
        read_key: String,
        write_key: String,
        admin_key: String,
    }

    impl Harness {
        fn new() -> Self {
            let runtime =
                RedDBRuntime::with_options(crate::api::RedDBOptions::in_memory()).expect("runtime");
            let auth_store = Arc::new(AuthStore::new(crate::auth::AuthConfig {
                enabled: true,
                require_auth: true,
                ..crate::auth::AuthConfig::default()
            }));
            auth_store
                .create_user("reader", "p", Role::Read)
                .expect("reader");
            auth_store
                .create_user("writer", "p", Role::Write)
                .expect("writer");
            auth_store
                .create_user("root", "p", Role::Admin)
                .expect("root");
            let read_key = auth_store
                .create_api_key("reader", "read", Role::Read)
                .expect("read key")
                .key;
            let write_key = auth_store
                .create_api_key("writer", "write", Role::Write)
                .expect("write key")
                .key;
            let admin_key = auth_store
                .create_api_key("root", "admin", Role::Admin)
                .expect("admin key")
                .key;
            Self {
                grpc: GrpcRuntime {
                    runtime,
                    auth_store,
                    prepared_registry: Arc::new(PreparedRegistry::new()),
                    oauth_validator: None,
                },
                read_key,
                write_key,
                admin_key,
            }
        }

        fn metadata(&self, principal: Principal) -> MetadataMap {
            let mut metadata = MetadataMap::new();
            let key = match principal {
                Principal::Anonymous => return metadata,
                Principal::Read => &self.read_key,
                Principal::Write => &self.write_key,
                Principal::Admin => &self.admin_key,
            };
            metadata.insert(
                "authorization",
                format!("Bearer {key}").parse().expect("metadata value"),
            );
            metadata
        }

        fn decide(&self, binding: &GrpcCommandBinding, principal: Principal) -> Outcome {
            outcome(authorize_binding(
                &self.grpc,
                binding,
                &self.metadata(principal),
                None,
            ))
        }
    }

    fn outcome(result: Result<GrpcDispatchContext, Status>) -> Outcome {
        match result {
            Ok(_) => Outcome::Allow,
            Err(status) => match status.code() {
                tonic::Code::Unauthenticated => Outcome::Unauthenticated,
                tonic::Code::PermissionDenied => Outcome::PermissionDenied,
                _ => Outcome::Other,
            },
        }
    }

    fn binding(rpc: &str) -> &'static GrpcCommandBinding {
        GRPC_COMMAND_BINDINGS
            .iter()
            .find(|binding| binding.rpc == rpc)
            .unwrap_or_else(|| panic!("no binding for {rpc}"))
    }

    fn install_replication_policy(store: &AuthStore, username: &str, id: &str, action: &str) {
        let policy_json = format!(
            r#"{{"id":"{id}","version":1,"statements":[{{"effect":"allow","actions":["{action}"],"resources":["cluster:replication"]}}]}}"#
        );
        store
            .put_policy(crate::auth::policies::Policy::from_json_str(&policy_json).expect("policy"))
            .expect("put policy");
        store
            .attach_policy(
                crate::auth::store::PrincipalRef::User(crate::auth::UserId::platform(username)),
                id,
            )
            .expect("attach policy");
    }

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
    fn pre_pr_guard_table_covers_every_binding() {
        let bound: Vec<&str> = GRPC_COMMAND_BINDINGS
            .iter()
            .map(|binding| binding.rpc)
            .collect();
        let recorded: Vec<&str> = PRE_PR_GRPC_GUARDS.iter().map(|(rpc, _)| *rpc).collect();
        assert_eq!(recorded, bound);
    }

    /// Issue #2146: dispatch authorization must reach the same decision the
    /// hand-written guards did, for every binding and every principal class.
    #[test]
    fn grpc_dispatch_matches_pre_pr_auth_matrix() {
        let harness = Harness::new();
        let mut checked = 0usize;
        let mut mismatches = Vec::new();

        for (rpc, guard) in PRE_PR_GRPC_GUARDS {
            let binding = binding(rpc);
            for principal in PRINCIPALS {
                let expected = expected_outcome(*guard, *principal);
                let actual = harness.decide(binding, *principal);
                checked += 1;
                if actual != expected {
                    mismatches.push(format!(
                        "{rpc} ({guard:?}) as {principal:?}: expected {expected:?}, got {actual:?}"
                    ));
                }
            }
        }

        assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
        assert_eq!(checked, GRPC_COMMAND_BINDINGS.len() * PRINCIPALS.len());
    }

    /// Issue #820: the stream and ack capabilities stay separate, and neither
    /// falls through to the legacy RBAC posture.
    #[test]
    fn replication_capabilities_are_policy_only_and_split() {
        let harness = Harness::new();
        install_replication_policy(
            &harness.grpc.auth_store,
            "reader",
            "p_stream_only",
            REPLICATION_STREAM_ACTION,
        );
        install_replication_policy(
            &harness.grpc.auth_store,
            "writer",
            "p_ack_only",
            REPLICATION_ACK_ACTION,
        );

        for rpc in ["PullWalRecords", "ReplicationSnapshot"] {
            assert_eq!(
                harness.decide(binding(rpc), Principal::Read),
                Outcome::Allow,
                "{rpc} with the stream capability"
            );
            assert_eq!(
                harness.decide(binding(rpc), Principal::Write),
                Outcome::PermissionDenied,
                "{rpc} with only the ack capability"
            );
        }
        assert_eq!(
            harness.decide(binding("AckReplicaLsn"), Principal::Write),
            Outcome::Allow,
        );
        assert_eq!(
            harness.decide(binding("AckReplicaLsn"), Principal::Read),
            Outcome::PermissionDenied,
        );
    }

    /// Only the replication bindings accept an mTLS peer certificate as the
    /// principal, so a cert-bearing client without an API key gains nothing
    /// on the other 133 RPCs.
    #[test]
    fn mtls_peer_identity_is_scoped_to_replication_bindings() {
        let peer_bound: Vec<&str> = GRPC_COMMAND_BINDINGS
            .iter()
            .filter(|binding| matches!(binding.auth, GrpcAuthClass::Replication(_)))
            .map(|binding| binding.rpc)
            .collect();
        assert_eq!(
            peer_bound,
            vec!["PullWalRecords", "ReplicationSnapshot", "AckReplicaLsn"]
        );

        let harness = Harness::new();
        let peer_auth = AuthResult::Authenticated {
            username: "node-a".into(),
            role: Role::Read,
            source: AuthSource::ClientCert,
        };
        assert_eq!(
            outcome(authorize_binding(
                &harness.grpc,
                binding("Scan"),
                &MetadataMap::new(),
                Some(peer_auth),
            )),
            Outcome::Unauthenticated,
        );
    }
}
