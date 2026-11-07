//! Repro for bench failure: seed 25k rows, CREATE INDEX age BTREE,
//! then `SELECT * FROM t WHERE age BETWEEN 0 AND 200` must return
//! every row.

use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

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
    let mut ids = r
        .result
        .records
        .iter()
        .map(|record| match record.get("id") {
            Some(Value::Integer(id)) => *id,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect::<Vec<_>>();
    ids.sort_unstable();
    let upper = i64::try_from(N).expect("fixture size fits i64");
    assert_eq!(ids, (0..upper).collect::<Vec<_>>());

    let explain = rt
        .explain_query("SELECT * FROM users WHERE age BETWEEN 0 AND 200")
        .expect("range query should explain");
    let mut nodes = vec![&explain.logical_plan.root];
    let mut observed = Vec::new();
    while let Some(node) = nodes.pop() {
        observed.push((node.operator.as_str(), node.details.get("index")));
        if node.operator == "index_seek"
            && node.details.get("index").map(String::as_str) == Some("idx_age")
        {
            return;
        }
        nodes.extend(&node.children);
    }
    panic!("expected idx_age index_seek, got {observed:?}");
}
