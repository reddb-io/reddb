//! Capability discovery HTTP endpoints.

use super::*;

impl RedDBServer {
    /// `GET /capabilities` — Red UI feature & capability discovery (#752),
    /// static / system level.
    ///
    /// One documented contract so the UI can adapt to enabled models,
    /// API contracts, auth mode, replication mode, vector/SIMD support,
    /// AI provider availability, and preview feature flags without a
    /// cascade of probes against a dozen status endpoints.
    ///
    /// This view is principal-independent — it answers "what can this
    /// server, as deployed, do?". The complementary effective view
    /// ("what can *I* do here?") is `GET /auth/capabilities`. Per the
    /// #752 decision, build support and authorization are kept apart so
    /// they never collapse into one ambiguous flag.
    pub(crate) fn handle_capabilities(&self) -> HttpResponse {
        use crate::api::Capability;
        use crate::presentation::capabilities_json::{
            system_capabilities_json, CapabilityState, SystemCapabilitiesInputs,
        };

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        // Build feature gates — supported (compiled-in + enabled) vs
        // disabled. Reads through the same `has_capability` gate the
        // engine consults.
        let db = self.runtime.db();
        let opts = db.options();
        let build = [
            Capability::Table,
            Capability::Graph,
            Capability::Vector,
            Capability::FullText,
            Capability::Security,
            Capability::Encryption,
        ]
        .into_iter()
        .map(|c| (c.as_str().to_string(), opts.has_capability(c)))
        .collect();

        // Vector / SIMD. The probe always returns a level on supported
        // platforms; `scalar` is a real (no-acceleration) level, not an
        // absence of measurement, so it is still `supported`.
        let simd_level = Some(
            match crate::storage::engine::simd_distance::simd_level() {
                crate::storage::engine::simd_distance::SimdLevel::Scalar => "scalar",
                crate::storage::engine::simd_distance::SimdLevel::Sse => "sse",
                crate::storage::engine::simd_distance::SimdLevel::Avx => "avx",
                crate::storage::engine::simd_distance::SimdLevel::AvxFma => "avx_fma",
            }
            .to_string(),
        );

        // AI providers this build can talk to + locally registered
        // models.
        let ai_providers = [
            crate::ai::AiProvider::OpenAi,
            crate::ai::AiProvider::Anthropic,
            crate::ai::AiProvider::Groq,
            crate::ai::AiProvider::OpenRouter,
            crate::ai::AiProvider::Together,
            crate::ai::AiProvider::Venice,
            crate::ai::AiProvider::Ollama,
            crate::ai::AiProvider::DeepSeek,
            crate::ai::AiProvider::HuggingFace,
            crate::ai::AiProvider::Local,
        ]
        .iter()
        .map(|p| p.token().to_string())
        .collect();
        let mut ai_model_names: Vec<String> = self
            .collect_ai_model_entries()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        ai_model_names.sort();

        // Auth mechanisms that are configured on this listener. This is
        // *which methods exist*, not *what any caller may do*.
        let auth_enabled = self
            .auth_store
            .as_ref()
            .map(|s| s.is_enabled())
            .unwrap_or(false);
        let on = |b: bool| {
            if b {
                CapabilityState::Supported
            } else {
                CapabilityState::Disabled
            }
        };
        let oauth_configured = self.runtime.oauth_validator().is_some()
            || self
                .auth_store
                .as_ref()
                .map(|s| s.config().oauth.enabled)
                .unwrap_or(false);
        let mtls_configured = self
            .auth_store
            .as_ref()
            .map(|s| s.config().cert.enabled)
            .unwrap_or(false);
        let admin_token_configured = super::routing::read_admin_token().is_some();
        let auth_methods = vec![
            ("password".to_string(), on(auth_enabled)),
            ("bearer".to_string(), on(auth_enabled)),
            ("oauth_jwt".to_string(), on(oauth_configured)),
            ("mtls".to_string(), on(mtls_configured)),
            ("admin_token".to_string(), on(admin_token_configured)),
        ];

        // Replication mode.
        let replication_role = match self.runtime.write_gate().role() {
            crate::replication::ReplicationRole::Standalone => "standalone",
            crate::replication::ReplicationRole::Primary => "primary",
            crate::replication::ReplicationRole::Replica { .. } => "replica",
        }
        .to_string();
        let commit_policy = self.runtime.commit_policy().label().to_string();
        let read_only = self.runtime.write_gate().is_read_only();

        // API contracts / transports — active listeners are supported;
        // listeners that failed to bind are unavailable with a reason.
        let transports_active = self
            .options
            .transport_readiness
            .active
            .iter()
            .map(|l| l.transport.clone())
            .collect();
        let transports_failed = self
            .options
            .transport_readiness
            .failed
            .iter()
            .map(|l| (l.transport.clone(), l.reason.clone()))
            .collect();

        // Preview feature flags. This build ships no preview-gated
        // features today; the section is present and empty so renderers
        // know the contract carries it. The `preview` state is exercised
        // by the presentation-layer tests.
        let preview_features = Vec::new();

        let inputs = SystemCapabilitiesInputs {
            snapshot_at_unix_ms: now_ms,
            version: env!("CARGO_PKG_VERSION").to_string(),
            build,
            simd_level,
            ai_model_names,
            ai_providers,
            auth_enabled,
            auth_methods,
            replication_role,
            commit_policy,
            read_only,
            transports_active,
            transports_failed,
            preview_features,
        };

        json_response(200, system_capabilities_json(&inputs))
    }

