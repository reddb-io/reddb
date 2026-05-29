//! Red UI feature & capability discovery (#752).
//!
//! Exposes what this RedDB build and deployment can do behind one
//! documented contract so Red UI adapts to enabled models, auth mode,
//! vector/SIMD support, replication mode, AI provider availability, the
//! active API contracts, and preview feature flags — without a cascade
//! of runtime probes against a dozen status endpoints.
//!
//! Per the #752 thread-discussion decision the contract is split across
//! **two levels** and these builders mirror that split:
//!
//! * [`system_capabilities_json`] — the static / system view served at
//!   `GET /capabilities`: build feature gates, deployment shape,
//!   transports, auth *methods that are configured*, replication mode,
//!   vector/SIMD, AI providers/models. It answers "what can this
//!   server, as deployed, do?" and is principal-independent.
//! * [`principal_capabilities_json`] — the effective / authenticated
//!   view served at `GET /auth/capabilities`: what the *current*
//!   principal/tenant can actually use given their role and the write
//!   gate. It answers "what can *I* do here?".
//!
//! Keeping the two apart is deliberate: build support and authorization
//! must never collapse into one ambiguous flag (a feature can be
//! compiled-in and serving yet forbidden to this caller, or permitted
//! yet unavailable on this node).
//!
//! ## State taxonomy
//!
//! Every capability carries a [`CapabilityState`] so renderers can
//! distinguish four genuinely different conditions instead of a single
//! boolean:
//!
//! * `supported`   — present in this build and active.
//! * `disabled`    — compiled-in but switched off by config.
//! * `unavailable` — could not be brought up on this node (e.g. a
//!   transport listener whose bind failed); carries a `reason`.
//! * `preview`     — present but gated as experimental / not yet
//!   covered by stability guarantees.
//!
//! The builders are pure functions over their `*Inputs` structs so the
//! handler wiring stays trivial and contract tests can pin the shape
//! against synthetic fixtures.

use crate::json::{Map, Value as JsonValue};

/// Discovery contract revision. Bumped when the *shape* changes in a
/// way clients must branch on; additive fields do not bump it.
pub(crate) const DISCOVERY_VERSION: u64 = 1;

/// The four conditions a capability can be in. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityState {
    Supported,
    Disabled,
    Unavailable,
    Preview,
}

impl CapabilityState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Supported => "supported",
            Self::Disabled => "disabled",
            Self::Unavailable => "unavailable",
            Self::Preview => "preview",
        }
    }

    /// Map a plain enabled/disabled boolean onto the two states a
    /// build-time feature gate can be in.
    pub(crate) fn from_enabled(enabled: bool) -> Self {
        if enabled {
            Self::Supported
        } else {
            Self::Disabled
        }
    }
}

/// `{ "state": "<state>" }`, plus an optional `"reason"` token when the
/// state needs explaining (today only `unavailable` carries one).
fn state_json(state: CapabilityState, reason: Option<&str>) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "state".to_string(),
        JsonValue::String(state.as_str().to_string()),
    );
    if let Some(reason) = reason {
        object.insert("reason".to_string(), JsonValue::String(reason.to_string()));
    }
    JsonValue::Object(object)
}

// ---------------------------------------------------------------------------
// System / static capabilities — GET /capabilities
// ---------------------------------------------------------------------------

/// Everything `GET /capabilities` reports. Pure data; the handler fills
/// it from runtime/options accessors.
#[derive(Debug, Clone)]
pub(crate) struct SystemCapabilitiesInputs {
    pub(crate) snapshot_at_unix_ms: u64,
    pub(crate) version: String,
    /// Build feature gates as `(capability_name, enabled)` — e.g.
    /// `("table", true)`. Order is preserved in the output object.
    pub(crate) build: Vec<(String, bool)>,
    /// Detected SIMD level label (`"avx_fma"`, `"avx"`, `"sse"`,
    /// `"scalar"`). `None` means the platform exposes no probe at all,
    /// which surfaces as `unavailable`.
    pub(crate) simd_level: Option<String>,
    /// Names of locally-registered AI models.
    pub(crate) ai_model_names: Vec<String>,
    /// AI provider integrations this build can talk to.
    pub(crate) ai_providers: Vec<String>,
    /// Master auth switch.
    pub(crate) auth_enabled: bool,
    /// `(method_name, state)` for each auth mechanism. `disabled` when
    /// not configured, `supported` when configured and active.
    pub(crate) auth_methods: Vec<(String, CapabilityState)>,
    pub(crate) replication_role: String,
    pub(crate) commit_policy: String,
    pub(crate) read_only: bool,
    /// Transports whose listener is up — the active API contracts.
    pub(crate) transports_active: Vec<String>,
    /// `(transport, reason)` for listeners that failed to bind.
    pub(crate) transports_failed: Vec<(String, String)>,
    /// Preview-gated features as `(name, state)`. Empty when the build
    /// ships no preview features.
    pub(crate) preview_features: Vec<(String, CapabilityState)>,
}

