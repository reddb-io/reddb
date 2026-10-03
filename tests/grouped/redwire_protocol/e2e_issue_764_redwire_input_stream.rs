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
async fn streams_check_their_own_connections_transaction() {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let (addr, runtime) = start_server().await;
        let mut transaction = TcpStream::connect(addr).await.expect("transaction socket");
        handshake_anonymous(&mut transaction).await;
        let begin = reddb_wire::redwire::build_query_frame(3, "BEGIN").expect("BEGIN frame");
        transaction
            .write_all(&encode_frame(&begin))
            .await
            .expect("send BEGIN");
        assert_eq!(read_frame(&mut transaction).await.kind, MessageKind::Result);

        for frame in [
            open_input_frame(4, 7),
            open_output_frame(5, 9, "SELECT * FROM sink"),
        ] {
            transaction
                .write_all(&encode_frame(&frame))
                .await
                .expect("open stream in transaction");
            let error = read_frame(&mut transaction).await;
            assert_eq!(error.kind, MessageKind::StreamError);
            assert_eq!(error.stream_id, frame.stream_id);
            let body: serde_json::Value =
                serde_json::from_slice(&error.payload).expect("stream error");
            assert_eq!(body["code"], "stream_in_transaction_unsupported");
        }

        // A transaction on another connection cannot disable autocommit streams.
        let mut observer = TcpStream::connect(addr).await.expect("observer socket");
        handshake_anonymous(&mut observer).await;
        observer
            .write_all(&encode_frame(&open_input_frame(6, 7)))
            .await
            .expect("open observer input stream");
        assert_eq!(read_frame(&mut observer).await.kind, MessageKind::OpenAck);
        observer
            .write_all(&encode_frame(&chunk_frame(
                6,
                7,
                0,
                serde_json::json!([{"id":1,"name":"committed"}]),
                true,
            )))
            .await
            .expect("observer input chunk");
        assert_eq!(read_frame(&mut observer).await.kind, MessageKind::StreamEnd);
        assert_eq!(
            runtime
                .execute_query("SELECT * FROM sink")
                .expect("committed stream row")
                .result
                .records
                .len(),
            1
        );
        observer
            .write_all(&encode_frame(&open_output_frame(
                7,
                9,
                "SELECT * FROM sink",
            )))
            .await
            .expect("open observer output stream");
        assert_eq!(read_frame(&mut observer).await.kind, MessageKind::OpenAck);
        let chunk = read_frame(&mut observer).await;
        assert_eq!(chunk.kind, MessageKind::StreamChunk);
        let body: serde_json::Value = serde_json::from_slice(&chunk.payload).expect("streamed row");
        assert_eq!(body["rows"].as_array().expect("rows").len(), 1);
        assert_eq!(read_frame(&mut observer).await.kind, MessageKind::StreamEnd);
        // Reverse ordering must also reject a chunk instead of staging it.
        observer
            .write_all(&encode_frame(&open_input_frame(8, 11)))
            .await
            .expect("open input before BEGIN");
        assert_eq!(read_frame(&mut observer).await.kind, MessageKind::OpenAck);
        observer
            .write_all(&encode_frame(
                &reddb_wire::redwire::build_query_frame(9, "BEGIN").expect("observer BEGIN"),
            ))
            .await
            .expect("BEGIN after open");
        assert_eq!(read_frame(&mut observer).await.kind, MessageKind::Result);
        observer
            .write_all(&encode_frame(&chunk_frame(
                8,
                11,
                0,
                serde_json::json!([{"id":2,"name":"uncommitted"}]),
                true,
            )))
            .await
            .expect("chunk after BEGIN");
        let error = read_frame(&mut observer).await;
        assert_eq!(error.kind, MessageKind::StreamError);
        let body: serde_json::Value = serde_json::from_slice(&error.payload).expect("chunk error");
        assert_eq!(body["code"], "stream_in_transaction_unsupported");
        assert_eq!(
            runtime
                .execute_query("SELECT * FROM sink")
                .expect("no staged stream row")
                .result
                .records
                .len(),
            1
        );
        let rollback =
            reddb_wire::redwire::build_query_frame(8, "ROLLBACK").expect("ROLLBACK frame");
        transaction
            .write_all(&encode_frame(&rollback))
            .await
            .expect("send ROLLBACK");
        assert_eq!(read_frame(&mut transaction).await.kind, MessageKind::Result);
    })
    .await
    .expect("stream transaction test deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnect_with_backpressured_output_stream_rolls_back_the_session() {
    use reddb::health::HealthProvider;
    use reddb::{EntityId, UnifiedEntity};
    use reddb_types::Value;

    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let (addr, runtime) = start_server().await;
        let auth = Arc::new(reddb::auth::AuthStore::new(
            reddb::auth::AuthConfig::default(),
        ));
        auth.ensure_vault_secret_key();
        runtime.set_auth_store(auth);
        runtime
            .execute_query("SET SECRET token = 'original'")
            .expect("seed secret");
        let store = runtime.db().store();
        // Exceed TCP buffers and the bounded outbound queue so the worker
        // remains active after the session's transaction responses arrive.
        let payload = "x".repeat(32_768);
        for id in 0..1024_i64 {
            store
                .insert_auto(
                    "sink",
                    UnifiedEntity::table_row(
                        EntityId::new(0),
                        "sink",
                        u64::try_from(id).expect("positive row ID"),
                        vec![Value::Integer(id), Value::text(payload.clone())],
                    ),
                )
                .expect("stream row");
        }
        let mut socket = TcpStream::connect(addr).await.expect("stream socket");
        handshake_anonymous(&mut socket).await;
        for frame in [
            open_output_frame(
                20,
                7,
                "SELECT * FROM sink WHERE $secrets.default.token = 'original'",
            ),
            reddb_wire::redwire::build_query_frame(21, "BEGIN").expect("BEGIN"),
            reddb_wire::redwire::build_query_frame(22, "SET SECRET token = 'abandoned'")
                .expect("pending write"),
        ] {
            socket
                .write_all(&encode_frame(&frame))
                .await
                .expect("send stream and transaction");
        }
        let mut transaction_results = 0;
        while transaction_results < 2 {
            let frame = read_frame(&mut socket).await;
            if matches!(frame.correlation_id, 21 | 22) {
                assert_eq!(frame.kind, MessageKind::Result);
                transaction_results += 1;
            }
        }
        assert_eq!(
            runtime
                .health()
                .diagnostics
                .get("runtime.active_connections"),
            Some(&"2".to_string()),
            "session and stream worker must each own a lease"
        );
        drop(socket);
        loop {
            if runtime
                .health()
                .diagnostics
                .get("runtime.active_connections")
                == Some(&"0".to_string())
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let revealed = runtime
            .execute_query("VAULT REVEAL red.vault.token")
            .expect("rollback on disconnect");
        assert_eq!(
            revealed.result.records[0].get("value"),
            Some(&Value::text("original"))
        );
        assert_eq!(
            runtime
                .execute_query("VAULT HISTORY red.vault.token")
                .expect("no abandoned version")
                .result
                .records
                .len(),
            1
        );
        let mut reused = reddb_client::redwire::RedWireClient::connect(
            reddb_client::redwire::ConnectOptions::new(addr.ip().to_string(), addr.port())
                .with_auth(reddb_client::redwire::Auth::Anonymous),
        )
        .await
        .expect("reused session");
        reused.query("BEGIN").await.expect("fresh transaction");
        let revealed = reused
            .query("VAULT REVEAL red.vault.token")
            .await
            .expect("recycled ID has no pending writes");
        assert_eq!(
            revealed.rows[0]
                .iter()
                .find(|(name, _)| name == "value")
                .map(|(_, value)| value),
            Some(&reddb_client::ValueOut::String("original".into()))
        );
        reused.query("COMMIT").await.expect("empty commit");
        reused.close().await.expect("close recycled session");
    })
    .await
    .expect("backpressured stream disconnect deadline");
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
