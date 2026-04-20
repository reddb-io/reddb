//! Profile SELECT * FROM t WHERE age BETWEEN low AND high (mirrors bench).

use reddb::api::RedDBOptions;
use reddb::application::{
    CreateRowInput, CreateRowsBatchInput, EntityUseCases, ExecuteQueryInput, QueryUseCases,
};
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::time::Instant;

const ROWS: usize = 25_000;
const QUERIES: usize = 500;

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
        query: "CREATE TABLE users (id INT, name TEXT, city TEXT, age INT)".into(),
    })
    .expect("ct");

    const CITIES: &[&str] = &["NYC", "LA", "Chi", "Hou", "Phx"];
    let mut chunks: Vec<Vec<CreateRowInput>> = Vec::new();
    for c in 0..(ROWS + 999) / 1000 {
        let from = c * 1000;
        let to = ((c + 1) * 1000).min(ROWS);
        chunks.push(
            (from..to)
                .map(|i| CreateRowInput {
                    collection: "users".into(),
                    fields: vec![
                        ("id".into(), Value::Integer(i as i64)),
                        ("name".into(), Value::text(format!("u{i}"))),
                        ("city".into(), Value::text(CITIES[i % CITIES.len()])),
                        ("age".into(), Value::Integer(18 + (i % 60) as i64)),
                    ],
                    metadata: vec![],
                    node_links: vec![],
                    vector_links: vec![],
                })
                .collect(),
        );
    }
    let t_seed = Instant::now();
    for rows in chunks {
        uc_e.create_rows_batch(CreateRowsBatchInput {
            collection: "users".into(),
            rows,
        })
        .expect("bulk");
    }
    eprintln!("seeded {} in {:.2}s", ROWS, t_seed.elapsed().as_secs_f64());

    uc_q.execute(ExecuteQueryInput {
        query: "CREATE INDEX idx_age ON users (age) USING BTREE".into(),
    })
    .expect("idx");

    // Run range queries
    let mut samples = Vec::with_capacity(QUERIES);
    let mut total_rows = 0u64;
    let t = Instant::now();
    for i in 0..QUERIES {
        let low = 18 + (i * 7) % 53;
        let high = low + 10;
        let sql = format!("SELECT * FROM users WHERE age BETWEEN {low} AND {high}");
        let t0 = Instant::now();
        let r = uc_q.execute(ExecuteQueryInput { query: sql }).expect("q");
        samples.push(t0.elapsed().as_nanos() as u64);
        total_rows += r.result.records.len() as u64;
    }
    let wall = t.elapsed();
    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[samples.len() * 99 / 100];
    println!("=== select_range (N={QUERIES}) ===");
    println!("wall    {:.2}s", wall.as_secs_f64());
    println!("ops/s   {:.0}", QUERIES as f64 / wall.as_secs_f64());
    println!("p50     {:>8} ns", p50);
    println!("p99     {:>8} ns", p99);
    println!("rows/q  {:.0}", total_rows as f64 / QUERIES as f64);

    match guard.report().build() {
        Ok(r) => {
            let f = std::fs::File::create("flame-select-range.svg").expect("create");
            r.flamegraph(f).expect("write");
            eprintln!("flame-select-range.svg");
        }
        Err(e) => eprintln!("pprof: {e}"),
    }
}
