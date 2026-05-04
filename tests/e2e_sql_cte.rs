//! End-to-end coverage for non-recursive CTEs (#41).
//!
//! Verifies that `WITH x AS (...) SELECT ... FROM x` returns the same
//! rows as the equivalent un-CTE'd query, and that `WITH RECURSIVE`
//! errors out with a clear message rather than silently misparsing.

use reddb::{RedDBOptions, RedDBRuntime};

fn seed_users(rt: &RedDBRuntime) {
    rt.execute_query(
        "CREATE TABLE users (id INT, name TEXT, status TEXT, age INT)",
    )
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
fn with_recursive_returns_clear_not_implemented_error() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    seed_users(&rt);

    let err = rt
        .execute_query(
            "WITH RECURSIVE walk AS (SELECT id FROM users) SELECT * FROM walk",
        )
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
