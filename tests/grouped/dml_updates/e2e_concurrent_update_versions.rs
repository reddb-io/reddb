//! Issue #2373 — concurrent autocommit UPDATEs of one row must never fork it
//! into several live versions that share one logical id (`rid`), and a
//! conditional UPDATE must behave as a compare-and-set: exactly one of N
//! concurrent `UPDATE t SET v = k WHERE id = 'row1' AND v = 0` statements
//! reports `affected_rows = 1`.
//!
//! Every scenario starts N writer threads behind one barrier against a single
//! seeded row and repeats the race for several trials, because a fork needs
//! two writers to read the same live version before either installs its
//! successor. The table shapes cover the three admission paths the issue
//! lists: no constraint, a default (BTREE) `CREATE UNIQUE INDEX`, and a
//! declared `PRIMARY KEY`.

use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier};
use std::thread;

const WRITER_COUNT: usize = 16;
const TRIAL_COUNT: usize = 20;

#[derive(Clone, Copy, Debug)]
enum TableShape {
    Unconstrained,
    UniqueIndex,
    PrimaryKey,
}

impl TableShape {
    fn create_statements(self, table: &str) -> Vec<String> {
        match self {
            TableShape::Unconstrained => vec![format!(
                "CREATE TABLE {table} (id TEXT, v INTEGER, ver INTEGER)"
            )],
            TableShape::UniqueIndex => vec![
                format!("CREATE TABLE {table} (id TEXT, v INTEGER, ver INTEGER)"),
                format!("CREATE UNIQUE INDEX {table}_id_unique ON {table} (id)"),
            ],
            TableShape::PrimaryKey => vec![format!(
                "CREATE TABLE {table} (id TEXT PRIMARY KEY, v INTEGER, ver INTEGER)"
            )],
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum UpdateMode {
    /// `UPDATE t SET v = k WHERE id = 'row1' AND v = 0`
    CompareAndSet,
    /// `UPDATE t SET v = k WHERE id = 'row1'`
    Plain,
}

impl UpdateMode {
    fn statement(self, table: &str, value: usize) -> String {
        match self {
            UpdateMode::CompareAndSet => {
                format!("UPDATE {table} SET v = {value} WHERE id = 'row1' AND v = 0")
            }
            UpdateMode::Plain => format!("UPDATE {table} SET v = {value} WHERE id = 'row1'"),
        }
    }
}

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn integer(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => i64::try_from(*value).expect("fits i64"),
        other => panic!("expected integer, got {other:?}"),
    }
}

/// Every live version of `row1` as `(rid, v)`.
fn live_versions(rt: &RedDBRuntime, table: &str) -> Vec<(i64, i64)> {
    exec(rt, &format!("SELECT rid, v FROM {table} WHERE id = 'row1'"))
        .result
        .records
        .iter()
        .map(|record| (integer(record.get("rid")), integer(record.get("v"))))
        .collect()
}

/// Runs `WRITER_COUNT` concurrent UPDATEs of `row1`; returns each writer's
/// `affected_rows`, indexed by writer (writer `i` writes value `i + 1`).
fn race_updates(rt: &Arc<RedDBRuntime>, table: &str, mode: UpdateMode) -> Vec<u64> {
    let barrier = Arc::new(Barrier::new(WRITER_COUNT));
    let handles: Vec<_> = (0..WRITER_COUNT)
        .map(|writer| {
            let rt = Arc::clone(rt);
            let barrier = Arc::clone(&barrier);
            let sql = mode.statement(table, writer + 1);
            thread::spawn(move || {
                barrier.wait();
                rt.execute_query(&sql)
                    .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
                    .affected_rows
            })
        })
        .collect();
    handles
        .into_iter()
        .map(|handle| handle.join().expect("writer thread"))
        .collect()
}

fn seed_row(rt: &RedDBRuntime, table: &str, shape: TableShape) {
    for statement in shape.create_statements(table) {
        exec(rt, &statement);
    }
    exec(
        rt,
        &format!("INSERT INTO {table} (id, v, ver) VALUES ('row1', 0, 0)"),
    );
}

/// Asserts the outcome of one race: one live version, and the affected-row
/// accounting matches the statement's semantics.
fn assert_race_outcome(
    rt: &RedDBRuntime,
    table: &str,
    mode: UpdateMode,
    affected: &[u64],
    context: &str,
) {
    let versions = live_versions(rt, table);
    assert_eq!(
        versions.len(),
        1,
        "{context}: row1 forked into {} live versions {versions:?} (affected {affected:?})",
        versions.len()
    );
    let (_, final_value) = versions[0];
    let winners: Vec<usize> = affected
        .iter()
        .enumerate()
        .filter(|(_, rows)| **rows > 0)
        .map(|(writer, _)| writer)
        .collect();
    assert!(
        affected.iter().all(|rows| *rows <= 1),
        "{context}: a single-row UPDATE reported more than one affected row: {affected:?}"
    );
    match mode {
        UpdateMode::CompareAndSet => {
            assert_eq!(
                winners.len(),
                1,
                "{context}: compare-and-set must have exactly one winner, got {winners:?}"
            );
            let expected = i64::try_from(winners[0] + 1).expect("fits i64");
            assert_eq!(
                final_value, expected,
                "{context}: the stored value must be the single winner's value"
            );
        }
        UpdateMode::Plain => {
            // A writer whose statement snapshot predates a peer's commit can
            // miss the row and report 0 (it never builds a successor), so
            // only the winners are pinned: at least one, and the surviving
            // value is one a winner wrote — never a loser's orphan version.
            assert!(
                !winners.is_empty(),
                "{context}: an unconditional UPDATE of an existing row must win once"
            );
            let written_by_winners: BTreeSet<i64> = winners
                .iter()
                .map(|writer| i64::try_from(writer + 1).expect("fits i64"))
                .collect();
            assert!(
                written_by_winners.contains(&final_value),
                "{context}: final value {final_value} was not written by a winner {winners:?}"
            );
        }
    }
}

fn assert_no_fork(shape: TableShape, mode: UpdateMode) {
    let rt = Arc::new(runtime());
    for trial in 0..TRIAL_COUNT {
        let table = format!("race_{shape:?}_{mode:?}_{trial}").to_lowercase();
        seed_row(&rt, &table, shape);
        let affected = race_updates(&rt, &table, mode);
        let context = format!("{shape:?}/{mode:?} trial {trial}");
        assert_race_outcome(&rt, &table, mode, &affected, &context);
    }
}

#[test]
fn concurrent_compare_and_set_update_unconstrained_has_one_winner_and_one_version() {
    assert_no_fork(TableShape::Unconstrained, UpdateMode::CompareAndSet);
}

#[test]
fn concurrent_plain_update_unconstrained_keeps_one_live_version() {
    assert_no_fork(TableShape::Unconstrained, UpdateMode::Plain);
}

#[test]
fn concurrent_compare_and_set_update_unique_index_has_one_winner_and_one_version() {
    assert_no_fork(TableShape::UniqueIndex, UpdateMode::CompareAndSet);
}

#[test]
fn concurrent_plain_update_unique_index_keeps_one_live_version() {
    assert_no_fork(TableShape::UniqueIndex, UpdateMode::Plain);
}

#[test]
fn concurrent_compare_and_set_update_primary_key_has_one_winner_and_one_version() {
    assert_no_fork(TableShape::PrimaryKey, UpdateMode::CompareAndSet);
}

#[test]
fn concurrent_plain_update_primary_key_keeps_one_live_version() {
    assert_no_fork(TableShape::PrimaryKey, UpdateMode::Plain);
}

/// A read-modify-write UPDATE (`n = n + 1`) and a plain UPDATE of another
/// column must serialize on the same per-row lock: the plain writer may not
/// build its successor from a pre-image an increment already superseded,
/// which would both fork the row and lose that increment.
#[test]
fn concurrent_plain_and_increment_updates_serialize_on_one_row() {
    let rt = Arc::new(runtime());
    for trial in 0..TRIAL_COUNT {
        let table = format!("race_mixed_{trial}");
        exec(
            &rt,
            &format!("CREATE TABLE {table} (id TEXT, n INTEGER, tag INTEGER)"),
        );
        exec(
            &rt,
            &format!("INSERT INTO {table} (id, n, tag) VALUES ('row1', 0, 0)"),
        );
        let barrier = Arc::new(Barrier::new(WRITER_COUNT));
        let handles: Vec<_> = (0..WRITER_COUNT)
            .map(|writer| {
                let rt = Arc::clone(&rt);
                let barrier = Arc::clone(&barrier);
                let sql = if writer % 2 == 0 {
                    format!("UPDATE {table} SET n = n + 1 WHERE id = 'row1'")
                } else {
                    format!("UPDATE {table} SET tag = {writer} WHERE id = 'row1'")
                };
                thread::spawn(move || {
                    barrier.wait();
                    rt.execute_query(&sql)
                        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
                        .affected_rows
                })
            })
            .collect();
        let affected: Vec<u64> = handles
            .into_iter()
            .map(|handle| handle.join().expect("writer thread"))
            .collect();
        assert!(
            affected.iter().all(|rows| *rows <= 1),
            "trial {trial}: {affected:?}"
        );
        let reported_increments = affected
            .iter()
            .enumerate()
            .filter(|(writer, rows)| writer % 2 == 0 && **rows == 1)
            .count();

        let rows = exec(&rt, &format!("SELECT n FROM {table} WHERE id = 'row1'"))
            .result
            .records;
        assert_eq!(rows.len(), 1, "trial {trial}: row1 forked: {rows:?}");
        assert_eq!(
            integer(rows[0].get("n")),
            i64::try_from(reported_increments).expect("fits i64"),
            "trial {trial}: an increment that reported success was lost {affected:?}"
        );
    }
}

/// After the race, a reopen replays the WAL; the replayed store must hold the
/// same single live version the live store held, never a resurrected fork.
#[test]
fn concurrent_updates_replay_to_one_live_version_after_reopen() {
    let dir = tempfile::Builder::new()
        .prefix("reddb-2373-update-fork-")
        .tempdir()
        .expect("temp db dir");
    let path = dir.path().join("data.rdb");
    let options = || {
        RedDBOptions::persistent(&path)
            .with_storage_profile(reddb::storage::StorageProfileSelection {
                deploy_profile: reddb::storage::DeployProfile::Embedded,
                packaging: reddb::storage::StoragePackaging::OperationalDirectory,
                replica_count: 0,
                managed_backup: false,
                wal_retention: false,
            })
            .expect("operational-directory profile")
    };

    let mut expected = Vec::new();
    {
        let rt = Arc::new(RedDBRuntime::with_options(options()).expect("persistent runtime"));
        for (trial, mode) in [UpdateMode::CompareAndSet, UpdateMode::Plain]
            .into_iter()
            .cycle()
            .take(8)
            .enumerate()
        {
            let table = format!("replay_{trial}");
            seed_row(&rt, &table, TableShape::Unconstrained);
            let affected = race_updates(&rt, &table, mode);
            let context = format!("pre-reopen {mode:?} trial {trial}");
            assert_race_outcome(&rt, &table, mode, &affected, &context);
            expected.push((table.clone(), live_versions(&rt, &table)));
        }
    }

    let rt = RedDBRuntime::with_options(options()).expect("reopened runtime");
    for (table, versions) in expected {
        assert_eq!(
            live_versions(&rt, &table),
            versions,
            "{table}: replay must recover exactly the pre-reopen live version"
        );
    }
}
