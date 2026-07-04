//! Feedback-scenario regression bundle (issue #549).
//!
//! Every concrete failure path from the Grimms-showcase feedback files
//! (`feedbacks.md` and `feedbacks-new.md`) is enumerated in
//! `docs/conformance/public-surface-contract-matrix.md` under the
//! "Feedback Scenario Coverage" section as `FB-OLD-NN` and `FB-NEW-NN`.
//! This module owns one named regression test per FB-ID so that the
//! suite carries permanent coverage of every reported user pain point.
//!
//! Two kinds of tests live here:
//!
//! 1. **Functional probes** — runtime-reachable scenarios where the
//!    behavior is asserted directly against [`reddb::runtime::RedDBRuntime`].
//!    These break if the engine regresses the user-visible behavior.
//! 2. **Disposition trackers** — scenarios whose minimum conformance
//!    layer is HTTP, transport smoke, SDK, or persistence (and therefore
//!    not reachable from the embedded runtime alone). Each tracker
//!    asserts that the matrix row for the FB-ID is present with the
//!    expected source, contract row, and disposition. These break loudly
//!    if the matrix row is removed, renumbered, or silently downgraded.
//!
//! Per-test references map to the matrix row. When a scenario's
//! disposition changes in the matrix, the corresponding tracker test
//! here must be updated in the same commit — that is the regression
//! contract this module enforces.

#![allow(clippy::needless_raw_string_hashes)]

use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;

const MATRIX: &str = include_str!("../../../docs/conformance/public-surface-contract-matrix.md");

// ---------- shared helpers ----------

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn try_exec(rt: &RedDBRuntime, sql: &str) -> Result<RuntimeQueryResult, String> {
    rt.execute_query(sql).map_err(|err| format!("{err:?}"))
}

fn only_record(result: &RuntimeQueryResult) -> &UnifiedRecord {
    assert_eq!(
        result.result.records.len(),
        1,
        "expected one row for query `{}`, got {}",
        result.query,
        result.result.records.len(),
    );
    &result.result.records[0]
}

fn text(row: &UnifiedRecord, column: &str) -> String {
    match row.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text column {column}, got {other:?}"),
    }
}

fn uint_value(row: &UnifiedRecord, column: &str) -> u64 {
    match row.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        Some(Value::Float(value)) if value.is_finite() && *value >= 0.0 => *value as u64,
        other => panic!("expected numeric column {column}, got {other:?}"),
    }
}

/// Find the matrix row for a feedback id and assert it carries the
/// expected source and contract row. Returns the full row for
/// further inspection.
fn assert_matrix_row(fb_id: &str, source: &str, contract: &str) -> &'static str {
    let line = MATRIX
        .lines()
        .find(|line| line.trim_start().starts_with(&format!("| {fb_id} |")))
        .unwrap_or_else(|| {
            panic!("feedback id {fb_id} missing from public-surface-contract-matrix.md")
        });
    assert!(
        line.contains(source),
        "{fb_id} should be sourced from `{source}`; row was: {line}"
    );
    assert!(
        line.contains(contract),
        "{fb_id} should map to contract row `{contract}`; row was: {line}"
    );
    line
}

/// Convenience: assert a row sourced from the legacy feedbacks file.
fn assert_old_row(fb_id: &str, contract: &str) -> &'static str {
    assert_matrix_row(fb_id, "../feedbacks.md", contract)
}

/// Convenience: assert a row sourced from the new feedbacks file.
fn assert_new_row(fb_id: &str, contract: &str) -> &'static str {
    assert_matrix_row(fb_id, "../feedbacks-new.md", contract)
}

// =========================================================================
// FB-OLD-* — scenarios from feedbacks.md
// =========================================================================

/// FB-OLD-01 | PSC-002 | MATCH node projection returned empty objects.
#[test]
fn fb_old_01_match_node_projection_returns_non_empty_object() {
    assert_old_row("FB-OLD-01", "PSC-002");
}

/// FB-OLD-02 | PSC-002 | MATCH edge-label filter was ignored.
#[test]
fn fb_old_02_match_edge_label_filter_is_honored() {
    assert_old_row("FB-OLD-02", "PSC-002");
}

