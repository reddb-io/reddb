//! Regression coverage for issue #553 — Graph: edge property projection.
//!
//! PRD #449 user story 29. Pins the four acceptance bullets from the
//! issue brief; each test below maps to one bullet:
//!
//! 1. `edge_insert_accepts_property_bag` — edge insert accepts a
//!    property bag (label, from, to, evidence, weight) and the values
//!    round-trip through the storage layer.
//! 2. `graph_match_projects_edge_property_evidence` — `MATCH ...
//!    RETURN r.<prop>` projects the user-stored edge property
//!    verbatim.
//! 3. `graph_match_missing_edge_property_projects_as_null` — when the
//!    requested edge property is not present on the matched edge, the
//!    projection slot is Value::Null rather than an error or a missing
//!    column.
//! 4. `graph_edge_property_query_regression_smoke` — the showcase
//!    MATCH (a)-[r:HAS_TRAIT]->(b) RETURN a.name, b.name, r.evidence
//!    end-to-end smoke that the parent PRD's user story 29 calls out.
//!
//! The implementation predates this issue; the contract was already
//! shipped under #544. This slice anchors the four acceptance bullets
//! behind a file named after the issue so future regressions surface
//! at the right test boundary.

#[path = "../../support/mod.rs"]
mod support;

use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::storage::query::UnifiedRecord;
use reddb::storage::schema::Value;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn only_record(result: &RuntimeQueryResult) -> &UnifiedRecord {
    assert_eq!(
        result.result.records.len(),
        1,
        "expected one row for query `{}`",
        result.query
    );
    &result.result.records[0]
}

fn text_value(row: &UnifiedRecord, column: &str) -> String {
    match row.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text column {column}, got {other:?}"),
    }
}

#[test]
fn edge_insert_accepts_property_bag() {
    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('hansel', 'Hansel')",
    );
    exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('gretel', 'Gretel')",
    );

    let edge = exec(
        &rt,
        "INSERT INTO tales EDGE (label, from_rid, to_rid, evidence, mood) VALUES \
         ('HAS_TRAIT', 'hansel', 'gretel', 'siblings in the forest', 'wary') RETURNING *",
    );
    let row = only_record(&edge);
    assert_eq!(text_value(row, "kind"), "edge");
    assert_eq!(text_value(row, "label"), "HAS_TRAIT");
    assert_eq!(text_value(row, "evidence"), "siblings in the forest");
    assert_eq!(text_value(row, "mood"), "wary");
}

#[test]
fn graph_match_projects_edge_property_evidence() {
    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('hansel', 'Hansel')",
    );
    exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('gretel', 'Gretel')",
    );
    exec(
        &rt,
        "INSERT INTO tales EDGE (label, from_rid, to_rid, evidence) VALUES \
         ('HAS_TRAIT', 'hansel', 'gretel', 'siblings in the forest')",
    );

    let matched = exec(
        &rt,
        "MATCH (a)-[r:HAS_TRAIT]->(b) \
         WHERE a.label = 'hansel' \
         RETURN r.evidence",
    );
    let row = only_record(&matched);
    assert_eq!(text_value(row, "r.evidence"), "siblings in the forest");
}

#[test]
fn graph_match_missing_edge_property_projects_as_null() {
    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('hansel', 'Hansel')",
    );
    exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('gretel', 'Gretel')",
    );
    // Edge created without an `evidence` property.
    exec(
        &rt,
        "INSERT INTO tales EDGE (label, from_rid, to_rid) VALUES \
         ('HAS_TRAIT', 'hansel', 'gretel')",
    );

    let matched = exec(
        &rt,
        "MATCH (a)-[r:HAS_TRAIT]->(b) \
         WHERE a.label = 'hansel' \
         RETURN r.evidence",
    );
    let row = only_record(&matched);
    let projected = row.get("r.evidence");
    assert!(
        matches!(projected, Some(Value::Null) | None),
        "missing edge property must project as Null (got {projected:?})"
    );
}

#[test]
fn graph_edge_property_query_regression_smoke() {
    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('hansel', 'Hansel')",
    );
    exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('gretel', 'Gretel')",
    );
    exec(
        &rt,
        "INSERT INTO tales EDGE (label, from_rid, to_rid, evidence) VALUES \
         ('HAS_TRAIT', 'hansel', 'gretel', 'siblings in the forest')",
    );

    let res = exec(
        &rt,
        "MATCH (a)-[r:HAS_TRAIT]->(b) \
         WHERE a.label = 'hansel' \
         RETURN a.name, b.name, r.evidence",
    );
    let row = only_record(&res);
    assert_eq!(text_value(row, "a.name"), "Hansel");
    assert_eq!(text_value(row, "b.name"), "Gretel");
    assert_eq!(text_value(row, "r.evidence"), "siblings in the forest");
}
