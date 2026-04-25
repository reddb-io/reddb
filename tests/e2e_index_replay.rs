//! Repro for T1.3: secondary indexes must survive a restart.
//!
//! Open a persistent DB, CREATE TABLE + INSERT + CREATE INDEX, drop the
//! runtime, reopen the same path, and assert that:
//!   1. table data is still there
//!   2. equality queries on the indexed column hit the index
//!      (correctness — they must return the right rows)

use reddb::{RedDBOptions, RedDBRuntime};
use std::path::PathBuf;

fn unique_data_dir(prefix: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("reddb-{prefix}-{pid}-{nanos}"));
    p
}

#[test]
fn persistent_reopen_restores_indexed_query_results() {
    let path = unique_data_dir("idx-replay");
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        rt.execute_query("CREATE TABLE users (id INT, age INT, city TEXT)")
            .unwrap();
        for i in 0..50u32 {
            rt.execute_query(&format!(
                "INSERT INTO users (id, age, city) VALUES ({i}, {}, 'NYC')",
                20 + (i % 10)
            ))
            .unwrap();
        }
        rt.execute_query("CREATE INDEX idx_city ON users (city) USING HASH")
            .unwrap();
        rt.execute_query("CREATE INDEX idx_age ON users (age) USING BTREE")
            .unwrap();

        let pre = rt
            .execute_query("SELECT * FROM users WHERE city = 'NYC'")
            .unwrap();
        assert_eq!(pre.result.records.len(), 50, "pre-restart sanity");
    }

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    let total = rt.execute_query("SELECT * FROM users").unwrap();
    assert_eq!(
        total.result.records.len(),
        50,
        "table data must survive restart, got {}",
        total.result.records.len()
    );

    let post = rt
        .execute_query("SELECT * FROM users WHERE city = 'NYC'")
        .unwrap();
    assert_eq!(
        post.result.records.len(),
        50,
        "after restart, indexed equality must still return all 50 rows: got {}",
        post.result.records.len()
    );

    let range = rt
        .execute_query("SELECT * FROM users WHERE age >= 25")
        .unwrap();
    assert_eq!(
        range.result.records.len(),
        25,
        "after restart, BTREE range must still return correct rows: got {}",
        range.result.records.len()
    );

    // Verify the index is actually re-hydrated (not just a full-scan
    // fallback yielding correct results). EXPLAIN should mention an
    // index_seek-style operator on `idx_city` for the equality, not a
    // raw `table_scan`.
    let explain = rt
        .execute_query("EXPLAIN SELECT * FROM users WHERE city = 'NYC'")
        .unwrap();
    let plan_text = explain
        .result
        .records
        .iter()
        .filter_map(|r| match r.get("op") {
            Some(reddb::storage::schema::Value::Text(t)) => Some(t.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(",");
    assert!(
        plan_text.contains("index_seek") || plan_text.contains("hash_index"),
        "after restart, indexed equality must use an index path, got plan ops: {}",
        plan_text
    );

    let _ = std::fs::remove_dir_all(&path);
}
