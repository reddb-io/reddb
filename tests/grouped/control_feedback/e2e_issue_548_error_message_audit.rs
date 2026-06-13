//! Issue #548 — Error-message audit: unsupported syntax points to the right API.
//!
//! Parent PRD #449, user story 51: when a user hits an unsupported SQL/DSL
//! form, the error message must name the supported alternative so the
//! interaction is a guided path instead of a dead end. This file is the
//! inventory plus regression tests for the highest-traffic public error
//! sites identified from the feedback-driven user stories.
//!
//! Inventory (source of truth):
//!
//! - `SELECT * FROM <hll|sketch|filter>` — already guided
//!   (`runtime/impl_probabilistic.rs`, pinned by
//!   `tests/e2e_issue_542_probabilistic_commands.rs`). This file adds a
//!   sentinel for sketch and filter so the contract is anchored on every
//!   probabilistic model, not just HLL.
//! - `CREATE TABLE ... WITH <unknown-option>` — `parser/ddl.rs`. Must name
//!   `TTL` as the supported option and show the duration shape.
//! - `CREATE TABLE ... WITH TTL <n> <bad-unit>` — `parser/ddl.rs`. Must
//!   list the supported units (ms, s, m, h, d).
//! - `INSERT/UPDATE ... WITH TTL <n> <bad-unit>` — `parser/dml.rs`. Same
//!   contract as the DDL TTL unit message.
//! - HTTP / SDK payload TTL field with bad unit — `application/ttl_payload.rs`.
//!   Same contract, but reached over the JSON transport rather than SQL.
//!   The crate-private helper is exercised indirectly through the HTTP
//!   suite; the SQL parser tests below pin the verbatim string both paths
//!   produce.
//! - `SELECT <expr> FROM QUEUE` with a non-trivial projection —
//!   `parser/table.rs`. Must point at `SELECT *` / bare columns / queue
//!   verbs.

#[path = "../../support/mod.rs"]
mod support;

use reddb::runtime::RedDBRuntime;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn err_text(rt: &RedDBRuntime, sql: &str) -> String {
    let err = rt
        .execute_query(sql)
        .err()
        .unwrap_or_else(|| panic!("expected `{sql}` to error, but it succeeded"));
    format!("{err:?}")
}

fn assert_contains_all(haystack: &str, needles: &[&str], context: &str) {
    for needle in needles {
        assert!(
            haystack.contains(needle),
            "{context}: expected error to contain {needle:?}, got: {haystack}"
        );
    }
}

#[test]
fn select_star_from_sketch_points_to_freq_read_form() {
    let rt = runtime();
    rt.execute_query("CREATE SKETCH clicks")
        .expect("create sketch");
    let message = err_text(&rt, "SELECT * FROM clicks");
    assert_contains_all(
        &message,
        &["clicks", "SELECT CARDINALITY", "FREQ(", "CONTAINS("],
        "SELECT * FROM <sketch>",
    );
}

#[test]
fn select_star_from_filter_points_to_contains_read_form() {
    let rt = runtime();
    rt.execute_query("CREATE FILTER sessions")
        .expect("create filter");
    let message = err_text(&rt, "SELECT * FROM sessions");
    assert_contains_all(
        &message,
        &["sessions", "SELECT CARDINALITY", "FREQ(", "CONTAINS("],
        "SELECT * FROM <filter>",
    );
}

#[test]
fn create_table_unsupported_option_points_to_ttl_syntax() {
    let rt = runtime();
    let message = err_text(
        &rt,
        "CREATE TABLE t (id INTEGER PRIMARY KEY) WITH RETENTION 7 d",
    );
    assert_contains_all(
        &message,
        &[
            "unsupported CREATE TABLE option",
            "TTL",
            "ms",
            "s",
            "m",
            "h",
            "d",
        ],
        "CREATE TABLE WITH <unknown>",
    );
}

#[test]
fn create_table_unsupported_ttl_unit_lists_supported_units() {
    let rt = runtime();
    let message = err_text(
        &rt,
        "CREATE TABLE t (id INTEGER PRIMARY KEY) WITH TTL 7 fortnights",
    );
    assert_contains_all(
        &message,
        &["unsupported TTL unit", "ms", "s", "m", "h", "d"],
        "CREATE TABLE WITH TTL <bad unit>",
    );
}

#[test]
fn insert_with_ttl_unsupported_unit_lists_supported_units() {
    let rt = runtime();
    rt.execute_query("CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT)")
        .expect("create table");
    let message = err_text(
        &rt,
        "INSERT INTO t (id, body) VALUES (1, 'x') WITH TTL 5 fortnights",
    );
    assert_contains_all(
        &message,
        &["unsupported TTL unit", "ms", "s", "m", "h", "d"],
        "INSERT WITH TTL <bad unit>",
    );
}

#[test]
fn select_from_queue_unsupported_projection_points_to_queue_verbs() {
    let rt = runtime();
    rt.execute_query("CREATE QUEUE jobs").expect("create queue");
    let message = err_text(&rt, "SELECT id + 1 FROM QUEUE jobs");
    assert_contains_all(
        &message,
        &[
            "unsupported SELECT FROM QUEUE projection",
            "SELECT *",
            "PUSH",
            "POP",
        ],
        "SELECT <expr> FROM QUEUE",
    );
}