/// Build the `GET /capabilities` payload.
pub(crate) fn system_capabilities_json(inputs: &SystemCapabilitiesInputs) -> JsonValue {
    let mut object = Map::new();

    object.insert(
        "snapshot_at_unix_ms".to_string(),
        JsonValue::Number(inputs.snapshot_at_unix_ms as f64),
    );
    object.insert(
        "version".to_string(),
        JsonValue::String(inputs.version.clone()),
    );
    object.insert(
        "discovery_version".to_string(),
        JsonValue::Number(DISCOVERY_VERSION as f64),
    );

    // Build feature gates.
    let mut build = Map::new();
    for (name, enabled) in &inputs.build {
        build.insert(
            name.clone(),
            state_json(CapabilityState::from_enabled(*enabled), None),
        );
    }
    object.insert("build".to_string(), JsonValue::Object(build));

    // Vector / SIMD.
    let mut vector = Map::new();
    let simd = match &inputs.simd_level {
        Some(level) => {
            let mut o = Map::new();
            o.insert(
                "state".to_string(),
                JsonValue::String(CapabilityState::Supported.as_str().to_string()),
            );
            o.insert("level".to_string(), JsonValue::String(level.clone()));
            JsonValue::Object(o)
        }
        None => state_json(CapabilityState::Unavailable, Some("simd_probe_unavailable")),
    };
    vector.insert("simd".to_string(), simd);
    object.insert("vector".to_string(), JsonValue::Object(vector));

    // AI providers + registered models.
    let mut ai = Map::new();
    let mut models = Map::new();
    models.insert(
        "count".to_string(),
        JsonValue::Number(inputs.ai_model_names.len() as f64),
    );
    models.insert(
        "names".to_string(),
        JsonValue::Array(
            inputs
                .ai_model_names
                .iter()
                .map(|n| JsonValue::String(n.clone()))
                .collect(),
        ),
    );
    ai.insert("models".to_string(), JsonValue::Object(models));
    let mut providers = Map::new();
    for provider in &inputs.ai_providers {
        providers.insert(
            provider.clone(),
            state_json(CapabilityState::Supported, None),
        );
    }
    ai.insert("providers".to_string(), JsonValue::Object(providers));
    object.insert("ai".to_string(), JsonValue::Object(ai));

    // Auth methods (configured mechanisms; NOT what any principal may
    // do — that lives in the principal contract).
    let mut auth = Map::new();
    auth.insert("enabled".to_string(), JsonValue::Bool(inputs.auth_enabled));
    let mut methods = Map::new();
    for (name, state) in &inputs.auth_methods {
        methods.insert(name.clone(), state_json(*state, None));
    }
    auth.insert("methods".to_string(), JsonValue::Object(methods));
    object.insert("auth".to_string(), JsonValue::Object(auth));

    // Replication mode.
    let mut replication = Map::new();
    replication.insert(
        "role".to_string(),
        JsonValue::String(inputs.replication_role.clone()),
    );
    replication.insert(
        "commit_policy".to_string(),
        JsonValue::String(inputs.commit_policy.clone()),
    );
    replication.insert("read_only".to_string(), JsonValue::Bool(inputs.read_only));
    object.insert("replication".to_string(), JsonValue::Object(replication));

    // API contracts / transports.
    let mut contracts = Map::new();
    for transport in &inputs.transports_active {
        contracts.insert(
            transport.clone(),
            state_json(CapabilityState::Supported, None),
        );
    }
    for (transport, reason) in &inputs.transports_failed {
        // A failed listener wins over a duplicate-named active entry so
        // a partially-up transport is reported honestly.
        contracts.insert(
            transport.clone(),
            state_json(CapabilityState::Unavailable, Some(reason)),
        );
    }
    object.insert("api_contracts".to_string(), JsonValue::Object(contracts));

    // Preview feature flags.
    let mut preview = Map::new();
    for (name, state) in &inputs.preview_features {
        preview.insert(name.clone(), state_json(*state, None));
    }
    object.insert("preview_features".to_string(), JsonValue::Object(preview));

    JsonValue::Object(object)
}

