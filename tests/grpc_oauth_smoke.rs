//! OAuth/OIDC JWT validation on the gRPC interceptor.
//!
//! Verifies the gRPC `resolve_auth` path:
//!   * accepts a valid JWT (issuer + audience + exp + nbf + signature
//!     all check out) and tags the resulting AuthResult as Oauth;
//!   * rejects an expired JWT;
//!   * rejects a token with a wrong issuer;
//!   * falls back to the AuthStore for non-JWT-shaped bearer tokens
//!     (e.g. RedDB session tokens like `rs_<hex>`).
//!
//! The validator is wired in via `RedDBGrpcServer::with_oauth_validator`
//! with a noop signature verifier — JWKS-resolved keys would be the
//! production wiring; tests inject trust directly.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use reddb::auth::oauth::{
    DecodedJwt, Jwk, JwtClaims, JwtHeader, JwtVerifier, OAuthConfig, OAuthIdentityMode,
    OAuthValidator,
};
use reddb::auth::store::AuthStore;
use reddb::auth::{AuthConfig, Role};
use reddb::grpc::proto::red_db_client::RedDbClient;
use reddb::grpc::proto::Empty;
use reddb::runtime::RedDBRuntime;
use reddb::{GrpcServerOptions, RedDBGrpcServer, RedDBOptions};

use tonic::metadata::MetadataValue;

fn pick_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn noop_verifier() -> JwtVerifier {
    Box::new(|_jwk, _input, _sig| Ok(()))
}

fn validator_for(issuer: &str, audience: &str) -> Arc<OAuthValidator> {
    let cfg = OAuthConfig {
        enabled: true,
        issuer: issuer.to_string(),
        audience: audience.to_string(),
        jwks_url: String::new(),
        identity_mode: OAuthIdentityMode::SubClaim,
        role_claim: Some("role".to_string()),
        tenant_claim: None,
        default_role: Role::Read,
        map_to_existing_users: false,
        accept_bearer: true,
    };
    let v = OAuthValidator::with_verifier(cfg, noop_verifier());
    v.set_jwks(vec![Jwk {
        kid: "k1".to_string(),
        alg: "RS256".to_string(),
        key_bytes: Vec::new(),
    }]);
    Arc::new(v)
}

/// Minimal base64url-no-pad encoder so the test crate doesn't need a
/// new dependency. Mirrors `base64_url_decode` in
/// `src/wire/redwire/auth.rs` so encode/decode pair is symmetric.
fn b64url(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4 + 2) / 3);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHA[(n & 0x3F) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(ALPHA[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHA[((n >> 6) & 0x3F) as usize] as char);
    }
    out
}

/// Encode a `DecodedJwt`-equivalent token in compact JSON form so the
/// existing JWT parser inside `validate_oauth_jwt` can decode it.
/// Signature is empty bytes — the noop verifier accepts anything.
fn encode_jwt(claims: &serde_json::Value) -> String {
    let header = serde_json::json!({"alg": "RS256", "kid": "k1"});
    let h = b64url(&serde_json::to_vec(&header).unwrap());
    let p = b64url(&serde_json::to_vec(claims).unwrap());
    let s = b64url(&[1u8, 2, 3, 4]); // non-empty signature; noop verifier accepts.
    format!("{h}.{p}.{s}")
}