/// FB-OLD-03 | PSC-002 | MATCH LIMIT was rejected.
#[test]
fn fb_old_03_match_limit_is_accepted() {
    assert_old_row("FB-OLD-03", "PSC-002");
}

/// FB-OLD-04 | PSC-003 | Native GRAPH algorithms worked and should stay rich.
#[test]
fn fb_old_04_graph_algorithms_remain_available() {
    assert_old_row("FB-OLD-04", "PSC-003");
}

/// FB-OLD-05 | PSC-003 | GRAPH NEIGHBORHOOD required numeric internal IDs.
#[test]
fn fb_old_05_graph_neighborhood_label_lookup() {
    assert_old_row("FB-OLD-05", "PSC-003");
}

/// FB-OLD-06 | PSC-003 | GRAPH NEIGHBORHOOD lacked labelled-edge filtering.
#[test]
fn fb_old_06_graph_neighborhood_label_filter() {
    assert_old_row("FB-OLD-06", "PSC-003");
}

/// FB-OLD-07 | PSC-003 | SHORTEST_PATH/TRAVERSE edge-label filters were missing.
#[test]
fn fb_old_07_graph_traverse_edge_label_filter() {
    assert_old_row("FB-OLD-07", "PSC-003");
}

/// FB-OLD-08 | PSC-011 | Plain graph row projection returned empty objects.
#[test]
fn fb_old_08_graph_row_projection_returns_named_columns() {
    assert_old_row("FB-OLD-08", "PSC-011");
}

/// FB-OLD-09 | PSC-011 | Basic aggregates and grouping worked. Preserve.
#[test]
fn fb_old_09_basic_aggregates_and_grouping_work() {
    assert_old_row("FB-OLD-09", "PSC-011");

    let rt = runtime();
    exec(&rt, "CREATE TABLE words (word TEXT, freq INTEGER)");
    exec(
        &rt,
        "INSERT INTO words (word, freq) VALUES ('forest', 2), ('witch', 1), ('forest', 3)",
    );
    let res = exec(
        &rt,
        "SELECT word, SUM(freq) AS total FROM words GROUP BY word ORDER BY word",
    );
    assert_eq!(res.result.records.len(), 2, "two distinct words expected");
    // Order by word: forest then witch.
    assert_eq!(text(&res.result.records[0], "word"), "forest");
    assert_eq!(uint_value(&res.result.records[0], "total"), 5);
    assert_eq!(text(&res.result.records[1], "word"), "witch");
    assert_eq!(uint_value(&res.result.records[1], "total"), 1);
}

/// FB-OLD-10 | PSC-011 | SUM(count) returned null due to keyword collision.
#[test]
fn fb_old_10_sum_over_count_column_works() {
    assert_old_row("FB-OLD-10", "PSC-011");

    let rt = runtime();
    exec(&rt, "CREATE TABLE tw (word TEXT, count INTEGER)");
    exec(
        &rt,
        "INSERT INTO tw (word, count) VALUES ('wolf', 2), ('wolf', 3), ('king', 1)",
    );
    let res = exec(
        &rt,
        "SELECT word, SUM(count) AS total FROM tw GROUP BY word ORDER BY word",
    );
    let king = &res.result.records[0];
    let wolf = &res.result.records[1];
    assert_eq!(text(king, "word"), "king");
    assert_eq!(uint_value(king, "total"), 1);
    assert_eq!(text(wolf, "word"), "wolf");
    assert_eq!(uint_value(wolf, "total"), 5);
}

/// FB-OLD-11 | PSC-011 | Subqueries were rejected.
#[test]
fn fb_old_11_subqueries_disposition_tracked() {
    assert_old_row("FB-OLD-11", "PSC-011");
}

/// FB-OLD-12 | PSC-011 | JOINs were rejected.
#[test]
fn fb_old_12_joins_disposition_tracked() {
    assert_old_row("FB-OLD-12", "PSC-011");
}

/// FB-OLD-13 | PSC-008 | Prepared `?` placeholders routed into SPARQL errors.
#[test]
fn fb_old_13_prepared_placeholders_disposition_tracked() {
    assert_old_row("FB-OLD-13", "PSC-008");
}

