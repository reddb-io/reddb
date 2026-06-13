//! Repro for bench failure: seed 25k rows, CREATE INDEX age BTREE,
//! then `SELECT * FROM t WHERE age BETWEEN 0 AND 200` must return
//! every row.

use reddb::{RedDBOptions, RedDBRuntime};

#[test]
fn full_range_after_create_index_returns_all_rows() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();

    rt.execute_query(
        "CREATE TABLE users (id INT, name TEXT, email TEXT, age INT, \
         city TEXT, score FLOAT, active BOOL)",
    )
    .unwrap();

    const N: usize = 2500; // smaller than 25k to keep test fast
    for i in 0..N {
        rt.execute_query(&format!(
            "INSERT INTO users (id, name, email, age, city, score, active) \
             VALUES ({i}, 'u{i}', 'e{i}@t.com', {}, 'NYC', 0.0, true)",
            18 + (i % 60)
        ))
        .unwrap();
    }

    // post_seed: indexes built AFTER insert
    rt.execute_query("CREATE INDEX idx_age ON users (age) USING BTREE")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_city ON users (city) USING HASH")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_id ON users (id) USING HASH")
        .unwrap();

    let r = rt
        .execute_query("SELECT * FROM users WHERE age BETWEEN 0 AND 200")
        .unwrap();
    assert_eq!(
        r.result.records.len(),
        N,
        "full-range select_range returned {} of {N}",
        r.result.records.len()
    );
}
