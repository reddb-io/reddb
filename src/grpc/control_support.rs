use super::*;

impl GrpcRuntime {
    /// Resolve the auth result for an incoming request.
    ///
    /// Checks (in order):
    /// 1. Session/API-key tokens via the `AuthStore`.
    /// 2. Anonymous (when auth is not required).
    /// 3. Denied.
    pub(crate) fn resolve_auth(&self, metadata: &MetadataMap) -> AuthResult {
        let token = grpc_token(metadata);

        // 1. Try AuthStore (session tokens / API keys) when auth is enabled.
        if self.auth_store.is_enabled() {
            if let Some(token) = token {
                if let Some((username, role)) = self.auth_store.validate_token(token) {
                    return AuthResult::Authenticated { username, role };
                }
                // Token was provided but invalid -- if require_auth is on, deny.
                if self.auth_store.config().require_auth {
                    return AuthResult::Denied("invalid or expired token".into());
                }
            } else if self.auth_store.config().require_auth {
                return AuthResult::Denied("authentication required".into());
            }
        }

        // 2. No token or auth not enabled -> anonymous.
        AuthResult::Anonymous
    }

    pub(crate) fn authorize_read(&self, metadata: &MetadataMap) -> Result<(), Status> {
        self.authorize(metadata, false)
    }

    pub(crate) fn authorize_write(&self, metadata: &MetadataMap) -> Result<(), Status> {
        self.authorize(metadata, true)
    }

    pub(crate) fn authorize(&self, metadata: &MetadataMap, is_write: bool) -> Result<(), Status> {
        let auth = self.resolve_auth(metadata);
        check_permission(&auth, is_write, false).map_err(|msg| Status::unauthenticated(msg))
    }

    pub(crate) fn authorize_admin(&self, metadata: &MetadataMap) -> Result<(), Status> {
        let auth = self.resolve_auth(metadata);
        check_permission(&auth, false, true).map_err(|msg| Status::permission_denied(msg))
    }

    pub(crate) fn start_graph_analytics_job(
        &self,
        kind: impl Into<String>,
        projection: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> Result<(), Status> {
        let kind = kind.into();
        self.admin_use_cases()
            .queue_analytics_job(kind.clone(), projection.clone(), metadata.clone())
            .map_err(to_status)?;
        self.admin_use_cases()
            .start_analytics_job(kind, projection, metadata)
            .map(|_| ())
            .map_err(to_status)
    }

    pub(crate) fn complete_graph_analytics_job(
        &self,
        kind: impl Into<String>,
        projection: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> Result<(), Status> {
        self.admin_use_cases()
            .complete_analytics_job(kind, projection, metadata)
            .map(|_| ())
            .map_err(to_status)
    }

    pub(crate) fn fail_graph_analytics_job(
        &self,
        kind: impl Into<String>,
        projection: Option<String>,
        metadata: BTreeMap<String, String>,
    ) -> Result<(), Status> {
        self.admin_use_cases()
            .fail_analytics_job(kind, projection, metadata)
            .map(|_| ())
            .map_err(to_status)
    }
}

pub(crate) fn to_status(err: crate::api::RedDBError) -> Status {
    Status::internal(err.to_string())
}

pub(crate) fn grpc_token<'a>(metadata: &'a MetadataMap) -> Option<&'a str> {
    if let Some(value) = metadata.get("authorization") {
        let value = value.to_str().ok()?;
        if let Some(token) = value.strip_prefix("Bearer ") {
            return Some(token);
        }
    }

    metadata.get("x-reddb-token")?.to_str().ok()
}

pub(crate) fn none_if_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

pub(crate) fn json_payload_reply(value: JsonValue) -> PayloadReply {
    PayloadReply {
        ok: true,
        payload: json_to_string(&value).unwrap_or_else(|_| "{}".to_string()),
    }
}

pub(crate) fn parse_json_payload_allow_empty(payload_json: &str) -> Result<JsonValue, Status> {
    if payload_json.trim().is_empty() {
        return Ok(JsonValue::Object(Map::new()));
    }
    parse_json_payload(payload_json)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrpcServerlessWarmupScope {
    Indexes,
    GraphProjections,
    AnalyticsJobs,
    NativeArtifacts,
}

#[derive(Debug, Default)]
pub(crate) struct GrpcServerlessAnalyticsWarmupTarget {
    pub(crate) kind: String,
    pub(crate) projection: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct GrpcServerlessWarmupPlan {
    pub(crate) indexes: Vec<String>,
    pub(crate) graph_projections: Vec<String>,
    pub(crate) analytics_jobs: Vec<GrpcServerlessAnalyticsWarmupTarget>,
    pub(crate) includes_native_artifacts: bool,
}

pub(crate) fn grpc_parse_serverless_readiness_requirements(
    payload: &JsonValue,
) -> Result<Vec<String>, String> {
    crate::application::serverless_payload::parse_serverless_readiness_requirements(payload)
}

pub(crate) fn grpc_parse_serverless_reclaim_operations(
    payload: &JsonValue,
) -> Result<Vec<String>, String> {
    crate::application::serverless_payload::parse_serverless_reclaim_operations(payload)
}

pub(crate) fn grpc_parse_serverless_warmup_scopes(
    payload: &JsonValue,
) -> Result<Vec<GrpcServerlessWarmupScope>, String> {
    crate::application::serverless_payload::parse_serverless_warmup_scopes(payload).map(
        |scopes| {
            scopes
                .into_iter()
                .map(|scope| match scope {
                    crate::application::serverless_payload::ServerlessWarmupScopeToken::Indexes => {
                        GrpcServerlessWarmupScope::Indexes
                    }
                    crate::application::serverless_payload::ServerlessWarmupScopeToken::GraphProjections => {
                        GrpcServerlessWarmupScope::GraphProjections
                    }
                    crate::application::serverless_payload::ServerlessWarmupScopeToken::AnalyticsJobs => {
                        GrpcServerlessWarmupScope::AnalyticsJobs
                    }
                    crate::application::serverless_payload::ServerlessWarmupScopeToken::NativeArtifacts => {
                        GrpcServerlessWarmupScope::NativeArtifacts
                    }
                })
                .collect()
        },
    )
}

pub(crate) fn grpc_serverless_readiness_summary_to_json(
    query_ready: bool,
    write_ready: bool,
    repair_ready: bool,
    health: &crate::health::HealthReport,
    authority: &crate::storage::unified::devx::PhysicalAuthorityStatus,
) -> JsonValue {
    crate::presentation::serverless_json::serverless_readiness_summary_json(
        query_ready,
        write_ready,
        repair_ready,
        health,
        authority,
        crate::presentation::ops_json::health_json,
        crate::presentation::ops_json::physical_authority_status_json,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrpcDeploymentProfile {
    Embedded,
    Server,
    Serverless,
}

pub(crate) fn grpc_deployment_profile_from_token(value: &str) -> Option<GrpcDeploymentProfile> {
    crate::application::serverless_payload::deployment_profile_from_token(value).map(|profile| {
        match profile {
            crate::application::serverless_payload::DeploymentProfileToken::Embedded => {
                GrpcDeploymentProfile::Embedded
            }
            crate::application::serverless_payload::DeploymentProfileToken::Server => {
                GrpcDeploymentProfile::Server
            }
            crate::application::serverless_payload::DeploymentProfileToken::Serverless => {
                GrpcDeploymentProfile::Serverless
            }
        }
    })
}