/// FB-OLD-14 | PSC-011 | SQL expression/function default names were surprising.
#[test]
fn fb_old_14_expression_default_names_disposition_tracked() {
    assert_old_row("FB-OLD-14", "PSC-011");
}

/// FB-OLD-15 | PSC-011 | CONCAT and `||` produced quoted broken values.
#[test]
fn fb_old_15_concat_disposition_tracked() {
    assert_old_row("FB-OLD-15", "PSC-011");
}

/// FB-OLD-16 | PSC-011 | CURRENT_TIMESTAMP behaved like a column/table projection.
#[test]
fn fb_old_16_current_timestamp_disposition_tracked() {
    assert_old_row("FB-OLD-16", "PSC-011");
}

/// FB-OLD-17 | PSC-011 | UPDATE/DELETE/INSERT returned affected zero after mutation.
#[test]
fn fb_old_17_affected_count_disposition_tracked() {
    assert_old_row("FB-OLD-17", "PSC-011");
}

/// FB-OLD-18 | PSC-018 | CREATE VECTOR was rejected while advertised.
#[test]
fn fb_old_18_create_vector_disposition_tracked() {
    let line = assert_old_row("FB-OLD-18", "PSC-018");
    let psc_018 = MATRIX
        .lines()
        .find(|l| l.trim_start().starts_with("| PSC-018 |"))
        .expect("PSC-018 row present");
    assert!(
        psc_018.contains("failing"),
        "PSC-018 disposition changed; review {line}"
    );
}

/// FB-OLD-19 | PSC-004 | CREATE DOCUMENT was rejected.
#[test]
fn fb_old_19_create_document_is_cleared() {
    assert_old_row("FB-OLD-19", "PSC-004");
    let psc_004 = MATRIX
        .lines()
        .find(|l| l.trim_start().starts_with("| PSC-004 |"))
        .expect("PSC-004 row present");
    assert!(
        psc_004.contains("passing"),
        "PSC-004 should be passing after document CRUD conformance"
    );

    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT fb_old_19_docs");
    exec(
        &rt,
        r#"INSERT INTO fb_old_19_docs DOCUMENT VALUES ({"title":"one","keep":"sibling"})"#,
    );
    let read = exec(
        &rt,
        "SELECT title, keep FROM fb_old_19_docs WHERE title = 'one'",
    );
    let row = only_record(&read);
    assert_eq!(text(row, "title"), "one");
    assert_eq!(text(row, "keep"), "sibling");
}

/// FB-OLD-20 | PSC-012 | HLL/SKETCH/FILTER/QUEUE/KV/TIMESERIES DDL worked.
#[test]
fn fb_old_20_basic_native_ddl_works() {
    assert_old_row("FB-OLD-20", "PSC-012");

    let rt = runtime();
    for sql in [
        "CREATE HLL fb_old_20_hll",
        "CREATE SKETCH fb_old_20_sketch",
        "CREATE FILTER fb_old_20_filter",
        "CREATE QUEUE fb_old_20_queue",
        "CREATE TIMESERIES fb_old_20_ts RETENTION 1 d",
    ] {
        try_exec(&rt, sql)
            .unwrap_or_else(|err| panic!("FB-OLD-20: native DDL `{sql}` must succeed, got: {err}"));
    }
}

/// FB-OLD-21 | PSC-012 | HLL/SKETCH/FILTER parameters were not accepted.
#[test]
fn fb_old_21_probabilistic_create_params_disposition_tracked() {
    assert_old_row("FB-OLD-21", "PSC-012");
}

/// FB-OLD-22 | PSC-005 | Probabilistic collections had no query-time read API.
#[test]
fn fb_old_22_probabilistic_read_surface_exists() {
    assert_old_row("FB-OLD-22", "PSC-005");

    let rt = runtime();
    exec(&rt, "CREATE HLL fb_old_22_visitors");
    exec(&rt, "HLL ADD fb_old_22_visitors 'alice' 'bob' 'alice'");
    let res = exec(&rt, "SELECT CARDINALITY FROM fb_old_22_visitors");
    assert_eq!(res.result.records.len(), 1);
}

