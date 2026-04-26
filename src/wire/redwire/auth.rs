//! Handshake state machine + auth method dispatch.
//!
//! Hello / HelloAck payloads are JSON for the initial cut. CBOR
//! migration tracked as a follow-up — JSON keeps the v2 wire
//! debuggable from a hex dump and reuses the engine's existing
//! `crate::serde_json` codec without a new dep.
//!
//! Auth methods supported in v2.1:
//!   - `bearer`     — token in AuthResponse, validated against AuthStore
//!   - `anonymous`  — only when AuthStore is disabled; no challenge

use crate::auth::store::AuthStore;
use crate::auth::Role;
use crate::serde_json::{self, Value as JsonValue};

/// Methods we know how to handle today.
///
/// `bearer` + `anonymous` are 1-RTT and fully wired.
/// `scram-sha-256` and `oauth-jwt` are advertised but the
/// validate_auth_response side returns AuthFail until the
/// AuthStore migration (Phase 3b/4) lands the verifier
/// storage + OAuth authenticator handle. Listing them keeps
/// Hello/HelloAck stable while the server-side wiring catches
/// up — clients can probe for the method without churning the
/// negotiation surface later.
pub const SUPPORTED_METHODS: &[&str] = &["bearer", "anonymous", "scram-sha-256", "oauth-jwt"];

/// Outcome of `validate_auth_response`.
#[derive(Debug, Clone)]
pub enum AuthOutcome {
    /// Auth succeeded; session id + role for downstream dispatch.
    Authenticated {
        username: String,
        role: Role,
        session_id: String,
    },
    /// Auth refused; the message is operator-readable.
    Refused(String),
}

/// Decode the JSON-shaped Hello payload sent by a v2 client.
#[derive(Debug, Clone)]
pub struct Hello {
    pub versions: Vec<u8>,
    pub auth_methods: Vec<String>,
    pub features: u32,
    pub client_name: Option<String>,
}

impl Hello {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, String> {
        let v: JsonValue = serde_json::from_slice(bytes)
            .map_err(|e| format!("Hello: invalid JSON: {e}"))?;
        let obj = match v {
            JsonValue::Object(o) => o,
            _ => return Err("Hello: payload must be a JSON object".into()),
        };
        let versions: Vec<u8> = obj
            .get("versions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|n| n.as_f64().map(|f| f as u8))
                    .collect()
            })
            .unwrap_or_default();
        let auth_methods: Vec<String> = obj
            .get("auth_methods")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let features = obj
            .get("features")
            .and_then(|v| v.as_f64())
            .map(|f| f as u32)
            .unwrap_or(0);
        let client_name = obj
            .get("client_name")
            .and_then(|v| v.as_str())
            .map(String::from);
        if versions.is_empty() {
            return Err("Hello: versions[] is empty".into());
        }
        if auth_methods.is_empty() {
            return Err("Hello: auth_methods[] is empty".into());
        }
        Ok(Self {
            versions,
            auth_methods,
            features,
            client_name,
        })
    }
}

