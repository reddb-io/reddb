//! End-to-end RedWire OAuth-JWT smoke.
//!
//! Engine-side smoke that exercises the full handshake without
//! the driver. Boots:
//!   1. An in-process JWKS HTTP server (port 0).
//!   2. An in-process RedWire listener (port 0) wired to an
//!      `OAuthValidator` with the test issuer + JWKS URL +
//!      audience and the public key seeded.
//!
//! Then drives the handshake by hand:
//!   magic byte → version → Hello → HelloAck →
//!   AuthResponse{jwt} → AuthOk / AuthFail.
//!
//! Threat-model coverage:
//!   - Happy path        — valid token, AuthOk with sub.
//!   - Expired           — exp in the past → AuthFail.
//!   - Wrong issuer      — iss does not match config → AuthFail.
//!   - Wrong audience    — aud does not contain config → AuthFail.
//!   - Tampered signature — JWT signed with a different key → AuthFail.
//!   - Unknown kid       — JWT carries a kid the validator never seeded → AuthFail.

mod common;

use std::sync::Arc;

use common::{jwks_server, jwt_mint};
use jwt_mint::Claims;

use reddb::api::RedDBOptions;
use reddb::auth::{OAuthConfig, OAuthIdentityMode, OAuthValidator, Role};
use reddb::wire::redwire::{
    decode_frame, encode_frame, start_redwire_listener, Frame, MessageKind, RedWireConfig,
    REDWIRE_MAGIC,
};
use reddb::RedDBRuntime;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const AUDIENCE: &str = "reddb-redwire-test";

struct ServerHandles {
    addr: std::net::SocketAddr,
    _jwks: jwks_server::JwksServer,
    issuer: String,
}

/// Spin up a RedWire listener wired to an OAuthValidator that
/// trusts the static test JWKS.
async fn start_server() -> ServerHandles {
    let jwks = jwks_server::spawn(jwt_mint::build_jwks()).await;

    let mut config = OAuthConfig::default();
    config.enabled = true;
    config.issuer = jwks.issuer.clone();
    config.audience = AUDIENCE.to_string();
    config.jwks_url = jwks.jwks_url.clone();
    config.identity_mode = OAuthIdentityMode::SubClaim;
    config.role_claim = Some("role".to_string());
    config.default_role = Role::Read;
    config.map_to_existing_users = false;
    config.accept_bearer = true;

    let validator = OAuthValidator::with_verifier(config, jwt_mint::build_verifier());
    validator.set_jwks(vec![jwt_mint::build_jwk_for_validator()]);

    // Bind redwire listener on :0; capture addr; let it serve.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind redwire");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);

    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("rt"));
    let cfg = RedWireConfig {
        bind_addr: addr.to_string(),
        auth_store: None,
        oauth: Some(Arc::new(validator)),
    };
    let issuer = jwks.issuer.clone();
    tokio::spawn(async move {
        let _ = start_redwire_listener(cfg, runtime).await;
    });
    // Give the listener a moment to bind.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    ServerHandles {
        addr,
        _jwks: jwks,
        issuer,
    }
}

/// Default valid claims for the configured issuer + audience.
fn happy_claims(issuer: &str) -> Claims {
    let now = jwt_mint::now_unix_secs();
    Claims {
        iss: issuer.to_string(),
        sub: "alice@example.com".to_string(),
        aud: AUDIENCE.to_string(),
        exp: now + 60,
        nbf: now - 1,
        iat: now,
        role: Some("admin".to_string()),
    }
}