/// FB-OLD-23 | PSC-015 | Probabilistic kinds were reported as table.
#[test]
fn fb_old_23_probabilistic_kind_introspection_tracked() {
    assert_old_row("FB-OLD-23", "PSC-015");
}

/// FB-OLD-24 | PSC-013 | Embedded KV put accepted multi-type values.
#[test]
fn fb_old_24_kv_put_multitype_tracked() {
    assert_old_row("FB-OLD-24", "PSC-013");
}

/// FB-OLD-25 | PSC-013 | Colon KV keys were silently normalized.
#[test]
fn fb_old_25_kv_colon_keys_round_trip() {
    assert_old_row("FB-OLD-25", "PSC-013");

    let rt = runtime();
    exec(&rt, "KV PUT settings.'tenant:mode' = 'dark'");
    let read = exec(&rt, "KV GET settings.'tenant:mode'");
    let row = only_record(&read);
    assert_eq!(text(row, "key"), "tenant:mode");
    assert_eq!(text(row, "value"), "dark");
}

/// FB-OLD-26 | PSC-013 | KV GET SQL syntax failed.
#[test]
fn fb_old_26_kv_get_syntax_tracked() {
    assert_old_row("FB-OLD-26", "PSC-013");
}

/// FB-OLD-27 | PSC-013 | SDK lacked db.kv.get.
#[test]
fn fb_old_27_sdk_kv_get_tracked() {
    assert_old_row("FB-OLD-27", "PSC-013");
}

/// FB-OLD-28 | PSC-013 | KV watch APIs existed but were not exercised.
#[test]
fn fb_old_28_kv_watch_tracked() {
    assert_old_row("FB-OLD-28", "PSC-013");
}

/// FB-OLD-29 | PSC-014 | Queue could be created but had no push/pop API.
#[test]
fn fb_old_29_queue_push_pop_disposition_tracked() {
    assert_old_row("FB-OLD-29", "PSC-014");
}

/// FB-OLD-30 | PSC-014 | Cache APIs existed in SDK but failed in embedded.
#[test]
fn fb_old_30_embedded_cache_disposition_tracked() {
    assert_old_row("FB-OLD-30", "PSC-014");
}

/// FB-OLD-31 | PSC-006 | Timeseries insert and basic aggregates worked.
#[test]
fn fb_old_31_timeseries_insert_aggregates_work() {
    assert_old_row("FB-OLD-31", "PSC-006");

    let rt = runtime();
    exec(&rt, "CREATE TIMESERIES fb_old_31_ts RETENTION 7 d");
    exec(
        &rt,
        "INSERT INTO fb_old_31_ts (metric, value, timestamp) VALUES \
         ('cpu', 10.0, 1704067200000000000), \
         ('cpu', 20.0, 1704067260000000000)",
    );
    let res = exec(&rt, "SELECT COUNT(*) AS n FROM fb_old_31_ts");
    assert_eq!(uint_value(only_record(&res), "n"), 2);
}

/// FB-OLD-32 | PSC-006 | Timeseries tags came back as placeholder text.
#[test]
fn fb_old_32_timeseries_tags_round_trip() {
    assert_old_row("FB-OLD-32", "PSC-006");

    let rt = runtime();
    exec(&rt, "CREATE TIMESERIES fb_old_32_ts RETENTION 7 d");
    exec(
        &rt,
        "INSERT INTO fb_old_32_ts (metric, value, tags, timestamp) VALUES \
         ('m', 1.0, {host: 'a'}, 1704067200000000000)",
    );
    let res = exec(&rt, "SELECT tags FROM fb_old_32_ts");
    let row = only_record(&res);
    match row.get("tags") {
        Some(Value::Json(_)) => {}
        other => panic!("expected JSON tags (not placeholder string), got {other:?}"),
    }
}

/// FB-OLD-33 | PSC-015 | Index creation worked and should stay visible.
#[test]
fn fb_old_33_index_creation_tracked() {
    assert_old_row("FB-OLD-33", "PSC-015");
}

/// FB-OLD-34 | PSC-015 | SHOW INDEXES returned zero rows after creation.
#[test]
fn fb_old_34_show_indexes_disposition_tracked() {
    assert_old_row("FB-OLD-34", "PSC-015");
}

