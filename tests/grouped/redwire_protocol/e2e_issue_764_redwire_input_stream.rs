//! End-to-end RedWire input-stream lifecycle (issue #764 / PRD #759 S5).
//!
//! Drives a real TCP RedWire listener through:
//!   magic → version → Hello → HelloAck → AuthResponse → AuthOk →
//!   OpenStream{direction:"in"} → OpenAck → StreamChunk… → StreamEnd
//!
//!   - AC #1: open input stream, write N Chunk frames, receive a
//!     single StreamEnd carrying the committed RID range + stats.
//!   - AC #2: an input stream and a concurrent output stream on the
//!     same connection do not interfere — dispatched by stream_id.
//!   - AC #3: a server-side error on chunk N emits one StreamError
//!     (carrying recoverable_rid) and no further frames for that
//!     stream_id; rows from chunks 1..N-1 stay durable.
//!   - AC #4: StreamCancel on an input stream is accepted; the
//!     in-flight chunk is discarded, prior committed chunks remain
//!     durable.

use std::sync::Arc;

use reddb::api::RedDBOptions;
use reddb::wire::redwire::{
    decode_frame, encode_frame, start_redwire_listener, Frame, MessageKind, RedWireConfig,
    REDWIRE_MAGIC,
};
use reddb::RedDBRuntime;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn start_server() -> (std::net::SocketAddr, Arc<RedDBRuntime>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind redwire");
    let addr = listener.local_addr().expect("local_addr");
    drop(listener);

    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("rt"));
    runtime
        .execute_query("CREATE TABLE sink (id INTEGER, name TEXT)")
        .expect("create");

    let cfg = RedWireConfig {
        bind_addr: addr.to_string(),
        auth_store: None,
        oauth: None,
    };
    let rt = Arc::clone(&runtime);
    tokio::spawn(async move {
        let _ = start_redwire_listener(cfg, rt).await;
    });
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    (addr, runtime)
}

