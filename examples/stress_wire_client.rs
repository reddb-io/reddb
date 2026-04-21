//! 16-client wire-level stress tool that targets an already-running
//! RedDB server on `127.0.0.1:5050`. Used in tandem with the
//! bench's Docker container to collect `REDDB_WIRE_TIMING=1` logs
//! without the bench-runner tearing the server down.
//!
//! Usage:
//!   1. In one terminal, bring up the reddb container with wire
//!      timing on:
//!        cd reddb-benchmark/docker
//!        REDDB_WIRE_TIMING=1 docker compose -p probe up -d reddb
//!   2. In another terminal, run this example:
//!        cargo run --release --example stress_wire_client
//!   3. Pull the timing output:
//!        docker logs probe-reddb-1 2>&1 | grep wire-timing
//!   4. Clean up:
//!        docker compose -p probe down -v

use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const ADDR: &str = "127.0.0.1:5050";
const WORKERS: usize = 16;
const OPS_PER_WORKER: usize = 600;
const COLLECTION: &str = "stress_users";
const MSG_BULK_INSERT_BINARY: u8 = 0x06;
const MSG_BULK_OK: u8 = 0x05;
const MSG_ERROR: u8 = 0xff;
const VAL_I64: u8 = 1;
const VAL_TEXT: u8 = 3;

fn payload(worker: usize, op: usize) -> Vec<u8> {
    let id = (worker as u64) * 1_000_000 + op as u64 + 1;
    let name = format!("w{worker}_i{op}");
    let mut p = Vec::with_capacity(96);
    p.extend_from_slice(&(COLLECTION.len() as u16).to_le_bytes());
    p.extend_from_slice(COLLECTION.as_bytes());
    let cols = ["id", "name", "age"];
    p.extend_from_slice(&(cols.len() as u16).to_le_bytes());
    for c in &cols {
        p.extend_from_slice(&(c.len() as u16).to_le_bytes());
        p.extend_from_slice(c.as_bytes());
    }
    p.extend_from_slice(&1u32.to_le_bytes());
    p.push(VAL_I64);
    p.extend_from_slice(&(id as i64).to_le_bytes());
    p.push(VAL_TEXT);
    p.extend_from_slice(&(name.len() as u32).to_le_bytes());
    p.extend_from_slice(name.as_bytes());
    p.push(VAL_I64);
    p.extend_from_slice(&25i64.to_le_bytes());
    p
}

fn frame(msg: u8, body: &[u8]) -> Vec<u8> {
    let total_len = (body.len() + 1) as u32;
    let mut f = Vec::with_capacity(5 + body.len());
    f.extend_from_slice(&total_len.to_le_bytes());
    f.push(msg);
    f.extend_from_slice(body);
    f
}

async fn worker(id: usize) -> (usize, Vec<u64>) {
    let mut s = TcpStream::connect(ADDR).await.expect("connect");
    s.set_nodelay(true).ok();
    let mut lats = Vec::with_capacity(OPS_PER_WORKER);
    let mut hdr = [0u8; 5];
    for op in 0..OPS_PER_WORKER {
        let body = payload(id, op);
        let req = frame(MSG_BULK_INSERT_BINARY, &body);
        let t0 = Instant::now();
        s.write_all(&req).await.expect("write");
        s.read_exact(&mut hdr).await.expect("hdr");
        let len = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as usize;
        let t = hdr[4];
        let mut rest = vec![0u8; len - 1];
        if !rest.is_empty() {
            s.read_exact(&mut rest).await.expect("rest");
        }
        if t == MSG_ERROR {
            panic!("server error: {}", String::from_utf8_lossy(&rest));
        }
        assert_eq!(t, MSG_BULK_OK);
        lats.push(t0.elapsed().as_nanos() as u64);
    }
    (id, lats)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 16)]
async fn main() {
    eprintln!("connecting {WORKERS} clients to {ADDR}, {OPS_PER_WORKER} ops each");
    let t = Instant::now();
    let mut joins = Vec::with_capacity(WORKERS);
    for i in 0..WORKERS {
        joins.push(tokio::spawn(worker(i)));
    }
    let mut all = Vec::new();
    for j in joins {
        let (_id, lats) = j.await.unwrap();
        all.extend(lats);
    }
    let wall = t.elapsed();
    all.sort_unstable();
    let ops = (WORKERS * OPS_PER_WORKER) as f64;
    println!("=== stress_wire_client ===");
    println!("wall     {:.2}s", wall.as_secs_f64());
    println!("ops/s    {:.0}", ops / wall.as_secs_f64());
    println!("p50      {:>9} ns", all[all.len() / 2]);
    println!("p95      {:>9} ns", all[all.len() * 95 / 100]);
    println!("p99      {:>9} ns", all[all.len() * 99 / 100]);
}