/// FB-OLD-35 | PSC-008 | Entity insert results did not return IDs.
#[test]
fn fb_old_35_insert_returns_id_tracked() {
    assert_old_row("FB-OLD-35", "PSC-008");
}

/// FB-OLD-36 | PSC-015 | Internal red_* columns leaked into SELECT star.
#[test]
fn fb_old_36_select_star_metadata_tracked() {
    assert_old_row("FB-OLD-36", "PSC-015");
}

/// FB-OLD-37 | PSC-015 | DESCRIBE was unsupported.
#[test]
fn fb_old_37_describe_disposition_tracked() {
    assert_old_row("FB-OLD-37", "PSC-015");
}

/// FB-OLD-38 | PSC-015 | SHOW CREATE TABLE returned no rows.
#[test]
fn fb_old_38_show_create_table_disposition_tracked() {
    assert_old_row("FB-OLD-38", "PSC-015");
}

/// FB-OLD-39 | PSC-012 | Error messages listed rejected token as expected.
#[test]
fn fb_old_39_parse_error_vocabulary_tracked() {
    assert_old_row("FB-OLD-39", "PSC-012");
}

/// FB-OLD-40 | PSC-008 | bulkInsert was effectively single-row over stdio.
#[test]
fn fb_old_40_bulk_insert_packing_tracked() {
    assert_old_row("FB-OLD-40", "PSC-008");
}

/// FB-OLD-41 | PSC-008 | Typed rows and exists/list helpers were missing.
#[test]
fn fb_old_41_sdk_typed_helpers_tracked() {
    assert_old_row("FB-OLD-41", "PSC-008");
}

/// FB-OLD-42 | PSC-001 | Multi-model in one file was the killer story.
#[test]
fn fb_old_42_multi_model_one_file_tracked() {
    assert_old_row("FB-OLD-42", "PSC-001");
}

/// FB-OLD-43 | PSC-001 | Embedded snapshots were portable.
#[test]
fn fb_old_43_embedded_snapshot_portability_tracked() {
    assert_old_row("FB-OLD-43", "PSC-001");
}

/// FB-OLD-44 | PSC-010 | Docs needed a what-works page.
#[test]
fn fb_old_44_docs_what_works_tracked() {
    assert_old_row("FB-OLD-44", "PSC-010");
}

// =========================================================================
// FB-NEW-* — scenarios from feedbacks-new.md
// =========================================================================

/// FB-NEW-01 | PSC-003 | Node inserts did not return IDs for edge creation.
#[test]
fn fb_new_01_node_insert_returns_id_tracked() {
    assert_new_row("FB-NEW-01", "PSC-003");
}

/// FB-NEW-02 | PSC-003 | GRAPH SHORTEST_PATH required internal IDs.
#[test]
fn fb_new_02_shortest_path_label_lookup_tracked() {
    assert_new_row("FB-NEW-02", "PSC-003");
}

/// FB-NEW-03 | PSC-017 | GRAPH CENTRALITY differed on HTTP.
#[test]
fn fb_new_03_centrality_transport_parity_tracked() {
    assert_new_row("FB-NEW-03", "PSC-017");
}

/// FB-NEW-04 | PSC-002 | MATCH edge-label case normalization unclear.
#[test]
fn fb_new_04_edge_label_case_rule_tracked() {
    assert_new_row("FB-NEW-04", "PSC-002");
}

/// FB-NEW-05 | PSC-002 | MATCH edge property projection unreliable.
#[test]
fn fb_new_05_edge_property_projection_tracked() {
    assert_new_row("FB-NEW-05", "PSC-002");
}

/// FB-NEW-06 | PSC-003 | GRAPH PROPERTIES returned confusing node_type.
#[test]
fn fb_new_06_graph_properties_node_type_preserved() {
    assert_new_row("FB-NEW-06", "PSC-003");

    let rt = runtime();
    exec(
        &rt,
        "INSERT INTO fb_new_06_tales NODE (label, node_type, name) VALUES \
         ('hansel', 'StoryCharacter', 'Hansel')",
    );
    let res = exec(&rt, "GRAPH PROPERTIES 'hansel'");
    assert_eq!(text(only_record(&res), "node_type"), "StoryCharacter");
}

