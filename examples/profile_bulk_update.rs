//! Profile bulk_update scenario: UPDATE t SET score = X WHERE id IN (50 ids).

use reddb::api::RedDBOptions;
use reddb::application::{
    CreateRowInput, CreateRowsBatchInput, EntityUseCases, ExecuteQueryInput, QueryUseCases,
};
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::time::Instant;

const ROWS: usize = 25_000;
const BATCHES: usize = 200;
const BATCH_SIZE: usize = 50;

fn main() {
    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(1000)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
        .expect("profiler");

    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("rt");
    let uc_e = EntityUseCases::new(&rt);
    let uc_q = QueryUseCases::new(&rt);

    uc_q.execute(ExecuteQueryInput {
        query: "CREATE TABLE users (id INT, name TEXT, score FLOAT)".into(),
    })
    .expect("ct");

    for c in 0..(ROWS + 999) / 1000 {
        let from = c * 1000;
        let to = ((c + 1) * 1000).min(ROWS);
        let rows: Vec<_> = (from..to)
            .map(|i| CreateRowInput {
                collection: "users".into(),
                fields: vec![
                    ("id".into(), Value::Integer(i as i64)),
                    ("name".into(), Value::Text(format!("u{i}"))),
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

    uc_q.execute(ExecuteQueryInput {
        query: "CREATE INDEX idx_id ON users (id) USING HASH".into(),
    })
    .expect("idx");

    // Warmup
    let _ = uc_q.execute(ExecuteQueryInput {
        query: "UPDATE users SET score = 0.99 WHERE id IN (0, 1, 2)".into(),
    });

    let mut samples = Vec::with_capacity(BATCHES);
    let t = Instant::now();
    for b in 0..BATCHES {
        let delta = 0.01 + b as f64 * 0.001;
        let base = (b * BATCH_SIZE) % (ROWS - BATCH_SIZE);
        let ids: Vec<String> = (base..base + BATCH_SIZE).map(|i| i.to_string()).collect();
        let sql = format!(
            "UPDATE users SET score = {delta} WHERE id IN ({})",
            ids.join(",")
        );
        let t0 = Instant::now();
        uc_q.execute(ExecuteQueryInput { query: sql }).expect("upd");
        samples.push(t0.elapsed().as_nanos() as u64);
    }
    let wall = t.elapsed();
    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[samples.len() * 99 / 100];
    println!("=== bulk_update (batches={BATCHES}, size={BATCH_SIZE}) ===");
    println!("wall    {:.2}s", wall.as_secs_f64());
    println!("ops/s   {:.0}", BATCHES as f64 / wall.as_secs_f64());
    println!("p50     {:>8} ns", p50);
    println!("p99     {:>8} ns", p99);

    match guard.report().build() {
        Ok(r) => {
            let f = std::fs::File::create("flame-bulk-update.svg").expect("create");
            r.flamegraph(f).expect("write");
            eprintln!("flame-bulk-update.svg");
        }
        Err(e) => eprintln!("pprof: {e}"),
    }
}
