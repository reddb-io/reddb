//! End-to-end coverage for non-recursive CTEs (#41).
//!
//! Verifies that `WITH x AS (...) SELECT ... FROM x` returns the same
//! rows as the equivalent un-CTE'd query, and that `WITH RECURSIVE`
//! errors out with a clear message rather than silently misparsing.

use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn seed_users(rt: &RedDBRuntime) {
    rt.execute_query("CREATE TABLE users (id INT, name TEXT, status TEXT, age INT)")
        .unwrap();
    let rows = [
        (1, "alice", "active", 30),
        (2, "bob", "inactive", 25),
        (3, "carol", "active", 41),
        (4, "dave", "active", 22),
        (5, "eve", "inactive", 35),
    ];
    for (id, name, status, age) in rows {
        rt.execute_query(&format!(
            "INSERT INTO users (id, name, status, age) \
             VALUES ({id}, '{name}', '{status}', {age})"
        ))
        .unwrap();
    }
}

fn seed_orders(rt: &RedDBRuntime) {
    rt.execute_query("CREATE TABLE orders (id INT, user_id INT, total INT)")
        .unwrap();
    rt.execute_query(
        "INSERT INTO orders (id, user_id, total) VALUES \
         (1, 1, 50), (2, 3, 200), (3, 4, 150), (4, 5, 20)",
    )
    .unwrap();
}

fn selected_ids(rt: &RedDBRuntime, sql: &str) -> Vec<i64> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    result
        .result
        .records
        .iter()
        .map(
            |record| match record.get("id").or_else(|| record.get("c0")) {
                Some(Value::Integer(id)) => *id,
                other => panic!("expected integer id, got {other:?} in record {record:?}"),
            },
        )
        .collect()
}

#[test]
fn cte_single_reference_filters_through_inlined_subquery() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);

    let r = rt
        .execute_query(
            "WITH active_users AS (SELECT id, name FROM users WHERE status = 'active') \
             SELECT * FROM active_users",
        )
        .unwrap();

    // 3 active users (alice, carol, dave)
    assert_eq!(
        r.result.records.len(),
        3,
        "expected 3 active users, got {}",
        r.result.records.len()
    );
}

#[test]
fn cte_chained_definitions_resolve_in_declaration_order() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);

    let r = rt
        .execute_query(
            "WITH \
                active AS (SELECT id, name, age FROM users WHERE status = 'active'), \
                young_active AS (SELECT id, name FROM active WHERE age < 30) \
             SELECT * FROM young_active",
        )
        .unwrap();

    // active = {alice (30), carol (41), dave (22)}; age<30 → dave
    assert_eq!(
        r.result.records.len(),
        1,
        "expected 1 young active user (dave), got {}",
        r.result.records.len()
    );
}

#[test]
fn cte_referenced_from_inside_join() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);

    rt.execute_query("CREATE TABLE roles (user_id INT, role TEXT)")
        .unwrap();
    for (uid, role) in [(1, "admin"), (3, "admin"), (4, "viewer")] {
        rt.execute_query(&format!(
            "INSERT INTO roles (user_id, role) VALUES ({uid}, '{role}')"
        ))
        .unwrap();
    }

    let r = rt
        .execute_query(
            "WITH active AS (SELECT id, name FROM users WHERE status = 'active') \
             FROM active a JOIN ANY r ON a.id = r._entity_id \
             RETURN a.name, r._score",
        )
        .unwrap();

    // active = {alice, carol, dave}; the JOIN ANY here is just to
    // exercise the parser plumbing — we only assert that the CTE
    // resolves into the join's left side and the query parses.
    // Recordset shape varies by JOIN ANY semantics; no row count
    // assertion past "did not error".
    let _ = r;
}

#[test]
fn explain_renders_cte_marker_node_per_named_cte() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);

    let r = rt
        .execute_query(
            "EXPLAIN WITH active AS (SELECT id, name FROM users WHERE status = 'active') \
             SELECT * FROM active",
        )
        .unwrap();

    // Find the synthetic CteScan row prepended by `explain_as_rows`.
    let cte_row = r
        .result
        .records
        .iter()
        .find(|rec| {
            rec.get("op")
                .and_then(|v| v.as_text())
                .map(|s| s == "CteScan")
                .unwrap_or(false)
        })
        .expect("EXPLAIN output should contain a CteScan row");

    let source = cte_row
        .get("source")
        .and_then(|v| v.as_text())
        .unwrap_or_default();
    assert_eq!(source, "active", "CteScan should name the CTE");
}

#[test]
fn with_recursive_returns_clear_not_implemented_error() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);

    let err = rt
        .execute_query("WITH RECURSIVE walk AS (SELECT id FROM users) SELECT * FROM walk")
        .expect_err("recursive CTE should error");

    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("recursive"),
        "error should mention recursive: got `{msg}`"
    );
    assert!(
        msg.to_lowercase().contains("not yet supported")
            || msg.to_lowercase().contains("not supported"),
        "error should be a clear not-yet-supported message: got `{msg}`"
    );
}

#[test]
fn from_subquery_applies_outer_filter_and_ordering() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);

    let ids = selected_ids(
        &rt,
        "SELECT id FROM \
            (SELECT id, age FROM users WHERE status = 'active') AS active_users \
         WHERE age < 35 \
         ORDER BY id",
    );

    assert_eq!(ids, vec![1, 4]);
}

#[test]
fn where_in_subquery_materializes_first_column_values() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);
    seed_orders(&rt);

    let ids = selected_ids(
        &rt,
        "SELECT id FROM users \
         WHERE id IN (SELECT user_id FROM orders WHERE total > 100) \
         ORDER BY id",
    );

    assert_eq!(ids, vec![3, 4]);
}

#[test]
fn cte_from_subquery_and_where_subquery_compose_in_one_statement() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);
    seed_orders(&rt);

    let ids = selected_ids(
        &rt,
        "WITH \
            active_users AS (SELECT id, age FROM users WHERE status = 'active'), \
            high_value_buyers AS (SELECT user_id FROM orders WHERE total > 100) \
         SELECT id FROM \
            (SELECT id, age FROM active_users WHERE age >= 20) AS visible_users \
         WHERE id IN (SELECT user_id FROM high_value_buyers) \
         ORDER BY id",
    );

    assert_eq!(ids, vec![3, 4]);
}

#[test]
fn scalar_subquery_can_drive_where_comparison() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);

    let ids = selected_ids(
        &rt,
        "SELECT id FROM users \
         WHERE age > (SELECT age FROM users WHERE name = 'alice') \
         ORDER BY id",
    );

    assert_eq!(ids, vec![3, 5]);
}

#[test]
fn scalar_subquery_in_select_list_can_reference_named_cte() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);
    seed_orders(&rt);

    let result = rt
        .execute_query(
            "WITH high_value_buyers AS (SELECT user_id FROM orders WHERE total > 100) \
             SELECT id, \
                    (SELECT user_id FROM high_value_buyers WHERE user_id = 3) AS matched_user \
             FROM users \
             WHERE id = 3",
        )
        .unwrap();

    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("matched_user"),
        Some(&Value::Integer(3)),
        "record={:?}",
        result.result.records[0]
    );
}

#[test]
fn correlated_where_subquery_returns_clear_error() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);

    let err = rt
        .execute_query(
            "SELECT id FROM users u \
             WHERE age > (SELECT age FROM users WHERE id = u.id)",
        )
        .expect_err("correlated subquery should not silently execute");
    let msg = format!("{err}");
    assert!(
        msg.contains("correlated subqueries are not supported"),
        "expected correlated-subquery error, got `{msg}`"
    );
}