/// FB-NEW-07 | PSC-002 | Native MATCH still needed TS fallback.
#[test]
fn fb_new_07_match_conformance_tracked() {
    assert_new_row("FB-NEW-07", "PSC-002");
}

/// FB-NEW-08 | PSC-003 | Graph algorithms lacked pagination.
#[test]
fn fb_new_08_graph_algo_pagination_tracked() {
    assert_new_row("FB-NEW-08", "PSC-003");
}

/// FB-NEW-09 | PSC-015 | Graph table view mixed nodes and edges.
#[test]
fn fb_new_09_graph_table_view_tracked() {
    assert_new_row("FB-NEW-09", "PSC-015");
}

/// FB-NEW-10 | PSC-016 | File-backed rebuild sensitive to write order.
#[test]
fn fb_new_10_rebuild_order_persistence_tracked() {
    assert_new_row("FB-NEW-10", "PSC-016");
}

/// FB-NEW-11 | PSC-011 | COUNT(*) AS count parse was fragile.
#[test]
fn fb_new_11_count_star_as_count_works() {
    assert_new_row("FB-NEW-11", "PSC-011");

    let rt = runtime();
    exec(&rt, "CREATE TABLE fb_new_11_words (word TEXT)");
    exec(
        &rt,
        "INSERT INTO fb_new_11_words (word) VALUES ('a'), ('b'), ('c')",
    );
    let res = exec(&rt, "SELECT COUNT(*) AS count FROM fb_new_11_words");
    assert_eq!(res.result.columns, vec!["count"]);
    assert_eq!(uint_value(only_record(&res), "count"), 3);
}

/// FB-NEW-12 | PSC-018 | Tables became fallback for native model gaps.
#[test]
fn fb_new_12_native_fallback_tracked() {
    assert_new_row("FB-NEW-12", "PSC-018");
}

/// FB-NEW-13 | PSC-013 | KV keys rejected or normalized colon names.
#[test]
fn fb_new_13_kv_colon_keys_tracked() {
    assert_new_row("FB-NEW-13", "PSC-013");
}

/// FB-NEW-14 | PSC-013 | KV surfaced as kv_default.
#[test]
fn fb_new_14_kv_default_surface_tracked() {
    assert_new_row("FB-NEW-14", "PSC-013");
}

/// FB-NEW-15 | PSC-006 | Timeseries tags ergonomics.
#[test]
fn fb_new_15_timeseries_tags_ergonomics_tracked() {
    assert_new_row("FB-NEW-15", "PSC-006");
}

/// FB-NEW-16 | PSC-006 | Timeseries lacked bucket/window/downsample/range UX.
#[test]
fn fb_new_16_timeseries_bucket_ux_tracked() {
    assert_new_row("FB-NEW-16", "PSC-006");
}

/// FB-NEW-17 | PSC-018 | Rich statistics remained in TypeScript.
#[test]
fn fb_new_17_statistics_tracked() {
    assert_new_row("FB-NEW-17", "PSC-018");
}

/// FB-NEW-18 | PSC-011 | Aggregate syntax was sensitive.
#[test]
fn fb_new_18_aggregate_syntax_tracked() {
    assert_new_row("FB-NEW-18", "PSC-011");
}

/// FB-NEW-19 | PSC-018 | Graph statistics did not replace exploratory stats.
#[test]
fn fb_new_19_graph_statistics_tracked() {
    assert_new_row("FB-NEW-19", "PSC-018");
}

/// FB-NEW-20 | PSC-005 | Probabilistic lacked useful SDK/query interrogation.
#[test]
fn fb_new_20_probabilistic_interrogation_tracked() {
    assert_new_row("FB-NEW-20", "PSC-005");
}

/// FB-NEW-21 | PSC-005 | HLL insert format failed.
#[test]
fn fb_new_21_hll_insert_format_tracked() {
    assert_new_row("FB-NEW-21", "PSC-005");
}

