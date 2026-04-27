//! Standalone smoke for the in-process JWKS test server.
//!
//! Boots the server, hits both endpoints, verifies the response
//! shapes. Lets us isolate "is the JWKS helper itself broken?"
//! from the bigger end-to-end OAuth handshake test.

mod common;

use common::{jwks_server, jwt_mint};

#[tokio::test]
async fn jwks_server_serves_discovery_and_keys() {
    let server = jwks_server::spawn(jwt_mint::build_jwks()).await;

    // GET /.well-known/openid-configuration
    let discovery_body = http_get(&server.discovery_url).await;
    let discovery: serde_json::Value =
        serde_json::from_slice(&discovery_body).expect("discovery body should be JSON");
    assert_eq!(discovery["issuer"].as_str(), Some(server.issuer.as_str()));
    assert_eq!(
        discovery["jwks_uri"].as_str(),
        Some(server.jwks_url.as_str())
    );

    // GET /jwks.json
    let jwks_body = http_get(&server.jwks_url).await;
    let jwks: serde_json::Value =
        serde_json::from_slice(&jwks_body).expect("jwks body should be JSON");
    let keys = jwks["keys"].as_array().expect("keys[] in jwks");
    assert_eq!(keys.len(), 1, "exactly one key in JWKS");
    assert_eq!(keys[0]["kid"].as_str(), Some(jwt_mint::KID));
    assert_eq!(keys[0]["alg"].as_str(), Some("RS256"));
    assert_eq!(keys[0]["kty"].as_str(), Some("RSA"));
    assert_eq!(keys[0]["n"].as_str(), Some(jwt_mint::TEST_RSA_N_B64URL));
    assert_eq!(keys[0]["e"].as_str(), Some(jwt_mint::TEST_RSA_E_B64URL));

    server.shutdown();
}

#[tokio::test]
async fn jwks_server_unknown_path_404s() {
    let server = jwks_server::spawn(jwt_mint::build_jwks()).await;
    let url = format!("{}/nope", server.issuer);
    let body = http_get_status(&url).await;
    assert!(
        body.starts_with(b"HTTP/1.1 404"),
        "unknown path should return 404, got {:?}",
        std::str::from_utf8(&body[..body.len().min(40)])
    );
    server.shutdown();
}

/// Minimal HTTP/1.1 GET helper — speaks just enough to drive the
/// JWKS server without dragging in reqwest. Returns the body
/// bytes (post-headers).
async fn http_get(url: &str) -> Vec<u8> {
    let raw = http_get_status(url).await;
    let headers_end = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("response should have header terminator");
    raw[headers_end + 4..].to_vec()
}

async fn http_get_status(url: &str) -> Vec<u8> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Parse `http://host:port/path` — keep it dumb, the test
    // only ever feeds well-formed loopback URLs from the helper.
    let stripped = url.strip_prefix("http://").expect("http:// scheme");
    let (host_port, path) = stripped.split_once('/').unwrap_or((stripped, ""));
    let mut stream = tokio::net::TcpStream::connect(host_port)
        .await
        .expect("connect");
    let req = format!("GET /{path} HTTP/1.1\r\nHost: {host_port}\r\nConnection: close\r\n\r\n",);
    stream.write_all(req.as_bytes()).await.expect("write");
    let mut buf = Vec::with_capacity(4096);
    stream.read_to_end(&mut buf).await.expect("read");
    buf
}