/// Drive the v2 handshake:
///   magic → minor version → Hello → HelloAck →
///   AuthResponse{jwt} → AuthOk / AuthFail.
///
/// Returns the final frame the server sent so the test can
/// assert AuthOk vs AuthFail and inspect the payload.
async fn run_handshake(addr: std::net::SocketAddr, jwt: &str) -> Frame {
    let mut sock = TcpStream::connect(addr).await.expect("connect");
    sock.write_all(&[REDWIRE_MAGIC, 0x01]).await.expect("magic");

    // Hello — advertise oauth-jwt only so the picker chooses it.
    let hello_body = br#"{"versions":[1],"auth_methods":["oauth-jwt"],"features":0,"client_name":"oauth-smoke"}"#.to_vec();
    let hello = Frame::new(MessageKind::Hello, 1, hello_body);
    sock.write_all(&encode_frame(&hello))
        .await
        .expect("write hello");

    // Read HelloAck; assert chosen=oauth-jwt.
    let ack = read_frame(&mut sock).await;
    assert_eq!(
        ack.kind,
        MessageKind::HelloAck,
        "server should send HelloAck"
    );
    let ack_obj: serde_json::Value = serde_json::from_slice(&ack.payload).expect("ack json");
    assert_eq!(
        ack_obj["auth"].as_str(),
        Some("oauth-jwt"),
        "server should pick oauth-jwt; got {ack_obj}"
    );

    // AuthResponse with the JWT.
    let body = serde_json::json!({ "jwt": jwt });
    let body_bytes = serde_json::to_vec(&body).expect("encode auth body");
    let resp = Frame::new(MessageKind::AuthResponse, 2, body_bytes);
    sock.write_all(&encode_frame(&resp))
        .await
        .expect("write auth response");

    // Final frame — AuthOk or AuthFail.
    read_frame(&mut sock).await
}

async fn read_frame(sock: &mut TcpStream) -> Frame {
    let mut header = [0u8; 16];
    sock.read_exact(&mut header).await.expect("read header");
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let mut buf = vec![0u8; len];
    buf[..16].copy_from_slice(&header);
    if len > 16 {
        sock.read_exact(&mut buf[16..]).await.expect("read body");
    }
    decode_frame(&buf).expect("decode").0
}

// -----------------------------------------------------------------
// Happy path.
// -----------------------------------------------------------------

#[tokio::test]
async fn oauth_jwt_handshake_happy_path() {
    let server = start_server().await;
    let claims = happy_claims(&server.issuer);
    let jwt = jwt_mint::mint_rs256(&claims);

    let final_frame = run_handshake(server.addr, &jwt).await;
    assert_eq!(
        final_frame.kind,
        MessageKind::AuthOk,
        "valid JWT should yield AuthOk; payload: {}",
        String::from_utf8_lossy(&final_frame.payload)
    );

    let ok: serde_json::Value = serde_json::from_slice(&final_frame.payload).expect("AuthOk json");
    assert_eq!(
        ok["username"].as_str(),
        Some("alice@example.com"),
        "AuthOk username should match JWT sub; got {ok}"
    );
    assert_eq!(
        ok["role"].as_str(),
        Some("admin"),
        "role claim 'admin' should map to Role::Admin"
    );
    assert!(
        ok["session_id"].as_str().is_some_and(|s| !s.is_empty()),
        "AuthOk should carry a non-empty session_id"
    );
    eprintln!(
        "happy path AuthOk payload = {}",
        String::from_utf8_lossy(&final_frame.payload)
    );
}

// -----------------------------------------------------------------
// Negative: expired JWT.
// -----------------------------------------------------------------

#[tokio::test]
async fn oauth_jwt_handshake_rejects_expired_token() {
    let server = start_server().await;
    let mut claims = happy_claims(&server.issuer);
    let now = jwt_mint::now_unix_secs();
    claims.exp = now - 60;
    claims.nbf = now - 120;
    claims.iat = now - 120;
    let jwt = jwt_mint::mint_rs256(&claims);

    let final_frame = run_handshake(server.addr, &jwt).await;
    assert_eq!(final_frame.kind, MessageKind::AuthFail);
    let reason = parse_reason(&final_frame.payload);
    assert!(
        reason.contains("expired"),
        "AuthFail reason should mention expiry; got {reason}"
    );
}

// -----------------------------------------------------------------
// Negative: wrong issuer.
// -----------------------------------------------------------------

#[tokio::test]
async fn oauth_jwt_handshake_rejects_wrong_issuer() {
    let server = start_server().await;
    let mut claims = happy_claims(&server.issuer);
    claims.iss = "https://evil.example.com".to_string();
    let jwt = jwt_mint::mint_rs256(&claims);

    let final_frame = run_handshake(server.addr, &jwt).await;
    assert_eq!(final_frame.kind, MessageKind::AuthFail);
    let reason = parse_reason(&final_frame.payload);
    assert!(
        reason.contains("issuer") || reason.contains("iss"),
        "AuthFail reason should mention issuer mismatch; got {reason}"
    );
}

// -----------------------------------------------------------------
// Negative: wrong audience.
// -----------------------------------------------------------------

