//! Microbenchmarks for the postgres-style perf sweep landed in
//! commits 5bbbca1..f53b28d. Each bench targets one of the seven
//! optimisations so we have real numbers (not theory) for the
//! deltas we ship.
//!
//! Run with:
//!     cargo bench --bench perf_sweep
//!
//! The benches are intentionally **micro** — they exercise one
//! hot-path primitive at a time so the signal is not lost in
//! end-to-end noise. End-to-end macro benches live in
//! `bench_embedded.rs`.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;

use reddb::storage::query::filter::{Filter, Predicate};
use reddb::storage::query::filter_compiled::CompiledFilter;
use reddb::storage::schema::Value;
use reddb::storage::wal::group_commit::GroupCommit;
use reddb::storage::wal::record::WalRecord;
use reddb::storage::wal::writer::WalWriter;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn temp_wal(name: &str) -> std::path::PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!(
        "rb_perf_sweep_{}_{}_{}.wal",
        name,
        std::process::id(),
        nanos
    ));
    let _ = std::fs::remove_file(&p);
    p
}

// ---------------------------------------------------------------------------
// Perf 1.1 + 1.2 — WAL writer hot path
//
// Compares the cost of N small appends followed by one sync. With
// the BufWriter (1.1) the appends should coalesce into a single
// write syscall before the fsync. With the read-only fast path (1.2)
// the sync is skipped entirely when there is nothing to commit.
// ---------------------------------------------------------------------------

fn bench_wal_append_then_sync(c: &mut Criterion) {
    let mut group = c.benchmark_group("perf/wal");
    for n in [1usize, 10, 100, 1000].iter() {
        group.bench_with_input(BenchmarkId::new("append_then_sync", n), n, |b, &n| {
            b.iter_with_setup(
                || {
                    let path = temp_wal(&format!("append_{}", n));
                    let writer = WalWriter::open(&path).unwrap();
                    (path, writer)
                },
                |(path, mut writer)| {
                    for i in 0..n {
                        writer
                            .append(&WalRecord::Begin { tx_id: i as u64 })
                            .unwrap();
                        writer
                            .append(&WalRecord::Commit { tx_id: i as u64 })
                            .unwrap();
                    }
                    writer.sync().unwrap();
                    drop(writer);
                    let _ = std::fs::remove_file(&path);
                },
            );
        });
    }
    group.finish();
}

fn bench_wal_sync_alone(c: &mut Criterion) {
    // Measures just the cost of a single sync() call on a freshly
    // opened WAL. With BufWriter, sync flushes the buffer (no-op
    // here, no records pending) and then fsyncs an unchanged file —
    // close to the OS floor of a fast no-op fsync.
    c.bench_function("perf/wal/sync_no_pending", |b| {
        let path = temp_wal("sync_no_pending");
        let mut writer = WalWriter::open(&path).unwrap();
        b.iter(|| {
            writer.sync().unwrap();
        });
        drop(writer);
        let _ = std::fs::remove_file(&path);
    });
}

// ---------------------------------------------------------------------------
// Perf 2.2 — Group commit
//
// Spawns N threads that each append + commit_at_least 50 records.
// With group commit, fsync count should be << total commits; the
// throughput should scale near-linearly with concurrency until the
// fsync floor is hit.
// ---------------------------------------------------------------------------

fn bench_group_commit_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("perf/wal/group_commit");
    for &writers in &[1usize, 2, 4, 8] {
        group.bench_with_input(
            BenchmarkId::new("commit_at_least", writers),
            &writers,
            |b, &writers| {
                b.iter_with_setup(
                    || {
                        let path = temp_wal(&format!("group_commit_{}", writers));
                        let wal = WalWriter::open(&path).unwrap();
                        let initial = wal.durable_lsn();
                        let wal = Arc::new(Mutex::new(wal));
                        let gc = Arc::new(GroupCommit::new(initial));
                        (path, wal, gc)
                    },
                    |(path, wal, gc)| {
                        let mut handles = Vec::with_capacity(writers);
                        for tx in 0..writers as u64 {
                            let wal_c = Arc::clone(&wal);
                            let gc_c = Arc::clone(&gc);
                            handles.push(thread::spawn(move || {
                                for i in 0..50u64 {
                                    let target = {
                                        let mut w = wal_c.lock().unwrap();
                                        w.append(&WalRecord::Begin {
                                            tx_id: tx * 1000 + i,
                                        })
                                        .unwrap();
                                        w.append(&WalRecord::Commit {
                                            tx_id: tx * 1000 + i,
                                        })
                                        .unwrap();
                                        w.current_lsn()
                                    };
                                    gc_c.commit_at_least(target, &wal_c).unwrap();
                                }
                            }));
                        }
                        for h in handles {
                            h.join().unwrap();
                        }
                        let _ = std::fs::remove_file(&path);
                    },
                );
            },
        );
    }
    group.finish();
}

