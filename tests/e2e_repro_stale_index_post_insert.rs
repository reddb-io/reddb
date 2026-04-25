//! Repro for `mixed_workload_indexed` mini-duel failure: post-CREATE-INDEX
//! inserts go missing from filtered queries.
//!
//! Scenario:
//!   1. Insert N rows
//!   2. CREATE INDEX
//!   3. Insert N more rows (single-row INSERT)
//!   4. Query WHERE col = X AND col2 > Y
//!
//! Expect: all 2N rows considered; bench reports about half are missing.

use reddb::application::{CreateRowInput, CreateRowsBatchInput, RuntimeEntityPort};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

/// Insert one row through the same `create_rows_batch` entry that the
/// wire layer's MSG_BULK_INSERT_BINARY (0x06) reaches. This is the
/// path `mixed_workload_indexed`'s `insert_one` exercises.
fn create_one_via_port(rt: &RedDBRuntime, id: u32, age: u32, city: &str) {
    let collection = "users".to_string();
    let row = CreateRowInput {
        collection: collection.clone(),
        fields: vec![
            ("id".into(), Value::Integer(id as i64)),
            ("age".into(), Value::Integer(age as i64)),
            ("city".into(), Value::text(city.to_string())),
        ],
        metadata: Vec::new(),
        node_links: Vec::new(),
        vector_links: Vec::new(),
    };
    rt.create_rows_batch(CreateRowsBatchInput {
        collection,
        rows: vec![row],
    })
    .unwrap();
}

#[test]
fn post_create_index_inserts_via_port_visible_to_filtered_query() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    rt.execute_query("CREATE TABLE users (id INT, age INT, city TEXT)")
        .unwrap();

    for i in 0..50u32 {
        create_one_via_port(&rt, i, 20 + (i % 30), "NYC");
    }
    rt.execute_query("CREATE INDEX idx_age ON users (age) USING BTREE")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_city ON users (city) USING HASH")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_city_age ON users (city, age) USING BTREE")
        .unwrap();
    for i in 50..100u32 {
        create_one_via_port(&rt, i, 20 + (i % 30), "NYC");
    }

    let mut expected = 0u32;
    for i in 0..100u32 {
        if 20 + (i % 30) > 30 {
            expected += 1;
        }
    }
    let total = rt.execute_query("SELECT * FROM users").unwrap();
    assert_eq!(total.result.records.len(), 100, "total rows");
    let filtered = rt
        .execute_query("SELECT * FROM users WHERE city = 'NYC' AND age > 30")
        .unwrap();
    assert_eq!(
        filtered.result.records.len(),
        expected as usize,
        "filtered via port (with composite idx): got {}, expected {}",
        filtered.result.records.len(),
        expected,
    );
}

#[test]
fn post_create_index_inserts_visible_to_filtered_query() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    rt.execute_query("CREATE TABLE users (id INT, age INT, city TEXT)")
        .unwrap();

    for i in 0..50u32 {
        rt.execute_query(&format!(
            "INSERT INTO users (id, age, city) VALUES ({i}, {}, 'NYC')",
            20 + (i % 30)
        ))
        .unwrap();
    }

    rt.execute_query("CREATE INDEX idx_city ON users (city) USING HASH")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_age ON users (age) USING BTREE")
        .unwrap();

    for i in 50..100u32 {
        rt.execute_query(&format!(
            "INSERT INTO users (id, age, city) VALUES ({i}, {}, 'NYC')",
            20 + (i % 30)
        ))
        .unwrap();
    }

    // Ground truth: 100 rows total, all city=NYC.
    // age > 30 → ages 31..=49 → for each i, age = 20 + (i%30).
    //   age > 30 means (i%30) > 10, i.e. (i%30) in 11..=29 → 19 values per 30.
    let mut expected = 0u32;
    for i in 0..100u32 {
        let age = 20 + (i % 30);
        if age > 30 {
            expected += 1;
        }
    }
    let total = rt.execute_query("SELECT * FROM users").unwrap();
    assert_eq!(
        total.result.records.len(),
        100,
        "all 100 rows must be present (got {})",
        total.result.records.len()
    );
    let city_only = rt
        .execute_query("SELECT * FROM users WHERE city = 'NYC'")
        .unwrap();
    assert_eq!(
        city_only.result.records.len(),
        100,
        "city = 'NYC' must match all 100 rows (got {})",
        city_only.result.records.len()
    );
    let filtered = rt
        .execute_query("SELECT * FROM users WHERE city = 'NYC' AND age > 30")
        .unwrap();
    assert_eq!(
        filtered.result.records.len(),
        expected as usize,
        "filtered query lost post-CREATE-INDEX rows: got {}, expected {}",
        filtered.result.records.len(),
        expected
    );
}
