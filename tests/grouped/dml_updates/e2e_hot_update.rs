//! Phase 3.T2/T3 — HOT-like fast-path on UPDATE.
//!
//! Three properties:
//!
//! 1. UPDATE on a table with no secondary index still works
//!    correctly (the HOT decision fires but there's nothing to skip).
//! 2. UPDATE on a table with an index on column X, modifying column
//!    Y (not X), runs without tripping index_entity_update.
//! 3. UPDATE modifying an indexed column takes the fallback path —
//!    index stays consistent (still uniquely locatable).

use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
fn update_on_unindexed_table_works() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE t (id INT, v INT)");
    exec(&rt, "INSERT INTO t (id, v) VALUES (1, 10), (2, 20)");

    exec(&rt, "UPDATE t SET v = 99 WHERE id = 1");

    let r = rt.execute_query("SELECT v FROM t").unwrap();
    let dbg = format!("{:?}", r.result.records);
    assert!(dbg.contains("Integer(99)"), "updated value missing: {dbg}");
    assert!(dbg.contains("Integer(20)"), "untouched row missing: {dbg}");
}

#[test]
fn update_non_indexed_column_preserves_index_on_indexed_column() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT, score INT)");
    exec(&rt, "CREATE INDEX idx_email ON users (email) USING HASH");
    exec(
        &rt,
        "INSERT INTO users (id, email, score) VALUES \
         (1, 'a@x', 100), (2, 'b@x', 200)",
    );

    // Update `score` (not indexed). HOT path fires. idx_email
    // should still work.
    exec(&rt, "UPDATE users SET score = 999 WHERE id = 1");

    let r = rt
        .execute_query("SELECT id, score FROM users WHERE email = 'a@x'")
        .unwrap();
    let dbg = format!("{:?}", r.result.records);
    assert!(dbg.contains("Integer(999)"), "HOT update lost: {dbg}");
    assert!(dbg.contains("Integer(1)"), "row id mismatch: {dbg}");
}

#[test]
fn update_indexed_column_still_consistent() {
    let rt = open_runtime();
    exec(&rt, "CREATE TABLE users (id INT, email TEXT)");
    exec(&rt, "CREATE INDEX idx_email ON users (email) USING HASH");
    exec(
        &rt,
        "INSERT INTO users (id, email) VALUES (1, 'a@x'), (2, 'b@x')",
    );

    // Update `email` (indexed). HOT denied, fallback rebuilds index.
    exec(&rt, "UPDATE users SET email = 'NEW@x' WHERE id = 1");

    // Old key gone.
    let r = rt
        .execute_query("SELECT id FROM users WHERE email = 'a@x'")
        .unwrap();
    assert_eq!(
        r.result.records.len(),
        0,
        "old email still indexed: {:?}",
        r.result.records
    );
    // New key present.
    let r = rt
        .execute_query("SELECT id FROM users WHERE email = 'NEW@x'")
        .unwrap();
    let dbg = format!("{:?}", r.result.records);
    assert!(dbg.contains("Integer(1)"), "new email not indexed: {dbg}");
}