#[tokio::test]
async fn oauth_jwt_handshake_rejects_wrong_audience() {
    let server = start_server().await;
    let mut claims = happy_claims(&server.issuer);
    claims.aud = "some-other-service".to_string();
    let jwt = jwt_mint::mint_rs256(&claims);

    let final_frame = run_handshake(server.addr, &jwt).await;
    assert_eq!(final_frame.kind, MessageKind::AuthFail);
    let reason = parse_reason(&final_frame.payload);
    assert!(
        reason.contains("audience") || reason.contains("aud"),
        "AuthFail reason should mention audience mismatch; got {reason}"
    );
}

// -----------------------------------------------------------------
// Negative: tampered signature (signed with a different key).
// -----------------------------------------------------------------

#[tokio::test]
async fn oauth_jwt_handshake_rejects_tampered_signature() {
    let server = start_server().await;
    let claims = happy_claims(&server.issuer);
    let jwt = jwt_mint::mint_rs256_with_bogus_key(&claims);

    let final_frame = run_handshake(server.addr, &jwt).await;
    assert_eq!(final_frame.kind, MessageKind::AuthFail);
    let reason = parse_reason(&final_frame.payload);
    assert!(
        reason.contains("signature") || reason.contains("verif"),
        "AuthFail reason should mention signature failure; got {reason}"
    );
}

// -----------------------------------------------------------------
// Negative: kid the validator never seeded — simulates the
// "rejects-list / unknown key" branch (server-side JWKS lookup
// returns no JWK so signature verification fails fast).
// -----------------------------------------------------------------

#[tokio::test]
async fn oauth_jwt_handshake_rejects_unknown_kid() {
    let server = start_server().await;
    let claims = happy_claims(&server.issuer);
    let jwt = jwt_mint::mint_rs256(&claims);

    // Rewrite the JWT header so the kid points at a key the
    // validator never seeded. Resigning is impossible (we don't
    // have a matching private key), but the validator rejects
    // before signature verification when no JWK matches kid+alg —
    // which is exactly the "kid is on the rejects list / never
    // imported" branch.
    let parts: Vec<&str> = jwt.split('.').collect();
    let mut new_header = serde_json::Map::new();
    new_header.insert("alg".into(), serde_json::Value::String("RS256".into()));
    new_header.insert(
        "kid".into(),
        serde_json::Value::String("rejected-kid".into()),
    );
    new_header.insert("typ".into(), serde_json::Value::String("JWT".into()));
    let header_bytes = serde_json::to_vec(&serde_json::Value::Object(new_header)).expect("header");
    let header_b64 = base64_url_no_pad(&header_bytes);
    let tampered = format!("{}.{}.{}", header_b64, parts[1], parts[2]);

    let final_frame = run_handshake(server.addr, &tampered).await;
    assert_eq!(final_frame.kind, MessageKind::AuthFail);
    let reason = parse_reason(&final_frame.payload);
    assert!(
        reason.contains("signature") || reason.contains("JWK") || reason.contains("kid"),
        "AuthFail reason should mention key lookup or signature; got {reason}"
    );
}

// -----------------------------------------------------------------
// Helpers.
// -----------------------------------------------------------------

fn parse_reason(payload: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(payload)
        .ok()
        .and_then(|v| v.as_object()?.get("reason")?.as_str().map(String::from))
        .unwrap_or_else(|| String::from_utf8_lossy(payload).to_string())
}

fn base64_url_no_pad(input: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    let chunks = input.chunks_exact(3);
    let rem = chunks.remainder();
    for c in chunks {
        let n = ((c[0] as u32) << 16) | ((c[1] as u32) << 8) | (c[2] as u32);
        out.push(A[((n >> 18) & 0x3F) as usize] as char);
        out.push(A[((n >> 12) & 0x3F) as usize] as char);
        out.push(A[((n >> 6) & 0x3F) as usize] as char);
        out.push(A[(n & 0x3F) as usize] as char);
    }
    match rem {
        [a] => {
            let n = (*a as u32) << 16;
            out.push(A[((n >> 18) & 0x3F) as usize] as char);
            out.push(A[((n >> 12) & 0x3F) as usize] as char);
        }
        [a, b] => {
            let n = ((*a as u32) << 16) | ((*b as u32) << 8);
            out.push(A[((n >> 18) & 0x3F) as usize] as char);
            out.push(A[((n >> 12) & 0x3F) as usize] as char);
            out.push(A[((n >> 6) & 0x3F) as usize] as char);
        }
        _ => {}
    }
    out
}
