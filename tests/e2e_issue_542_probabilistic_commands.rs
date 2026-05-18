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

fn uint_value(row: &UnifiedRecord, column: &str) -> u64 {
    match row.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer column {column}, got {other:?}"),
    }
}

fn bool_value(row: &UnifiedRecord, column: &str) -> bool {
    match row.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected bool column {column}, got {other:?}"),
    }
}

#[test]
fn hll_count_sketch_count_and_filter_check_return_documented_columns() {
    let rt = runtime();

    exec(&rt, "CREATE HLL visitors");
    exec(&rt, "HLL ADD visitors 'alice' 'bob' 'alice' 'carol'");
    let hll = exec(&rt, "HLL COUNT visitors");
    assert_eq!(hll.result.columns, vec!["count"]);
    assert_eq!(uint_value(only_record(&hll), "count"), 3);

    exec(&rt, "CREATE SKETCH clicks");
    exec(&rt, "SKETCH ADD clicks 'signup' 7");
    let sketch = exec(&rt, "SKETCH COUNT clicks 'signup'");
    assert_eq!(sketch.result.columns, vec!["estimate"]);
    assert_eq!(uint_value(only_record(&sketch), "estimate"), 7);

    exec(&rt, "CREATE FILTER sessions");
    exec(&rt, "FILTER ADD sessions 'sess:abc'");
    let hit = exec(&rt, "FILTER CHECK sessions 'sess:abc'");
    assert_eq!(hit.result.columns, vec!["exists"]);
    assert!(bool_value(only_record(&hit), "exists"));

    let miss = exec(&rt, "FILTER CHECK sessions 'sess:never'");
    assert!(!bool_value(only_record(&miss), "exists"));
}

#[test]
fn select_star_from_hll_sketch_or_filter_returns_guided_error() {
    let rt = runtime();

    exec(&rt, "CREATE HLL visitors");
    exec(&rt, "CREATE SKETCH clicks");
    exec(&rt, "CREATE FILTER sessions");

    for (collection, sql) in [
        ("visitors", "SELECT * FROM visitors"),
        ("clicks", "SELECT * FROM clicks"),
        ("sessions", "SELECT * FROM sessions"),
    ] {
        let err = rt
            .execute_query(sql)
            .expect_err(&format!("SELECT * from {collection} should be rejected"));
        let message = format!("{err:?}");
        assert!(
            message.contains(collection),
            "error for {sql:?} should name the collection, got: {message}"
        );
        assert!(
            message.contains("SELECT CARDINALITY")
                && message.contains("FREQ(")
                && message.contains("CONTAINS("),
            "error for {sql:?} should point at SELECT CARDINALITY / FREQ(...) / CONTAINS(...), got: {message}"
        );
    }
}

#[test]
fn runtime_error_message_for_star_select_names_correct_command_forms() {
    let rt = runtime();
    exec(&rt, "CREATE HLL visitors");

    let err = rt
        .execute_query("SELECT * FROM visitors")
        .expect_err("SELECT * from an HLL should fail with guided error");
    let message = format!("{err:?}");
    assert!(
        message.contains(
            "supports SELECT CARDINALITY, FREQ(...), or CONTAINS(...) read forms"
        ),
        "error message must spell out the read forms verbatim so callers can copy-paste; got: {message}"
    );
}
