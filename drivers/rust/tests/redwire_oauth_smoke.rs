//! Driver-side OAuth-JWT smoke for the RedWire transport.
//!
//! The Rust driver's `Auth` enum doesn't currently expose an
//! `OauthJwt` variant, so this smoke speaks the wire protocol
//! directly using `reddb_client::redwire::codec` + `Frame`.
//! Once an `Auth::OauthJwt(token)` variant ships in the driver,
//! this test should be rewritten to use it; the framing here
//! mirrors what that variant must produce.
//!
//! Coverage:
//!   - happy path                 — server returns AuthOk + sub
//!   - expired token              — server returns AuthFail
//!   - tampered signature         — server returns AuthFail
//!
//! Engine plumbing (JWKS server, JWT mint) is inlined here so
//! the driver crate doesn't depend on `tests/common/` from the
//! parent engine crate.

#![cfg(all(feature = "redwire", feature = "embedded"))]

use std::net::SocketAddr;
use std::sync::Arc;

use reddb::api::RedDBOptions;
use reddb::auth::{OAuthConfig, OAuthIdentityMode, OAuthValidator, Role};
use reddb::wire::redwire::{start_redwire_listener, RedWireConfig};
use reddb::RedDBRuntime;
use reddb_client::redwire::codec::{decode_frame, encode_frame};
use reddb_client::redwire::{Frame, MessageKind, MAGIC, SUPPORTED_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const AUDIENCE: &str = "reddb-driver-oauth-smoke";

const TEST_RSA_PRIVATE_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQCiYyHy8BHB5mBV
txAiYvOU8sVxJlNBsBmvqY8nyaeP8Hf17Pz9BJO2IV/vJODZpgymYtjmGlS/fa3/
hT/8aAMQbOk9llDErcbcuNqcOhY+4IRCeA7ovfUbAd3MMMty3Z/HuWLy+sjKMb9y
Bs/WeSXqNv/Zz870Xv2B5XImBroKkybrEwjyYEhioxDLwFEm/whl/Ep2HcJjxgPr
+OpcD84ZCRVWO8ibR9A4BZAgDszO6d5H9KjgGw/FlwAcp1r3ADj8m/uSBxv4pzpd
ACIwkVYMay7/6c7+hKBEPcQuP4Ej+hLbdWm82LsBrmNuBNt+YTvJ7MhseebBowvY
SGfoGxbpAgMBAAECggEABFtxmUGs0E2cryAa3DlYfNooxxj2qfAOOGLt1uz3xIp4
xY4G2ckqJ3xsxQ9xwxVMCJjlZgM12++E4DLUnTKzRlkNxxvF7gkVqW2CXCfI2gYP
NnNfPwp9zaw2pdh3VQ0yUNseFxP4mEhOcUJSiFg21rqEEfWcAX2dAsPD1NZgXpE6
Oku3Zg0qbeHJ/cFI9En4LhJLFEbbu+UVG0H9D79xctXvHnU1BucsevLKgB6Jjo/H
H1NnKcvaRMpnR6RGRTTkhXu6JFoJRQG2CMZljww/Tq1Cy98phxbqgRrI6e2hCRhN
O7Lf3XXWbo54F0rCYnSEOX5mLd9gYq9WUAoKGx4SGwKBgQDkgTK+PMLTZTyhVFiW
BDDUTYk2W0xtAlsX/k6bWl55+fSsRT9jhQRpd0R0Pt5/8zUZmmhq3HF6/8MSyJhT
KmOYcgJQoLdJkc64wv2n70gTTGktgLVctObvpssNItk9uwUAGZDMRXQOhF7OUkfL
Ru7fv/HBdck+tU6T2LNeb8X7MwKBgQC17UTbvVJEuTzRgdi+CWnCP0AIAvOwQe02
2hI0jLg3CbJerFf3Tf2dCTaGh28XfT/qJjqTANJCyoi5ttNtx6NvpSHWms/p1Wpe
vNAhrbKbowca4Yyb4usfwuR7ZilYDK8l1AB77z9r3jU2FI3ewuzpGmm27bqXTDTg
59/X4K/FcwKBgF2LGofQff1ma0ysJ9u5+XdgCnTrKT1TApGu9OUaOKT8k5JWgt2t
3aGDRs3D0vhUSv+hO2/LsNUmkOhGoD0jlEQbICF7uazveM4gXRD7nujvlfsfvp8m
G4guIt/MzVw9DI3+6U0Gfb1XqSwTePqZnj6Q6FpHasw2EuXph3x4i3cLAoGBAK9f
cimBb3TgPEiaKx3GZTTjVA5lChS2+L0Pqs0NeedUaaXp7UJw5DIlV3KHzAeQrbRB
9eUPvaC1LOgZ3ebNtDdDsEL4KcT3/foleV1929c8aPT4yFrdfFq5vRdXfDNsxspo
e679Ct4o7pKbbcd3kHmFBLNap6yBwdesrpOj/M0RAoGAJRayi/XN7rU41OkVOwcF
QK9V1zwKRDYU6bZRyHHmd++yp1w86Hr5zytqqWH5s6Gwcrj67OopRLLFAf75mTo+
JJG0153BPTEl87d6fm0OHUdMeypO3Y7mrVg8VC5Fhrjf8TbgjqVHWq32z4l9qT8l
s9mdObHlvBle/104fdejzJM=
-----END PRIVATE KEY-----
";

const TEST_RSA_PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAomMh8vARweZgVbcQImLz
lPLFcSZTQbAZr6mPJ8mnj/B39ez8/QSTtiFf7yTg2aYMpmLY5hpUv32t/4U//GgD
EGzpPZZQxK3G3LjanDoWPuCEQngO6L31GwHdzDDLct2fx7li8vrIyjG/cgbP1nkl
6jb/2c/O9F79geVyJga6CpMm6xMI8mBIYqMQy8BRJv8IZfxKdh3CY8YD6/jqXA/O
GQkVVjvIm0fQOAWQIA7MzuneR/So4BsPxZcAHKda9wA4/Jv7kgcb+Kc6XQAiMJFW
DGsu/+nO/oSgRD3ELj+BI/oS23VpvNi7Aa5jbgTbfmE7yezIbHnmwaML2Ehn6BsW
6QIDAQAB
-----END PUBLIC KEY-----
";

const TEST_RSA_N_B64URL: &str = "omMh8vARweZgVbcQImLzlPLFcSZTQbAZr6mPJ8mnj_B39ez8_QSTtiFf7yTg2aYMpmLY5hpUv32t_4U__GgDEGzpPZZQxK3G3LjanDoWPuCEQngO6L31GwHdzDDLct2fx7li8vrIyjG_cgbP1nkl6jb_2c_O9F79geVyJga6CpMm6xMI8mBIYqMQy8BRJv8IZfxKdh3CY8YD6_jqXA_OGQkVVjvIm0fQOAWQIA7MzuneR_So4BsPxZcAHKda9wA4_Jv7kgcb-Kc6XQAiMJFWDGsu_-nO_oSgRD3ELj-BI_oS23VpvNi7Aa5jbgTbfmE7yezIbHnmwaML2Ehn6BsW6Q";

const KID: &str = "test-kid";

#[derive(Debug, Clone)]
struct Claims {
    iss: String,
    sub: String,
    aud: String,
    exp: i64,
    nbf: i64,
    iat: i64,
    role: Option<String>,
}

impl Claims {
    fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        obj.insert("iss".into(), serde_json::Value::String(self.iss.clone()));
        obj.insert("sub".into(), serde_json::Value::String(self.sub.clone()));
        obj.insert("aud".into(), serde_json::Value::String(self.aud.clone()));
        obj.insert(
            "exp".into(),
            serde_json::Value::Number(self.exp.into()),
        );
        obj.insert(
            "nbf".into(),
            serde_json::Value::Number(self.nbf.into()),
        );
        obj.insert(
            "iat".into(),
            serde_json::Value::Number(self.iat.into()),
        );
        if let Some(role) = &self.role {
            obj.insert("role".into(), serde_json::Value::String(role.clone()));
        }
        serde_json::Value::Object(obj)
    }
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn mint_rs256(claims: &Claims) -> String {
    use jsonwebtoken::{Algorithm, EncodingKey, Header};
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(KID.to_string());
    let key = EncodingKey::from_rsa_pem(TEST_RSA_PRIVATE_PEM.as_bytes())
        .expect("test private PEM should parse");
    jsonwebtoken::encode(&header, &claims.to_json(), &key).expect("JWT mint")
}

fn build_verifier() -> reddb::auth::oauth::JwtVerifier {
    use jsonwebtoken::{Algorithm, DecodingKey, Validation};
    let decoding_key = DecodingKey::from_rsa_pem(TEST_RSA_PUBLIC_PEM.as_bytes())
        .expect("test public PEM should parse");
    Box::new(move |_jwk, signing_input, signature| {
        let signing_input_str = std::str::from_utf8(signing_input)
            .map_err(|_| "signing_input not utf-8".to_string())?;
        let sig_b64 = base64_url_no_pad(signature);
        let mut validation = Validation::new(Algorithm::RS256);
        validation.required_spec_claims.clear();
        validation.validate_aud = false;
        validation.validate_exp = false;
        validation.validate_nbf = false;
        let ok = jsonwebtoken::crypto::verify(
            &sig_b64,
            signing_input_str.as_bytes(),
            &decoding_key,
            Algorithm::RS256,
        )
        .map_err(|e| format!("RS256 verify: {e}"))?;
        if ok {
            Ok(())
        } else {
            Err("RS256 signature did not verify".into())
        }
    })
}

fn build_jwks() -> serde_json::Value {
    serde_json::json!({
        "keys": [
            {
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": KID,
                "n": TEST_RSA_N_B64URL,
                "e": "AQAB",
            }
        ]
    })
}

// -------- Minimal JWKS HTTP/1.1 server --------

#[allow(dead_code)]
struct JwksServer {
    addr: SocketAddr,
    issuer: String,
    jwks_url: String,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for JwksServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn spawn_jwks_server(jwks_body: serde_json::Value) -> JwksServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind jwks");
    let addr = listener.local_addr().expect("local_addr");
    let issuer = format!("http://{addr}");
    let jwks_url = format!("{issuer}/jwks.json");
    let body = Arc::new(serde_json::to_vec(&jwks_body).unwrap());
    let discovery = Arc::new(
        serde_json::to_vec(&serde_json::json!({
            "issuer": issuer,
            "jwks_uri": jwks_url,
        }))
        .unwrap(),
    );

    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _peer)) = listener.accept().await else {
                break;
            };
            let body = body.clone();
            let discovery = discovery.clone();
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let mut total = 0usize;
                loop {
                    let n = match stream.read(&mut buf[total..]).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    total += n;
                    if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    if total == buf.len() {
                        return;
                    }
                }
                let req = std::str::from_utf8(&buf[..total]).unwrap_or("");
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/");
                let (status, payload): (&str, &[u8]) = if path == "/jwks.json" {
                    ("200 OK", body.as_ref())
                } else if path == "/.well-known/openid-configuration" {
                    ("200 OK", discovery.as_ref())
                } else {
                    ("404 Not Found", b"not found")
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.write_all(payload).await;
            });
        }
    });
    JwksServer {
        addr,
        issuer,
        jwks_url,
        handle,
    }
}

