//! End-to-end smoke for RedWire v2: spin up the engine listener
//! on an ephemeral port, drive it from the reference client.
//!
//! Validates handshake (anonymous), `version` query round-trip,
//! ping/pong, and clean Bye close.

#![cfg(all(feature = "redwire", feature = "embedded"))]

use std::net::SocketAddr;
use std::sync::Arc;

use reddb::api::RedDBOptions;
use reddb::wire::redwire::{start_redwire_listener, RedWireConfig};
use reddb::RedDBRuntime;
use reddb_client::redwire::{Auth, ConnectOptions, RedWireClient};
use tokio::net::TcpListener;

async fn start_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    // Bind to :0 so the OS picks a free port — keeps tests
    // parallelisable and isolated from any running server.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
    let cfg = RedWireConfig {
        bind_addr: addr.to_string(),
        auth_store: None,
    };
    let handle = tokio::spawn(async move {
        let _ = start_redwire_listener(cfg, runtime).await;
    });
    // Give the listener a moment to bind. 50 ms is overkill but
    // makes the test stable on slow CI runners.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    (addr, handle)
}

#[tokio::test]
async fn handshake_query_close_round_trip() {
    let (addr, _server) = start_server().await;
    let mut client = RedWireClient::connect(
        ConnectOptions::new(addr.ip().to_string(), addr.port()).with_auth(Auth::Anonymous),
    )
    .await
    .expect("connect");

    // Bounce a trivial query through the v2 wire.
    let result = client.query("SELECT 1").await.expect("query");
    assert!(!result.statement.is_empty(), "server populated statement");

    // Ping/pong.
    client.ping().await.expect("ping");

    // Clean shutdown.
    client.close().await.expect("close");
}

#[tokio::test]
async fn bearer_required_when_anonymous_unsupported() {
    // Spin up a listener with an auth store that has auth.enabled.
    // A v2 client offering only `anonymous` should be refused.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
    let mut auth = reddb::auth::AuthConfig::default();
    auth.enabled = true;
    let store = Arc::new(reddb::auth::store::AuthStore::new(auth));
    let cfg = RedWireConfig {
        bind_addr: addr.to_string(),
        auth_store: Some(store),
    };
    tokio::spawn(async move {
        let _ = start_redwire_listener(cfg, runtime).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let err = RedWireClient::connect(
        ConnectOptions::new(addr.ip().to_string(), addr.port()).with_auth(Auth::Anonymous),
    )
    .await
    .expect_err("anonymous should be refused");
    assert_eq!(err.code, reddb_client::ErrorCode::AuthRefused);
}
