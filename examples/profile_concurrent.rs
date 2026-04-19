//! Profile the `concurrent` scenario: N threads each doing insert_one.
//! Mirrors what bench does at level 16. Goal is to find the lock
//! everybody is waiting on.

// use reddb::api::RedDBOptions;
use reddb::application::{CreateRowInput, CreateRowsBatchInput, EntityUseCases};
use reddb::storage::schema::Value;
use reddb::RedDBRuntime;
use std::thread;
use std::time::Instant;

const WORKERS: usize = 16;
const OPS_PER_WORKER: usize = 500;

fn main() {
    let guard = pprof::ProfilerGuardBuilder::default()
        .frequency(1000)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
        .expect("profiler");

    // Leak a runtime so all threads share the same &'static reference —
    // avoids fighting the `EntityUseCases<&RedDBRuntime>` lifetime bounds
    // that don't accept Arc<RedDBRuntime>. Scope is 'static so this is
    // safe in a main() example.
    let mut opts = reddb::api::RedDBOptions::in_memory();
    // Async: writers don't block on condvar waiting for background
    // fsync — they return as soon as WAL bytes are in the ring buffer.
    // Isolates the Mutex<WalWriter> contention from the condvar wait
    // loop in wait_until_durable so we can see the writer throughput
    // without group-commit coordination.
    opts.durability_mode = reddb::api::DurabilityMode::Async;
    let rt: &'static RedDBRuntime = Box::leak(Box::new(
        RedDBRuntime::with_options(opts).expect("rt"),
    ));

    // Ensure collection exists
    let uc_e = EntityUseCases::new(rt);
    uc_e.create_row(CreateRowInput {
        collection: "users".into(),
        fields: vec![
            ("id".into(), Value::Integer(0)),
            ("name".into(), Value::Text("seed".into())),
        ],
        metadata: vec![],
        node_links: vec![],
        vector_links: vec![],
    })
    .expect("seed");

    let t = Instant::now();
    let mut handles = Vec::new();
    for w in 0..WORKERS {
        handles.push(thread::spawn(move || {
            let uc_e = EntityUseCases::new(rt);
            let offset = (w * OPS_PER_WORKER) as u64 + 1;
            let mut lats = Vec::with_capacity(OPS_PER_WORKER);
            for i in 0..OPS_PER_WORKER {
                let id = offset + i as u64;
                let t0 = Instant::now();
                uc_e.create_rows_batch(CreateRowsBatchInput {
                    collection: "users".into(),
                    rows: vec![CreateRowInput {
                        collection: "users".into(),
                        fields: vec![
                            ("id".into(), Value::Integer(id as i64)),
                            ("name".into(), Value::Text(format!("w{w}_i{i}"))),
                            ("age".into(), Value::Integer(25)),
                        ],
                        metadata: vec![],
                        node_links: vec![],
                        vector_links: vec![],
                    }],
                })
                .expect("insert");
                lats.push(t0.elapsed().as_nanos() as u64);
            }
            lats
        }));
    }

    let mut all_lats: Vec<u64> = Vec::with_capacity(WORKERS * OPS_PER_WORKER);
    for h in handles {
        all_lats.extend(h.join().unwrap());
    }
    let wall = t.elapsed();
    all_lats.sort_unstable();
    let ops = (WORKERS * OPS_PER_WORKER) as f64;
    println!("=== concurrent (workers={WORKERS}, ops/worker={OPS_PER_WORKER}) ===");
    println!("wall      {:.2}s", wall.as_secs_f64());
    println!("total ops {:.0}", ops);
    println!("ops/s     {:.0}", ops / wall.as_secs_f64());
    println!("p50       {:>8} ns", all_lats[all_lats.len() / 2]);
    println!("p95       {:>8} ns", all_lats[all_lats.len() * 95 / 100]);
    println!("p99       {:>8} ns", all_lats[all_lats.len() * 99 / 100]);

    match guard.report().build() {
        Ok(report) => {
            let file = std::fs::File::create("flame-concurrent.svg").expect("create");
            report.flamegraph(file).expect("write");
            eprintln!("flame-concurrent.svg written");
        }
        Err(e) => eprintln!("pprof: {e}"),
    }
}
