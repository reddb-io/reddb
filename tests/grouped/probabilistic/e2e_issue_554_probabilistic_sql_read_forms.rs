//! Regression coverage for issue #554 — Probabilistic: SQL-read forms
//! `CARDINALITY`, `FREQ`, `CONTAINS` + alias respect.
//!
//! PRD #449 user stories 23 and 24. Pins the three acceptance bullets:
//!
//! 1. `select_cardinality_returns_hll_count` — `SELECT CARDINALITY
//!    FROM <hll>` returns the same count as the `HLL COUNT` command.
//! 2. `select_freq_returns_sketch_estimate` — `SELECT FREQ('key')
//!    FROM <sketch>` returns the same estimate as `SKETCH COUNT`.
//! 3. `select_contains_returns_filter_membership` — `SELECT
//!    CONTAINS('item') FROM <filter>` returns the same boolean as
//!    `FILTER CHECK`.
//! 4. `select_read_forms_preserve_user_alias` — `AS <alias>` is
//!    preserved in the result envelope for each of the three forms.
//!
//! The implementation predates this issue (shipped under #542); the
//! contract was anchored under `runtime_query_behavior.rs` for the
//! command layer, but the SQL-read forms + alias round-trip were
//! not pinned behind a file named after this issue. This slice anchors
//! the four acceptance bullets so future regressions surface at the
//! right test boundary.

#[path = "../../support/mod.rs"]
mod support;

use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn only_record(result: &RuntimeQueryResult) -> &UnifiedRecord {
    assert_eq!(
        result.result.records.len(),
        1,
        "expected one row for query `{}`",
        result.query
    );
    &result.result.records[0]
}

fn uint_value(row: &UnifiedRecord, column: &str) -> u64 {
    match row.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer column {column}, got {other:?}"),
    }
}

fn bool_value(row: &UnifiedRecord, column: &str) -> bool {
    match row.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected bool column {column}, got {other:?}"),
    }
}

#[test]
fn select_cardinality_returns_hll_count() {
    let rt = runtime();
    exec(&rt, "CREATE HLL visitors");
    exec(&rt, "HLL ADD visitors 'alice' 'bob' 'alice' 'carol'");

    let command = exec(&rt, "HLL COUNT visitors");
    let sql = exec(&rt, "SELECT CARDINALITY FROM visitors");

    assert_eq!(sql.result.columns, vec!["cardinality"]);
    assert_eq!(
        uint_value(only_record(&sql), "cardinality"),
        uint_value(only_record(&command), "count"),
    );
    assert_eq!(uint_value(only_record(&sql), "cardinality"), 3);
}

#[test]
fn select_freq_returns_sketch_estimate() {
    let rt = runtime();
    exec(&rt, "CREATE SKETCH clicks");
    exec(&rt, "SKETCH ADD clicks 'signup' 7");
    exec(&rt, "SKETCH ADD clicks 'login' 2");

    let command = exec(&rt, "SKETCH COUNT clicks 'signup'");
    let sql = exec(&rt, "SELECT FREQ('signup') FROM clicks");

    assert_eq!(sql.result.columns, vec!["freq"]);
    assert_eq!(
        uint_value(only_record(&sql), "freq"),
        uint_value(only_record(&command), "estimate"),
    );
    assert_eq!(uint_value(only_record(&sql), "freq"), 7);
}

#[test]
fn select_contains_returns_filter_membership() {
    let rt = runtime();
    exec(&rt, "CREATE FILTER sessions");
    exec(&rt, "FILTER ADD sessions 'sess:abc'");

    let command_hit = exec(&rt, "FILTER CHECK sessions 'sess:abc'");
    let sql_hit = exec(&rt, "SELECT CONTAINS('sess:abc') FROM sessions");
    assert_eq!(sql_hit.result.columns, vec!["contains"]);
    assert_eq!(
        bool_value(only_record(&sql_hit), "contains"),
        bool_value(only_record(&command_hit), "exists"),
    );
    assert!(bool_value(only_record(&sql_hit), "contains"));

    let sql_miss = exec(&rt, "SELECT CONTAINS('sess:never') FROM sessions");
    assert!(!bool_value(only_record(&sql_miss), "contains"));
}

#[test]
fn select_read_forms_preserve_user_alias() {
    let rt = runtime();
    exec(&rt, "CREATE HLL visitors");
    exec(&rt, "HLL ADD visitors 'a' 'b' 'c'");
    exec(&rt, "CREATE SKETCH clicks");
    exec(&rt, "SKETCH ADD clicks 'signup' 5");
    exec(&rt, "CREATE FILTER sessions");
    exec(&rt, "FILTER ADD sessions 'sess:abc'");

    let card = exec(&rt, "SELECT CARDINALITY AS uniques FROM visitors");
    assert_eq!(card.result.columns, vec!["uniques"]);
    assert_eq!(uint_value(only_record(&card), "uniques"), 3);

    let freq = exec(&rt, "SELECT FREQ('signup') AS signups FROM clicks");
    assert_eq!(freq.result.columns, vec!["signups"]);
    assert_eq!(uint_value(only_record(&freq), "signups"), 5);

    let contains = exec(&rt, "SELECT CONTAINS('sess:abc') AS seen FROM sessions");
    assert_eq!(contains.result.columns, vec!["seen"]);
    assert!(bool_value(only_record(&contains), "seen"));
}
