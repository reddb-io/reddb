//! Profile insert_sequential: single-row inserts to a single table.
//! The bench does 25k × create_rows_batch([single]) via gRPC bulk
//! binary path, but embedded can exercise the same code via
//! `create_rows_batch` directly.

use reddb::api::RedDBOptions;
use reddb::application::{CreateRowInput, CreateRowsBatchInput, EntityUseCases};
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::time::Instant;

const N: usize = 10_000;

fn main() {
    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(1000)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
        .expect("profiler");

    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("rt");
    let uc_e = EntityUseCases::new(&rt);

    // Warmup
    for i in 0..100u64 {
        uc_e.create_rows_batch(CreateRowsBatchInput {
            collection: "users".into(),
            rows: vec![CreateRowInput {
                collection: "users".into(),
                fields: vec![
                    ("id".into(), Value::Integer(i as i64)),
                    ("name".into(), Value::text(format!("w{i}"))),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            }],
        })
        .expect("warm");
    }

    let t = Instant::now();
    let mut samples = Vec::with_capacity(N);
    for i in 0..N {
        let t0 = Instant::now();
        uc_e.create_rows_batch(CreateRowsBatchInput {
            collection: "users".into(),
            rows: vec![CreateRowInput {
                collection: "users".into(),
                fields: vec![
                    ("id".into(), Value::Integer(i as i64 + 1000)),
                    ("name".into(), Value::text(format!("u{i}"))),
                    ("age".into(), Value::Integer(25 + (i % 40) as i64)),
                    ("city".into(), Value::text("X")),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            }],
        })
        .expect("ins");
        samples.push(t0.elapsed().as_nanos() as u64);
    }
    let wall = t.elapsed();
    samples.sort_unstable();
    println!("=== insert_sequential (N={N}) ===");
    println!("wall    {:.2}s", wall.as_secs_f64());
    println!("ops/s   {:.0}", N as f64 / wall.as_secs_f64());
    println!("p50     {:>8} ns", samples[N / 2]);
    println!("p95     {:>8} ns", samples[N * 95 / 100]);
    println!("p99     {:>8} ns", samples[N * 99 / 100]);

    match guard.report().build() {
        Ok(r) => {
            let f = std::fs::File::create("flame-insert-seq.svg").expect("create");
            r.flamegraph(f).expect("write");
            eprintln!("flame-insert-seq.svg");
        }
        Err(e) => eprintln!("pprof: {e}"),
    }
}
