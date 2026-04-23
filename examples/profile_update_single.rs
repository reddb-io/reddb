//! Profile update_single scenario — mirror bench_fast.py.
//!
//! Workload: `UPDATE users SET score = N WHERE _entity_id = id` in a
//! tight loop. Seeds 100K rows, then 2000 random-id updates.
//!
//! Run:
//!     cargo run --release --example profile_update_single
//! Artifacts: `flame-update-single.svg` + stdout latency histogram.

use std::time::Instant;

use reddb::api::RedDBOptions;
use reddb::application::{
    CreateRowInput, CreateRowsBatchInput, EntityUseCases, ExecuteQueryInput, QueryUseCases,
};
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

const ROWS: usize = 100_000;
const UPDATES: usize = 20_000;

fn main() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("rt");
    let uc_e = EntityUseCases::new(&rt);
    let uc_q = QueryUseCases::new(&rt);

    uc_q.execute(ExecuteQueryInput {
        query: "CREATE TABLE users (id INT, name TEXT, city TEXT, age INT, score FLOAT)".into(),
    })
    .expect("ct");

    const CITIES: &[&str] = &["NYC", "LA", "Chi", "Hou", "Phx"];
    let t_seed = Instant::now();
    for c in 0..(ROWS + 999) / 1000 {
        let from = c * 1000;
        let to = ((c + 1) * 1000).min(ROWS);
        let rows: Vec<_> = (from..to)
            .map(|i| CreateRowInput {
                collection: "users".into(),
                fields: vec![
                    ("id".into(), Value::Integer(i as i64)),
                    ("name".into(), Value::text(format!("u{i}"))),
                    ("city".into(), Value::text(CITIES[i % CITIES.len()])),
                    ("age".into(), Value::Integer(18 + (i % 60) as i64)),
                    ("score".into(), Value::Float(0.0)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .collect();
        uc_e.create_rows_batch(CreateRowsBatchInput {
            collection: "users".into(),
            rows,
        })
        .expect("bulk");
    }
    eprintln!(
        "seeded {} rows in {:.2}s",
        ROWS,
        t_seed.elapsed().as_secs_f64()
    );

    let ids: Vec<usize> = (0..UPDATES).map(|i| 1 + (i * 991) % ROWS).collect();

    // Start profiler AFTER seed so the bulk-insert rayon dispatch and
    // the serialize fan-out don't show up on top of the UPDATE samples.
    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(1000)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
        .expect("profiler");

    let mut samples = Vec::with_capacity(UPDATES);
    let t = Instant::now();
    for (iter, id) in ids.iter().enumerate() {
        let sql = format!("UPDATE users SET score = {} WHERE _entity_id = {id}", iter as f64);
        let t0 = Instant::now();
        uc_q.execute(ExecuteQueryInput { query: sql }).expect("q");
        samples.push(t0.elapsed().as_nanos() as u64);
    }
    let wall = t.elapsed();

    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    let p99 = samples[samples.len() * 99 / 100];
    let p999 = samples[samples.len() * 999 / 1000];
    let avg: u64 = samples.iter().sum::<u64>() / samples.len() as u64;

    println!("=== update_single (N={UPDATES}, rows={ROWS}) ===");
    println!("wall    {:.2}s", wall.as_secs_f64());
    println!("ops/s   {:.0}", UPDATES as f64 / wall.as_secs_f64());
    println!("avg     {:>8} ns ({:.2} ms)", avg, avg as f64 / 1e6);
    println!("p50     {:>8} ns ({:.2} ms)", p50, p50 as f64 / 1e6);
    println!("p95     {:>8} ns ({:.2} ms)", p95, p95 as f64 / 1e6);
    println!("p99     {:>8} ns ({:.2} ms)", p99, p99 as f64 / 1e6);
    println!("p99.9   {:>8} ns ({:.2} ms)", p999, p999 as f64 / 1e6);

    match guard.report().build() {
        Ok(r) => {
            let f = std::fs::File::create("flame-update-single.svg").expect("create");
            r.flamegraph(f).expect("write");
            eprintln!("flame-update-single.svg");
        }
        Err(e) => eprintln!("pprof: {e}"),
    }
}
