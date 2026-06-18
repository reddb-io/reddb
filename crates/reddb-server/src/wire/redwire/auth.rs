//! Handshake state machine + auth method dispatch.
//!
//! Hello / HelloAck payloads are JSON for the initial cut. CBOR
//! migration tracked as a follow-up — JSON keeps the v2 wire
//! debuggable from a hex dump. The payload contract lives in
//! `reddb-wire`; this module keeps server auth policy and validation.
//!
//! Auth methods supported in v2.1:
//!   - `bearer`     — token in AuthResponse, validated against AuthStore
//!   - `anonymous`  — only when AuthStore is disabled; no challenge

use crate::auth::store::AuthStore;
use crate::auth::Role;
use crate::serde_json::{self, Value as JsonValue};
use reddb_wire::redwire::handshake::{
    base64_std, base64_std_decode, build_scram_auth_ok_payload, parse_auth_response_bearer_token,
};

/// Outcome of `validate_auth_response`.
#[derive(Debug, Clone)]
pub enum AuthOutcome {
    /// Auth succeeded; session id + role for downstream dispatch.
    Authenticated {
        username: String,
        role: Role,
        tenant: Option<String>,
        session_id: String,
    },
    /// Auth refused; the message is operator-readable.
    Refused(String),
}

/// Server's policy for picking an auth method given the client's
/// preferences. Strongest-first ordering — but when the server
/// has no auth backend configured (`server_anon_ok = true`),
/// `anonymous` wins over `bearer` because bearer validation
/// would fail anyway. v2.1 supports bearer + anonymous; future
/// versions prepend scram-sha-256, mtls, oauth-jwt to the
/// priority list.
pub fn pick_auth_method(client_methods: &[String], server_anon_ok: bool) -> Option<&'static str> {
    // SCRAM (no-plaintext-on-the-wire) > OAuth-JWT (federated)
    // > bearer (session token / API key) > anonymous.
    // No-auth servers prefer anonymous so the handshake succeeds
    // without an AuthStore lookup.
    let priority: &[&'static str] = if server_anon_ok {
        &["anonymous", "scram-sha-256", "oauth-jwt", "bearer"]
    } else {
        &["scram-sha-256", "oauth-jwt", "bearer", "anonymous"]
    };
    for method in priority {
        if !client_methods.iter().any(|m| m == *method) {
            continue;
        }
        if *method == "anonymous" && !server_anon_ok {
            continue;
        }
        return Some(*method);
    }
    None
}

/// Validate the AuthResponse payload for the chosen method.
pub fn validate_auth_response(
    method: &str,
    payload: &[u8],
    auth_store: Option<&AuthStore>,
) -> AuthOutcome {
    match method {
        "anonymous" => {
            // Only legitimate when auth is disabled. Caller already
            // gated this in `pick_auth_method`; double-check here.
            if let Some(store) = auth_store {
                if store.is_enabled() {
                    return AuthOutcome::Refused(
                        "anonymous auth refused — server has auth enabled".into(),
                    );
                }
            }
            AuthOutcome::Authenticated {
                username: "anonymous".to_string(),
                role: Role::Read,
                tenant: None,
                session_id: new_session_id(),
            }
        }
        "bearer" => {
            let token = parse_auth_response_bearer_token(payload).unwrap_or_default();
            let Some(store) = auth_store else {
                return AuthOutcome::Refused(
                    "bearer auth refused — server has no auth store configured".into(),
                );
            };
            match store.validate_token_full(&token) {
                Some((user_id, role)) => AuthOutcome::Authenticated {
                    username: user_id.username,
                    role,
                    tenant: user_id.tenant,
                    session_id: new_session_id(),
                },
                None => AuthOutcome::Refused("bearer token invalid".into()),
            }
        }
        "scram-sha-256" => AuthOutcome::Refused(
            "scram-sha-256 must be driven through perform_scram_handshake — \
             the 1-RTT validate_auth_response path doesn't apply"
                .to_string(),
        ),
        "oauth-jwt" => {
            // The OAuthValidator handle is expected via the
            // RedWireConfig.oauth slot — plumbing happens in
            // session::handle_session. When called here without
            // it (e.g. test paths that don't set the handle),
            // the v2 handshake refuses cleanly.
            AuthOutcome::Refused(
                "oauth-jwt requires RedWireConfig.oauth to be set. Pass an \
                 OAuthValidator with the issuer + JWKS configured."
                    .to_string(),
            )
        }
        other => AuthOutcome::Refused(format!("auth method '{other}' is not supported in v2.1")),
    }
}

