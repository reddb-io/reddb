//! Standalone profiler for the select_point hot path.
//!
//! No criterion scaffolding, no unrelated benches — runs the exact
//! workload that benchmarks rate at ~93 ops/s and emits a latency
//! histogram so we can see where the tail lives.
//!
//! Usage:
//!     cargo run --release --bin profile_hot
//!
//! Prints per-phase breakdown and can be re-run after each
//! optimisation to quantify the delta without fighting criterion
//! statistics.

use std::time::Instant;

use reddb::application::{CreateRowInput, EntityUseCases, ExecuteQueryInput, QueryUseCases};
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

const SEED_ROWS: usize = 5000;
const LOOKUP_ITERS: usize = 50_000;

fn main() {
    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(1000)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
        .expect("start profiler");

    // ── Setup ────────────────────────────────────────────────────
    // Default `in_memory()` now uses window_ms=0 (see api.rs) so
    // single-writer workloads don't pay the 1ms group-commit timer.
    let rt = RedDBRuntime::in_memory().expect("open runtime");
    let uc_entity = EntityUseCases::new(&rt);
    let uc_query = QueryUseCases::new(&rt);

    let t_seed = Instant::now();
    for i in 0..SEED_ROWS {
        uc_entity
            .create_row(CreateRowInput {
                collection: "users5k".into(),
                fields: vec![
                    ("name".into(), Value::Text(format!("User_{i}"))),
                    ("age".into(), Value::Integer(18 + (i % 63) as i64)),
                    (
                        "city".into(),
                        Value::Text(["NYC", "London", "Tokyo", "Paris", "Berlin"][i % 5].into()),
                    ),
                    ("email".into(), Value::Text(format!("u{i}@t.com"))),
                    ("score".into(), Value::Float(i as f64 * 0.02)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .expect("seed row");
    }
    let seed_elapsed = t_seed.elapsed();
    eprintln!(
        "seeded {} rows in {:?} ({:.0} ins/s)",
        SEED_ROWS,
        seed_elapsed,
        SEED_ROWS as f64 / seed_elapsed.as_secs_f64()
    );

    // Warmup — let caches populate + JIT-ish paths settle.
    for i in 0..1000 {
        let id = (i % SEED_ROWS as u64) + 1;
        let _ = uc_query
            .execute(ExecuteQueryInput {
                query: format!("SELECT * FROM users5k WHERE _entity_id = {id}"),
            })
            .expect("warmup lookup");
    }

    // ── Main loop — measure per-call latency ────────────────────
    let mut samples: Vec<u64> = Vec::with_capacity(LOOKUP_ITERS);
    let mut id = 1u64;
    let t_total = Instant::now();
    for _ in 0..LOOKUP_ITERS {
        let q = format!("SELECT * FROM users5k WHERE _entity_id = {id}");
        let t = Instant::now();
        let _ = uc_query
            .execute(ExecuteQueryInput { query: q })
            .expect("lookup");
        samples.push(t.elapsed().as_nanos() as u64);
        id = (id % SEED_ROWS as u64) + 1;
    }
    let total_elapsed = t_total.elapsed();

    // ── Stats ────────────────────────────────────────────────────
    samples.sort_unstable();
    let mean_ns: u64 = samples.iter().sum::<u64>() / samples.len() as u64;
    let p50 = samples[samples.len() / 2];
    let p95 = samples[samples.len() * 95 / 100];
    let p99 = samples[samples.len() * 99 / 100];
    let min = samples[0];
    let max = samples[samples.len() - 1];
    let ops = LOOKUP_ITERS as f64 / total_elapsed.as_secs_f64();

    println!("=== select_point (N={}) ===", LOOKUP_ITERS);
    println!("wall    {:>10.2}s", total_elapsed.as_secs_f64());
    println!("ops/s   {:>10.0}", ops);
    println!("min     {:>10} ns", min);
    println!("mean    {:>10} ns", mean_ns);
    println!("p50     {:>10} ns", p50);
    println!("p95     {:>10} ns", p95);
    println!("p99     {:>10} ns", p99);
    println!("max     {:>10} ns", max);

    // ── Isolate store.get() cost ────────────────────────────────
    // Same workload without the SQL parse/plan/dispatch pipeline —
    // just the raw primitive. If the gap between this and the SQL
    // path is large, execute_query overhead is the target.
    let mut raw_samples: Vec<u64> = Vec::with_capacity(LOOKUP_ITERS);
    let mut id = 1u64;
    let t_raw = Instant::now();
    for _ in 0..LOOKUP_ITERS {
        let t = Instant::now();
        let _ = rt.db().store().get("users5k", reddb::EntityId::new(id));
        raw_samples.push(t.elapsed().as_nanos() as u64);
        id = (id % SEED_ROWS as u64) + 1;
    }
    let raw_elapsed = t_raw.elapsed();
    raw_samples.sort_unstable();
    let raw_mean: u64 = raw_samples.iter().sum::<u64>() / raw_samples.len() as u64;
    let raw_p50 = raw_samples[raw_samples.len() / 2];
    let raw_p99 = raw_samples[raw_samples.len() * 99 / 100];
    let raw_ops = LOOKUP_ITERS as f64 / raw_elapsed.as_secs_f64();

    println!();
    println!("=== raw store.get() (N={}) ===", LOOKUP_ITERS);
    println!("wall    {:>10.2}s", raw_elapsed.as_secs_f64());
    println!("ops/s   {:>10.0}", raw_ops);
    println!("mean    {:>10} ns", raw_mean);
    println!("p50     {:>10} ns", raw_p50);
    println!("p99     {:>10} ns", raw_p99);
    println!();
    println!(
        "SQL overhead vs raw: {:.1}× slower ({} ns extra per call)",
        mean_ns as f64 / raw_mean as f64,
        mean_ns as i64 - raw_mean as i64
    );

    // ── Insert profile — pure sequential, via use-case API ─────
    let rt2 = RedDBRuntime::in_memory().expect("open second runtime");
    let uc_ins = EntityUseCases::new(&rt2);
    let n_ins = 5_000usize;
    let mut ins_samples: Vec<u64> = Vec::with_capacity(n_ins);
    let t_ins = Instant::now();
    for i in 0..n_ins {
        let t = Instant::now();
        uc_ins
            .create_row(CreateRowInput {
                collection: "users".into(),
                fields: vec![
                    ("name".into(), Value::Text(format!("User_{i}"))),
                    ("age".into(), Value::Integer((i % 100) as i64)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .expect("insert");
        ins_samples.push(t.elapsed().as_nanos() as u64);
    }
    let ins_elapsed = t_ins.elapsed();
    ins_samples.sort_unstable();
    let ins_mean = ins_samples.iter().sum::<u64>() / ins_samples.len() as u64;
    let ins_p50 = ins_samples[ins_samples.len() / 2];
    let ins_p95 = ins_samples[ins_samples.len() * 95 / 100];
    let ins_p99 = ins_samples[ins_samples.len() * 99 / 100];
    let ins_max = ins_samples[ins_samples.len() - 1];
    let ins_ops = n_ins as f64 / ins_elapsed.as_secs_f64();

    println!();
    println!("=== insert_sequential (N={}) ===", n_ins);
    println!("wall    {:>10.2}s", ins_elapsed.as_secs_f64());
    println!("ops/s   {:>10.0}", ins_ops);
    println!("mean    {:>10} ns", ins_mean);
    println!("p50     {:>10} ns", ins_p50);
    println!("p95     {:>10} ns", ins_p95);
    println!("p99     {:>10} ns", ins_p99);
    println!("max     {:>10} ns", ins_max);
    println!(
        "  (p99/p50 ratio = {:.1}× — high ratio = stalls/GC pauses)",
        ins_p99 as f64 / ins_p50 as f64
    );

    match guard.report().build() {
        Ok(report) => {
            let file = std::fs::File::create("flame-profile-hot.svg").expect("create svg");
            report.flamegraph(file).expect("write flamegraph");
            eprintln!("flame-profile-hot.svg written");
        }
        Err(err) => eprintln!("pprof report error: {err}"),
    }
}
