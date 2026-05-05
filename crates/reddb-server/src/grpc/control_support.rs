use super::*;

impl GrpcRuntime {
    /// Resolve the auth result for an incoming request.
    ///
    /// Checks (in order):
    /// 1. OAuth/OIDC JWT validation (when the bearer is JWT-shaped and an
    ///    `OAuthValidator` is configured). Hard-rejects malformed-or-bad-JWTs
    ///    when both OAuth + AuthStore.require_auth are on so attackers
    ///    can't downgrade to AuthStore.
    /// 2. Session/API-key tokens via the `AuthStore`.
    /// 3. Anonymous (when auth is not required).
    /// 4. Denied.
    pub(crate) fn resolve_auth(&self, metadata: &MetadataMap) -> AuthResult {
        let token = grpc_token(metadata);

        // 1. OAuth/OIDC: only attempt when (a) a validator is wired up,
        //    (b) a token is present, and (c) the token has 3-part JWT
        //    shape. Non-JWT-shaped bearers fall straight through to the
        //    AuthStore session/api-key path.
        if let Some(token_str) = token {
            let log_prefix = bearer_token_fingerprint_prefix(token_str);
            if is_jwt_shape(token_str) {
                if let Some(validator) = self.oauth_validator() {
                    match crate::wire::redwire::auth::validate_oauth_jwt(&validator, token_str) {
                        Ok((username, role)) => {
                            tracing::info!(
                                target: "reddb::security",
                                transport = "grpc",
                                token_sha256_prefix = %log_prefix,
                                username = %username,
                                role = %role.as_str(),
                                "gRPC OAuth JWT accepted"
                            );
                            // Use AuthSource::Oauth so audit/who-am-i emits the
                            // right tag (service_impl.rs:2886).
                            let identity = crate::auth::OAuthIdentity {
                                username,
                                tenant: None,
                                role,
                                issuer: validator.config().issuer.clone(),
                                subject: None,
                                expires_at_unix_secs: None,
                            };
                            return AuthResult::from_oauth(identity);
                        }
                        Err(reason) => {
                            // JWT-shaped + validator configured + failed
                            // validation = hard reject. Falling back to
                            // AuthStore would let an attacker forge a JWT
                            // and ride a session-id collision.
                            tracing::warn!(
                                target: "reddb::security",
                                transport = "grpc",
                                token_sha256_prefix = %log_prefix,
                                reason = %reason,
                                "gRPC OAuth JWT rejected"
                            );
                            return AuthResult::Denied(format!("oauth jwt: {reason}"));
                        }
                    }
                }
                // No validator configured but token IS JWT-shaped — fall
                // through to AuthStore. A deployment may carry both
                // session tokens that happen to be 3-segment AND no JWT
                // validator; we don't ban that combination.
            }
        }

        // 2. Try AuthStore (session tokens / API keys) when auth is enabled.
        if self.auth_store.is_enabled() {
            if let Some(token) = token {
                if let Some((username, role)) = self.auth_store.validate_token(token) {
                    return AuthResult::password(username, role);
                }
                // Token was provided but invalid -- if require_auth is on, deny.
                if self.auth_store.config().require_auth {
                    return AuthResult::Denied("invalid or expired token".into());
                }
            } else if self.auth_store.config().require_auth {
                return AuthResult::Denied("authentication required".into());
            }
        }

        // 3. No token or auth not enabled -> anonymous.
        AuthResult::Anonymous
    }

    /// Return the lazily-constructed gRPC OAuth validator, when one is
    /// configured on the embedded `AuthStore`. The validator is built
    /// once per `GrpcRuntime` (cloned across requests) and re-used.
    pub(crate) fn oauth_validator(&self) -> Option<std::sync::Arc<crate::auth::OAuthValidator>> {
        self.oauth_validator.clone()
    }

    pub(crate) fn authorize_read(&self, metadata: &MetadataMap) -> Result<(), Status> {
        self.authorize(metadata, false)
    }

    pub(crate) fn authorize_write(&self, metadata: &MetadataMap) -> Result<(), Status> {
        self.authorize(metadata, true)
    }

    pub(crate) fn authorize(&self, metadata: &MetadataMap, is_write: bool) -> Result<(), Status> {
        let auth = self.resolve_auth(metadata);
        check_permission(&auth, is_write, false).map_err(Status::unauthenticated)?;
        // PLAN.md W1: every gRPC mutation RPC funnels through
        // `authorize_write()`, so consulting the public-mutation gate
        // here covers Insert/Update/Delete, BulkInsert, DDL helpers,
        // and the serverless lifecycle endpoints in one place. Read
        // RPCs (`is_write = false`) skip the gate so a replica can
        // continue serving SELECTs.
        if is_write {
            self.runtime
                .check_write(crate::runtime::write_gate::WriteKind::Dml)
                .map_err(|err| Status::failed_precondition(err.to_string()))?;
        }
        Ok(())
    }

    pub(crate) fn authorize_admin(&self, metadata: &MetadataMap) -> Result<(), Status> {
        let auth = self.resolve_auth(metadata);
        check_permission(&auth, false, true).map_err(Status::permission_denied)
    }

    /// PLAN.md Phase 11.4 — call after a successful gRPC write to
    /// enforce the configured commit policy. When policy is `Local`
    /// (default) this returns immediately. When policy is
    /// `AckN(n)` and `RED_COMMIT_FAIL_ON_TIMEOUT=true`, a missed
    /// ack window is mapped to `Status::deadline_exceeded` so
    /// clients map it to a retry.
    ///
    /// Each create_* / update / delete RPC calls this right before
    /// building its `Response::new(reply)`. The post_lsn is the
    /// CDC current LSN at call time — which is the LSN of the
    /// just-completed write because the runtime advances it
    /// synchronously inside the storage path.
    pub(crate) fn enforce_commit_policy_after_write(&self) -> Result<(), Status> {
        let post_lsn = self.runtime.cdc_current_lsn();
        self.runtime
            .enforce_commit_policy(post_lsn)
            .map(|_| ())
            .map_err(|err| Status::deadline_exceeded(err.to_string()))
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

pub(crate) fn grpc_token(metadata: &MetadataMap) -> Option<&str> {
    if let Some(value) = metadata.get("authorization") {
        let value = value.to_str().ok()?;
        // Case-insensitive "Bearer " prefix per RFC 6750 §2.1.
        let prefix = "Bearer ";
        if value.len() > prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return Some(value[prefix.len()..].trim());
        }
    }

    metadata.get("x-reddb-token")?.to_str().ok()
}

/// Return true when `token` looks like a compact-serialized JWT
/// (header.payload.signature). The gRPC interceptor uses this as a
/// cheap classifier so non-JWT bearers (RedDB session tokens like
/// `rs_<hex32>`, API keys like `rk_<hex32>`) skip the JWT path.
pub(crate) fn is_jwt_shape(token: &str) -> bool {
    let mut segments = 0usize;
    for seg in token.split('.') {
        if seg.is_empty() {
            return false;
        }
        // base64url alphabet: a-z A-Z 0-9 - _ ; padding '=' optional.
        if !seg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'=')
        {
            return false;
        }
        segments += 1;
        if segments > 3 {
            return false;
        }
    }
    segments == 3
}

/// Compute the first 8 hex chars of `sha256(token)` so audit logs
/// can correlate failed/successful auth events without leaking the
/// token itself.
pub(crate) fn bearer_token_fingerprint_prefix(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    let digest = h.finalize();
    // 4 bytes = 8 hex chars; cheap to eyeball, useless for replay.
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        digest[0], digest[1], digest[2], digest[3]
    )
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
