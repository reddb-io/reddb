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
        oauth: None,
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
async fn scram_sha_256_end_to_end() {
    // Engine listener with auth.enabled = true and a user that
    // has a SCRAM verifier. Client sends client-first / parses
    // server-first / sends client-final / receives AuthOk.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
    let mut auth = reddb::auth::AuthConfig::default();
    auth.enabled = true;
    let store = Arc::new(reddb::auth::store::AuthStore::new(auth));
    store
        .create_user("alice", "hunter2", reddb::auth::Role::Write)
        .expect("create user");
    let cfg = reddb::wire::redwire::RedWireConfig {
        bind_addr: addr.to_string(),
        auth_store: Some(store),
        oauth: None,
    };
    tokio::spawn(async move {
        let _ = reddb::wire::redwire::start_redwire_listener(cfg, runtime).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Drive the SCRAM exchange manually so we exercise every
    // frame the server expects. Driver-side helper landing in
    // the next Phase 3c bump.
    use reddb_client::redwire::codec::{decode_frame, encode_frame};
    use reddb_client::redwire::scram;
    use std::io::Cursor;

    let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut socket = stream;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Magic + minor version.
    socket
        .write_all(&[reddb_client::redwire::MAGIC, 0x01])
        .await
        .unwrap();

    async fn send_frame(s: &mut tokio::net::TcpStream, frame: reddb_client::redwire::Frame) {
        use reddb_client::redwire::codec::encode_frame;
        let bytes = encode_frame(&frame);
        s.write_all(&bytes).await.unwrap();
    }
    async fn read_frame(s: &mut tokio::net::TcpStream) -> reddb_client::redwire::Frame {
        use reddb_client::redwire::codec::decode_frame;
        let mut header = [0u8; 16];
        s.read_exact(&mut header).await.unwrap();
        let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let mut buf = vec![0u8; len];
        buf[..16].copy_from_slice(&header);
        s.read_exact(&mut buf[16..]).await.unwrap();
        decode_frame(&buf).unwrap().0
    }

    // Hello — offer SCRAM only so the picker chooses it.
    let hello_body = br#"{"versions":[1],"auth_methods":["scram-sha-256"],"features":0}"#.to_vec();
    send_frame(
        &mut socket,
        reddb_client::redwire::Frame::new(MessageKind::Hello, 1, hello_body),
    )
    .await;
    let ack = read_frame(&mut socket).await;
    assert_eq!(ack.kind, MessageKind::HelloAck);
    let ack_obj: serde_json::Value = serde_json::from_slice(&ack.payload).unwrap();
    assert_eq!(ack_obj["auth"], "scram-sha-256");

    // SCRAM client-first.
    let client_nonce = "fyko+d2lbbFgONRv9qkxdawL";
    let client_first_bare = format!("n=alice,r={client_nonce}");
    let client_first = format!("n,,{client_first_bare}");
    send_frame(
        &mut socket,
        reddb_client::redwire::Frame::new(
            MessageKind::AuthResponse,
            2,
            client_first.into_bytes(),
        ),
    )
    .await;

    // Server-first comes back via AuthRequest.
    let server_first_frame = read_frame(&mut socket).await;
    assert_eq!(server_first_frame.kind, MessageKind::AuthRequest);
    let server_first = String::from_utf8(server_first_frame.payload).unwrap();

    // Parse salt + iter + combined nonce.
    let mut salt_b64 = String::new();
    let mut iter: u32 = 0;
    let mut combined_nonce = String::new();
    for part in server_first.split(',') {
        if let Some(v) = part.strip_prefix("r=") {
            combined_nonce = v.to_string();
        } else if let Some(v) = part.strip_prefix("s=") {
            salt_b64 = v.to_string();
        } else if let Some(v) = part.strip_prefix("i=") {
            iter = v.parse().unwrap();
        }
    }
    let salt = base64_decode(&salt_b64);

    // Client-final.
    let client_final_no_proof = format!("c=biws,r={combined_nonce}");
    let auth_message = format!("{client_first_bare},{server_first},{client_final_no_proof}");
    let proof = scram::client_proof(b"hunter2", &salt, iter, auth_message.as_bytes());
    let proof_b64 = base64_encode(&proof);
    let client_final = format!("{client_final_no_proof},p={proof_b64}");
    send_frame(
        &mut socket,
        reddb_client::redwire::Frame::new(
            MessageKind::AuthResponse,
            3,
            client_final.into_bytes(),
        ),
    )
    .await;

    let ok = read_frame(&mut socket).await;
    assert_eq!(ok.kind, MessageKind::AuthOk, "SCRAM should succeed");
    let ok_obj: serde_json::Value = serde_json::from_slice(&ok.payload).unwrap();
    assert_eq!(ok_obj["username"], "alice");
    assert_eq!(ok_obj["role"], "write");
    assert!(ok_obj["v"].is_string(), "AuthOk carries server signature");

    // Bye + cleanup.
    send_frame(
        &mut socket,
        reddb_client::redwire::Frame::new(MessageKind::Bye, 4, vec![]),
    )
    .await;

    // Tiny base64 helpers — keep the test self-contained without
    // pulling another crate.
    fn base64_encode(bytes: &[u8]) -> String {
        const A: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
        let chunks = bytes.chunks_exact(3);
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
                out.push_str("==");
            }
            [a, b] => {
                let n = ((*a as u32) << 16) | ((*b as u32) << 8);
                out.push(A[((n >> 18) & 0x3F) as usize] as char);
                out.push(A[((n >> 12) & 0x3F) as usize] as char);
                out.push(A[((n >> 6) & 0x3F) as usize] as char);
                out.push('=');
            }
            _ => {}
        }
        out
    }
    fn base64_decode(input: &str) -> Vec<u8> {
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
                _ => continue,
            };
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(((buf >> bits) & 0xFF) as u8);
            }
        }
        out
    }
    let _ = Cursor::new(0);
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
        oauth: None,
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
