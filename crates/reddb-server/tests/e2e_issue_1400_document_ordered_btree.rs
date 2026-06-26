//! Issue #1400 — DOCUMENT: ordered (B-tree) secondary index + `CREATE INDEX`.
//!
//! Verifies, through the high-level RQL e2e seam (`RedDBRuntime::execute_query`),
//! that an ordered B-tree index declared on a document field supports range
//! predicates, `ORDER BY`, and top-N while keeping equality served by the
//! retained hash side, and that it refreshes on insert / update / delete.
//!
//! The ordered B-tree's distinguishing capability over the hash index is the
//! *range* scan — these tests assert both that results come back correctly
//! ordered AND, via `EXPLAIN`, that range/equality lookups are index-backed
//! (`index_seek`) rather than full collection scans.

use reddb_server::storage::schema::Value;
use reddb_server::{RedDBOptions, RedDBRuntime, RuntimeQueryResult};

/// A document collection `docs` with an ordered B-tree index on `body.age`,
/// seeded with five documents whose ages are deliberately out of order so a
/// passing range/order assertion can only come from a working index + sort.
fn docs_runtime() -> RedDBRuntime {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE DOCUMENT docs")
        .expect("CREATE DOCUMENT provisions the document collection");
    for (id, age) in [(1, 30), (2, 10), (3, 50), (4, 20), (5, 40)] {
        rt.execute_query(&format!(
            "INSERT INTO docs DOCUMENT (body) VALUES ('{{\"id\":{id},\"age\":{age}}}')"
        ))
        .unwrap_or_else(|err| panic!("insert doc id={id}: {err}"));
    }
    rt
}

/// Project the `body.age` column out of a result as `f64`, regardless of
/// whether the engine renders it as an integer or float Value.
fn ages(result: &RuntimeQueryResult) -> Vec<f64> {
    result
        .result
        .records
        .iter()
        .map(|record| match record.get("body.age") {
            Some(Value::Float(value)) => *value,
            Some(Value::Integer(value)) => *value as f64,
            Some(Value::UnsignedInteger(value)) => *value as f64,
            other => panic!("expected numeric body.age, got {other:?}"),
        })
        .collect()
}