/// Build the AuthOk payload the server sends after a successful
/// auth.
pub fn build_auth_ok(
    session_id: &str,
    username: &str,
    role: Role,
    server_features: u32,
) -> Vec<u8> {
    let role_str = role.to_string();
    reddb_wire::redwire::handshake::build_auth_ok_payload(
        session_id,
        username,
        &role_str,
        server_features,
    )
}

/// Build the AuthOk payload for a successful SCRAM completion.
/// Carries the server signature so the client can verify the
/// server also knew the verifier.
pub fn build_scram_auth_ok(
    session_id: &str,
    username: &str,
    role: Role,
    server_features: u32,
    server_signature: &[u8],
) -> Vec<u8> {
    let role = role.to_string();
    build_scram_auth_ok_payload(
        session_id,
        username,
        &role,
        server_features,
        server_signature,
    )
}

/// Generate a 24-byte server nonce, base64-encoded. Cryptographic
/// randomness sourced from the engine's existing `random_bytes`
/// helper so SCRAM doesn't introduce a new RNG path.
pub fn new_server_nonce() -> String {
    base64_std(&crate::auth::store::random_bytes(18))
}

pub(crate) fn new_session_id_for_scram() -> String {
    new_session_id()
}

/// Parse a compact-serialized JWT into a `DecodedJwt`. RFC 7519
/// shape: `<base64url(header)>.<base64url(payload)>.<base64url(signature)>`.
/// The validator does the heavy lifting (signature, claims,
/// expiry); this function just splits + decodes.
pub fn parse_jwt(token: &str) -> Result<crate::auth::oauth::DecodedJwt, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(format!(
            "expected 3 dot-separated parts, got {}",
            parts.len()
        ));
    }
    let header_bytes =
        base64_url_decode(parts[0]).ok_or_else(|| "header is not valid base64url".to_string())?;
    let payload_bytes =
        base64_url_decode(parts[1]).ok_or_else(|| "payload is not valid base64url".to_string())?;
    let signature = base64_url_decode(parts[2])
        .ok_or_else(|| "signature is not valid base64url".to_string())?;

    let header_json: JsonValue =
        serde_json::from_slice(&header_bytes).map_err(|e| format!("header JSON: {e}"))?;
    let payload_json: JsonValue =
        serde_json::from_slice(&payload_bytes).map_err(|e| format!("payload JSON: {e}"))?;

    let header = jwt_header_from(&header_json)?;
    let claims = jwt_claims_from(&payload_json);

    let signing_input = format!("{}.{}", parts[0], parts[1]).into_bytes();

    Ok(crate::auth::oauth::DecodedJwt {
        header,
        claims,
        signing_input,
        signature,
    })
}

fn jwt_header_from(v: &JsonValue) -> Result<crate::auth::oauth::JwtHeader, String> {
    let obj = v
        .as_object()
        .ok_or_else(|| "JWT header must be a JSON object".to_string())?;
    let alg = obj
        .get("alg")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "JWT header missing 'alg'".to_string())?
        .to_string();
    let kid = obj.get("kid").and_then(|x| x.as_str()).map(String::from);
    Ok(crate::auth::oauth::JwtHeader { alg, kid })
}