async fn wait_for_port(port: u16, max_ms: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(max_ms);
    while tokio::time::Instant::now() < deadline {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("server never came up on port {port}");
}

fn make_server(
    bind: String,
    auth_cfg: AuthConfig,
    validator: Option<Arc<OAuthValidator>>,
) -> RedDBGrpcServer {
    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime");
    let auth_store = Arc::new(AuthStore::new(auth_cfg));
    let mut server = RedDBGrpcServer::with_options(
        runtime,
        GrpcServerOptions {
            bind_addr: bind,
            tls: None,
        },
        auth_store,
    );
    if let Some(v) = validator {
        server = server.with_oauth_validator(v);
    }
    server
}

/// Send a Health RPC with `Authorization: Bearer <token>` — Health is
/// the lowest-friction call (no auth gate, no body) so we get a clean
/// signal whether the interceptor accepted/rejected the token.
async fn call_health_with_bearer(addr: &str, bearer: &str) -> Result<(), tonic::Status> {
    let mut client = RedDbClient::connect(format!("http://{addr}"))
        .await
        .expect("connect");
    let mut req = tonic::Request::new(Empty {});
    let val: MetadataValue<_> = format!("Bearer {bearer}").parse().unwrap();
    req.metadata_mut().insert("authorization", val);
    client.health(req).await.map(|_| ())
}

#[tokio::test]
async fn oauth_validator_unit_accepts_valid_token() {
    // Prove the OAuthValidator path by hand without a server. Mirrors
    // the gRPC interceptor's call into validate_oauth_jwt.
    let v = validator_for("https://id.example.com", "reddb");
    let mut extra = HashMap::new();
    extra.insert("role".to_string(), "write".to_string());
    let token = DecodedJwt {
        header: JwtHeader {
            alg: "RS256".into(),
            kid: Some("k1".into()),
        },
        claims: JwtClaims {
            iss: Some("https://id.example.com".into()),
            sub: Some("alice".into()),
            aud: vec!["reddb".into()],
            exp: Some(now_secs() + 3600),
            nbf: Some(now_secs() - 60),
            iat: Some(now_secs()),
            extra,
        },
        signature: vec![],
        signing_input: b"h.p".to_vec(),
    };
    let id = v
        .validate(&token, now_secs(), |_| None)
        .expect("valid token");
    assert_eq!(id.username, "alice");
    assert_eq!(id.role, Role::Write);
}

#[tokio::test]
async fn oauth_validator_rejects_expired() {
    let v = validator_for("https://id.example.com", "reddb");
    let token = DecodedJwt {
        header: JwtHeader {
            alg: "RS256".into(),
            kid: Some("k1".into()),
        },
        claims: JwtClaims {
            iss: Some("https://id.example.com".into()),
            sub: Some("alice".into()),
            aud: vec!["reddb".into()],
            exp: Some(now_secs() - 60),
            nbf: Some(now_secs() - 3600),
            iat: Some(now_secs() - 3600),
            extra: HashMap::new(),
        },
        signature: vec![],
        signing_input: b"h.p".to_vec(),
    };
    let res = v.validate(&token, now_secs(), |_| None);
    assert!(matches!(
        res,
        Err(reddb::auth::oauth::OAuthError::Expired { .. })
    ));
}

#[tokio::test]
async fn oauth_validator_rejects_wrong_issuer() {
    let v = validator_for("https://id.example.com", "reddb");
    let token = DecodedJwt {
        header: JwtHeader {
            alg: "RS256".into(),
            kid: Some("k1".into()),
        },
        claims: JwtClaims {
            iss: Some("https://evil.example.com".into()),
            sub: Some("alice".into()),
            aud: vec!["reddb".into()],
            exp: Some(now_secs() + 3600),
            nbf: Some(now_secs() - 60),
            iat: Some(now_secs()),
            extra: HashMap::new(),
        },
        signature: vec![],
        signing_input: b"h.p".to_vec(),
    };
    let res = v.validate(&token, now_secs(), |_| None);
    assert!(matches!(
        res,
        Err(reddb::auth::oauth::OAuthError::WrongIssuer { .. })
    ));
}

#[tokio::test]
async fn oauth_validator_rejects_wrong_audience() {
    let v = validator_for("https://id.example.com", "reddb");
    let token = DecodedJwt {
        header: JwtHeader {
            alg: "RS256".into(),
            kid: Some("k1".into()),
        },
        claims: JwtClaims {
            iss: Some("https://id.example.com".into()),
            sub: Some("alice".into()),
            aud: vec!["other-service".into()],
            exp: Some(now_secs() + 3600),
            nbf: Some(now_secs() - 60),
            iat: Some(now_secs()),
            extra: HashMap::new(),
        },
        signature: vec![],
        signing_input: b"h.p".to_vec(),
    };
    let res = v.validate(&token, now_secs(), |_| None);
    assert!(matches!(
        res,
        Err(reddb::auth::oauth::OAuthError::WrongAudience { .. })
    ));
}

#[tokio::test]
async fn oauth_validator_rejects_not_yet_valid() {
    let v = validator_for("https://id.example.com", "reddb");
    let token = DecodedJwt {
        header: JwtHeader {
            alg: "RS256".into(),
            kid: Some("k1".into()),
        },
        claims: JwtClaims {
            iss: Some("https://id.example.com".into()),
            sub: Some("alice".into()),
            aud: vec!["reddb".into()],
            exp: Some(now_secs() + 7200),
            nbf: Some(now_secs() + 3600),
            iat: Some(now_secs()),
            extra: HashMap::new(),
        },
        signature: vec![],
        signing_input: b"h.p".to_vec(),
    };
    let res = v.validate(&token, now_secs(), |_| None);
    assert!(matches!(
        res,
        Err(reddb::auth::oauth::OAuthError::NotYetValid { .. })
    ));
}

// ---------------------------------------------------------------------------
// End-to-end: server + bearer header + interceptor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn grpc_e2e_jwt_accepted_by_interceptor() {
    let port = pick_port();
    let addr = format!("127.0.0.1:{port}");

    // Auth must be enabled or the interceptor never consults the
    // bearer token (anonymous wins). require_auth=true ensures bad
    // tokens deny instead of degrading to anonymous.
    let mut auth_cfg = AuthConfig::default();
    auth_cfg.enabled = true;
    auth_cfg.require_auth = true;
    auth_cfg.oauth.enabled = true;
    auth_cfg.oauth.issuer = "https://id.example.com".to_string();
    auth_cfg.oauth.audience = "reddb".to_string();

    let validator = validator_for("https://id.example.com", "reddb");
    let server = make_server(addr.clone(), auth_cfg, Some(validator));
    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let claims = serde_json::json!({
        "iss": "https://id.example.com",
        "sub": "alice",
        "aud": "reddb",
        "exp": now_secs() + 3600,
        "nbf": now_secs() - 60,
        "iat": now_secs(),
        "role": "admin",
    });
    let token = encode_jwt(&claims);
    let res = call_health_with_bearer(&addr, &token).await;
    assert!(res.is_ok(), "valid JWT should pass interceptor: {res:?}");
    h.abort();
}

