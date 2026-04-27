//! Minimal HTTP/1.1 JWKS endpoint for OAuth-JWT smoke tests.
//!
//! Uses raw `tokio::net::TcpListener` + a hand-rolled request
//! reader so we don't drag axum/hyper into dev-dependencies.
//! HTTP/1.1 surface is intentionally tiny:
//!
//!   GET /.well-known/openid-configuration → discovery JSON
//!   GET /jwks.json                        → JWK set JSON
//!
//! Anything else gets a 404. The server runs until its
//! `JoinHandle` is aborted by the test.
//!
//! The server binds to `127.0.0.1:0` and surfaces the resolved
//! `SocketAddr` via [`spawn`] so the test can plug it into
//! `OAuthConfig.jwks_url`.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Handle to a running JWKS server.
pub struct JwksServer {
    pub addr: SocketAddr,
    pub issuer: String,
    pub jwks_url: String,
    pub discovery_url: String,
    handle: JoinHandle<()>,
}

impl JwksServer {
    /// Stop the server. Idempotent — fine to drop the handle
    /// without calling shutdown explicitly.
    pub fn shutdown(self) {
        self.handle.abort();
    }
}

impl Drop for JwksServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Spin up an in-process JWKS server bound to an OS-picked port.
///
/// `jwks_body` is the JSON returned at `/jwks.json`. The
/// `iss` claim should match the returned `issuer`.
pub async fn spawn(jwks_body: serde_json::Value) -> JwksServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind jwks");
    let addr = listener.local_addr().expect("local_addr");
    let issuer = format!("http://{addr}");
    let jwks_url = format!("{issuer}/jwks.json");
    let discovery_url = format!("{issuer}/.well-known/openid-configuration");

    let body = Arc::new(serde_json::to_vec(&jwks_body).expect("serialize jwks"));
    let discovery = Arc::new(
        serde_json::to_vec(&serde_json::json!({
            "issuer": issuer,
            "jwks_uri": jwks_url,
        }))
        .expect("serialize discovery"),
    );

    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _peer)) => {
                    let body = body.clone();
                    let discovery = discovery.clone();
                    tokio::spawn(async move {
                        if let Err(err) = serve_one(stream, body, discovery).await {
                            // Don't escalate — connection drops are
                            // benign on test teardown.
                            let _ = err;
                        }
                    });
                }
                Err(_) => break,
            }
        }
    });

    JwksServer {
        addr,
        issuer,
        jwks_url,
        discovery_url,
        handle,
    }
}

async fn serve_one(
    mut stream: tokio::net::TcpStream,
    jwks_body: Arc<Vec<u8>>,
    discovery: Arc<Vec<u8>>,
) -> std::io::Result<()> {
    // Read request bytes until we hit the end-of-headers marker.
    // Headers cap at 4 KiB — way more than the GETs we serve need.
    let mut buf = [0u8; 4096];
    let mut total = 0usize;
    loop {
        let n = stream.read(&mut buf[total..]).await?;
        if n == 0 {
            return Ok(());
        }
        total += n;
        if buf[..total].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if total == buf.len() {
            // Headers exceeded the cap — drop the connection.
            return Ok(());
        }
    }
    let request = std::str::from_utf8(&buf[..total]).unwrap_or("");
    let first_line = request.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    let (status, body): (&str, &[u8]) = if path == "/jwks.json" {
        ("200 OK", jwks_body.as_ref())
    } else if path == "/.well-known/openid-configuration" {
        ("200 OK", discovery.as_ref())
    } else {
        ("404 Not Found", b"not found")
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}