fn jwt_claims_from(v: &JsonValue) -> crate::auth::oauth::JwtClaims {
    let obj = v.as_object().cloned().unwrap_or_default();
    let mut claims = crate::auth::oauth::JwtClaims::default();
    if let Some(s) = obj.get("iss").and_then(|x| x.as_str()) {
        claims.iss = Some(s.to_string());
    }
    if let Some(s) = obj.get("sub").and_then(|x| x.as_str()) {
        claims.sub = Some(s.to_string());
    }
    if let Some(s) = obj.get("aud").and_then(|x| x.as_str()) {
        claims.aud = vec![s.to_string()];
    } else if let Some(arr) = obj.get("aud").and_then(|x| x.as_array()) {
        claims.aud = arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }
    if let Some(n) = obj.get("exp").and_then(|x| x.as_f64()) {
        claims.exp = Some(n as i64);
    }
    if let Some(n) = obj.get("nbf").and_then(|x| x.as_f64()) {
        claims.nbf = Some(n as i64);
    }
    if let Some(n) = obj.get("iat").and_then(|x| x.as_f64()) {
        claims.iat = Some(n as i64);
    }
    for (k, v) in obj.iter() {
        if matches!(k.as_str(), "iss" | "sub" | "aud" | "exp" | "nbf" | "iat") {
            continue;
        }
        if let Some(s) = v.as_str() {
            claims.extra.insert(k.clone(), s.to_string());
        }
    }
    claims
}

/// Validate a JWT through the supplied `OAuthValidator`. Returns
/// `(username, role)` on success, or a refusal reason.
pub fn validate_oauth_jwt(
    validator: &crate::auth::oauth::OAuthValidator,
    raw_token: &str,
) -> Result<(String, Role), String> {
    validate_oauth_jwt_full(validator, raw_token).map(|(_tenant, username, role)| (username, role))
}

