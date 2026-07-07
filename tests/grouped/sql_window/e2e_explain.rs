//! EXPLAIN clause end-to-end tests (T2 / PG gap item #13).
//!
//! Covers the plain `EXPLAIN <stmt>` surface: the runtime intercepts
//! before SQL parsing, runs the planner on the inner statement
//! without executing it, and returns the CanonicalLogicalNode tree
//! as rows. Columns: op, source, estimated_rows, estimated_cost, depth.
//!
//! `EXPLAIN ALTER FOR CREATE TABLE ...` is a separate schema-diff
//! command and stays on the regular SQL path.

use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

const EXPLAIN_PLAN_COLUMNS: &[&str] =
    &["op", "source", "estimated_rows", "estimated_cost", "depth"];
const EXPLAIN_ANALYZE_COLUMNS: &[&str] = &[
    "op",
    "source",
    "estimated_rows",
    "estimated_cost",
    "actual_rows",
    "actual_ms",
    "depth",
];

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn uint_value(row: &reddb::storage::query::unified::UnifiedRecord, column: &str) -> u64 {
    match row.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => *value as u64,
        other => panic!("{column} must be an integer count, got {other:?}"),
    }
}

fn analyze_dml(rt: &RedDBRuntime, sql: &str, expected_rows: u64) {
    let analyzed = rt.execute_query(sql).expect(sql);
    assert_eq!(analyzed.statement, "explain_analyze", "{sql}");
    assert_eq!(analyzed.statement_type, "select", "{sql}");
    assert_eq!(analyzed.affected_rows, 0, "{sql}");
    assert_eq!(analyzed.result.columns, EXPLAIN_ANALYZE_COLUMNS, "{sql}");
    assert_eq!(analyzed.result.records.len(), 1, "{sql}");

    let root = &analyzed.result.records[0];
    assert!(
        matches!(root.get("op"), Some(Value::Text(op)) if op.as_ref() == "dml_ddl"),
        "{sql}: {root:?}"
    );
    assert_eq!(uint_value(root, "actual_rows"), expected_rows, "{sql}");
    assert!(
        matches!(root.get("actual_ms"), Some(Value::Float(ms)) if *ms >= 0.0),
        "{sql}: {root:?}"
    );
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
    assert_eq!(result.result.columns, EXPLAIN_PLAN_COLUMNS);

    assert!(
        !result.result.records.is_empty(),
        "plan must have at least a root node"
    );

    // Root node: depth=0, op populated.
    let root = &result.result.records[0];
    match root.get("depth") {
        Some(Value::Integer(0)) => {}
        other => panic!("root depth must be 0, got {other:?}"),
    }
    match root.get("op") {
        Some(Value::Text(op)) => assert!(!op.is_empty(), "root op must be non-empty"),
        other => panic!("root op must be Text, got {other:?}"),
    }
    // Planner row counts and costs must be explicitly labeled as estimates.
    assert!(matches!(root.get("estimated_rows"), Some(Value::Float(_))));
    assert!(matches!(root.get("estimated_cost"), Some(Value::Float(_))));
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
fn explain_dml_returns_plan_rows_without_executing_inner_statement() {
    let rt = rt();
    exec(&rt, "CREATE TABLE audit (id INT, note TEXT)");
    exec(
        &rt,
        "INSERT INTO audit (id, note) VALUES (1, 'keep'), (2, 'remove')",
    );

    for sql in [
        "EXPLAIN INSERT INTO audit (id, note) VALUES (99, 'boom')",
        "EXPLAIN UPDATE audit SET note = 'changed' WHERE id = 1",
        "EXPLAIN DELETE FROM audit WHERE id = 2",
    ] {
        let explained = rt.execute_query(sql).expect(sql);
        assert_eq!(explained.statement, "explain", "{sql}");
        assert_eq!(explained.affected_rows, 0, "{sql}");
        assert_eq!(explained.result.columns, EXPLAIN_PLAN_COLUMNS, "{sql}");
        assert_eq!(explained.result.records.len(), 1, "{sql}");

        let root = &explained.result.records[0];
        assert!(
            matches!(root.get("op"), Some(Value::Text(op)) if op.as_ref() == "dml_ddl"),
            "{sql}: {root:?}"
        );
        assert!(
            matches!(root.get("source"), Some(Value::Null)),
            "{sql}: {root:?}"
        );
        assert!(
            matches!(root.get("estimated_rows"), Some(Value::Float(0.0))),
            "{sql}: {root:?}"
        );
        assert!(
            matches!(root.get("estimated_cost"), Some(Value::Float(1.0))),
            "{sql}: {root:?}"
        );
        assert!(
            matches!(root.get("depth"), Some(Value::Integer(0))),
            "{sql}: {root:?}"
        );
    }

    let after = rt
        .execute_query("SELECT id, note FROM audit")
        .expect("SELECT after EXPLAIN");
    assert_eq!(
        after.result.records.len(),
        2,
        "EXPLAIN must not materialise INSERT, UPDATE, or DELETE"
    );
    assert!(after.result.records.iter().any(|row| {
        matches!(row.get("id"), Some(Value::Integer(1)))
            && matches!(row.get("note"), Some(Value::Text(note)) if note.as_ref() == "keep")
    }));
    assert!(after.result.records.iter().any(|row| {
        matches!(row.get("id"), Some(Value::Integer(2)))
            && matches!(row.get("note"), Some(Value::Text(note)) if note.as_ref() == "remove")
    }));
}

#[test]
fn explain_analyze_dml_reports_real_counts_and_always_aborts() {
    let rt = rt();
    exec(&rt, "CREATE TABLE audit_analyze (id INT, note TEXT)");
    exec(
        &rt,
        "INSERT INTO audit_analyze (id, note) VALUES (1, 'keep'), (2, 'update'), (3, 'delete')",
    );
    exec(
        &rt,
        "ALTER TABLE audit_analyze ENABLE EVENTS TO audit_analyze_events",
    );
    exec(&rt, "QUEUE GROUP CREATE audit_analyze_events readers");

    analyze_dml(
        &rt,
        "EXPLAIN ANALYZE UPDATE audit_analyze SET note = 'changed' WHERE id IN (1, 2)",
        2,
    );
    analyze_dml(
        &rt,
        "EXPLAIN ANALYZE DELETE FROM audit_analyze WHERE id = 3",
        1,
    );
    analyze_dml(
        &rt,
        "EXPLAIN ANALYZE INSERT INTO audit_analyze (id, note) VALUES (4, 'inserted')",
        1,
    );

    let after = rt
        .execute_query("SELECT id, note FROM audit_analyze")
        .expect("SELECT after EXPLAIN ANALYZE");
    assert_eq!(
        after.result.records.len(),
        3,
        "EXPLAIN ANALYZE must not materialise INSERT, UPDATE, or DELETE"
    );
    assert!(after.result.records.iter().any(|row| {
        matches!(row.get("id"), Some(Value::Integer(1)))
            && matches!(row.get("note"), Some(Value::Text(note)) if note.as_ref() == "keep")
    }));
    assert!(after.result.records.iter().any(|row| {
        matches!(row.get("id"), Some(Value::Integer(2)))
            && matches!(row.get("note"), Some(Value::Text(note)) if note.as_ref() == "update")
    }));
    assert!(after.result.records.iter().any(|row| {
        matches!(row.get("id"), Some(Value::Integer(3)))
            && matches!(row.get("note"), Some(Value::Text(note)) if note.as_ref() == "delete")
    }));

    let events = rt
        .execute_query("QUEUE READ audit_analyze_events GROUP readers CONSUMER c1 COUNT 10")
        .expect("read event queue after EXPLAIN ANALYZE");
    assert_eq!(
        events.result.records.len(),
        0,
        "EXPLAIN ANALYZE DML must not leak commit-time effects"
    );
}

#[test]
fn explain_analyze_dml_error_path_aborts_staged_writes() {
    let rt = rt();
    exec(
        &rt,
        "CREATE TABLE analyze_error_path (id INT PRIMARY KEY, note TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO analyze_error_path (id, note) VALUES (1, 'base')",
    );

    let err = rt
        .execute_query(
            "EXPLAIN ANALYZE INSERT INTO analyze_error_path (id, note) \
             VALUES (2, 'staged'), (1, 'duplicate')",
        )
        .expect_err("duplicate insert under EXPLAIN ANALYZE should fail");
    let message = format!("{err:?}");
    assert!(
        message.contains("duplicate") || message.contains("unique") || message.contains("violated"),
        "error should name the uniqueness failure: {err:?}"
    );

    let after = rt
        .execute_query("SELECT id, note FROM analyze_error_path")
        .expect("SELECT after failed EXPLAIN ANALYZE");
    assert_eq!(
        after.result.records.len(),
        1,
        "failed EXPLAIN ANALYZE must roll back any staged rows"
    );
    assert!(after.result.records.iter().any(|row| {
        matches!(row.get("id"), Some(Value::Integer(1)))
            && matches!(row.get("note"), Some(Value::Text(note)) if note.as_ref() == "base")
    }));
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
    // Both generic EXPLAIN and the existing EXPLAIN ALTER set
    // statement="explain", so distinguish by the column shape: the
    // schema-diff command reports ["table","format","diff"], while
    // the plan-tree shape is ["op","source","estimated_rows","estimated_cost","depth"].
    assert_eq!(
        r.result.columns,
        vec!["table", "format", "diff"],
        "EXPLAIN ALTER must route to the schema-diff shape"
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
    exec(&rt, "CREATE INDEX idx_users_email ON users (email)");

    let result = rt
        .execute_query("EXPLAIN SELECT id FROM users WHERE email = 'a@x'")
        .expect("EXPLAIN SELECT with index");

    // At least one node references the target table in `source`.
    let has_user_source = result
        .result
        .records
        .iter()
        .any(|rec| matches!(rec.get("source"), Some(Value::Text(s)) if s.as_ref() == "users"));
    assert!(
        has_user_source,
        "plan should mention the `users` source somewhere; records={:?}",
        result.result.records
    );
}