/// Build the HelloAck the server sends back. `chosen_auth` is the
/// strongest method both sides support; `chosen_version` is
/// `min(client_max, server_max)`.
pub fn build_hello_ack(
    chosen_version: u8,
    chosen_auth: &str,
    server_features: u32,
) -> Vec<u8> {
    let mut obj = crate::serde_json::Map::new();
    obj.insert(
        "version".to_string(),
        JsonValue::Number(chosen_version as f64),
    );
    obj.insert(
        "auth".to_string(),
        JsonValue::String(chosen_auth.to_string()),
    );
    obj.insert(
        "features".to_string(),
        JsonValue::Number(server_features as f64),
    );
    obj.insert(
        "server".to_string(),
        JsonValue::String(format!("reddb/{}", env!("CARGO_PKG_VERSION"))),
    );
    JsonValue::Object(obj).to_string_compact().into_bytes()
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
                session_id: new_session_id(),
            }
        }
        "bearer" => {
            let token = parse_bearer_response(payload).unwrap_or_default();
            let Some(store) = auth_store else {
                return AuthOutcome::Refused(
                    "bearer auth refused — server has no auth store configured".into(),
                );
            };
            match store.validate_token(&token) {
                Some((username, role)) => AuthOutcome::Authenticated {
                    username,
                    role,
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

fn parse_bearer_response(payload: &[u8]) -> Option<String> {
    let v: JsonValue = serde_json::from_slice(payload).ok()?;
    let token = v.as_object()?.get("token")?.as_str()?;
    Some(token.to_string())
}

/// Build the AuthOk payload the server sends after a successful
/// auth.
pub fn build_auth_ok(session_id: &str, username: &str, role: Role, server_features: u32) -> Vec<u8> {
    let mut obj = crate::serde_json::Map::new();
    obj.insert(
        "session_id".to_string(),
        JsonValue::String(session_id.to_string()),
    );
    obj.insert(
        "username".to_string(),
        JsonValue::String(username.to_string()),
    );
    obj.insert("role".to_string(), JsonValue::String(role.to_string()));
    obj.insert(
        "features".to_string(),
        JsonValue::Number(server_features as f64),
    );
    JsonValue::Object(obj).to_string_compact().into_bytes()
}

pub fn build_auth_fail(reason: &str) -> Vec<u8> {
    let mut obj = crate::serde_json::Map::new();
    obj.insert("reason".to_string(), JsonValue::String(reason.to_string()));
    JsonValue::Object(obj).to_string_compact().into_bytes()
}

/// Parse a SCRAM client-first-message.
/// Format: `n,,n=<user>,r=<client_nonce>` (no channel binding,
/// no authzid). Returns `(username, client_nonce, bare_message)`.
pub fn parse_scram_client_first(payload: &[u8]) -> Result<(String, String, String), String> {
    let s = std::str::from_utf8(payload)
        .map_err(|_| "client-first not UTF-8".to_string())?;
    // Strip the GS2 header `n,,` (or `y,,` / `p=...,`). v2.1 only
    // accepts `n,,` — explicit no-channel-binding.
    let bare = s
        .strip_prefix("n,,")
        .ok_or_else(|| "client-first must start with 'n,,' (no channel binding)".to_string())?;
    let mut user = None;
    let mut nonce = None;
    for part in bare.split(',') {
        if let Some(v) = part.strip_prefix("n=") {
            user = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("r=") {
            nonce = Some(v.to_string());
        }
    }
    let user = user.ok_or_else(|| "missing n=<user>".to_string())?;
    let nonce = nonce.ok_or_else(|| "missing r=<nonce>".to_string())?;
    Ok((user, nonce, bare.to_string()))
}

/// Build the SCRAM server-first-message. Sent in `AuthRequest`.
/// Format: `r=<client_nonce><server_nonce>,s=<salt_b64>,i=<iter>`.
pub fn build_scram_server_first(
    client_nonce: &str,
    server_nonce: &str,
    salt: &[u8],
    iter: u32,
) -> String {
    format!(
        "r={client_nonce}{server_nonce},s={},i={iter}",
        base64_std(salt)
    )
}

/// Parse SCRAM client-final-message.
/// Format: `c=<channel_binding_b64>,r=<combined_nonce>,p=<proof_b64>`.
pub fn parse_scram_client_final(
    payload: &[u8],
) -> Result<(String, Vec<u8>, String), String> {
    let s = std::str::from_utf8(payload)
        .map_err(|_| "client-final not UTF-8".to_string())?;
    let mut channel_binding = None;
    let mut nonce = None;
    let mut proof_b64 = None;
    for part in s.split(',') {
        if let Some(v) = part.strip_prefix("c=") {
            channel_binding = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("r=") {
            nonce = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("p=") {
            proof_b64 = Some(v.to_string());
        }
    }
    let channel_binding =
        channel_binding.ok_or_else(|| "missing c=<channel-binding>".to_string())?;
    let nonce = nonce.ok_or_else(|| "missing r=<nonce>".to_string())?;
    let proof_b64 = proof_b64.ok_or_else(|| "missing p=<proof>".to_string())?;
    let proof = base64_std_decode(&proof_b64)
        .ok_or_else(|| "client proof is not valid base64".to_string())?;
    // c=biws is base64("n,,") — the canonical no-channel-binding GS2 header.
    if channel_binding != "biws" {
        return Err(format!(
            "channel binding must be 'biws' (n,,), got '{channel_binding}'"
        ));
    }
    let no_proof = format!("c={channel_binding},r={nonce}");
    Ok((nonce, proof, no_proof))
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
    let mut obj = crate::serde_json::Map::new();
    obj.insert("session_id".to_string(), JsonValue::String(session_id.to_string()));
    obj.insert("username".to_string(), JsonValue::String(username.to_string()));
    obj.insert("role".to_string(), JsonValue::String(role.to_string()));
    obj.insert("features".to_string(), JsonValue::Number(server_features as f64));
    obj.insert(
        "v".to_string(),
        JsonValue::String(base64_std(server_signature)),
    );
    JsonValue::Object(obj).to_string_compact().into_bytes()
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

// ---------------------------------------------------------------
// Tiny base64 — RFC 4648 standard alphabet. Only used for SCRAM
// payloads + AuthOk signature, low-frequency so a hand-rolled
// codec is fine and avoids pulling another crate.
// ---------------------------------------------------------------

const B64_ALPHA: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_std(input: &[u8]) -> String {
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    let chunks = input.chunks_exact(3);
    let rem = chunks.remainder();
    for c in chunks {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32);
        out.push(B64_ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(B64_ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(B64_ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push(B64_ALPHA[(n & 0x3F) as usize] as char);
    }
    match rem {
        [a] => {
            let n = (*a as u32) << 16;
            out.push(B64_ALPHA[((n >> 18) & 0x3F) as usize] as char);
            out.push(B64_ALPHA[((n >> 12) & 0x3F) as usize] as char);
            out.push('=');
            out.push('=');
        }
        [a, b] => {
            let n = ((*a as u32) << 16) | ((*b as u32) << 8);
            out.push(B64_ALPHA[((n >> 18) & 0x3F) as usize] as char);
            out.push(B64_ALPHA[((n >> 12) & 0x3F) as usize] as char);
            out.push(B64_ALPHA[((n >> 6) & 0x3F) as usize] as char);
            out.push('=');
        }
        _ => {}
    }
    out
}

pub fn base64_std_decode(input: &str) -> Option<Vec<u8>> {
    let trimmed = input.trim_end_matches('=');
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for ch in trimmed.bytes() {
        let v: u32 = match ch {
            b'A'..=b'Z' => (ch - b'A') as u32,
            b'a'..=b'z' => (ch - b'a' + 26) as u32,
            b'0'..=b'9' => (ch - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

/// Parse a compact-serialized JWT into a `DecodedJwt`. RFC 7519
/// shape: `<base64url(header)>.<base64url(payload)>.<base64url(signature)>`.
/// The validator does the heavy lifting (signature, claims,
/// expiry); this function just splits + decodes.
pub fn parse_jwt(token: &str) -> Result<crate::auth::oauth::DecodedJwt, String> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(format!("expected 3 dot-separated parts, got {}", parts.len()));
    }
    let header_bytes = base64_url_decode(parts[0])
        .ok_or_else(|| "header is not valid base64url".to_string())?;
    let payload_bytes = base64_url_decode(parts[1])
        .ok_or_else(|| "payload is not valid base64url".to_string())?;
    let signature = base64_url_decode(parts[2])
        .ok_or_else(|| "signature is not valid base64url".to_string())?;

    let header_json: JsonValue = serde_json::from_slice(&header_bytes)
        .map_err(|e| format!("header JSON: {e}"))?;
    let payload_json: JsonValue = serde_json::from_slice(&payload_bytes)
        .map_err(|e| format!("payload JSON: {e}"))?;

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
        claims.aud = arr.iter().filter_map(|v| v.as_str().map(String::from)).collect();
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
    let token = parse_jwt(raw_token).map_err(|e| format!("decode JWT: {e}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    // sub-claim mode: the JWT subject IS the RedDB username.
    // Roles map from a `role` custom claim; default to Read.
    let identity = validator
        .validate(&token, now, |sub| {
            Some(crate::auth::User {
                username: sub.to_string(),
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
    Ok((identity.username, identity.role))
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
    while s.len() % 4 != 0 {
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
        let pref = vec!["scram-sha-256".to_string()];
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
}