// ---------------------------------------------------------------------------
// Principal / effective capabilities — GET /auth/capabilities
// ---------------------------------------------------------------------------

/// Everything `GET /auth/capabilities` reports for the calling
/// principal.
#[derive(Debug, Clone)]
pub(crate) struct PrincipalCapabilitiesInputs {
    pub(crate) snapshot_at_unix_ms: u64,
    /// Whether the server has auth enabled at all.
    pub(crate) auth_enabled: bool,
    /// Whether the caller presented credentials that resolved to a
    /// known identity.
    pub(crate) authenticated: bool,
    /// A non-sensitive principal label (`"anon"`, `"tenant/user"`, a
    /// bearer fingerprint, ...).
    pub(crate) principal: String,
    pub(crate) tenant: Option<String>,
    /// `"read"` / `"write"` / `"admin"`, or `None` when unauthenticated.
    pub(crate) role: Option<String>,
    /// Whether the node is currently read-only (gates effective write).
    pub(crate) read_only: bool,
    pub(crate) can_read: bool,
    pub(crate) can_write: bool,
    pub(crate) can_admin: bool,
}

/// Build the `GET /auth/capabilities` payload.
pub(crate) fn principal_capabilities_json(inputs: &PrincipalCapabilitiesInputs) -> JsonValue {
    let mut object = Map::new();

    object.insert(
        "snapshot_at_unix_ms".to_string(),
        JsonValue::Number(inputs.snapshot_at_unix_ms as f64),
    );
    object.insert(
        "auth_enabled".to_string(),
        JsonValue::Bool(inputs.auth_enabled),
    );
    object.insert(
        "authenticated".to_string(),
        JsonValue::Bool(inputs.authenticated),
    );
    object.insert(
        "principal".to_string(),
        JsonValue::String(inputs.principal.clone()),
    );
    object.insert(
        "tenant".to_string(),
        match &inputs.tenant {
            Some(t) => JsonValue::String(t.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "role".to_string(),
        match &inputs.role {
            Some(r) => JsonValue::String(r.clone()),
            None => JsonValue::Null,
        },
    );

    let mut effective = Map::new();
    effective.insert(
        "read".to_string(),
        state_json(
            if inputs.can_read {
                CapabilityState::Supported
            } else {
                CapabilityState::Disabled
            },
            None,
        ),
    );
    // Write is disabled (with a reason) when the role permits it but the
    // node is read-only — the principal *could* write elsewhere, just
    // not here right now. That distinction matters to the UI.
    let (write_state, write_reason) = if !inputs.can_write {
        (CapabilityState::Disabled, None)
    } else if inputs.read_only {
        (CapabilityState::Unavailable, Some("node_read_only"))
    } else {
        (CapabilityState::Supported, None)
    };
    effective.insert("write".to_string(), state_json(write_state, write_reason));
    effective.insert(
        "admin".to_string(),
        state_json(
            if inputs.can_admin {
                CapabilityState::Supported
            } else {
                CapabilityState::Disabled
            },
            None,
        ),
    );
    object.insert("effective".to_string(), JsonValue::Object(effective));

    JsonValue::Object(object)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: &JsonValue) -> &Map<String, JsonValue> {
        v.as_object().expect("expected JSON object")
    }
    fn s<'a>(v: &'a JsonValue, key: &str) -> &'a str {
        obj(v)
            .get(key)
            .and_then(JsonValue::as_str)
            .unwrap_or_else(|| {
                panic!("expected string at `{key}`");
            })
    }
    fn state_of<'a>(section: &'a JsonValue, key: &str) -> &'a str {
        s(obj(section).get(key).expect("missing capability"), "state")
    }

    fn baseline_inputs() -> SystemCapabilitiesInputs {
        SystemCapabilitiesInputs {
            snapshot_at_unix_ms: 1_700_000_000_000,
            version: "9.9.9".to_string(),
            build: vec![
                ("table".to_string(), true),
                ("graph".to_string(), true),
                ("vector".to_string(), true),
                ("security".to_string(), false),
            ],
            simd_level: Some("avx_fma".to_string()),
            ai_model_names: vec![],
            ai_providers: vec!["openai".to_string(), "anthropic".to_string()],
            auth_enabled: false,
            auth_methods: vec![
                ("bearer".to_string(), CapabilityState::Disabled),
                ("oauth_jwt".to_string(), CapabilityState::Disabled),
            ],
            replication_role: "standalone".to_string(),
            commit_policy: "local".to_string(),
            read_only: false,
            transports_active: vec!["http".to_string()],
            transports_failed: vec![],
            preview_features: vec![],
        }
    }

    #[test]
    fn capability_state_strings_are_the_four_documented_tokens() {
        assert_eq!(CapabilityState::Supported.as_str(), "supported");
        assert_eq!(CapabilityState::Disabled.as_str(), "disabled");
        assert_eq!(CapabilityState::Unavailable.as_str(), "unavailable");
        assert_eq!(CapabilityState::Preview.as_str(), "preview");
        // All four must be distinct so the UI can branch on them.
        let all = [
            CapabilityState::Supported.as_str(),
            CapabilityState::Disabled.as_str(),
            CapabilityState::Unavailable.as_str(),
            CapabilityState::Preview.as_str(),
        ];
        let unique: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(unique.len(), 4, "states must be distinct tokens");
    }

    #[test]
    fn system_capabilities_baseline_standalone_shape() {
        let json = system_capabilities_json(&baseline_inputs());

        assert_eq!(s(&json, "version"), "9.9.9");
        assert_eq!(
            obj(&json)
                .get("discovery_version")
                .and_then(JsonValue::as_u64),
            Some(DISCOVERY_VERSION)
        );

        // Build gates distinguish supported vs disabled.
        let build = obj(&json).get("build").expect("build");
        assert_eq!(state_of(build, "table"), "supported");
        assert_eq!(state_of(build, "graph"), "supported");
        assert_eq!(state_of(build, "security"), "disabled");

        // SIMD carries its detected level.
        let simd = obj(obj(&json).get("vector").expect("vector"))
            .get("simd")
            .expect("simd");
        assert_eq!(s(simd, "state"), "supported");
        assert_eq!(s(simd, "level"), "avx_fma");

        // Replication baseline.
        let repl = obj(&json).get("replication").expect("replication");
        assert_eq!(s(repl, "role"), "standalone");
        assert_eq!(s(repl, "commit_policy"), "local");
        assert_eq!(
            obj(repl).get("read_only").and_then(JsonValue::as_bool),
            Some(false)
        );

        // Auth disabled in the baseline.
        let auth = obj(&json).get("auth").expect("auth");
        assert_eq!(
            obj(auth).get("enabled").and_then(JsonValue::as_bool),
            Some(false)
        );
        assert_eq!(
            state_of(obj(auth).get("methods").expect("methods"), "bearer"),
            "disabled"
        );

        // Active HTTP contract is supported.
        let contracts = obj(&json).get("api_contracts").expect("api_contracts");
        assert_eq!(state_of(contracts, "http"), "supported");

        // AI providers reported, models empty.
        let ai = obj(&json).get("ai").expect("ai");
        assert_eq!(
            obj(obj(ai).get("models").expect("models"))
                .get("count")
                .and_then(JsonValue::as_u64),
            Some(0)
        );
        assert_eq!(
            state_of(obj(ai).get("providers").expect("providers"), "openai"),
            "supported"
        );

        // preview_features object present (empty here).
        assert!(matches!(
            obj(&json).get("preview_features"),
            Some(JsonValue::Object(_))
        ));
    }

    #[test]
    fn system_capabilities_auth_enabled_marks_methods_supported() {
        let mut inputs = baseline_inputs();
        inputs.auth_enabled = true;
        inputs.auth_methods = vec![
            ("bearer".to_string(), CapabilityState::Supported),
            ("oauth_jwt".to_string(), CapabilityState::Supported),
            ("mtls".to_string(), CapabilityState::Disabled),
        ];
        let json = system_capabilities_json(&inputs);

        let auth = obj(&json).get("auth").expect("auth");
        assert_eq!(
            obj(auth).get("enabled").and_then(JsonValue::as_bool),
            Some(true)
        );
        let methods = obj(auth).get("methods").expect("methods");
        assert_eq!(state_of(methods, "bearer"), "supported");
        assert_eq!(state_of(methods, "oauth_jwt"), "supported");
        assert_eq!(state_of(methods, "mtls"), "disabled");
    }

    #[test]
    fn system_capabilities_unavailable_transport_carries_reason() {
        let mut inputs = baseline_inputs();
        inputs.transports_failed = vec![("grpc".to_string(), "address in use".to_string())];
        let json = system_capabilities_json(&inputs);

        let contracts = obj(&json).get("api_contracts").expect("api_contracts");
        let grpc = obj(contracts).get("grpc").expect("grpc contract");
        assert_eq!(s(grpc, "state"), "unavailable");
        assert_eq!(s(grpc, "reason"), "address in use");
    }

    #[test]
    fn system_capabilities_no_simd_probe_is_unavailable() {
        let mut inputs = baseline_inputs();
        inputs.simd_level = None;
        let json = system_capabilities_json(&inputs);
        let simd = obj(obj(&json).get("vector").expect("vector"))
            .get("simd")
            .expect("simd");
        assert_eq!(s(simd, "state"), "unavailable");
        assert_eq!(s(simd, "reason"), "simd_probe_unavailable");
    }

    #[test]
    fn system_capabilities_preview_feature_renders_preview_state() {
        let mut inputs = baseline_inputs();
        inputs.preview_features =
            vec![("binary_quantization".to_string(), CapabilityState::Preview)];
        let json = system_capabilities_json(&inputs);
        let preview = obj(&json)
            .get("preview_features")
            .expect("preview_features");
        assert_eq!(state_of(preview, "binary_quantization"), "preview");
    }

    #[test]
    fn principal_capabilities_anonymous_read_only_access() {
        let json = principal_capabilities_json(&PrincipalCapabilitiesInputs {
            snapshot_at_unix_ms: 1,
            auth_enabled: false,
            authenticated: false,
            principal: "anon".to_string(),
            tenant: None,
            role: None,
            read_only: false,
            can_read: true,
            can_write: false,
            can_admin: false,
        });
        assert_eq!(s(&json, "principal"), "anon");
        assert!(matches!(obj(&json).get("tenant"), Some(JsonValue::Null)));
        assert!(matches!(obj(&json).get("role"), Some(JsonValue::Null)));
        let eff = obj(&json).get("effective").expect("effective");
        assert_eq!(state_of(eff, "read"), "supported");
        assert_eq!(state_of(eff, "write"), "disabled");
        assert_eq!(state_of(eff, "admin"), "disabled");
    }

    #[test]
    fn principal_capabilities_writer_on_readonly_node_is_unavailable() {
        let json = principal_capabilities_json(&PrincipalCapabilitiesInputs {
            snapshot_at_unix_ms: 1,
            auth_enabled: true,
            authenticated: true,
            principal: "acme/alice".to_string(),
            tenant: Some("acme".to_string()),
            role: Some("write".to_string()),
            read_only: true,
            can_read: true,
            can_write: true,
            can_admin: false,
        });
        assert_eq!(s(&json, "principal"), "acme/alice");
        assert_eq!(s(&json, "tenant"), "acme");
        assert_eq!(s(&json, "role"), "write");
        let eff = obj(&json).get("effective").expect("effective");
        assert_eq!(state_of(eff, "read"), "supported");
        // Role allows write, but the node is read-only → unavailable.
        let write = obj(eff).get("write").expect("write");
        assert_eq!(s(write, "state"), "unavailable");
        assert_eq!(s(write, "reason"), "node_read_only");
        assert_eq!(state_of(eff, "admin"), "disabled");
    }

    #[test]
    fn principal_capabilities_admin_has_full_effective_access() {
        let json = principal_capabilities_json(&PrincipalCapabilitiesInputs {
            snapshot_at_unix_ms: 1,
            auth_enabled: true,
            authenticated: true,
            principal: "root".to_string(),
            tenant: None,
            role: Some("admin".to_string()),
            read_only: false,
            can_read: true,
            can_write: true,
            can_admin: true,
        });
        let eff = obj(&json).get("effective").expect("effective");
        assert_eq!(state_of(eff, "read"), "supported");
        assert_eq!(state_of(eff, "write"), "supported");
        assert_eq!(state_of(eff, "admin"), "supported");
    }
}
