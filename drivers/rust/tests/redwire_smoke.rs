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
use reddb_client::redwire::{Auth, BinaryValue, ConnectOptions, Flags, Frame, MessageKind, RedWireClient};
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
async fn insert_dispatch_round_trip() {
    let (addr, _server) = start_server().await;
    let mut client = RedWireClient::connect(
        ConnectOptions::new(addr.ip().to_string(), addr.port()).with_auth(Auth::Anonymous),
    )
    .await
    .expect("connect");

    // Create a tiny table the inserts can target.
    client
        .query("CREATE TABLE smoke_users (name TEXT, age INTEGER)")
        .await
        .expect("create");

    // Single insert via the BulkInsert (0x04) frame with a
    // `payload` key (single-row shape).
    let mut row = serde_json::Map::new();
    row.insert("name".into(), serde_json::Value::String("alice".into()));
    row.insert("age".into(), serde_json::Value::Number(30.into()));
    let affected = client
        .insert("smoke_users", serde_json::Value::Object(row))
        .await
        .expect("insert");
    assert!(affected >= 1, "single insert affected at least one row");

    // Bulk insert.
    let mut rows = Vec::new();
    for n in 0..3 {
        let mut r = serde_json::Map::new();
        r.insert("name".into(), serde_json::Value::String(format!("u{n}")));
        r.insert("age".into(), serde_json::Value::Number((20 + n).into()));
        rows.push(serde_json::Value::Object(r));
    }
    let affected = client
        .bulk_insert("smoke_users", rows)
        .await
        .expect("bulk_insert");
    assert!(affected >= 3, "bulk insert affected at least 3 rows");

    client.close().await.expect("close");
}

#[tokio::test]
async fn binary_bulk_insert_fast_path() {
    let (addr, _server) = start_server().await;
    let mut client = RedWireClient::connect(
        ConnectOptions::new(addr.ip().to_string(), addr.port()).with_auth(Auth::Anonymous),
    )
    .await
    .expect("connect");

    client
        .query("CREATE TABLE bin_users (name TEXT, age INTEGER)")
        .await
        .expect("create");

    // Binary fast path: typed values, zero JSON encoding/decoding.
    // Reuses the v1 MSG_BULK_INSERT_BINARY codec on the engine
    // side — same hot-loop perf as examples/stress_wire_client.rs.
    let columns = ["name", "age"];
    let rows = vec![
        vec![BinaryValue::Text("alice".into()), BinaryValue::I64(30)],
        vec![BinaryValue::Text("bob".into()), BinaryValue::I64(25)],
        vec![BinaryValue::Text("carol".into()), BinaryValue::I64(40)],
    ];
    let affected = client
        .bulk_insert_binary("bin_users", &columns, &rows)
        .await
        .expect("binary bulk insert");
    assert_eq!(affected, 3, "binary path inserted all rows");

    client.close().await.expect("close");
}

#[tokio::test]
async fn frame_round_trip_with_zstd_compression() {
    use reddb_client::redwire::codec::{decode_frame, encode_frame};
    // Highly compressible payload — zstd level 1 should cut this
    // by ~80% even on a tiny dataset.
    let payload = b"abcabc".repeat(500);
    let frame = Frame::new(MessageKind::Result, 1, payload.clone()).with_flags(Flags::COMPRESSED);
    let bytes = encode_frame(&frame);
    assert!(bytes.len() < 16 + payload.len());
    let (decoded, _) = decode_frame(&bytes).unwrap();
    assert_eq!(decoded.payload, payload);
    assert!(decoded.flags.contains(Flags::COMPRESSED));
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
