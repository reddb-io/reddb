//! Profile typed_insert in-process — isolates the CPU cost of the
//! 15-column bulk insert path from any wire/network overhead.
//!
//! Mirrors the bench's `typed_insert_bulk` shape: batches of
//! `BULK_BATCH_SIZE` rows, each with 15 strongly-typed columns
//! (id, ip, mac, port, email, uuid, phone, url, lat, lon, country,
//! currency, version, score, ts), run end-to-end through
//! `create_rows_batch_prevalidated` on an on-disk `WalDurableGrouped`
//! runtime.
//!
//! Output:
//!   - per-batch wall time
//!   - total ops/s
//!   - `REDDB_BULK_TIMING=1` breakdown emitted to stderr (enabled
//!     via env).
//!
//! Usage:
//!   REDDB_BULK_TIMING=1 RUST_LOG=reddb=debug \
//!     cargo run --release --example profile_typed_insert

use reddb::api::{DurabilityMode, RedDBOptions};
use reddb::application::ports::RuntimeEntityPort;
use reddb::application::{CreateRowInput, CreateRowsBatchInput};
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::sync::Arc;
use std::time::Instant;

const N_BATCHES: usize = 25;
const BATCH_SIZE: usize = 1000;
const COLLECTION: &str = "typed_bench";

fn make_row(id: u64) -> CreateRowInput {
    let fields = vec![
        ("id".into(), Value::UnsignedInteger(id)),
        ("ip".into(), Value::text(format!("10.0.{}.{}", id / 256, id % 256))),
        (
            "mac".into(),
            Value::text(format!(
                "aa:bb:cc:{:02x}:{:02x}:{:02x}",
                (id >> 16) & 0xff,
                (id >> 8) & 0xff,
                id & 0xff
            )),
        ),
        ("port".into(), Value::Integer((id % 65535) as i64)),
        (
            "email".into(),
            Value::text(format!("user{id}@example.com")),
        ),
        (
            "uuid".into(),
            Value::text(format!("{:016x}-0000-0000-0000-{:012x}", id, id)),
        ),
        ("phone".into(), Value::text(format!("+1-555-{:07}", id % 10_000_000))),
        (
            "url".into(),
            Value::text(format!("https://example.com/{id}")),
        ),
        ("lat".into(), Value::Float((id as f64 * 0.001) - 90.0)),
        ("lon".into(), Value::Float((id as f64 * 0.001) - 180.0)),
        ("country".into(), Value::text("US")),
        ("currency".into(), Value::text("USD")),
        ("version".into(), Value::UnsignedInteger(1)),
        ("score".into(), Value::Float((id as f64) * 1.5)),
        ("ts".into(), Value::UnsignedInteger(1_700_000_000 + id)),
    ];
    CreateRowInput {
        collection: COLLECTION.into(),
        fields,
        metadata: vec![],
        node_links: vec![],
        vector_links: vec![],
    }
}

fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("reddb=debug")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let tmp = std::env::temp_dir().join(format!("reddb-typed-{}.rdb", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("rdb-uwal"));

    let mut opts = RedDBOptions::persistent(&tmp);
    // Async — bypasses the synchronous wait_until_durable round-trip
    // so we measure the CPU cost of serialize + entity-build +
    // btree insert in isolation. Durability behaviour is not the
    // subject here; we already know the grouped path works on the
    // real server (Docker bench hits it at 6k ops/s). This harness
    // isolates the per-row CPU cost for 15-column typed rows.
    opts.durability_mode = DurabilityMode::Async;
    let runtime: Arc<RedDBRuntime> =
        Arc::new(RedDBRuntime::with_options(opts).expect("runtime"));

    // Seed the collection via one small insert so the table exists
    // before the measured batches start.
    runtime
        .create_rows_batch_prevalidated(CreateRowsBatchInput {
            collection: COLLECTION.into(),
            rows: vec![make_row(0)],
        })
        .expect("seed");

    let mut per_batch_ms: Vec<f64> = Vec::with_capacity(N_BATCHES);
    let t_total = Instant::now();
    for b in 0..N_BATCHES {
        let base = (b * BATCH_SIZE + 1) as u64;
        let rows: Vec<CreateRowInput> = (0..BATCH_SIZE)
            .map(|i| make_row(base + i as u64))
            .collect();
        let t0 = Instant::now();
        let n = runtime
            .create_rows_batch_prevalidated(CreateRowsBatchInput {
                collection: COLLECTION.into(),
                rows,
            })
            .expect("batch");
        let dt = t0.elapsed();
        per_batch_ms.push(dt.as_secs_f64() * 1000.0);
        assert_eq!(n, BATCH_SIZE);
    }
    let wall = t_total.elapsed();
    let total_rows = (N_BATCHES * BATCH_SIZE) as f64;

    per_batch_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = per_batch_ms[per_batch_ms.len() / 2];
    let p95 = per_batch_ms[per_batch_ms.len() * 95 / 100];
    let p99 = per_batch_ms[per_batch_ms.len() * 99 / 100];

    println!("=== typed_insert in-process ===");
    println!("batches   {N_BATCHES} × {BATCH_SIZE} rows = {} rows", N_BATCHES * BATCH_SIZE);
    println!("wall      {:.2}s", wall.as_secs_f64());
    println!("ops/s     {:.0}", total_rows / wall.as_secs_f64());
    println!("p50 batch {:>7.2} ms", p50);
    println!("p95 batch {:>7.2} ms", p95);
    println!("p99 batch {:>7.2} ms", p99);

    drop(runtime);
    let _ = std::fs::remove_file(&tmp);
    let _ = std::fs::remove_file(tmp.with_extension("rdb-uwal"));
}
