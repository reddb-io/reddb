//! HTTP OAuth-JWT smoke test — exercises the bearer-extraction
//! branching that hands JWT-shaped tokens to `OAuthValidator` before
//! falling back to AuthStore.
//!
//! Coverage:
//!   * Valid JWT (correct issuer + audience + signature + future exp)
//!     authenticates and reaches /auth/whoami.
//!   * Expired JWT → 401.
//!   * Wrong-issuer JWT → 401.
//!   * Wrong-audience JWT → 401.
//!   * Unknown-kid JWT → 401.
//!   * Opaque AuthStore session token still works (fallback path).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reddb::auth::store::AuthStore;
use reddb::auth::{AuthConfig, Jwk, OAuthConfig, OAuthIdentityMode, OAuthValidator, Role};
use reddb::server::RedDBServer;
use reddb::RedDBRuntime;

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// Base64url-encode without padding.
fn b64url(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    // Minimal, dependency-free base64url encoder.
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len() * 4 / 3 + 4);
    let mut i = 0;
    while i + 3 <= bytes.len() {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8) | (bytes[i + 2] as u32);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        out.push(TABLE[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = bytes.len() - i;
    if rem == 1 {
        let n = (bytes[i] as u32) << 16;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
    } else if rem == 2 {
        let n = ((bytes[i] as u32) << 16) | ((bytes[i + 1] as u32) << 8);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
    }
    let _ = write!(out, ""); // placate unused import
    out
}

/// Build a JWT compact serialization. Signature is opaque (the test
/// validator's verifier closure unconditionally accepts) — we just
/// need the parser to produce a valid `DecodedJwt`.
fn make_jwt(header: &str, payload: &str) -> String {
    let h = b64url(header.as_bytes());
    let p = b64url(payload.as_bytes());
    let s = b64url(b"sig");
    format!("{h}.{p}.{s}")
}

fn build_runtime_with_oauth(oauth: Option<Arc<OAuthValidator>>) -> (RedDBRuntime, Arc<AuthStore>) {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    let cfg = AuthConfig {
        enabled: true,
        session_ttl_secs: 60,
        require_auth: true,
        auto_encrypt_storage: false,
        vault_enabled: false,
        cert: Default::default(),
        oauth: Default::default(),
    };
    let auth_store = Arc::new(AuthStore::new(cfg));
    auth_store
        .create_user("alice", "password123", Role::Write)
        .unwrap();
    rt.set_auth_store(Arc::clone(&auth_store));
    if let Some(v) = oauth {
        rt.set_oauth_validator(Some(v));
    }
    (rt, auth_store)
}

fn build_oauth_validator() -> Arc<OAuthValidator> {
    let cfg = OAuthConfig {
        enabled: true,
        issuer: "https://id.example.com".to_string(),
        audience: "reddb".to_string(),
        jwks_url: String::new(),
        identity_mode: OAuthIdentityMode::SubClaim,
        role_claim: Some("role".into()),
        tenant_claim: None,
        default_role: Role::Read,
        map_to_existing_users: false,
        accept_bearer: true,
    };
    let verifier = Box::new(|_jwk: &Jwk, _input: &[u8], _sig: &[u8]| Ok(()));
    let v = OAuthValidator::with_verifier(cfg, verifier);
    v.set_jwks(vec![Jwk {
        kid: "k1".to_string(),
        alg: "RS256".to_string(),
        key_bytes: Vec::new(),
    }]);
    Arc::new(v)
}

fn spawn_http(rt: RedDBRuntime, auth_store: Arc<AuthStore>) -> String {
    let server = RedDBServer::new(rt).with_auth(auth_store);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    thread::sleep(Duration::from_millis(80));
    addr.to_string()
}

fn http_get(addr: &str, path: &str, bearer: Option<&str>) -> (u16, String) {
    let mut tcp = TcpStream::connect(addr).expect("connect");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let auth_header = bearer
        .map(|b| format!("Authorization: Bearer {b}\r\n"))
        .unwrap_or_default();
    let req =
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n{auth_header}Connection: close\r\n\r\n");
    tcp.write_all(req.as_bytes()).unwrap();
    tcp.flush().unwrap();
    let mut buf = Vec::new();
    let _ = tcp.read_to_end(&mut buf);
    let resp = String::from_utf8_lossy(&buf).to_string();
    let status = resp
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    (status, resp)
}

#[test]
fn valid_jwt_is_accepted_via_oauth_validator() {
    let validator = build_oauth_validator();
    let (rt, auth_store) = build_runtime_with_oauth(Some(validator));
    let addr = spawn_http(rt, auth_store);

    let now = now_secs();
    let header = r#"{"alg":"RS256","kid":"k1","typ":"JWT"}"#;
    let payload = format!(
        r#"{{"iss":"https://id.example.com","sub":"alice","aud":"reddb","exp":{exp},"nbf":{nbf},"role":"write"}}"#,
        exp = now + 3600,
        nbf = now - 60,
    );
    let token = make_jwt(header, &payload);

    let (status, body) = http_get(&addr, "/auth/whoami", Some(&token));
    assert_eq!(status, 200, "expected 200, got body:\n{body}");
    assert!(body.contains("\"username\":\"alice\""), "body = {body}");
    assert!(body.contains("\"method\":\"oauth_jwt\""), "body = {body}");
}

#[test]
fn expired_jwt_rejected_with_401() {
    let validator = build_oauth_validator();
    let (rt, auth_store) = build_runtime_with_oauth(Some(validator));
    let addr = spawn_http(rt, auth_store);

    let now = now_secs();
    let header = r#"{"alg":"RS256","kid":"k1","typ":"JWT"}"#;
    let payload = format!(
        r#"{{"iss":"https://id.example.com","sub":"alice","aud":"reddb","exp":{exp},"nbf":{nbf}}}"#,
        exp = now - 3600,
        nbf = now - 7200,
    );
    let token = make_jwt(header, &payload);

    let (status, _body) = http_get(&addr, "/auth/whoami", Some(&token));
    assert_eq!(status, 401);
}

#[test]
fn wrong_issuer_jwt_rejected() {
    let validator = build_oauth_validator();
    let (rt, auth_store) = build_runtime_with_oauth(Some(validator));
    let addr = spawn_http(rt, auth_store);

    let now = now_secs();
    let header = r#"{"alg":"RS256","kid":"k1","typ":"JWT"}"#;
    let payload = format!(
        r#"{{"iss":"https://attacker.example.com","sub":"alice","aud":"reddb","exp":{exp}}}"#,
        exp = now + 3600,
    );
    let token = make_jwt(header, &payload);

    let (status, _body) = http_get(&addr, "/auth/whoami", Some(&token));
    assert_eq!(status, 401);
}

#[test]
fn wrong_audience_jwt_rejected() {
    let validator = build_oauth_validator();
    let (rt, auth_store) = build_runtime_with_oauth(Some(validator));
    let addr = spawn_http(rt, auth_store);

    let now = now_secs();
    let header = r#"{"alg":"RS256","kid":"k1","typ":"JWT"}"#;
    let payload = format!(
        r#"{{"iss":"https://id.example.com","sub":"alice","aud":"other-app","exp":{exp}}}"#,
        exp = now + 3600,
    );
    let token = make_jwt(header, &payload);

    let (status, _body) = http_get(&addr, "/auth/whoami", Some(&token));
    assert_eq!(status, 401);
}

#[test]
fn unknown_kid_jwt_rejected() {
    let validator = build_oauth_validator();
    let (rt, auth_store) = build_runtime_with_oauth(Some(validator));
    let addr = spawn_http(rt, auth_store);

    let now = now_secs();
    let header = r#"{"alg":"RS256","kid":"revoked","typ":"JWT"}"#;
    let payload = format!(
        r#"{{"iss":"https://id.example.com","sub":"alice","aud":"reddb","exp":{exp}}}"#,
        exp = now + 3600,
    );
    let token = make_jwt(header, &payload);

    let (status, _body) = http_get(&addr, "/auth/whoami", Some(&token));
    assert_eq!(status, 401);
}

#[test]
fn opaque_api_key_still_works_with_oauth_configured() {
    let validator = build_oauth_validator();
    let (rt, auth_store) = build_runtime_with_oauth(Some(validator));
    let addr = spawn_http(rt.clone(), auth_store.clone());

    let api_key = auth_store
        .create_api_key("alice", "ci-token", Role::Write)
        .expect("create api key");
    assert!(api_key.key.starts_with("rk_"));

    let (status, body) = http_get(&addr, "/auth/whoami", Some(&api_key.key));
    assert_eq!(
        status, 200,
        "fallback bearer path must succeed; body={body}"
    );
    assert!(body.contains("\"username\":\"alice\""));
}

#[test]
fn jwt_shaped_token_falls_back_to_authstore_when_no_oauth_configured() {
    // No OAuthValidator wired. JWT-shaped token must be tried against
    // AuthStore (where it doesn't exist) → 401.
    let (rt, auth_store) = build_runtime_with_oauth(None);
    let addr = spawn_http(rt, auth_store);

    let header = r#"{"alg":"RS256","kid":"k1","typ":"JWT"}"#;
    let payload = r#"{"iss":"x","sub":"y","aud":"z"}"#;
    let token = make_jwt(header, payload);

    let (status, _body) = http_get(&addr, "/auth/whoami", Some(&token));
    assert_eq!(status, 401);
}
