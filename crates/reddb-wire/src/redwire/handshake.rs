//! RedWire handshake payload contracts.
//!
//! Authentication policy and credential validation belong in the
//! server. This module owns only the wire-visible JSON shapes used by
//! Hello, HelloAck, AuthResponse, AuthOk, and AuthFail.

use serde_json::Value as JsonValue;

use super::{BuildError, Frame, FrameBuilder, MessageKind, MAX_KNOWN_MINOR_VERSION};

/// Methods RedWire v2.1 knows how to negotiate.
pub const SUPPORTED_METHODS: &[&str] = &["bearer", "anonymous", "scram-sha-256", "oauth-jwt"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub versions: Vec<u8>,
    pub auth_methods: Vec<String>,
    pub features: u32,
    pub client_name: Option<String>,
}

impl Hello {
    pub fn to_payload(&self) -> Vec<u8> {
        build_hello_payload(
            &self.versions,
            self.auth_methods.iter().map(String::as_str),
            self.features,
            self.client_name.as_deref(),
        )
    }

    pub fn from_payload(bytes: &[u8]) -> Result<Self, String> {
        let v: JsonValue =
            serde_json::from_slice(bytes).map_err(|e| format!("Hello: invalid JSON: {e}"))?;
        let obj = match v {
            JsonValue::Object(o) => o,
            _ => return Err("Hello: payload must be a JSON object".into()),
        };
        let versions: Vec<u8> = obj
            .get("versions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|n| n.as_u64().map(|u| u as u8))
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
            .and_then(|v| v.as_u64())
            .map(|u| u as u32)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelloAck {
    pub version: u8,
    pub auth: String,
    pub features: u32,
    pub server: Option<String>,
    pub topology: Option<String>,
}

impl HelloAck {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, String> {
        let obj = object_from_payload("HelloAck", bytes)?;
        let version = required_u8(&obj, "HelloAck", "version")?;
        let auth = required_string(&obj, "HelloAck", "auth")?;
        let features = optional_u32(&obj, "features").unwrap_or(0);
        let server = optional_string(&obj, "server");
        let topology = optional_string(&obj, "topology");
        Ok(Self {
            version,
            auth,
            features,
            server,
            topology,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthOk {
    pub session_id: String,
    pub username: Option<String>,
    pub role: Option<String>,
    pub features: u32,
    pub server_signature: Option<String>,
}

impl AuthOk {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, String> {
        let obj = object_from_payload("AuthOk", bytes)?;
        let session_id = required_string(&obj, "AuthOk", "session_id")?;
        let username = optional_string(&obj, "username");
        let role = optional_string(&obj, "role");
        let features = optional_u32(&obj, "features").unwrap_or(0);
        let server_signature = optional_string(&obj, "v");
        Ok(Self {
            session_id,
            username,
            role,
            features,
            server_signature,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthFail {
    pub reason: String,
}

impl AuthFail {
    pub fn from_payload(bytes: &[u8]) -> Result<Self, String> {
        let obj = object_from_payload("AuthFail", bytes)?;
        Ok(Self {
            reason: required_string(&obj, "AuthFail", "reason")?,
        })
    }
}

pub fn build_hello_payload<'a, I>(
    versions: &[u8],
    auth_methods: I,
    features: u32,
    client_name: Option<&str>,
) -> Vec<u8>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut obj = serde_json::Map::new();
    obj.insert(
        "versions".to_string(),
        JsonValue::Array(
            versions
                .iter()
                .map(|version| JsonValue::Number((*version).into()))
                .collect(),
        ),
    );
    obj.insert(
        "auth_methods".to_string(),
        JsonValue::Array(
            auth_methods
                .into_iter()
                .map(|method| JsonValue::String(method.to_string()))
                .collect(),
        ),
    );
    obj.insert("features".to_string(), JsonValue::Number(features.into()));
    if let Some(name) = client_name {
        obj.insert(
            "client_name".to_string(),
            JsonValue::String(name.to_string()),
        );
    }
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_client_hello_payload<'a, I>(
    auth_methods: I,
    features: u32,
    client_name: Option<&str>,
) -> Vec<u8>
where
    I: IntoIterator<Item = &'a str>,
{
    build_hello_payload(
        &[MAX_KNOWN_MINOR_VERSION],
        auth_methods,
        features,
        client_name,
    )
}

pub fn choose_hello_minor_version(client_versions: &[u8]) -> Option<u8> {
    client_versions
        .iter()
        .copied()
        .filter(|version| *version > 0 && *version <= MAX_KNOWN_MINOR_VERSION)
        .max()
}

pub fn build_hello_ack(
    chosen_version: u8,
    chosen_auth: &str,
    server_features: u32,
    topology: Option<&crate::topology::Topology>,
) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "version".to_string(),
        JsonValue::Number(chosen_version.into()),
    );
    obj.insert(
        "auth".to_string(),
        JsonValue::String(chosen_auth.to_string()),
    );
    obj.insert(
        "features".to_string(),
        JsonValue::Number(server_features.into()),
    );
    obj.insert(
        "server".to_string(),
        JsonValue::String(format!("reddb/{}", env!("CARGO_PKG_VERSION"))),
    );
    if let Some(topo) = topology {
        obj.insert(
            "topology".to_string(),
            JsonValue::String(crate::topology::encode_topology_for_hello_ack(topo)),
        );
    }
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_hello_ack_frame(
    correlation_id: u64,
    chosen_version: u8,
    chosen_auth: &str,
    server_features: u32,
    topology: Option<&crate::topology::Topology>,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::HelloAck)
        .payload(build_hello_ack(
            chosen_version,
            chosen_auth,
            server_features,
            topology,
        ))
        .build()
}

pub fn build_auth_response_anonymous_payload() -> Vec<u8> {
    Vec::new()
}

pub fn build_auth_response_bearer_payload(token: &str) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert("token".to_string(), JsonValue::String(token.to_string()));
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn parse_auth_response_bearer_token(payload: &[u8]) -> Result<String, String> {
    let obj = object_from_payload("AuthResponse", payload)?;
    required_string(&obj, "AuthResponse", "token")
}

pub fn build_auth_response_oauth_jwt_payload(jwt: &str) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert("jwt".to_string(), JsonValue::String(jwt.to_string()));
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn parse_auth_response_oauth_jwt(payload: &[u8]) -> Result<String, String> {
    let obj = object_from_payload("AuthResponse", payload)?;
    required_string(&obj, "AuthResponse", "jwt")
}

pub fn build_auth_ok_payload(
    session_id: &str,
    username: &str,
    role: &str,
    server_features: u32,
) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
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
        JsonValue::Number(server_features.into()),
    );
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_auth_ok_frame_from_payload(
    correlation_id: u64,
    payload: Vec<u8>,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::AuthOk)
        .payload(payload)
        .build()
}

pub fn build_auth_fail_frame(correlation_id: u64, reason: &str) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::AuthFail)
        .payload(build_auth_fail_payload(reason))
        .build()
}

pub fn build_scram_auth_ok_payload(
    session_id: &str,
    username: &str,
    role: &str,
    server_features: u32,
    server_signature: &[u8],
) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
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
        JsonValue::Number(server_features.into()),
    );
    obj.insert(
        "v".to_string(),
        JsonValue::String(base64_std(server_signature)),
    );
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_auth_fail_payload(reason: &str) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert("reason".to_string(), JsonValue::String(reason.to_string()));
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

/// Parse a SCRAM client-first-message.
///
/// Format: `n,,n=<user>,r=<client_nonce>` (no channel binding, no authzid).
/// Returns `(username, client_nonce, bare_message)`.
pub fn parse_scram_client_first(payload: &[u8]) -> Result<(String, String, String), String> {
    let s = std::str::from_utf8(payload).map_err(|_| "client-first not UTF-8".to_string())?;
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

/// Build the SCRAM server-first-message.
///
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
///
/// Format: `c=<channel_binding_b64>,r=<combined_nonce>,p=<proof_b64>`.
pub fn parse_scram_client_final(payload: &[u8]) -> Result<(String, Vec<u8>, String), String> {
    let s = std::str::from_utf8(payload).map_err(|_| "client-final not UTF-8".to_string())?;
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
    if channel_binding != "biws" {
        return Err(format!(
            "channel binding must be 'biws' (n,,), got '{channel_binding}'"
        ));
    }
    let no_proof = format!("c={channel_binding},r={nonce}");
    Ok((nonce, proof, no_proof))
}

const B64_ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_std(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
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

fn object_from_payload(
    name: &str,
    bytes: &[u8],
) -> Result<serde_json::Map<String, JsonValue>, String> {
    let v: JsonValue =
        serde_json::from_slice(bytes).map_err(|e| format!("{name}: invalid JSON: {e}"))?;
    match v {
        JsonValue::Object(o) => Ok(o),
        _ => Err(format!("{name}: payload must be a JSON object")),
    }
}

fn required_string(
    obj: &serde_json::Map<String, JsonValue>,
    name: &str,
    field: &str,
) -> Result<String, String> {
    obj.get(field)
        .and_then(JsonValue::as_str)
        .map(String::from)
        .ok_or_else(|| format!("{name}: missing {field} string"))
}

fn optional_string(obj: &serde_json::Map<String, JsonValue>, field: &str) -> Option<String> {
    obj.get(field).and_then(JsonValue::as_str).map(String::from)
}

fn required_u8(
    obj: &serde_json::Map<String, JsonValue>,
    name: &str,
    field: &str,
) -> Result<u8, String> {
    let n = obj
        .get(field)
        .and_then(JsonValue::as_u64)
        .ok_or_else(|| format!("{name}: missing {field} number"))?;
    u8::try_from(n).map_err(|_| format!("{name}: {field} out of range for u8"))
}

fn optional_u32(obj: &serde_json::Map<String, JsonValue>, field: &str) -> Option<u32> {
    obj.get(field)
        .and_then(JsonValue::as_u64)
        .and_then(|n| u32::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::{Endpoint, ReplicaInfo, Topology};

    #[test]
    fn hello_parses_client_payload() {
        let payload =
            br#"{"versions":[1],"auth_methods":["bearer"],"features":1,"client_name":"x"}"#;
        let hello = Hello::from_payload(payload).unwrap();
        assert_eq!(hello.versions, vec![1]);
        assert_eq!(hello.auth_methods, vec!["bearer"]);
        assert_eq!(hello.features, 1);
        assert_eq!(hello.client_name.as_deref(), Some("x"));
    }

    #[test]
    fn hello_builds_client_payload() {
        let bytes = build_hello_payload(&[1], ["anonymous", "bearer"], 7, Some("client"));
        let hello = Hello::from_payload(&bytes).unwrap();
        assert_eq!(hello.versions, vec![1]);
        assert_eq!(hello.auth_methods, vec!["anonymous", "bearer"]);
        assert_eq!(hello.features, 7);
        assert_eq!(hello.client_name.as_deref(), Some("client"));
    }

    #[test]
    fn client_hello_payload_uses_current_minor_version() {
        let bytes = build_client_hello_payload(["anonymous"], 0, Some("client"));
        let hello = Hello::from_payload(&bytes).unwrap();
        assert_eq!(hello.versions, vec![MAX_KNOWN_MINOR_VERSION]);
        assert_eq!(hello.auth_methods, vec!["anonymous"]);
        assert_eq!(hello.client_name.as_deref(), Some("client"));
    }

    #[test]
    fn hello_minor_version_negotiation_picks_highest_supported_nonzero_version() {
        assert_eq!(
            choose_hello_minor_version(&[0, MAX_KNOWN_MINOR_VERSION]),
            Some(MAX_KNOWN_MINOR_VERSION)
        );
        assert_eq!(
            choose_hello_minor_version(&[
                MAX_KNOWN_MINOR_VERSION.saturating_add(1),
                MAX_KNOWN_MINOR_VERSION,
                1,
            ]),
            Some(MAX_KNOWN_MINOR_VERSION)
        );
        assert_eq!(choose_hello_minor_version(&[]), None);
        assert_eq!(choose_hello_minor_version(&[0]), None);
        assert_eq!(
            choose_hello_minor_version(&[MAX_KNOWN_MINOR_VERSION.saturating_add(1)]),
            None
        );
    }

    #[test]
    fn hello_requires_versions_and_auth_methods() {
        assert!(Hello::from_payload(br#"{"auth_methods":["bearer"]}"#).is_err());
        assert!(Hello::from_payload(br#"{"versions":[1]}"#).is_err());
    }

    #[test]
    fn hello_ack_can_embed_topology() {
        let topology = Topology {
            epoch: 7,
            primary: Endpoint {
                addr: "127.0.0.1:5050".to_string(),
                region: "local".to_string(),
            },
            replicas: vec![ReplicaInfo {
                addr: "127.0.0.1:5051".to_string(),
                region: "local".to_string(),
                healthy: true,
                lag_ms: 3,
                last_applied_lsn: 9,
                rebootstrapping: false,
            }],
        };
        let bytes = build_hello_ack(1, "bearer", 0, Some(&topology));
        let json: JsonValue = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["version"], 1);
        assert!(json["topology"].as_str().is_some());
        let ack = HelloAck::from_payload(&bytes).unwrap();
        assert_eq!(ack.version, 1);
        assert_eq!(ack.auth, "bearer");
        assert_eq!(ack.features, 0);
        assert!(ack.topology.is_some());
    }

    #[test]
    fn auth_response_builders_are_pinned() {
        assert!(build_auth_response_anonymous_payload().is_empty());

        let bearer: JsonValue =
            serde_json::from_slice(&build_auth_response_bearer_payload("token")).unwrap();
        assert_eq!(bearer["token"], "token");

        let oauth: JsonValue =
            serde_json::from_slice(&build_auth_response_oauth_jwt_payload("jwt")).unwrap();
        assert_eq!(oauth["jwt"], "jwt");
    }

    #[test]
    fn auth_ok_and_fail_parse_payloads() {
        let ok = AuthOk::from_payload(&build_auth_ok_payload("s1", "alice", "admin", 3)).unwrap();
        assert_eq!(ok.session_id, "s1");
        assert_eq!(ok.username.as_deref(), Some("alice"));
        assert_eq!(ok.role.as_deref(), Some("admin"));
        assert_eq!(ok.features, 3);
        assert_eq!(ok.server_signature.as_deref(), None);

        let scram_ok = AuthOk::from_payload(&build_scram_auth_ok_payload(
            "s1", "alice", "admin", 3, b"sig",
        ))
        .unwrap();
        assert_eq!(scram_ok.server_signature.as_deref(), Some("c2ln"));

        let fail = AuthFail::from_payload(&build_auth_fail_payload("nope")).unwrap();
        assert_eq!(fail.reason, "nope");
    }

    #[test]
    fn handshake_frame_builders_pin_message_kinds() {
        let hello_ack = build_hello_ack_frame(7, 1, "anonymous", 3, None).unwrap();
        assert_eq!(hello_ack.kind, MessageKind::HelloAck);
        assert_eq!(hello_ack.correlation_id, 7);
        assert_eq!(
            HelloAck::from_payload(&hello_ack.payload).unwrap().auth,
            "anonymous"
        );

        let auth_ok =
            build_auth_ok_frame_from_payload(8, build_auth_ok_payload("s1", "alice", "admin", 3))
                .unwrap();
        assert_eq!(auth_ok.kind, MessageKind::AuthOk);
        assert_eq!(auth_ok.correlation_id, 8);
        assert_eq!(
            AuthOk::from_payload(&auth_ok.payload)
                .unwrap()
                .username
                .as_deref(),
            Some("alice")
        );

        let auth_fail = build_auth_fail_frame(9, "nope").unwrap();
        assert_eq!(auth_fail.kind, MessageKind::AuthFail);
        assert_eq!(auth_fail.correlation_id, 9);
        assert_eq!(
            AuthFail::from_payload(&auth_fail.payload).unwrap().reason,
            "nope"
        );
    }

    #[test]
    fn auth_response_parsers_are_pinned() {
        assert_eq!(
            parse_auth_response_bearer_token(&build_auth_response_bearer_payload("token")).unwrap(),
            "token"
        );
        assert_eq!(
            parse_auth_response_oauth_jwt(&build_auth_response_oauth_jwt_payload("jwt")).unwrap(),
            "jwt"
        );
        assert!(parse_auth_response_bearer_token(br#"{"jwt":"x"}"#).is_err());
    }

    #[test]
    fn scram_wire_messages_round_trip() {
        let (user, nonce, bare) = parse_scram_client_first(b"n,,n=alice,r=client").unwrap();
        assert_eq!(user, "alice");
        assert_eq!(nonce, "client");
        assert_eq!(bare, "n=alice,r=client");

        let server_first = build_scram_server_first("client", "server", b"salt", 4096);
        assert_eq!(server_first, "r=clientserver,s=c2FsdA==,i=4096");

        let proof = base64_std(b"proof");
        let final_msg = format!("c=biws,r=clientserver,p={proof}");
        let (combined, decoded_proof, without_proof) =
            parse_scram_client_final(final_msg.as_bytes()).unwrap();
        assert_eq!(combined, "clientserver");
        assert_eq!(decoded_proof, b"proof");
        assert_eq!(without_proof, "c=biws,r=clientserver");
    }
}