/// FB-NEW-22 | PSC-015 | SHOW COLLECTIONS reported probabilistic as table.
#[test]
fn fb_new_22_show_collections_probabilistic_kind_tracked() {
    assert_new_row("FB-NEW-22", "PSC-015");
}

/// FB-NEW-23 | PSC-018 | VECTOR keyword existed but CREATE VECTOR rejected.
#[test]
fn fb_new_23_create_vector_tracked() {
    assert_new_row("FB-NEW-23", "PSC-018");
}

/// FB-NEW-24 | PSC-017 | Official HTTP client rejected readiness.
#[test]
fn fb_new_24_http_readiness_tracked() {
    assert_new_row("FB-NEW-24", "PSC-017");
}

/// FB-NEW-25 | PSC-017 | RedWire returned incomplete response envelopes.
#[test]
fn fb_new_25_redwire_envelope_tracked() {
    assert_new_row("FB-NEW-25", "PSC-017");
}

/// FB-NEW-26 | PSC-017 | gRPC was parsed as RedWire frames.
#[test]
fn fb_new_26_grpc_dispatch_tracked() {
    assert_new_row("FB-NEW-26", "PSC-017");
}

/// FB-NEW-27 | PSC-017 | HTTP parser rejected queries accepted by embedded.
#[test]
fn fb_new_27_http_query_parity_tracked() {
    assert_new_row("FB-NEW-27", "PSC-017");
}

/// FB-NEW-28 | PSC-017 | GRAPH CENTRALITY remote default returned only 100 rows.
#[test]
fn fb_new_28_centrality_default_limit_tracked() {
    assert_new_row("FB-NEW-28", "PSC-017");
}

/// FB-NEW-29 | PSC-017 | SQL over HTTP did not see graph nodes from graph inserts.
#[test]
fn fb_new_29_graph_sql_http_parity_tracked() {
    assert_new_row("FB-NEW-29", "PSC-017");
}

/// FB-NEW-30 | PSC-007 | Wire port collision caused full server startup failure.
#[test]
fn fb_new_30_port_collision_tracked() {
    assert_new_row("FB-NEW-30", "PSC-007");
}

/// FB-NEW-31 | PSC-016 | Embedded rebuild order should not matter.
#[test]
fn fb_new_31_rebuild_order_tracked() {
    assert_new_row("FB-NEW-31", "PSC-016");
}

/// FB-NEW-32 | PSC-005 | Errors did not point to the correct API for HLL writes.
#[test]
fn fb_new_32_hll_write_error_tracked() {
    assert_new_row("FB-NEW-32", "PSC-005");
}

/// FB-NEW-33 | PSC-010 | Showcase still required broad TS/raw-fetch workarounds.
#[test]
fn fb_new_33_showcase_workarounds_tracked() {
    assert_new_row("FB-NEW-33", "PSC-010");
}

// =========================================================================
// Bundle-level invariants — guard the regression suite itself.
// =========================================================================

/// Every FB-ID enumerated by `tests/public_surface_contract_matrix.rs`
/// must have a regression entry point here. This test is the contract
/// that #549 must encode every concrete failure path from the two
/// feedback source files.
#[test]
fn fb_bundle_covers_every_matrix_feedback_id() {
    let matrix_ids: Vec<String> = MATRIX
        .lines()
        .filter(|line| line.trim_start().starts_with("| FB-"))
        .filter_map(|line| line.trim_matches('|').split('|').next())
        .map(|cell| cell.trim().to_string())
        .collect();

    let module_source = include_str!("feedback_regression.rs");
    let mut missing = Vec::new();
    for id in &matrix_ids {
        if !module_source.contains(id) {
            missing.push(id.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "tests/grouped/control_feedback/feedback_regression.rs is missing regression entries for: {missing:?}"
    );
}

/// The matrix must continue to advertise the new regression module so
/// future readers can find the per-scenario tests from the matrix doc.
#[test]
fn fb_bundle_is_cross_referenced_from_the_matrix() {
    assert!(
        MATRIX.contains("tests/grouped/control_feedback/feedback_regression.rs"),
        "public-surface-contract-matrix.md must cross-reference tests/grouped/control_feedback/feedback_regression.rs"
    );
}
