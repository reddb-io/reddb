//! Issue #1400 — ordered (B-tree) secondary index for document fields,
//! surfaced via `CREATE INDEX ... USING BTREE`, verified end-to-end
//! through the high-level RQL seam (`RedDBRuntime::execute_query`).
//!
//! Acceptance criteria exercised here:
//!  - `CREATE INDEX` declares an ordered index on a document field.
//!  - Range predicates (`>`, `<`, BETWEEN), `ORDER BY`, and top-N return
//!    correctly ordered results via the index.
//!  - Equality still resolves (hash index retained for high-cardinality
//!    fields, plus the companion hash the BTREE builds for its own column).
//!  - The index refreshes correctly on insert / update / delete.

use reddb_server::storage::schema::Value;
use reddb_server::{RedDBOptions, RedDBRuntime, RuntimeQueryResult};

/// Collect the `name` column of every returned record, preserving result
/// order — the row identity we assert ordering against.
fn names(res: &RuntimeQueryResult) -> Vec<String> {
    res.result
        .records
        .iter()
        .map(|r| match r.get("name") {
            Some(Value::Text(t)) => t.to_string(),
            other => panic!("expected text `name`, got {other:?}"),
        })
        .collect()
}

/// Spin up an in-memory document collection `docs(name, score)` seeded with
/// four rows and an ordered BTREE index on the `score` document field.
fn seeded_runtime() -> RedDBRuntime {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots");
    rt.execute_query("CREATE DOCUMENT docs")
        .expect("create document collection");
    for (name, score) in [("alice", 30), ("bob", 10), ("carol", 50), ("dave", 20)] {
        rt.execute_query(&format!(
            "INSERT INTO docs DOCUMENT (body) VALUES ('{{\"name\":\"{name}\",\"score\":{score}}}')"
        ))
        .expect("insert document");
    }
    rt.execute_query("CREATE INDEX idx_score ON docs (score) USING BTREE")
        .expect("create ordered index on a document field");
    rt
}

#[test]
fn create_index_declares_ordered_index_on_document_field() {
    let rt = seeded_runtime();
    // The ordered (sorted/BTree) index is registered for docs.score.
    assert!(
        rt.index_store_ref().sorted.has_index("docs", "score"),
        "CREATE INDEX ... USING BTREE must register an ordered index on the document field"
    );
    // A document-path index (dot notation) is also accepted and built.
    rt.execute_query("CREATE INDEX idx_body_score ON docs (body.score) USING BTREE")
        .expect("create ordered index on a dotted document path");
    assert!(
        rt.index_store_ref().sorted.has_index("docs", "body.score"),
        "dotted document-path ordered index must be registered"
    );
}

#[test]
fn range_predicates_resolve_via_ordered_index() {
    let rt = seeded_runtime();

    // Greater-than, ordered ascending by the indexed field.
    let gt = rt
        .execute_query("SELECT name FROM docs WHERE score > 20 ORDER BY score ASC")
        .expect("range > query");
    assert_eq!(names(&gt), vec!["alice", "carol"]);

    // Less-than.
    let lt = rt
        .execute_query("SELECT name FROM docs WHERE score < 30 ORDER BY score ASC")
        .expect("range < query");
    assert_eq!(names(&lt), vec!["bob", "dave"]);

    // BETWEEN (inclusive on both ends).
    let between = rt
        .execute_query("SELECT name FROM docs WHERE score BETWEEN 15 AND 35 ORDER BY score ASC")
        .expect("range BETWEEN query");
    assert_eq!(names(&between), vec!["dave", "alice"]);
}

#[test]
fn order_by_and_top_n_return_correctly_ordered_results() {
    let rt = seeded_runtime();

    // Full ascending sort over the indexed document field.
    let asc = rt
        .execute_query("SELECT name FROM docs ORDER BY score ASC")
        .expect("ORDER BY ASC");
    assert_eq!(names(&asc), vec!["bob", "dave", "alice", "carol"]);

    // Descending.
    let desc = rt
        .execute_query("SELECT name FROM docs ORDER BY score DESC")
        .expect("ORDER BY DESC");
    assert_eq!(names(&desc), vec!["carol", "alice", "dave", "bob"]);

    // Top-N / pagination: the two highest scores.
    let top2 = rt
        .execute_query("SELECT name FROM docs ORDER BY score DESC LIMIT 2")
        .expect("top-N query");
    assert_eq!(names(&top2), vec!["carol", "alice"]);

    // Pagination via OFFSET: the next page after the top two.
    let next = rt
        .execute_query("SELECT name FROM docs ORDER BY score DESC LIMIT 2 OFFSET 2")
        .expect("paginated query");
    assert_eq!(names(&next), vec!["dave", "bob"]);
}

#[test]
fn equality_resolves_with_hash_index_retained() {
    let rt = seeded_runtime();

    // Equality on the BTREE-indexed column resolves (companion hash index).
    let eq = rt
        .execute_query("SELECT name FROM docs WHERE score = 50")
        .expect("equality on ordered-indexed field");
    assert_eq!(names(&eq), vec!["carol"]);

    // A dedicated HASH index on a high-cardinality field is retained and
    // resolves equality alongside the ordered index.
    rt.execute_query("CREATE INDEX idx_name ON docs (name) USING HASH")
        .expect("create hash index on high-cardinality field");
    let by_name = rt
        .execute_query("SELECT name, score FROM docs WHERE name = 'dave'")
        .expect("equality via hash index");
    assert_eq!(names(&by_name), vec!["dave"]);
    assert_eq!(
        match by_name.result.records[0].get("score") {
            Some(Value::Integer(v)) => *v,
            other => panic!("expected integer score, got {other:?}"),
        },
        20
    );
}

#[test]
fn ordered_index_refreshes_on_insert_update_delete() {
    let rt = seeded_runtime();

    // INSERT: a new low score sorts to the front.
    rt.execute_query("INSERT INTO docs DOCUMENT (body) VALUES ('{\"name\":\"eve\",\"score\":5}')")
        .expect("insert refreshes the ordered index");
    let after_insert = rt
        .execute_query("SELECT name FROM docs ORDER BY score ASC")
        .expect("query after insert");
    assert_eq!(
        names(&after_insert),
        vec!["eve", "bob", "dave", "alice", "carol"]
    );

    // UPDATE: bumping bob 10 -> 99 re-positions it to the top and makes it
    // appear in a range that previously excluded it.
    rt.execute_query("UPDATE docs DOCUMENTS SET score = 99 WHERE name = 'bob'")
        .expect("document update refreshes the ordered index");
    let after_update = rt
        .execute_query("SELECT name FROM docs WHERE score > 40 ORDER BY score ASC")
        .expect("range query after update");
    assert_eq!(names(&after_update), vec!["carol", "bob"]);

    // DELETE: removing carol drops it from index-backed range results.
    rt.execute_query("DELETE FROM docs WHERE name = 'carol'")
        .expect("delete refreshes the ordered index");
    let after_delete = rt
        .execute_query("SELECT name FROM docs WHERE score > 40 ORDER BY score ASC")
        .expect("range query after delete");
    assert_eq!(names(&after_delete), vec!["bob"]);

    // Final ordering reflects every mutation.
    let final_order = rt
        .execute_query("SELECT name FROM docs ORDER BY score ASC")
        .expect("final ordering");
    assert_eq!(names(&final_order), vec!["eve", "dave", "alice", "bob"]);
}
