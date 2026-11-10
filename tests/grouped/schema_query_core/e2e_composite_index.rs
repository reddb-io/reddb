//! Composite (multi-column) B-tree index — correctness tests.
//!
//! Validates that `CREATE INDEX ... ON t (a, b) USING BTREE` builds a
//! composite index, and that `WHERE a = X AND b [>|BETWEEN|<] Y` picks
//! up that index instead of falling back to two single-col scans + intersect.

use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
}

fn selected_ids(result: &reddb::runtime::RuntimeQueryResult) -> Vec<i64> {
    let mut ids = result
        .result
        .records
        .iter()
        .map(|record| match record.get("id") {
            Some(Value::Integer(id)) => *id,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids
}

fn assert_uses_index(rt: &RedDBRuntime, query: &str, expected_index: &str) {
    let explain = rt.explain_query(query).expect("query should explain");
    let mut nodes = vec![&explain.logical_plan.root];
    let mut observed = Vec::new();
    while let Some(node) = nodes.pop() {
        observed.push((node.operator.as_str(), node.details.get("index")));
        if node.operator == "index_seek"
            && node.details.get("index").map(String::as_str) == Some(expected_index)
        {
            return;
        }
        nodes.extend(&node.children);
    }
    panic!("expected index_seek on {expected_index}, got {observed:?}");
}

#[test]
fn composite_index_matches_and_eq_range() {
    let rt = open_runtime();
    rt.execute_query("CREATE TABLE users (id INT, city TEXT, age INT, score FLOAT)")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_city_age ON users (city, age) USING BTREE")
        .unwrap();

    let cities = ["NYC", "LA", "CHI", "HOU", "PHX"];
    for i in 0..500 {
        let city = cities[i % cities.len()];
        let age = 18 + (i % 50);
        rt.execute_query(&format!(
            "INSERT INTO users (id, city, age, score) VALUES ({i}, '{city}', {age}, 0.0)"
        ))
        .unwrap();
    }

    // Point+range via composite: city='NYC' AND age BETWEEN 20 AND 40.
    // Compare the exact result set and the selected access path; a partial
    // candidate set and a full-scan fallback must both fail this test.
    let q = "SELECT * FROM users WHERE city = 'NYC' AND age BETWEEN 20 AND 40";
    let r = rt.execute_query(q).expect("ok");
    let expected = (0..500)
        .filter(|i| i % cities.len() == 0 && (20..=40).contains(&(18 + (i % 50))))
        .map(|i| i64::try_from(i).expect("fixture id fits i64"))
        .collect::<Vec<_>>();
    assert_eq!(selected_ids(&r), expected);
    assert_uses_index(&rt, q, "idx_city_age");
}

#[test]
fn composite_index_matches_and_eq_gt() {
    let rt = open_runtime();
    rt.execute_query("CREATE TABLE users (id INT, city TEXT, age INT)")
        .unwrap();
    rt.execute_query("CREATE INDEX idx_city_age ON users (city, age) USING BTREE")
        .unwrap();

    let cities = ["NYC", "LA", "CHI"];
    for i in 0..300 {
        let city = cities[i % cities.len()];
        let age = 18 + (i % 40);
        rt.execute_query(&format!(
            "INSERT INTO users (id, city, age) VALUES ({i}, '{city}', {age})"
        ))
        .unwrap();
    }

    let q = "SELECT * FROM users WHERE city = 'NYC' AND age > 30";
    let r = rt.execute_query(q).expect("ok");
    let expected = (0..300)
        .filter(|i| i % cities.len() == 0 && 18 + (i % 40) > 30)
        .map(|i| i64::try_from(i).expect("fixture id fits i64"))
        .collect::<Vec<_>>();
    assert_eq!(selected_ids(&r), expected);
    assert_uses_index(&rt, q, "idx_city_age");
}

#[test]
fn composite_returns_same_rows_as_no_index_baseline() {
    // Parity guard: run the same query against two runtimes — one with the
    // composite index and one without — and compare their exact id sets.
    let rt_comp = open_runtime();
    rt_comp
        .execute_query("CREATE TABLE t (id INT, city TEXT, age INT)")
        .unwrap();
    rt_comp
        .execute_query("CREATE INDEX idx_city_age ON t (city, age) USING BTREE")
        .unwrap();
    let rt_plain = open_runtime();
    rt_plain
        .execute_query("CREATE TABLE t (id INT, city TEXT, age INT)")
        .unwrap();

    let cities = ["NYC", "LA"];
    for i in 0..200 {
        let city = cities[i % cities.len()];
        let age = 18 + (i % 30);
        let sql = format!("INSERT INTO t (id, city, age) VALUES ({i}, '{city}', {age})");
        rt_comp.execute_query(&sql).unwrap();
        rt_plain.execute_query(&sql).unwrap();
    }

    let q = "SELECT * FROM t WHERE city = 'NYC' AND age BETWEEN 22 AND 35";
    let r_comp = rt_comp.execute_query(q).unwrap();
    let r_plain = rt_plain.execute_query(q).unwrap();
    let expected = (0..200)
        .filter(|i| i % cities.len() == 0 && (22..=35).contains(&(18 + (i % 30))))
        .map(|i| i64::try_from(i).expect("fixture id fits i64"))
        .collect::<Vec<_>>();
    assert_eq!(selected_ids(&r_comp), expected);
    assert_eq!(selected_ids(&r_comp), selected_ids(&r_plain));
    assert_uses_index(&rt_comp, q, "idx_city_age");
}
