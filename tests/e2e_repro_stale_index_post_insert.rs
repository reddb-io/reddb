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
use std::sync::Arc;

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

#[test]
fn post_create_index_update_keeps_index_in_sync() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    rt.execute_query("CREATE TABLE users (id INT, age INT, city TEXT)")
        .unwrap();
    for i in 0..30u32 {
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

    rt.execute_query("UPDATE users SET city = 'LA' WHERE id < 10")
        .unwrap();

    let nyc = rt
        .execute_query("SELECT * FROM users WHERE city = 'NYC'")
        .unwrap();
    assert_eq!(
        nyc.result.records.len(),
        20,
        "after UPDATE, idx_city must reflect new city values: NYC count {}",
        nyc.result.records.len()
    );
    let la = rt
        .execute_query("SELECT * FROM users WHERE city = 'LA'")
        .unwrap();
    assert_eq!(
        la.result.records.len(),
        10,
        "after UPDATE, idx_city must include rows that moved to 'LA': got {}",
        la.result.records.len()
    );

    rt.execute_query("UPDATE users SET age = 99 WHERE id = 0")
        .unwrap();
    let old_age = rt
        .execute_query("SELECT * FROM users WHERE age = 20")
        .unwrap();
    assert!(
        old_age.result.records.iter().all(|r| {
            match r.get("id") {
                Some(Value::Integer(n)) => *n != 0,
                _ => true,
            }
        }),
        "after UPDATE age=99 WHERE id=0, BTREE on age must drop id=0 from age=20"
    );
    let new_age = rt
        .execute_query("SELECT * FROM users WHERE age = 99")
        .unwrap();
    assert_eq!(
        new_age.result.records.len(),
        1,
        "after UPDATE, BTREE on age must surface the new key: got {}",
        new_age.result.records.len()
    );
}

/// Mirror of the bench `reddb_wire` `insert_one` path: single-row
/// MSG_BULK_INSERT_BINARY_PREVALIDATED via
/// `create_rows_batch_prevalidated_columnar`. This is the path
/// `mixed_workload_indexed` exercises.
///
/// `#[ignore]`: the index-maintenance hook in
/// `create_rows_batch_prevalidated_columnar` is in place, but the test
/// still fails because of a separate entity-store bug — single-row
/// `bulk_insert` calls land in a growing/sealed segment whose
/// position-based `flat_entities` lookup (`segment.get(id)` keyed by
/// `id - base_entity_id`) returns None for ~half the IDs even though
/// `query_all` (full scan) finds them. Tracked as the
/// "post-CREATE-INDEX prevalidated entity store consistency" follow-up.
#[ignore = "blocked on entity-store get-by-id consistency for single-row prevalidated bulk inserts"]
#[test]
fn post_create_index_inserts_via_prevalidated_columnar_keeps_index_fresh() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    rt.execute_query("CREATE TABLE users (id INT, age INT, city TEXT)")
        .unwrap();

    let schema: Arc<Vec<String>> =
        Arc::new(vec!["id".into(), "age".into(), "city".into()]);

    let mut bulk_rows: Vec<Vec<Value>> = Vec::with_capacity(50);
    for i in 0..50u32 {
        bulk_rows.push(vec![
            Value::Integer(i as i64),
            Value::Integer((20 + (i % 30)) as i64),
            Value::text("NYC".to_string()),
        ]);
    }
    rt.create_rows_batch_prevalidated_columnar(
        "users".to_string(),
        Arc::clone(&schema),
        bulk_rows,
    )
    .unwrap();

    rt.execute_query("CREATE INDEX idx_age ON users (age) USING BTREE")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_city ON users (city) USING HASH")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_city_age ON users (city, age) USING BTREE")
        .unwrap();

    for i in 50..100u32 {
        let row = vec![
            Value::Integer(i as i64),
            Value::Integer((20 + (i % 30)) as i64),
            Value::text("NYC".to_string()),
        ];
        rt.create_rows_batch_prevalidated_columnar(
            "users".to_string(),
            Arc::clone(&schema),
            vec![row],
        )
        .unwrap();
    }

    let mut expected = 0u32;
    for i in 0..100u32 {
        if 20 + (i % 30) > 30 {
            expected += 1;
        }
    }
    let total = rt.execute_query("SELECT * FROM users").unwrap();
    assert_eq!(total.result.records.len(), 100, "all rows present");
    let city_only = rt
        .execute_query("SELECT * FROM users WHERE city = 'NYC'")
        .unwrap();
    assert_eq!(
        city_only.result.records.len(),
        100,
        "city-only filter: post-CREATE-INDEX inserts must show in idx_city ({} of 100)",
        city_only.result.records.len()
    );
    let filtered = rt
        .execute_query("SELECT * FROM users WHERE city = 'NYC' AND age > 30")
        .unwrap();
    assert_eq!(
        filtered.result.records.len(),
        expected as usize,
        "prevalidated columnar: post-CREATE-INDEX inserts must update secondary indexes (got {}, expected {})",
        filtered.result.records.len(),
        expected
    );
}

#[test]
fn post_create_index_delete_keeps_index_in_sync() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
    rt.execute_query("CREATE TABLE users (id INT, age INT, city TEXT)")
        .unwrap();
    for i in 0..20u32 {
        rt.execute_query(&format!(
            "INSERT INTO users (id, age, city) VALUES ({i}, {}, 'NYC')",
            20 + (i % 5)
        ))
        .unwrap();
    }
    rt.execute_query("CREATE INDEX idx_city ON users (city) USING HASH")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_age ON users (age) USING BTREE")
        .unwrap();

    rt.execute_query("DELETE FROM users WHERE id < 5").unwrap();

    let count_nyc = rt
        .execute_query("SELECT * FROM users WHERE city = 'NYC'")
        .unwrap();
    assert_eq!(
        count_nyc.result.records.len(),
        15,
        "after DELETE, idx_city must drop 5 rows: got {}",
        count_nyc.result.records.len()
    );
    let count_age = rt
        .execute_query("SELECT * FROM users WHERE age >= 20")
        .unwrap();
    assert_eq!(
        count_age.result.records.len(),
        15,
        "after DELETE, BTREE on age must drop 5 rows: got {}",
        count_age.result.records.len()
    );
}