// -------- Server-side fixture: JWKS + RedWire listener wired with OAuthValidator --------

struct Fixture {
    redwire_addr: SocketAddr,
    issuer: String,
    _jwks: JwksServer,
}

async fn start_fixture() -> Fixture {
    let jwks = spawn_jwks_server(build_jwks()).await;

    let mut config = OAuthConfig::default();
    config.enabled = true;
    config.issuer = jwks.issuer.clone();
    config.audience = AUDIENCE.to_string();
    config.jwks_url = jwks.jwks_url.clone();
    config.identity_mode = OAuthIdentityMode::SubClaim;
    config.role_claim = Some("role".to_string());
    config.default_role = Role::Read;
    config.map_to_existing_users = false;

    let validator = OAuthValidator::with_verifier(config, build_verifier());
    validator.set_jwks(vec![reddb::auth::oauth::Jwk {
        kid: KID.to_string(),
        alg: "RS256".to_string(),
        key_bytes: Vec::new(),
    }]);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind redwire");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);

    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
    let cfg = RedWireConfig {
        bind_addr: addr.to_string(),
        auth_store: None,
        oauth: Some(Arc::new(validator)),
    };
    let issuer = jwks.issuer.clone();
    tokio::spawn(async move {
        let _ = start_redwire_listener(cfg, runtime).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    Fixture {
        redwire_addr: addr,
        issuer,
        _jwks: jwks,
    }
}

fn happy_claims(issuer: &str) -> Claims {
    let now = now_unix_secs();
    Claims {
        iss: issuer.to_string(),
        sub: "driver-smoke@example.com".into(),
        aud: AUDIENCE.into(),
        exp: now + 60,
        nbf: now - 1,
        iat: now,
        role: Some("admin".into()),
    }
}

// -------- Drive the v2 handshake by hand, since the driver
// `Auth` enum has no OauthJwt variant yet. --------

async fn run_handshake(addr: SocketAddr, jwt: &str) -> Frame {
    let mut sock = TcpStream::connect(addr).await.expect("connect");
    sock.write_all(&[MAGIC, SUPPORTED_VERSION])
        .await
        .expect("magic+version");

    let hello_body =
        br#"{"versions":[1],"auth_methods":["oauth-jwt"],"features":0,"client_name":"reddb-rs-oauth-smoke"}"#
            .to_vec();
    let hello = Frame::new(MessageKind::Hello, 1, hello_body);
    sock.write_all(&encode_frame(&hello)).await.expect("hello");

    let ack = read_frame(&mut sock).await;
    assert_eq!(ack.kind, MessageKind::HelloAck);
    let ack_obj: serde_json::Value = serde_json::from_slice(&ack.payload).unwrap();
    assert_eq!(ack_obj["auth"], "oauth-jwt");

    let body = serde_json::json!({ "jwt": jwt });
    let body_bytes = serde_json::to_vec(&body).unwrap();
    let resp = Frame::new(MessageKind::AuthResponse, 2, body_bytes);
    sock.write_all(&encode_frame(&resp))
        .await
        .expect("auth response");

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

// -------- Tests --------

#[tokio::test]
async fn oauth_jwt_smoke_happy_path() {
    let fix = start_fixture().await;
    let claims = happy_claims(&fix.issuer);
    let jwt = mint_rs256(&claims);

    let frame = run_handshake(fix.redwire_addr, &jwt).await;
    assert_eq!(
        frame.kind,
        MessageKind::AuthOk,
        "happy path should AuthOk; got {:?} {}",
        frame.kind,
        String::from_utf8_lossy(&frame.payload)
    );

    let ok: serde_json::Value = serde_json::from_slice(&frame.payload).unwrap();
    assert_eq!(ok["username"], "driver-smoke@example.com");
    assert_eq!(ok["role"], "admin");
    assert!(ok["session_id"].as_str().is_some_and(|s| !s.is_empty()));
}

#[tokio::test]
async fn oauth_jwt_smoke_expired_refused() {
    let fix = start_fixture().await;
    let mut claims = happy_claims(&fix.issuer);
    let now = now_unix_secs();
    claims.exp = now - 60;
    claims.nbf = now - 120;
    claims.iat = now - 120;
    let jwt = mint_rs256(&claims);

    let frame = run_handshake(fix.redwire_addr, &jwt).await;
    assert_eq!(frame.kind, MessageKind::AuthFail);
    let reason: serde_json::Value = serde_json::from_slice(&frame.payload).unwrap();
    let reason_str = reason["reason"].as_str().unwrap_or("");
    assert!(
        reason_str.contains("expired"),
        "expired token reason should mention expiry: {reason_str}"
    );
}

#[tokio::test]
async fn oauth_jwt_smoke_tampered_signature_refused() {
    let fix = start_fixture().await;
    let claims = happy_claims(&fix.issuer);
    // Mint a real JWT, then flip the first byte of the signature
    // segment so the RS256 verify fails.
    let real = mint_rs256(&claims);
    let parts: Vec<&str> = real.split('.').collect();
    assert_eq!(parts.len(), 3);
    let mut sig = parts[2].chars();
    let first = sig.next().unwrap_or('A');
    let flipped = if first == 'A' { 'B' } else { 'A' };
    let rest: String = sig.collect();
    let tampered = format!("{}.{}.{}{}", parts[0], parts[1], flipped, rest);

    let frame = run_handshake(fix.redwire_addr, &tampered).await;
    assert_eq!(frame.kind, MessageKind::AuthFail);
    let reason: serde_json::Value = serde_json::from_slice(&frame.payload).unwrap();
    let reason_str = reason["reason"].as_str().unwrap_or("");
    assert!(
        reason_str.contains("signature") || reason_str.contains("verif"),
        "tampered sig reason should mention signature: {reason_str}"
    );
}

// -------- helpers --------

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