// ---------------------------------------------------------------------------
// Perf 2.1 — CompiledFilter vs legacy walker
//
// The headline win of the sweep. Compares the compiled opcode loop
// against the recursive walker + closure callback on the same
// filter + row workload.
// ---------------------------------------------------------------------------

fn build_3pred_filter() -> Filter {
    Filter::And(vec![
        Filter::Predicate(Predicate::eq("a", Value::Integer(1))),
        Filter::Predicate(Predicate::gt("b", Value::Integer(10))),
        Filter::Predicate(Predicate::lt("c", Value::Integer(100))),
    ])
}

fn bench_filter_compiled_vs_legacy(c: &mut Criterion) {
    let filter = build_3pred_filter();
    let columns = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let schema: HashMap<String, usize> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| (c.clone(), i))
        .collect();
    let compiled = CompiledFilter::compile(&filter, &schema).unwrap();

    // Generate 10k rows with a mix of matches and non-matches so the
    // branch predictor doesn't get a free ride.
    let rows: Vec<Vec<Value>> = (0..10_000u64)
        .map(|i| {
            vec![
                Value::Integer((i % 3) as i64),
                Value::Integer((i % 50) as i64),
                Value::Integer((i % 200) as i64),
            ]
        })
        .collect();

    let mut group = c.benchmark_group("perf/filter");

    group.bench_function("compiled_evaluate_10k_rows", |b| {
        b.iter(|| {
            let mut hits = 0u64;
            for row in &rows {
                if compiled.evaluate(black_box(row)) {
                    hits += 1;
                }
            }
            black_box(hits);
        });
    });

    group.bench_function("legacy_evaluate_10k_rows", |b| {
        b.iter(|| {
            let mut hits = 0u64;
            for row in &rows {
                let row_map: HashMap<String, Value> = columns
                    .iter()
                    .zip(row.iter())
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                if filter.evaluate(&|c| row_map.get(c).cloned()) {
                    hits += 1;
                }
            }
            black_box(hits);
        });
    });

    group.bench_function("legacy_evaluate_10k_rows_no_map_alloc", |b| {
        // Fairer baseline: hand-built closure that doesn't allocate
        // a HashMap per row. This is what the OLD MemoryExecutor
        // did before this sweep — it walked the columns Vec per
        // predicate via .iter().position(), no per-row HashMap.
        b.iter(|| {
            let mut hits = 0u64;
            for row in &rows {
                if filter.evaluate(&|col| {
                    columns
                        .iter()
                        .position(|c| c == col)
                        .and_then(|i| row.get(i).cloned())
                }) {
                    hits += 1;
                }
            }
            black_box(hits);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Perf 1.5 — Group key allocation
//
// Compares the new 1-allocation push_str builder against a
// `format!` + `join` baseline that mirrors the OLD make_group_key
// from before this sweep.
// ---------------------------------------------------------------------------

fn bench_group_key_build(c: &mut Criterion) {
    use std::fmt::Write;

    let column_names = ["dept", "city", "level"];
    let row_count = 10_000usize;
    // Pre-build the rows so the bench measures only the key
    // construction.
    let rows: Vec<(String, String, i64)> = (0..row_count)
        .map(|i| {
            (
                format!("dept_{}", i % 7),
                format!("city_{}", i % 13),
                (i % 5) as i64,
            )
        })
        .collect();

    let mut group = c.benchmark_group("perf/group_key");

    group.bench_function("inlined_single_alloc", |b| {
        b.iter(|| {
            let mut sink = String::new();
            for (a, b_, c_) in &rows {
                let mut key = String::with_capacity(64);
                key.push_str(column_names[0]);
                key.push('=');
                key.push_str(a);
                key.push('|');
                key.push_str(column_names[1]);
                key.push('=');
                key.push_str(b_);
                key.push('|');
                key.push_str(column_names[2]);
                key.push('=');
                let _ = write!(key, "{c_}");
                sink.push_str(&key);
                sink.clear();
            }
            black_box(sink);
        });
    });

    group.bench_function("legacy_format_join", |b| {
        b.iter(|| {
            let mut sink = String::new();
            for (a, b_, c_) in &rows {
                // Mirrors the old make_group_key:
                //   parts.push(format!("{}={}", var, value_str));
                //   parts.join("|")
                let parts: Vec<String> = vec![
                    format!("{}={}", column_names[0], a),
                    format!("{}={}", column_names[1], b_),
                    format!("{}={}", column_names[2], c_),
                ];
                let key = parts.join("|");
                sink.push_str(&key);
                sink.clear();
            }
            black_box(sink);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Bench groups
// ---------------------------------------------------------------------------

criterion_group!(
    perf_sweep,
    bench_wal_append_then_sync,
    bench_wal_sync_alone,
    bench_group_commit_concurrent,
    bench_filter_compiled_vs_legacy,
    bench_group_key_build,
);
criterion_main!(perf_sweep);