/// Tenant-aware variant of [`validate_oauth_jwt`]. Returns
/// `(tenant, username, role)` so the caller can mint a session pinned
/// to the tenant carried by the configured `tenant_claim`.
pub fn validate_oauth_jwt_full(
    validator: &crate::auth::oauth::OAuthValidator,
    raw_token: &str,
) -> Result<(Option<String>, String, Role), String> {
    let token = parse_jwt(raw_token).map_err(|e| format!("decode JWT: {e}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // sub-claim mode: the JWT subject IS the RedDB username. Roles map
    // from a `role` custom claim; tenant from the configured tenant
    // claim (default "tenant"). The lookup closure mirrors the same
    // claims so `map_to_existing_users=false` deployments still get a
    // tenant-tagged identity.
    let identity = validator
        .validate(&token, now, |sub| {
            Some(crate::auth::User {
                username: sub.to_string(),
                tenant_id: token.claims.extra.get("tenant").cloned(),
                password_hash: String::new(),
                scram_verifier: None,
                role: token
                    .claims
                    .extra
                    .get("role")
                    .and_then(|s| Role::from_str(s))
                    .unwrap_or(Role::Read),
                api_keys: Vec::new(),
                created_at: 0,
                updated_at: 0,
                enabled: true,
            })
        })
        .map_err(|e| format!("{e}"))?;
    Ok((identity.tenant, identity.username, identity.role))
}

fn base64_url_decode(input: &str) -> Option<Vec<u8>> {
    // base64url = '+' → '-', '/' → '_', stripped padding.
    let mut s = String::with_capacity(input.len() + 4);
    for ch in input.chars() {
        match ch {
            '-' => s.push('+'),
            '_' => s.push('/'),
            _ => s.push(ch),
        }
    }
    while !s.len().is_multiple_of(4) {
        s.push('=');
    }
    base64_std_decode(&s)
}

/// Generate a session id. Format: `rwsess-<unix_micros>-<rand>`.
/// Not cryptographically random; the security boundary is the
/// auth method, not session-id unguessability.
fn new_session_id() -> String {
    let now_us = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros())
        .unwrap_or(0);
    let rand = crate::utils::now_unix_nanos() & 0xFFFF_FFFF;
    format!("rwsess-{now_us}-{rand:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_wire::redwire::handshake::{build_hello_ack, Hello};

    #[test]
    fn hello_round_trip() {
        let payload = br#"{"versions":[1],"auth_methods":["bearer","anonymous"],"features":3,"client_name":"reddb-rs/0.1"}"#;
        let h = Hello::from_payload(payload).unwrap();
        assert_eq!(h.versions, vec![1]);
        assert_eq!(h.auth_methods, vec!["bearer", "anonymous"]);
        assert_eq!(h.features, 3);
        assert_eq!(h.client_name.as_deref(), Some("reddb-rs/0.1"));
    }

    #[test]
    fn hello_rejects_empty_methods() {
        let payload = br#"{"versions":[1],"auth_methods":[]}"#;
        assert!(Hello::from_payload(payload).is_err());
    }

    #[test]
    fn pick_auth_prefers_anonymous_when_server_has_no_auth_store() {
        // Without an auth store, bearer validation can't succeed.
        // Picker should prefer anonymous so the handshake works.
        let pref = vec!["anonymous".to_string(), "bearer".to_string()];
        assert_eq!(pick_auth_method(&pref, true), Some("anonymous"));
    }

    #[test]
    fn pick_auth_picks_bearer_when_anonymous_blocked() {
        // Server has auth enabled (no anonymous) — bearer wins.
        let pref = vec!["anonymous".to_string(), "bearer".to_string()];
        assert_eq!(pick_auth_method(&pref, false), Some("bearer"));
    }

    #[test]
    fn pick_auth_skips_anonymous_when_server_blocks_it() {
        let pref = vec!["anonymous".to_string()];
        assert_eq!(pick_auth_method(&pref, false), None);
    }

    #[test]
    fn pick_auth_returns_none_when_nothing_overlaps() {
        let pref = vec!["kerberos".to_string(), "future-method".to_string()];
        assert_eq!(pick_auth_method(&pref, true), None);
    }

    #[test]
    fn anonymous_validates_only_when_store_disabled() {
        let outcome = validate_auth_response("anonymous", &[], None);
        assert!(matches!(outcome, AuthOutcome::Authenticated { .. }));
    }

    #[test]
    fn bearer_without_store_refuses() {
        let outcome = validate_auth_response("bearer", br#"{"token":"x"}"#, None);
        assert!(matches!(outcome, AuthOutcome::Refused(_)));
    }

    #[test]
    fn hello_ack_omits_topology_field_when_caller_passes_none() {
        // Backwards-compat: callers that haven't picked up the
        // advertiser yet pass `None` and the JSON envelope keeps
        // the same shape as pre-#167.
        let bytes = build_hello_ack(1, "bearer", 0, None);
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(!s.contains("\"topology\""));
    }

    #[test]
    fn hello_ack_embeds_topology_field_when_caller_passes_payload() {
        // Issue #167: HelloAck builder inserts the canonical bytes
        // base64-wrapped under JSON key `topology`. Round-trip via
        // the wire decoder pins byte-for-byte equivalence with the
        // canonical encoder (#166).
        let topo = reddb_wire::topology::Topology {
            epoch: 17,
            primary: reddb_wire::topology::Endpoint {
                addr: "primary:5050".into(),
                region: "us-east-1".into(),
            },
            replicas: Vec::new(),
        };
        let bytes = build_hello_ack(1, "bearer", 0, Some(&topo));
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(s.contains("\"topology\""), "missing topology key in {s}");

        // Extract and round-trip the field through the wire decoder.
        let v: JsonValue = crate::serde_json::from_slice(&bytes).unwrap();
        let field = v
            .as_object()
            .and_then(|o| o.get("topology"))
            .and_then(|t| t.as_str())
            .expect("topology key must be present and a string");
        let decoded = reddb_wire::topology::decode_topology_from_hello_ack(field).expect("decode");
        assert_eq!(decoded.expect("v1 known"), topo);
    }
}
