//! EXPLAIN clause end-to-end tests (T2 / PG gap item #13).
//!
//! Covers the plain `EXPLAIN <stmt>` surface: the runtime intercepts
//! before SQL parsing, runs the planner on the inner statement
//! without executing it, and returns the CanonicalLogicalNode tree
//! as rows. Columns: op, source, est_rows, est_cost, depth.
//!
//! `EXPLAIN ALTER FOR CREATE TABLE ...` is a separate schema-diff
//! command and stays on the regular SQL path.

use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
fn explain_select_returns_plan_tree_rows() {
    let rt = rt();
    exec(&rt, "CREATE TABLE hosts (id INT, os TEXT)");
    exec(
        &rt,
        "INSERT INTO hosts (id, os) VALUES (1, 'Linux'), (2, 'Darwin'), (3, 'Linux')",
    );

    let result = rt
        .execute_query("EXPLAIN SELECT * FROM hosts WHERE os = 'Linux'")
        .expect("EXPLAIN should succeed");

    assert_eq!(result.statement, "explain");
    assert_eq!(result.statement_type, "select");
    assert_eq!(result.affected_rows, 0);

    // Column contract — HTTP/gRPC surface depends on this shape.
    assert_eq!(
        result.result.columns,
        vec!["op", "source", "est_rows", "est_cost", "depth"]
    );

    assert!(
        !result.result.records.is_empty(),
        "plan must have at least a root node"
    );

    // Root node: depth=0, op populated.
    let root = &result.result.records[0];
    match root.values.get("depth") {
        Some(Value::Integer(0)) => {}
        other => panic!("root depth must be 0, got {other:?}"),
    }
    match root.values.get("op") {
        Some(Value::Text(op)) => assert!(!op.is_empty(), "root op must be non-empty"),
        other => panic!("root op must be Text, got {other:?}"),
    }
    // est_rows/est_cost are floats.
    assert!(matches!(root.values.get("est_rows"), Some(Value::Float(_))));
    assert!(matches!(root.values.get("est_cost"), Some(Value::Float(_))));
}

#[test]
fn explain_is_case_insensitive() {
    let rt = rt();
    exec(&rt, "CREATE TABLE t (id INT)");
    exec(&rt, "INSERT INTO t (id) VALUES (1)");
    let upper = rt.execute_query("EXPLAIN SELECT * FROM t").unwrap();
    let lower = rt.execute_query("explain select * from t").unwrap();
    let mixed = rt.execute_query("Explain Select * From t").unwrap();
    assert_eq!(upper.statement, "explain");
    assert_eq!(lower.statement, "explain");
    assert_eq!(mixed.statement, "explain");
}

#[test]
fn explain_does_not_execute_the_inner_statement() {
    let rt = rt();
    exec(&rt, "CREATE TABLE audit (id INT, note TEXT)");

    // EXPLAIN on an INSERT must not actually write the row.
    let _ = rt
        .execute_query("EXPLAIN INSERT INTO audit (id, note) VALUES (99, 'boom')")
        .expect("EXPLAIN INSERT should succeed");

    let after = rt
        .execute_query("SELECT * FROM audit")
        .expect("SELECT after EXPLAIN");
    assert_eq!(
        after.result.records.len(),
        0,
        "EXPLAIN must not materialise the INSERT"
    );
}

#[test]
fn explain_alter_still_routes_to_schema_diff() {
    let rt = rt();
    exec(&rt, "CREATE TABLE diffable (id INT, name TEXT)");
    // The schema-diff command preserves its existing shape (non-empty
    // text-y result, no panic). Just confirm it still parses and
    // returns something — the dedicated DDL tests exercise the
    // diff semantics in more detail.
    let result =
        rt.execute_query("EXPLAIN ALTER FOR CREATE TABLE diffable (id INT, name TEXT, email TEXT)");
    assert!(
        result.is_ok(),
        "EXPLAIN ALTER FOR should still route to the schema diff path: {:?}",
        result.err()
    );
    let r = result.unwrap();
    assert_ne!(
        r.statement, "explain",
        "EXPLAIN ALTER must not use the plan-tree shape"
    );
}

#[test]
fn explain_indexed_select_mentions_index_or_scan_op() {
    let rt = rt();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT)");
    exec(
        &rt,
        "INSERT INTO users (id, email) VALUES (1, 'a@x'), (2, 'b@x')",
    );
    exec(&rt, "CREATE INDEX ON users (email)");

    let result = rt
        .execute_query("EXPLAIN SELECT id FROM users WHERE email = 'a@x'")
        .expect("EXPLAIN SELECT with index");

    // At least one node references the target table in `source`.
    let has_user_source = result
        .result
        .records
        .iter()
        .any(|rec| matches!(rec.values.get("source"), Some(Value::Text(s)) if s == "users"));
    assert!(
        has_user_source,
        "plan should mention the `users` source somewhere; records={:?}",
        result.result.records
    );
}
