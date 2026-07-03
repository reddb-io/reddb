//! Concurrent commit throughput benchmark (issue #1650).
//!
//! Run:
//!   cargo bench -p reddb-io-server --bench concurrent_commit_bench
//!
//! Scenarios cover 1/2/4/8 concurrent SQL transactions on one runtime at low
//! conflict (one row per writer) and high conflict (all writers share one row).

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use reddb_server::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb_server::{RedDBOptions, RedDBRuntime};
use std::hint::black_box;
use std::sync::{Arc, Barrier};
use std::thread;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn target_row(writer: usize, conflict_rows: usize) -> usize {
    (writer % conflict_rows) + 1
}

fn selected_u64(rt: &RedDBRuntime, sql: &str, column: &str) -> u64 {
    let result = rt.execute_query(sql).expect("select");
    let row = result.result.records.first().expect("one row");
    match row.get(column) {
        Some(reddb_server::storage::schema::Value::UnsignedInteger(value)) => *value,
        Some(reddb_server::storage::schema::Value::Integer(value)) => *value as u64,
        other => panic!("expected integer column {column}, got {other:?}"),
    }
}

fn run_workload(connections: usize, conflict_rows: usize) -> usize {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE bench_concurrent_commit (id INT, value INT, marker TEXT)",
    );
    for id in 1..=conflict_rows {
        exec(
            &rt,
            &format!(
                "INSERT INTO bench_concurrent_commit (id, value, marker) VALUES ({id}, 0, 'seed')"
            ),
        );
    }
    let row_rids: Vec<u64> = (1..=conflict_rows)
        .map(|id| {
            selected_u64(
                &rt,
                &format!("SELECT rid FROM bench_concurrent_commit WHERE id = {id}"),
                "rid",
            )
        })
        .collect();

    let rt = Arc::new(rt);
    let begin_barrier = Arc::new(Barrier::new(connections));
    let update_barrier = Arc::new(Barrier::new(connections));
    let mut handles = Vec::with_capacity(connections);

    for writer in 0..connections {
        let rt = Arc::clone(&rt);
        let begin_barrier = Arc::clone(&begin_barrier);
        let update_barrier = Arc::clone(&update_barrier);
        let row_id = target_row(writer, conflict_rows);
        let row_rid = row_rids[row_id - 1];
        handles.push(thread::spawn(move || {
            set_current_connection_id(1650_500 + writer as u64);
            exec(&rt, "BEGIN");
            begin_barrier.wait();
            let update = rt
                .execute_query(&format!(
                    "UPDATE bench_concurrent_commit SET value += 1 WHERE rid = {row_rid}"
                ))
                .map(|_| ())
                .map_err(|err| err.to_string());
            update_barrier.wait();
            let committed = update.is_ok() && rt.execute_query("COMMIT").is_ok();
            if !committed {
                let _ = rt.execute_query("ROLLBACK");
            }
            clear_current_connection_id();
            committed
        }));
    }

    handles
        .into_iter()
        .map(|handle| handle.join().expect("writer thread should not panic"))
        .filter(|committed| *committed)
        .count()
}

fn bench_concurrent_commit(c: &mut Criterion) {
    let mut group = c.benchmark_group("tm_concurrent_commit");
    for connections in [1usize, 2, 4, 8] {
        for (label, conflict_rows) in [("low_conflict", connections), ("high_conflict", 1usize)] {
            group.throughput(Throughput::Elements(connections as u64));
            group.bench_with_input(
                BenchmarkId::new(label, connections),
                &(connections, conflict_rows),
                |b, &(connections, conflict_rows)| {
                    b.iter(|| {
                        black_box(run_workload(
                            black_box(connections),
                            black_box(conflict_rows),
                        ))
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_concurrent_commit);
criterion_main!(benches);
