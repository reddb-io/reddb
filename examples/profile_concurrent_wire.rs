//! Profile the `concurrent` scenario via the actual wire path.
//!
//! Replicates what `reddb-benchmark`'s `concurrent` scenario does:
//! spawn the RedDB TCP wire listener against an on-disk, grouped-
//! durability store, then drive it with 16 tokio clients each
//! opening its own TCP connection and sending `MSG_BULK_INSERT_BINARY`
//! with `nrows=1` in a tight loop.
//!
//! Output:
//!   - ops/s + p50/p95/p99 latency
//!   - `flame-concurrent-wire.svg` — what the server is actually
//!     spending CPU on during 16-way concurrent single-row inserts.
//!
//! Usage:
//!   cargo run --release --example profile_concurrent_wire

use reddb::api::{DurabilityMode, RedDBOptions};
use reddb::wire::redwire::{
    decode_frame, encode_frame, start_redwire_listener_on, Frame, MessageKind, FRAME_HEADER_SIZE,
    REDWIRE_MAGIC,
};
use reddb::RedDBRuntime;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const WORKERS: usize = 16;
const OPS_PER_WORKER: usize = 500;
const COLLECTION: &str = "users";

const VAL_I64: u8 = 1;
const VAL_TEXT: u8 = 3;

fn build_bulk_payload(worker: usize, op: usize) -> Vec<u8> {
    // Layout:
    //   [coll_len u16][coll bytes]
    //   [ncols u16]([col_name_len u16][col_name bytes])*ncols
    //   [nrows u32]
    //   rows: per row, per col: [tag u8][value]
    let id = (worker as u64) * 1_000_000 + op as u64 + 1;
    let name = format!("w{worker}_i{op}");
    let mut p = Vec::with_capacity(128);

    p.extend_from_slice(&(COLLECTION.len() as u16).to_le_bytes());
    p.extend_from_slice(COLLECTION.as_bytes());

    let cols = ["id", "name", "age"];
    p.extend_from_slice(&(cols.len() as u16).to_le_bytes());
    for c in &cols {
        p.extend_from_slice(&(c.len() as u16).to_le_bytes());
        p.extend_from_slice(c.as_bytes());
    }

    p.extend_from_slice(&1u32.to_le_bytes()); // nrows = 1

    // id: i64
    p.push(VAL_I64);
    p.extend_from_slice(&(id as i64).to_le_bytes());
    // name: text
    p.push(VAL_TEXT);
    p.extend_from_slice(&(name.len() as u32).to_le_bytes());
    p.extend_from_slice(name.as_bytes());
    // age: i64
    p.push(VAL_I64);
    p.extend_from_slice(&25i64.to_le_bytes());

    p
}

async fn read_redwire_frame(s: &mut TcpStream) -> Frame {
    let mut header = [0u8; FRAME_HEADER_SIZE];
    s.read_exact(&mut header).await.expect("redwire header");
    let total_len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let mut buf = header.to_vec();
    buf.resize(total_len, 0);
    if total_len > FRAME_HEADER_SIZE {
        s.read_exact(&mut buf[FRAME_HEADER_SIZE..])
            .await
            .expect("redwire body");
    }
    let (frame, _) = decode_frame(&buf).expect("decode redwire frame");
    frame
}

async fn redwire_handshake(s: &mut TcpStream) {
    s.write_all(&[REDWIRE_MAGIC, 0x01]).await.expect("magic");

    let hello = Frame::new(
        MessageKind::Hello,
        1,
        br#"{"versions":[1],"auth_methods":["anonymous"],"features":0,"client_name":"profile-concurrent-wire"}"#
            .to_vec(),
    );
    s.write_all(&encode_frame(&hello)).await.expect("hello");

    let ack = read_redwire_frame(s).await;
    assert_eq!(ack.kind, MessageKind::HelloAck, "handshake ack: {ack:?}");

    let auth = Frame::new(MessageKind::AuthResponse, 2, Vec::new());
    s.write_all(&encode_frame(&auth)).await.expect("auth");

    let ok = read_redwire_frame(s).await;
    assert_eq!(ok.kind, MessageKind::AuthOk, "auth result: {ok:?}");
}

async fn client_loop(addr: String, worker: usize) -> Vec<u64> {
    let mut s = TcpStream::connect(&addr).await.expect("connect");
    s.set_nodelay(true).ok();
    redwire_handshake(&mut s).await;

    let mut lats = Vec::with_capacity(OPS_PER_WORKER);
    for op in 0..OPS_PER_WORKER {
        let payload = build_bulk_payload(worker, op);
        let req = Frame::new(
            MessageKind::BulkInsertBinary,
            ((worker as u64) << 32) | op as u64,
            payload,
        );
        let t0 = Instant::now();
        s.write_all(&encode_frame(&req)).await.expect("write");
        let resp = read_redwire_frame(&mut s).await;
        if resp.kind == MessageKind::Error {
            panic!("server error: {}", String::from_utf8_lossy(&resp.payload));
        }
        debug_assert_eq!(resp.kind, MessageKind::BulkOk);
        lats.push(t0.elapsed().as_nanos() as u64);
    }
    lats
}

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() {
    // On-disk, grouped durability — identical to bench container.
    let tmp = std::env::temp_dir().join(format!("reddb-concurrent-wire-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("tmpdir");
    let db_path = tmp.join("reddb.rdb");
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(db_path.with_extension("rdb-uwal"));

    let mut opts = RedDBOptions::persistent(&db_path);
    opts.durability_mode = DurabilityMode::WalDurableGrouped;
    let runtime = Arc::new(RedDBRuntime::with_options(opts).expect("runtime"));

    // No explicit seed — MSG_BULK_INSERT_BINARY auto-creates the
    // collection on first insert. Skipping the seed avoids triggering
    // any runtime-init path that differs from the Docker server.

    // Bind wire listener to an ephemeral port.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    let rt_listener = Arc::clone(&runtime);
    tokio::spawn(async move {
        if let Err(e) = start_redwire_listener_on(listener, rt_listener).await {
            eprintln!("listener err: {e}");
        }
    });

    // Give the listener a beat to be ready.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let t = Instant::now();
    let mut joins = Vec::with_capacity(WORKERS);
    for w in 0..WORKERS {
        let addr_s = addr.to_string();
        joins.push(tokio::spawn(async move { client_loop(addr_s, w).await }));
    }
    let mut all_lats: Vec<u64> = Vec::with_capacity(WORKERS * OPS_PER_WORKER);
    for j in joins {
        all_lats.extend(j.await.unwrap());
    }
    let wall = t.elapsed();
    all_lats.sort_unstable();
    let ops = (WORKERS * OPS_PER_WORKER) as f64;
    println!("=== concurrent-wire (workers={WORKERS}, ops/worker={OPS_PER_WORKER}) ===");
    println!("wall      {:.2}s", wall.as_secs_f64());
    println!("total ops {:.0}", ops);
    println!("ops/s     {:.0}", ops / wall.as_secs_f64());
    println!("p50       {:>9} ns", all_lats[all_lats.len() / 2]);
    println!("p95       {:>9} ns", all_lats[all_lats.len() * 95 / 100]);
    println!("p99       {:>9} ns", all_lats[all_lats.len() * 99 / 100]);

    // Drop the runtime first so WAL drains cleanly.
    drop(runtime);
    let _ = std::fs::remove_dir_all(&tmp);
}
