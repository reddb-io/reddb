//! Concurrent multi-writer MVCC harness.
//!
//! The workload opens N independent SQL transactions on one in-memory runtime,
//! maps them deterministically onto a bounded set of logical rows, then releases
//! them to update concurrently before committing in a deterministic order. A row
//! can have at most one successful first-committer-wins update from transactions
//! that share the same snapshot.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;

#[derive(Clone, Copy)]
struct HarnessConfig {
    connections: usize,
    transactions_per_connection: usize,
    conflict_rows: usize,
    seed: u64,
}

#[derive(Debug)]
struct WriterOutcome {
    row_id: usize,
    committed: bool,
    error: Option<String>,
}

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn selected_i64(rt: &RedDBRuntime, sql: &str, column: &str) -> i64 {
    let result = rt.execute_query(sql).expect("select");
    let Some(row) = result.result.records.first() else {
        panic!("expected one row for {sql}");
    };
    match row.get(column) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => *value as i64,
        other => panic!("expected integer column {column}, got {other:?}"),
    }
}

fn target_row(seed: u64, writer: usize, rows: usize) -> usize {
    if rows == 1 {
        return 1;
    }
    let offset = seed as usize % rows;
    ((writer + offset) % rows) + 1
}

fn run_harness(config: HarnessConfig) {
    assert!(config.connections > 0);
    assert!(config.transactions_per_connection > 0);
    assert!(config.conflict_rows > 0);
    assert!(config.conflict_rows <= config.connections);

    let rt = rt();
    exec(
        &rt,
        "CREATE TABLE tm_multi_writer (id INT, value INT, marker TEXT)",
    );
    for id in 1..=config.conflict_rows {
        exec(
            &rt,
            &format!("INSERT INTO tm_multi_writer (id, value, marker) VALUES ({id}, 0, 'seed')"),
        );
    }
    let row_rids: Vec<u64> = (1..=config.conflict_rows)
        .map(|id| {
            selected_i64(
                &rt,
                &format!("SELECT rid FROM tm_multi_writer WHERE id = {id}"),
                "rid",
            ) as u64
        })
        .collect();

    let next_xid_before = rt.snapshot_manager().peek_next_xid();
    let expected_rows: BTreeSet<usize> = (0..config.connections)
        .map(|writer| target_row(config.seed, writer, config.conflict_rows))
        .collect();

    let rt = Arc::new(rt);
    let mut outcomes = Vec::with_capacity(config.connections * config.transactions_per_connection);

    for transaction_index in 0..config.transactions_per_connection {
        let begin_barrier = Arc::new(Barrier::new(config.connections));
        let update_barrier = Arc::new(Barrier::new(config.connections));
        let commit_turn = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::with_capacity(config.connections);

        for writer in 0..config.connections {
            let rt = Arc::clone(&rt);
            let begin_barrier = Arc::clone(&begin_barrier);
            let update_barrier = Arc::clone(&update_barrier);
            let commit_turn = Arc::clone(&commit_turn);
            let row_id = target_row(config.seed, writer, config.conflict_rows);
            let row_rid = row_rids[row_id - 1];
            handles.push(thread::spawn(move || {
                set_current_connection_id(
                    1650_000 + (transaction_index as u64 * 100) + writer as u64,
                );
                exec(&rt, "BEGIN");
                begin_barrier.wait();

                let update = rt
                    .execute_query(&format!(
                        "UPDATE tm_multi_writer SET value += 1 WHERE rid = {row_rid}"
                    ))
                    .map(|_| ())
                    .map_err(|err| err.to_string());
                update_barrier.wait();
                while commit_turn.load(Ordering::Acquire) != writer {
                    thread::yield_now();
                }

                let outcome = match update {
                    Ok(()) => match rt.execute_query("COMMIT") {
                        Ok(_) => WriterOutcome {
                            row_id,
                            committed: true,
                            error: None,
                        },
                        Err(err) => WriterOutcome {
                            row_id,
                            committed: false,
                            error: {
                                let message = err.to_string();
                                let _ = rt.execute_query("ROLLBACK");
                                Some(message)
                            },
                        },
                    },
                    Err(err) => {
                        let _ = rt.execute_query("ROLLBACK");
                        WriterOutcome {
                            row_id,
                            committed: false,
                            error: Some(err),
                        }
                    }
                };

                commit_turn.fetch_add(1, Ordering::Release);
                clear_current_connection_id();
                outcome
            }));
        }

        outcomes.extend(
            handles
                .into_iter()
                .map(|handle| handle.join().expect("writer thread should not panic")),
        );
    }

    let committed = outcomes.iter().filter(|outcome| outcome.committed).count();
    let aborted = outcomes.len() - committed;
    let expected_commits = expected_rows.len() * config.transactions_per_connection;

    assert_eq!(
        committed, expected_commits,
        "one writer per contended logical row should commit: {outcomes:?}"
    );
    assert_eq!(
        aborted,
        outcomes.len() - expected_commits,
        "abort count should follow the conflict dial: {outcomes:?}"
    );
    for outcome in outcomes.iter().filter(|outcome| !outcome.committed) {
        let message = outcome.error.as_deref().unwrap_or("");
        assert!(
            message.contains("serialization conflict"),
            "expected serialization conflict for row {}, got {message}",
            outcome.row_id
        );
    }

    set_current_connection_id(1650_999);
    let visible_sum = selected_i64(
        &rt,
        "SELECT SUM(value) AS total FROM tm_multi_writer",
        "total",
    );
    assert_eq!(
        visible_sum, committed as i64,
        "every successful commit's effect must be observable"
    );
    let changed_rows = selected_i64(
        &rt,
        "SELECT COUNT(*) AS row_count FROM tm_multi_writer WHERE value > 0",
        "row_count",
    );
    assert_eq!(
        changed_rows,
        expected_rows.len() as i64,
        "each round should update the same deterministic logical row set"
    );

    exec(&rt, "VACUUM tm_multi_writer");
    let snapshot_manager = rt.snapshot_manager();
    assert_eq!(
        snapshot_manager.oldest_active_xid(),
        None,
        "writer transactions must not leak active xids"
    );
    assert_eq!(
        snapshot_manager.oldest_pinned_xid(),
        None,
        "harness should leave no pinned snapshots"
    );
    assert!(
        snapshot_manager.peek_next_xid() > next_xid_before,
        "commits/aborts should advance the MVCC xid floor"
    );
    clear_current_connection_id();
}

#[test]
fn concurrent_multi_writer_harness_low_conflict_ci() {
    run_harness(HarnessConfig {
        connections: 4,
        transactions_per_connection: 2,
        conflict_rows: 4,
        seed: 0x1650_0001,
    });
}

#[test]
fn concurrent_multi_writer_harness_high_conflict_ci() {
    run_harness(HarnessConfig {
        connections: 2,
        transactions_per_connection: 1,
        conflict_rows: 1,
        seed: 0x1650_0002,
    });
}

#[test]
#[ignore = "larger local/soak parametrization for TM commit concurrency"]
fn concurrent_multi_writer_soak_harness() {
    run_harness(HarnessConfig {
        connections: 16,
        transactions_per_connection: 4,
        conflict_rows: 16,
        seed: 0x1650_5000,
    });
}