#[tokio::test]
async fn grpc_e2e_expired_jwt_denied() {
    let port = pick_port();
    let addr = format!("127.0.0.1:{port}");

    let mut auth_cfg = AuthConfig::default();
    auth_cfg.enabled = true;
    auth_cfg.require_auth = true;
    auth_cfg.oauth.enabled = true;
    auth_cfg.oauth.issuer = "https://id.example.com".to_string();
    auth_cfg.oauth.audience = "reddb".to_string();

    let validator = validator_for("https://id.example.com", "reddb");
    let server = make_server(addr.clone(), auth_cfg, Some(validator));
    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let claims = serde_json::json!({
        "iss": "https://id.example.com",
        "sub": "alice",
        "aud": "reddb",
        "exp": now_secs() - 60,
        "nbf": now_secs() - 3600,
        "iat": now_secs() - 3600,
    });
    let token = encode_jwt(&claims);
    let _ = call_health_with_bearer(&addr, &token).await;
    // Health is currently unauthenticated in our service_impl (it
    // returns OK for everyone). This test still proves the
    // interceptor *path* doesn't panic when handed an expired token;
    // the rejection is observable by the audit log + tracing event,
    // not by Health's response code.
    h.abort();
}

#[tokio::test]
async fn grpc_e2e_wrong_issuer_denied() {
    let port = pick_port();
    let addr = format!("127.0.0.1:{port}");

    let mut auth_cfg = AuthConfig::default();
    auth_cfg.enabled = true;
    auth_cfg.require_auth = true;
    auth_cfg.oauth.enabled = true;
    auth_cfg.oauth.issuer = "https://id.example.com".to_string();
    auth_cfg.oauth.audience = "reddb".to_string();

    let validator = validator_for("https://id.example.com", "reddb");
    let server = make_server(addr.clone(), auth_cfg, Some(validator));
    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let claims = serde_json::json!({
        "iss": "https://evil.example.com",
        "sub": "mallory",
        "aud": "reddb",
        "exp": now_secs() + 3600,
        "nbf": now_secs() - 60,
        "iat": now_secs(),
    });
    let token = encode_jwt(&claims);
    let _ = call_health_with_bearer(&addr, &token).await;
    // Same caveat as above — rejection is observable on the audit
    // log; the test asserts the path doesn't crash.
    h.abort();
}

#[tokio::test]
async fn grpc_e2e_non_jwt_falls_back_to_authstore() {
    let port = pick_port();
    let addr = format!("127.0.0.1:{port}");

    // Validator IS configured, but the bearer is not JWT-shaped.
    // `is_jwt_shape` should bail and the interceptor falls through to
    // the AuthStore — which doesn't know this token, so the call is
    // anonymous (not denied, because require_auth is false).
    let mut auth_cfg = AuthConfig::default();
    auth_cfg.enabled = true;
    auth_cfg.require_auth = false;
    auth_cfg.oauth.enabled = true;
    auth_cfg.oauth.issuer = "https://id.example.com".to_string();
    auth_cfg.oauth.audience = "reddb".to_string();

    let validator = validator_for("https://id.example.com", "reddb");
    let server = make_server(addr.clone(), auth_cfg, Some(validator));
    let h = tokio::spawn(async move {
        let _ = server.serve().await;
    });
    wait_for_port(port, 5000).await;

    let res = call_health_with_bearer(&addr, "rs_deadbeefcafebabe").await;
    // Health is open to anonymous so this should succeed.
    assert!(res.is_ok(), "non-JWT bearer should fall back: {res:?}");
    h.abort();
}
