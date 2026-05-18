//! Regression coverage for issue #544.
//!
//! Pins three graph-surface behaviors that have shipped under the #449
//! parent so future breakage is localised to a file named after the issue:
//!
//! 1. `INSERT INTO <coll> NODE (...) ... RETURNING *` surfaces the user
//!    `label` and a generated `rid` without the caller having to guess
//!    them from the input.
//! 2. A custom edge label (`HAS_TRAIT`) inserted via SQL round-trips and
//!    is matchable via Cypher-style `MATCH (a)-[r:HAS_TRAIT]->(b)`.
//! 3. `GRAPH PROPERTIES` does not overwrite a user-set `node_type` with
//!    the runtime's internal node label.

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

fn uint_value(row: &UnifiedRecord, column: &str) -> u64 {
    match row.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer column {column}, got {other:?}"),
    }
}

#[test]
fn graph_insert_returning_surfaces_label_and_generated_rid() {
    let rt = runtime();

    let inserted = exec(
        &rt,
        "INSERT INTO tales NODE (label, name) VALUES ('hansel', 'Hansel') RETURNING *",
    );
    let row = only_record(&inserted);

    assert_eq!(text_value(row, "label"), "hansel");
    assert_eq!(text_value(row, "kind"), "node");
    assert_eq!(text_value(row, "collection"), "tales");
    assert_eq!(text_value(row, "name"), "Hansel");

    let rid = uint_value(row, "rid");
    assert!(rid > 0, "generated rid should be non-zero");

    let by_label = exec(&rt, "SELECT * FROM tales WHERE label = 'hansel'");
    assert_eq!(uint_value(only_record(&by_label), "rid"), rid);
}

#[test]
fn custom_edge_label_round_trips_via_match() {
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
        "INSERT INTO tales EDGE (label, from_rid, to_rid, evidence) VALUES \
         ('HAS_TRAIT', 'hansel', 'gretel', 'siblings in the forest') RETURNING *",
    );
    let edge_row = only_record(&edge);
    assert_eq!(text_value(edge_row, "kind"), "edge");
    assert_eq!(
        text_value(edge_row, "label"),
        "HAS_TRAIT",
        "edge insert should preserve the user-provided label verbatim"
    );

    let matched = exec(
        &rt,
        "MATCH (a)-[r:HAS_TRAIT]->(b) \
         WHERE a.label = 'hansel' \
         RETURN a.name, b.name, r.evidence",
    );
    let matched_row = only_record(&matched);
    assert_eq!(text_value(matched_row, "a.name"), "Hansel");
    assert_eq!(text_value(matched_row, "b.name"), "Gretel");
    assert_eq!(
        text_value(matched_row, "r.evidence"),
        "siblings in the forest"
    );
}

#[test]
fn graph_properties_preserves_user_set_node_type() {
    let rt = runtime();

    exec(
        &rt,
        "INSERT INTO tales NODE (label, node_type, name) VALUES \
         ('hansel', 'StoryCharacter', 'Hansel')",
    );

    let props = exec(&rt, "GRAPH PROPERTIES 'hansel'");
    let row = only_record(&props);

    assert_eq!(text_value(row, "label"), "hansel");
    assert_eq!(
        text_value(row, "node_type"),
        "StoryCharacter",
        "GRAPH PROPERTIES must not overwrite user-set node_type with the internal node label"
    );
    assert_eq!(text_value(row, "name"), "Hansel");
}