    /// `GET /auth/capabilities` — Red UI effective-capability discovery
    /// (#752), principal level.
    ///
    /// Resolves the calling principal/tenant from the request and
    /// reports what *they* can actually do here: read / write / admin,
    /// each gated by both their role and the node's write gate. This is
    /// deliberately separate from `GET /capabilities` so build support
    /// and authorization never collapse into one flag.
    pub(crate) fn handle_auth_capabilities(
        &self,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> HttpResponse {
        use crate::presentation::capabilities_json::{
            principal_capabilities_json, PrincipalCapabilitiesInputs,
        };

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let auth_enabled = self
            .auth_store
            .as_ref()
            .map(|s| s.is_enabled())
            .unwrap_or(false);
        let read_only = self.runtime.write_gate().is_read_only();

        let bearer = headers
            .get("authorization")
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .filter(|t| !t.is_empty());

        let mut authenticated = false;
        let mut tenant: Option<String> = None;
        let mut role: Option<crate::auth::Role> = None;
        let mut principal = super::routing::principal_for(headers);

        if auth_enabled {
            if let (Some(token), Some(store)) = (bearer, self.auth_store.as_ref()) {
                // Tenant-aware AuthStore identity first (API keys /
                // session tokens), then fall back to the OAuth/JWT path
                // which yields a role but no tenant.
                if let Some((uid, r)) = store.validate_token_full(token) {
                    authenticated = true;
                    tenant = uid.tenant.clone();
                    role = Some(r);
                    principal = uid.to_string();
                } else if let super::routing::BearerOutcome::Valid(r) =
                    super::routing::resolve_bearer_role(token, &self.runtime, store)
                {
                    authenticated = true;
                    role = Some(r);
                }
            }
        }

        // Effective permissions.
        let (can_read, can_write, can_admin) = if !auth_enabled {
            // Auth disabled -> the server bypasses authorization entirely.
            (true, true, true)
        } else if let Some(r) = role {
            (r.can_read(), r.can_write(), r.can_admin())
        } else {
            // Auth enabled, caller unauthenticated. Reads are open only
            // when the deployment does not require auth for reads.
            let require_auth = self
                .auth_store
                .as_ref()
                .map(|s| s.config().require_auth)
                .unwrap_or(true);
            (!require_auth, false, false)
        };

        let inputs = PrincipalCapabilitiesInputs {
            snapshot_at_unix_ms: now_ms,
            auth_enabled,
            authenticated,
            principal,
            tenant,
            role: role.map(|r| r.as_str().to_string()),
            read_only,
            can_read,
            can_write,
            can_admin,
        };

        json_response(200, principal_capabilities_json(&inputs))
    }
}
