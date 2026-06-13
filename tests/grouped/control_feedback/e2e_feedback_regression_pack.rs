use reddb::json::Value as JsonValue;
use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
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

fn text(row: &UnifiedRecord, column: &str) -> String {
    match row.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text column {column}, got {other:?}"),
    }
}

fn bool_value(row: &UnifiedRecord, column: &str) -> bool {
    match row.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected bool column {column}, got {other:?}"),
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
fn feedback_probabilistic_sql_read_forms_have_stable_columns() {
    let rt = runtime();

    exec(&rt, "CREATE HLL unique_visitors");
    exec(&rt, "HLL ADD unique_visitors 'alice' 'bob' 'alice'");
    let hll = exec(
        &rt,
        "SELECT CARDINALITY AS distinct_visitors FROM unique_visitors",
    );
    assert_eq!(hll.result.columns, vec!["distinct_visitors"]);
    assert_eq!(uint_value(only_record(&hll), "distinct_visitors"), 2);

    exec(&rt, "CREATE SKETCH tale_terms");
    exec(&rt, "SKETCH ADD tale_terms 'forest' 3");
    let sketch = exec(&rt, "SELECT FREQ('forest') AS forest_count FROM tale_terms");
    assert_eq!(sketch.result.columns, vec!["forest_count"]);
    assert_eq!(uint_value(only_record(&sketch), "forest_count"), 3);

    exec(&rt, "CREATE FILTER seen_tales");
    exec(&rt, "FILTER ADD seen_tales 'hansel:gretel'");
    let filter = exec(
        &rt,
        "SELECT CONTAINS('hansel:gretel') AS seen FROM seen_tales",
    );
    assert_eq!(filter.result.columns, vec!["seen"]);
    assert!(bool_value(only_record(&filter), "seen"));
}

#[test]
fn feedback_count_star_as_count_returns_count_column() {
    let rt = runtime();

    exec(&rt, "CREATE TABLE words (word TEXT)");
    exec(
        &rt,
        "INSERT INTO words (word) VALUES ('forest'), ('witch'), ('forest')",
    );

    let result = exec(&rt, "SELECT COUNT(*) AS count FROM words");
    assert_eq!(result.result.columns, vec!["count"]);
    assert_eq!(uint_value(only_record(&result), "count"), 3);
}

#[test]
fn feedback_kv_preserves_quoted_colon_keys_in_sql_and_dsl() {
    let rt = runtime();

    exec(
        &rt,
        "INSERT INTO settings KV (key, value) VALUES ('tenant:feature', 'enabled')",
    );
    let sql_read = exec(
        &rt,
        "SELECT key, value FROM settings WHERE key = 'tenant:feature'",
    );
    let sql_row = only_record(&sql_read);
    assert_eq!(text(sql_row, "key"), "tenant:feature");
    assert_eq!(text(sql_row, "value"), "enabled");

    exec(&rt, "KV PUT settings.'tenant:mode' = 'dark'");
    let dsl_read = exec(&rt, "KV GET settings.'tenant:mode'");
    let dsl_row = only_record(&dsl_read);
    assert_eq!(text(dsl_row, "collection"), "settings");
    assert_eq!(text(dsl_row, "key"), "tenant:mode");
    assert_eq!(text(dsl_row, "value"), "dark");
}

#[test]
fn feedback_timeseries_tags_return_json_values() {
    let rt = runtime();

    exec(&rt, "CREATE TIMESERIES tale_metrics RETENTION 7 d");
    exec(
        &rt,
        "INSERT INTO tale_metrics (metric, value, tags, timestamp) VALUES \
         ('tale.reads', 2.0, {tale: 'hansel-gretel', region: 'black-forest'}, 1704067200000000000)",
    );

    let result = exec(&rt, "SELECT metric, tags FROM tale_metrics");
    let row = only_record(&result);
    assert_eq!(text(row, "metric"), "tale.reads");
    match row.get("tags") {
        Some(Value::Json(bytes)) => {
            let tags: JsonValue = reddb::json::from_slice(bytes).expect("tags should be JSON");
            assert_eq!(
                tags.get("tale").and_then(JsonValue::as_str),
                Some("hansel-gretel")
            );
            assert_eq!(
                tags.get("region").and_then(JsonValue::as_str),
                Some("black-forest")
            );
        }
        other => panic!("expected JSON tags, got {other:?}"),
    }
}

#[test]
fn feedback_graph_properties_preserves_node_type() {
    let rt = runtime();

    exec(
        &rt,
        "INSERT INTO tales NODE (label, node_type, name) VALUES \
         ('hansel', 'StoryCharacter', 'Hansel')",
    );

    let result = exec(&rt, "GRAPH PROPERTIES 'hansel'");
    let row = only_record(&result);
    assert_eq!(text(row, "label"), "hansel");
    assert_eq!(text(row, "node_type"), "StoryCharacter");
    assert_eq!(text(row, "name"), "Hansel");
}
