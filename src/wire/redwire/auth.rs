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

/// Methods we know how to handle today. SCRAM/mTLS/OAuth land
/// in follow-up PRs (each is its own state machine).
pub const SUPPORTED_METHODS: &[&str] = &["bearer", "anonymous"];

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
/// preferences. Strongest-first ordering. v2.1 supports bearer
/// and anonymous; future versions will append scram-sha-256, mtls,
/// oauth-jwt to the front.
pub fn pick_auth_method(client_methods: &[String], server_anon_ok: bool) -> Option<&'static str> {
    let priority = ["bearer", "anonymous"];
    for method in priority {
        if !client_methods.iter().any(|m| m == method) {
            continue;
        }
        if method == "anonymous" && !server_anon_ok {
            continue;
        }
        return Some(method);
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
    fn pick_auth_picks_strongest_first() {
        let pref = vec!["anonymous".to_string(), "bearer".to_string()];
        assert_eq!(pick_auth_method(&pref, true), Some("bearer"));
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
