//! Profile the `select_filtered` scenario offline.
//! Mirrors what bench does: bulk-insert 25k users, create hash index on
//! city + btree on age, then run WHERE city='X' AND age > Y.

use reddb::api::RedDBOptions;
use reddb::application::{
    CreateRowInput, CreateRowsBatchInput, EntityUseCases, ExecuteQueryInput, QueryUseCases,
};
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::time::Instant;

const ROWS: usize = 25_000;
const QUERIES: usize = 1_000;

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
        query: "CREATE TABLE users (id INT, name TEXT, city TEXT, age INT, email TEXT)".into(),
    })
    .expect("ct");

    const CITIES: &[&str] = &["New York", "Los Angeles", "Chicago", "Houston", "Phoenix"];
    let t_seed = Instant::now();
    let chunks = (ROWS + 999) / 1000;
    for c in 0..chunks {
        let from = c * 1000;
        let to = ((c + 1) * 1000).min(ROWS);
        let rows: Vec<_> = (from..to)
            .map(|i| CreateRowInput {
                collection: "users".into(),
                fields: vec![
                    ("id".into(), Value::Integer(i as i64)),
                    ("name".into(), Value::Text(format!("User_{i}"))),
                    ("city".into(), Value::Text(CITIES[i % CITIES.len()].into())),
                    ("age".into(), Value::Integer(18 + (i % 60) as i64)),
                    ("email".into(), Value::Text(format!("u{i}@t.com"))),
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

    // Build indices
    uc_q.execute(ExecuteQueryInput {
        query: "CREATE INDEX idx_city ON users (city) USING HASH".into(),
    })
    .expect("idx_city");
    uc_q.execute(ExecuteQueryInput {
        query: "CREATE INDEX idx_age ON users (age) USING BTREE".into(),
    })
    .expect("idx_age");

    // EXPLAIN skipped — RedDB's EXPLAIN grammar differs; flamegraph
    // will reveal the plan shape instead.

    // Run query repeatedly
    let t = Instant::now();
    let mut total_rows = 0u64;
    let mut samples = Vec::with_capacity(QUERIES);
    for i in 0..QUERIES {
        let city = CITIES[i % CITIES.len()];
        let min_age = 18 + ((i * 7) % 50) as i64;
        let sql = format!("SELECT * FROM users WHERE city = '{city}' AND age > {min_age}");
        let t0 = Instant::now();
        let r = uc_q
            .execute(ExecuteQueryInput { query: sql })
            .expect("select");
        samples.push(t0.elapsed().as_nanos() as u64);
        total_rows += r.result.records.len() as u64;
    }
    let wall = t.elapsed();
    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[samples.len() * 99 / 100];
    println!();
    println!("=== select_filtered (N={QUERIES}) ===");
    println!("wall     {:.2}s", wall.as_secs_f64());
    println!("ops/s    {:.0}", QUERIES as f64 / wall.as_secs_f64());
    println!("p50      {:>8} ns", p50);
    println!("p99      {:>8} ns", p99);
    println!("rows/q   {:.1}", total_rows as f64 / QUERIES as f64);

    match guard.report().build() {
        Ok(report) => {
            let file = std::fs::File::create("flame-select-filtered.svg").expect("create svg");
            report.flamegraph(file).expect("write");
            eprintln!("flame-select-filtered.svg written");
        }
        Err(err) => eprintln!("pprof: {err}"),
    }
}