/// Collect the `op` (operator) column from an `EXPLAIN` result.
fn plan_ops(result: &RuntimeQueryResult) -> Vec<String> {
    result
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("op") {
            Some(Value::Text(value)) => Some(value.as_ref().to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn create_index_declares_ordered_btree_on_document_field() {
    let rt = docs_runtime();
    let created = rt
        .execute_query("CREATE INDEX idx_age ON docs (body.age) USING BTREE")
        .expect("CREATE INDEX declares an ordered index on the document field");
    let message = match created.result.records[0].get("message") {
        Some(Value::Text(value)) => value.as_ref().to_string(),
        other => panic!("expected DDL message, got {other:?}"),
    };
    assert!(message.contains("BTREE"), "{message}");
    // All five seeded documents must be folded into the index, not zero.
    assert!(message.contains("5 entities indexed"), "{message}");
}

#[test]
fn range_predicates_return_ordered_results_via_index() {
    let rt = docs_runtime();
    rt.execute_query("CREATE INDEX idx_age ON docs (body.age) USING BTREE")
        .expect("create ordered index");

    // Greater-than.
    let gt = rt
        .execute_query("SELECT body.age FROM docs WHERE body.age > 25 ORDER BY body.age")
        .expect("range > query");
    assert_eq!(ages(&gt), vec![30.0, 40.0, 50.0]);

    // Less-than.
    let lt = rt
        .execute_query("SELECT body.age FROM docs WHERE body.age < 25 ORDER BY body.age")
        .expect("range < query");
    assert_eq!(ages(&lt), vec![10.0, 20.0]);

    // BETWEEN (inclusive).
    let between = rt
        .execute_query(
            "SELECT body.age FROM docs WHERE body.age BETWEEN 20 AND 40 ORDER BY body.age",
        )
        .expect("range BETWEEN query");
    assert_eq!(ages(&between), vec![20.0, 30.0, 40.0]);

    // The range scan must be served by the ordered index, not a full scan.
    let plan = rt
        .execute_query("EXPLAIN SELECT body.age FROM docs WHERE body.age > 25")
        .expect("explain range query");
    let ops = plan_ops(&plan);
    assert!(
        ops.iter().any(|op| op == "index_seek"),
        "range predicate should be index-backed, plan was {ops:?}"
    );
}

#[test]
fn order_by_and_top_n_return_correctly_ordered_results() {
    let rt = docs_runtime();
    rt.execute_query("CREATE INDEX idx_age ON docs (body.age) USING BTREE")
        .expect("create ordered index");

    let asc = rt
        .execute_query("SELECT body.age FROM docs ORDER BY body.age ASC")
        .expect("ORDER BY ascending");
    assert_eq!(ages(&asc), vec![10.0, 20.0, 30.0, 40.0, 50.0]);

    let desc = rt
        .execute_query("SELECT body.age FROM docs ORDER BY body.age DESC")
        .expect("ORDER BY descending");
    assert_eq!(ages(&desc), vec![50.0, 40.0, 30.0, 20.0, 10.0]);

    // Top-N: the three largest ages, descending.
    let top_n = rt
        .execute_query("SELECT body.age FROM docs ORDER BY body.age DESC LIMIT 3")
        .expect("top-N query");
    assert_eq!(ages(&top_n), vec![50.0, 40.0, 30.0]);
}

#[test]
fn equality_resolves_via_retained_hash_index() {
    let rt = docs_runtime();
    rt.execute_query("CREATE INDEX idx_age ON docs (body.age) USING BTREE")
        .expect("create ordered index");

    let eq = rt
        .execute_query("SELECT body.age FROM docs WHERE body.age = 40")
        .expect("equality query");
    assert_eq!(ages(&eq), vec![40.0]);

    // Equality stays index-backed (the B-tree carries a retained hash side).
    let plan = rt
        .execute_query("EXPLAIN SELECT body.age FROM docs WHERE body.age = 40")
        .expect("explain equality query");
    let ops = plan_ops(&plan);
    assert!(
        ops.iter().any(|op| op == "index_seek"),
        "equality predicate should be index-backed, plan was {ops:?}"
    );
}

#[test]
fn index_refreshes_on_insert_update_delete() {
    let rt = docs_runtime();
    rt.execute_query("CREATE INDEX idx_age ON docs (body.age) USING BTREE")
        .expect("create ordered index");

    // INSERT — a new age must appear in subsequent range scans.
    rt.execute_query("INSERT INTO docs DOCUMENT (body) VALUES ('{\"id\":6,\"age\":35}')")
        .expect("insert refreshes the index");
    let after_insert = rt
        .execute_query("SELECT body.age FROM docs WHERE body.age >= 30 ORDER BY body.age")
        .expect("range after insert");
    assert_eq!(ages(&after_insert), vec![30.0, 35.0, 40.0, 50.0]);

    // UPDATE — moving age 30 -> 99 must drop the old key and add the new one.
    rt.execute_query("UPDATE docs DOCUMENTS SET age = 99 WHERE id = 1")
        .expect("update refreshes the index");
    let after_update = rt
        .execute_query("SELECT body.age FROM docs WHERE body.age >= 60 ORDER BY body.age")
        .expect("range after update");
    assert_eq!(ages(&after_update), vec![99.0]);
    let old_key_gone = rt
        .execute_query("SELECT body.age FROM docs WHERE body.age = 30")
        .expect("equality after update");
    assert!(
        ages(&old_key_gone).is_empty(),
        "the pre-update key 30 must no longer resolve"
    );

    // DELETE — removing age 50 must drop it from the index.
    rt.execute_query("DELETE FROM docs WHERE body.id = 3")
        .expect("delete refreshes the index");
    let after_delete = rt
        .execute_query("SELECT body.age FROM docs WHERE body.age >= 40 ORDER BY body.age")
        .expect("range after delete");
    assert_eq!(ages(&after_delete), vec![40.0, 99.0]);
}