async fn handshake_anonymous(sock: &mut TcpStream) {
    sock.write_all(&[REDWIRE_MAGIC, 0x01]).await.unwrap();
    let hello_body =
        br#"{"versions":[1],"auth_methods":["anonymous"],"features":0,"client_name":"s5-smoke"}"#
            .to_vec();
    let hello = Frame::new(MessageKind::Hello, 1, hello_body);
    sock.write_all(&encode_frame(&hello)).await.unwrap();
    let ack = read_frame(sock).await;
    assert_eq!(ack.kind, MessageKind::HelloAck, "expected HelloAck");
    let resp = Frame::new(MessageKind::AuthResponse, 2, b"{}".to_vec());
    sock.write_all(&encode_frame(&resp)).await.unwrap();
    let ok = read_frame(sock).await;
    assert_eq!(ok.kind, MessageKind::AuthOk, "expected AuthOk");
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

fn open_input_frame(corr: u64, stream_id: u16) -> Frame {
    let payload = serde_json::json!({
        "direction": "in",
        "target": "sink",
        "columns": ["id", "name"],
    });
    Frame::new(
        MessageKind::OpenStream,
        corr,
        serde_json::to_vec(&payload).unwrap(),
    )
    .with_stream(stream_id)
}

fn chunk_frame(
    corr: u64,
    stream_id: u16,
    seq: u64,
    rows: serde_json::Value,
    terminal: bool,
) -> Frame {
    let payload = serde_json::json!({ "seq": seq, "rows": rows, "terminal": terminal });
    Frame::new(
        MessageKind::StreamChunk,
        corr,
        serde_json::to_vec(&payload).unwrap(),
    )
    .with_stream(stream_id)
}

fn open_output_frame(corr: u64, stream_id: u16, sql: &str) -> Frame {
    let payload = serde_json::json!({ "sql": sql, "opts": {} });
    Frame::new(
        MessageKind::OpenStream,
        corr,
        serde_json::to_vec(&payload).unwrap(),
    )
    .with_stream(stream_id)
}

#[tokio::test]
async fn ac1_open_input_write_chunks_then_stream_end() {
    let (addr, runtime) = start_server().await;
    let mut sock = TcpStream::connect(addr).await.unwrap();
    handshake_anonymous(&mut sock).await;

    sock.write_all(&encode_frame(&open_input_frame(10, 7)))
        .await
        .unwrap();
    let ack = read_frame(&mut sock).await;
    assert_eq!(ack.kind, MessageKind::OpenAck);
    assert_eq!(ack.stream_id, 7);

    // Three chunks of rows, last one terminal.
    sock.write_all(&encode_frame(&chunk_frame(
        10,
        7,
        0,
        serde_json::json!([{"id":1,"name":"a"},{"id":2,"name":"b"}]),
        false,
    )))
    .await
    .unwrap();
    sock.write_all(&encode_frame(&chunk_frame(
        10,
        7,
        1,
        serde_json::json!([{"id":3,"name":"c"}]),
        false,
    )))
    .await
    .unwrap();
    sock.write_all(&encode_frame(&chunk_frame(
        10,
        7,
        2,
        serde_json::json!([{"id":4,"name":"d"}]),
        true,
    )))
    .await
    .unwrap();

    let end = read_frame(&mut sock).await;
    assert_eq!(end.kind, MessageKind::StreamEnd);
    assert_eq!(end.stream_id, 7);
    let v: serde_json::Value = serde_json::from_slice(&end.payload).unwrap();
    assert_eq!(v["stats"]["row_count"].as_u64(), Some(4));
    assert_eq!(v["stats"]["chunk_count"].as_u64(), Some(3));
    assert_eq!(v["stats"]["cancelled"].as_bool(), Some(false));
    // committed RID range: snapshot_lsn .. committed_rid, the latter
    // advanced past the former by the per-chunk commits.
    let snap = v["stats"]["snapshot_lsn"].as_u64().unwrap();
    let committed = v["stats"]["committed_rid"].as_u64().unwrap();
    assert!(committed >= snap, "committed_rid must not precede snapshot");

    // All four rows are durable.
    let qr = runtime
        .execute_query("SELECT name FROM sink ORDER BY id ASC")
        .expect("scan");
    assert_eq!(qr.result.records.len(), 4);
}

#[tokio::test]
async fn ac2_input_and_output_stream_coexist() {
    let (addr, runtime) = start_server().await;
    // Seed a few rows so the output stream has something to emit.
    for i in 1..=3 {
        runtime
            .execute_query(&format!(
                "INSERT INTO sink (id, name) VALUES ({i}, 'seed-{i}')"
            ))
            .unwrap();
    }
    let mut sock = TcpStream::connect(addr).await.unwrap();
    handshake_anonymous(&mut sock).await;

    // Open an output stream (sid 9) and an input stream (sid 7).
    sock.write_all(&encode_frame(&open_output_frame(
        20,
        9,
        "SELECT * FROM sink",
    )))
    .await
    .unwrap();
    sock.write_all(&encode_frame(&open_input_frame(21, 7)))
        .await
        .unwrap();
    // Feed the input stream a terminal chunk.
    sock.write_all(&encode_frame(&chunk_frame(
        21,
        7,
        0,
        serde_json::json!([{"id":100,"name":"in"}]),
        true,
    )))
    .await
    .unwrap();

    let mut input_ended = false;
    let mut output_ended = false;
    let mut saw_output_chunk = false;
    while !(input_ended && output_ended) {
        let f = read_frame(&mut sock).await;
        assert!(
            f.stream_id == 7 || f.stream_id == 9,
            "unexpected stream_id {}",
            f.stream_id
        );
        match (f.stream_id, f.kind) {
            (_, MessageKind::OpenAck) => {}
            (9, MessageKind::StreamChunk) => saw_output_chunk = true,
            (9, MessageKind::StreamEnd) => output_ended = true,
            (7, MessageKind::StreamEnd) => input_ended = true,
            (sid, kind) => panic!("unexpected envelope on stream {sid}: {kind:?}"),
        }
    }
    assert!(saw_output_chunk, "output stream must emit chunks");
    // The input row landed without the output stream interfering.
    let qr = runtime
        .execute_query("SELECT id FROM sink WHERE id = 100")
        .expect("scan");
    assert_eq!(qr.result.records.len(), 1);
}

#[tokio::test]
async fn ac3_error_on_chunk_emits_one_stream_error_prior_durable() {
    let (addr, runtime) = start_server().await;
    let mut sock = TcpStream::connect(addr).await.unwrap();
    handshake_anonymous(&mut sock).await;

    sock.write_all(&encode_frame(&open_input_frame(10, 7)))
        .await
        .unwrap();
    let ack = read_frame(&mut sock).await;
    assert_eq!(ack.kind, MessageKind::OpenAck);

    // Chunk 0 commits cleanly.
    sock.write_all(&encode_frame(&chunk_frame(
        10,
        7,
        0,
        serde_json::json!([{"id":1,"name":"ok"}]),
        false,
    )))
    .await
    .unwrap();
    // Chunk 1 carries a non-object row → invalid_row, fails to commit.
    sock.write_all(&encode_frame(&chunk_frame(
        10,
        7,
        1,
        serde_json::json!([42]),
        false,
    )))
    .await
    .unwrap();

    let err = read_frame(&mut sock).await;
    assert_eq!(err.kind, MessageKind::StreamError);
    assert_eq!(err.stream_id, 7);
    let v: serde_json::Value = serde_json::from_slice(&err.payload).unwrap();
    assert_eq!(v["code"].as_str(), Some("invalid_row"));
    assert!(v["recoverable_rid"].as_u64().is_some());

    // Chunk 0's row is durable despite chunk 1's failure.
    let qr = runtime
        .execute_query("SELECT name FROM sink WHERE id = 1")
        .expect("scan");
    assert_eq!(qr.result.records.len(), 1);

    // Connection survives: a Ping still answers Pong, and no further
    // frame was emitted for stream 7 before the Pong.
    let ping = Frame::new(MessageKind::Ping, 99, vec![]);
    sock.write_all(&encode_frame(&ping)).await.unwrap();
    let pong = read_frame(&mut sock).await;
    assert_eq!(pong.kind, MessageKind::Pong);
}

#[tokio::test]
async fn ac4_stream_cancel_keeps_prior_chunks_durable() {
    let (addr, runtime) = start_server().await;
    let mut sock = TcpStream::connect(addr).await.unwrap();
    handshake_anonymous(&mut sock).await;

    sock.write_all(&encode_frame(&open_input_frame(10, 7)))
        .await
        .unwrap();
    let ack = read_frame(&mut sock).await;
    assert_eq!(ack.kind, MessageKind::OpenAck);

    // Commit one chunk, then cancel before sending a terminal frame.
    sock.write_all(&encode_frame(&chunk_frame(
        10,
        7,
        0,
        serde_json::json!([{"id":7,"name":"kept"}]),
        false,
    )))
    .await
    .unwrap();
    let cancel = Frame::new(
        MessageKind::StreamCancel,
        11,
        br#"{"reason":"client-abort"}"#.to_vec(),
    )
    .with_stream(7);
    sock.write_all(&encode_frame(&cancel)).await.unwrap();

    let end = read_frame(&mut sock).await;
    assert_eq!(end.kind, MessageKind::StreamEnd);
    assert_eq!(end.stream_id, 7);
    let v: serde_json::Value = serde_json::from_slice(&end.payload).unwrap();
    assert_eq!(v["stats"]["cancelled"].as_bool(), Some(true));
    assert_eq!(v["stats"]["row_count"].as_u64(), Some(1));

    // The committed chunk survived the cancel.
    let qr = runtime
        .execute_query("SELECT name FROM sink WHERE id = 7")
        .expect("scan");
    assert_eq!(qr.result.records.len(), 1);
}
