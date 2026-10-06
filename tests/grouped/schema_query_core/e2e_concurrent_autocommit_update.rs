//! #2373: concurrent autocommit UPDATEs of one row must never leave more than
//! one live version of it, and a conditional UPDATE must be compare-and-set.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};
use std::sync::{Arc, Barrier};

const WRITERS: usize = 20;
const TRIALS: usize = 20;

/// `setup` runs once per trial table (`{t}` is its name); then `WRITERS`
/// threads run `statement(i)` together. Returns the sum of `affected_rows` and
/// the number of live rows with id 'row1', for the worst trial of each.
fn race(setup: &[&str], statement: impl Fn(&str, usize) -> String + Sync) -> (Vec<u64>, usize) {
    let runtime = Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime"));
    let mut winners = Vec::new();
    let mut most_live_rows = 0;
    for trial in 0..TRIALS {
        let table = format!("race_{trial}");
        runtime
            .execute_query(&format!(
                "CREATE TABLE {table} (id TEXT, v INTEGER, ver INTEGER)"
            ))
            .expect("table");
        runtime
            .execute_query(&format!(
                "INSERT INTO {table} (id, v, ver) VALUES ('row1', 0, 0)"
            ))
            .expect("seed");
        for statement in setup {
            runtime
                .execute_query(&statement.replace("{t}", &table))
                .expect("setup");
        }
        let barrier = Arc::new(Barrier::new(WRITERS));
        let workers: Vec<_> = (0..WRITERS)
            .map(|writer| {
                let runtime = Arc::clone(&runtime);
                let barrier = Arc::clone(&barrier);
                let sql = statement(&table, writer);
                std::thread::spawn(move || {
                    set_current_connection_id(2_373_000 + writer as u64);
                    barrier.wait();
                    let result = runtime.execute_query(&sql);
                    clear_current_connection_id();
                    result.map(|result| result.affected_rows).unwrap_or(0)
                })
            })
            .collect();
        winners.push(
            workers
                .into_iter()
                .map(|worker| worker.join().expect("writer finishes"))
                .sum(),
        );
        let live = runtime
            .execute_query(&format!("SELECT v FROM {table} WHERE id = 'row1'"))
            .expect("read")
            .result
            .records
            .len();
        most_live_rows = most_live_rows.max(live);
    }
    (winners, most_live_rows)
}

const SETUPS: [&[&str]; 3] = [
    &[],
    &["CREATE UNIQUE INDEX {t}_id ON {t} (id)"],
    &["CREATE UNIQUE INDEX {t}_id ON {t} (id) USING HASH"],
];

#[test]
fn a_conditional_update_has_exactly_one_winner_and_never_forks_the_row() {
    for setup in SETUPS {
        let (winners, live) = race(setup, |table, writer| {
            format!(
                "UPDATE {table} SET v = {} WHERE id = 'row1' AND v = 0",
                writer + 1
            )
        });
        assert!(
            winners.iter().all(|sum| *sum == 1),
            "{setup:?}: every trial has exactly one winner, got {winners:?}"
        );
        assert_eq!(live, 1, "{setup:?}: the row was forked");
    }
}

#[test]
fn unconditional_updates_serialize_without_forking_the_row() {
    for setup in SETUPS {
        let (winners, live) = race(setup, |table, writer| {
            format!("UPDATE {table} SET v = {} WHERE id = 'row1'", writer + 1)
        });
        assert!(
            winners.iter().all(|sum| *sum == WRITERS as u64),
            "{setup:?}: each of the {WRITERS} updates applies once, got {winners:?}"
        );
        assert_eq!(live, 1, "{setup:?}: the row was forked");
    }
}

#[test]
fn a_self_referencing_conditional_update_keeps_its_single_winner() {
    let (winners, live) = race(&[], |table, writer| {
        format!(
            "UPDATE {table} SET v = {}, ver = ver + 1 WHERE id = 'row1' AND v = 0",
            writer + 1
        )
    });
    assert!(winners.iter().all(|sum| *sum == 1), "{winners:?}");
    assert_eq!(live, 1);
}
