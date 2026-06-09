#[allow(dead_code)]
mod support;

use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn only_record(result: &reddb::runtime::RuntimeQueryResult) -> &UnifiedRecord {
    assert_eq!(result.result.records.len(), 1, "expected one row");
    &result.result.records[0]
}

fn int_field(record: &UnifiedRecord, field: &str) -> i64 {
    match record.get(field) {
        Some(Value::Integer(value)) => *value,
        other => panic!("expected {field} integer field, got {other:?} in {record:?}"),
    }
}

fn float_field(record: &UnifiedRecord, field: &str) -> f64 {
    match record.get(field) {
        Some(Value::Float(value)) => *value,
        other => panic!("expected {field} float field, got {other:?} in {record:?}"),
    }
}

fn read_event_payload(rt: &RedDBRuntime, queue: &str) -> serde_json::Value {
    let result = exec(
        rt,
        &format!("QUEUE READ {queue} GROUP evt_readers CONSUMER c1 COUNT 1"),
    );
    let record = result
        .result
        .records
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no event in queue {queue}"));
    match record.get("payload") {
        Some(Value::Json(bytes)) => {
            serde_json::from_slice(bytes).expect("event payload should be valid JSON")
        }
        other => panic!("expected Json payload, got {other:?}"),
    }
}

#[test]
fn update_compound_add_matches_explicit_expression() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE compound_add (id INT, score INT)");
    exec(&rt, "INSERT INTO compound_add (id, score) VALUES (1, 10)");
    exec(&rt, "INSERT INTO compound_add (id, score) VALUES (2, 10)");

    let compound = exec(
        &rt,
        "UPDATE compound_add SET score += 5 WHERE id = 1 RETURNING score",
    );
    let explicit = exec(
        &rt,
        "UPDATE compound_add SET score = score + 5 WHERE id = 2 RETURNING score",
    );

    assert_eq!(
        int_field(only_record(&compound), "score"),
        int_field(only_record(&explicit), "score")
    );
    assert_eq!(int_field(only_record(&compound), "score"), 15);
}

#[test]
fn update_compound_supports_all_numeric_operators() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE compound_ops (id INT, add_v INT, sub_v INT, mul_v INT, div_v FLOAT, rem_v INT)",
    );
    exec(
        &rt,
        "INSERT INTO compound_ops (id, add_v, sub_v, mul_v, div_v, rem_v) VALUES (1, 10, 10, 10, 9.0, 10)",
    );

    let result = exec(
        &rt,
        "UPDATE compound_ops SET add_v += 2, sub_v -= 3, mul_v *= 4, div_v /= 2, rem_v %= 4 WHERE id = 1 RETURNING add_v, sub_v, mul_v, div_v, rem_v",
    );
    let record = only_record(&result);

    assert_eq!(int_field(record, "add_v"), 12);
    assert_eq!(int_field(record, "sub_v"), 7);
    assert_eq!(int_field(record, "mul_v"), 40);
    assert_eq!(float_field(record, "div_v"), 4.5);
    assert_eq!(int_field(record, "rem_v"), 2);
}

#[test]
fn update_compound_multiple_assignments_read_pre_image() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE compound_pre_image (id INT, a INT, b INT)",
    );
    exec(
        &rt,
        "INSERT INTO compound_pre_image (id, a, b) VALUES (1, 10, 1)",
    );

    let result = exec(
        &rt,
        "UPDATE compound_pre_image SET a += 5, b += a WHERE id = 1 RETURNING a, b",
    );
    let record = only_record(&result);

    assert_eq!(int_field(record, "a"), 15);
    assert_eq!(int_field(record, "b"), 11);
}

#[test]
fn update_compound_rejects_invalid_left_hand_fields_without_partial_write() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE compound_invalid (id INT, n INT, label TEXT, nullable INT)",
    );
    exec(
        &rt,
        "INSERT INTO compound_invalid (id, n, label, nullable) VALUES (1, 10, 'text', NULL)",
    );

    assert!(rt
        .execute_query("UPDATE compound_invalid SET missing += 1 WHERE id = 1")
        .is_err());
    assert!(rt
        .execute_query("UPDATE compound_invalid SET nullable += 1 WHERE id = 1")
        .is_err());
    assert!(rt
        .execute_query("UPDATE compound_invalid SET label += 1 WHERE id = 1")
        .is_err());
    assert!(rt
        .execute_query("UPDATE compound_invalid SET n += 1, label += 1 WHERE id = 1")
        .is_err());

    let result = exec(&rt, "SELECT n FROM compound_invalid WHERE id = 1");
    assert_eq!(int_field(only_record(&result), "n"), 10);
}

#[test]
fn update_compound_rejects_zero_division_modulo_and_overflow() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE compound_failures (id INT, n INT)");
    exec(
        &rt,
        "INSERT INTO compound_failures (id, n) VALUES (1, 10), (2, 9223372036854775807)",
    );

    assert!(rt
        .execute_query("UPDATE compound_failures SET n /= 0 WHERE id = 1")
        .is_err());
    assert!(rt
        .execute_query("UPDATE compound_failures SET n %= 0 WHERE id = 1")
        .is_err());
    assert!(rt
        .execute_query("UPDATE compound_failures SET n += 1 WHERE id = 2")
        .is_err());

    let row_one = exec(&rt, "SELECT n FROM compound_failures WHERE id = 1");
    let row_two = exec(&rt, "SELECT n FROM compound_failures WHERE id = 2");
    assert_eq!(int_field(only_record(&row_one), "n"), 10);
    assert_eq!(
        int_field(only_record(&row_two), "n"),
        9_223_372_036_854_775_807
    );
}

#[test]
fn update_compound_refreshes_indexes_events_and_persists_materialized_value() {
    let dir = support::temp_data_dir("e2e-compound-assignment");
    let path = dir.join("data.rdb");
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
            .expect("persistent runtime");
        exec(
            &rt,
            "CREATE TABLE compound_materialized (id INT, score INT) WITH EVENTS (UPDATE)",
        );
        exec(
            &rt,
            "CREATE INDEX idx_compound_score ON compound_materialized (score) USING HASH",
        );
        exec(
            &rt,
            "QUEUE GROUP CREATE compound_materialized_events evt_readers",
        );
        exec(
            &rt,
            "INSERT INTO compound_materialized (id, score) VALUES (1, 10)",
        );

        exec(
            &rt,
            "UPDATE compound_materialized SET score += 5 WHERE id = 1",
        );
        let indexed = exec(
            &rt,
            "SELECT id, score FROM compound_materialized WHERE score = 15",
        );
        assert_eq!(int_field(only_record(&indexed), "score"), 15);

        let payload = read_event_payload(&rt, "compound_materialized_events");
        assert_eq!(payload["op"].as_str(), Some("update"));
        assert_eq!(payload["after"]["score"].as_i64(), Some(15));
    }

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
        .expect("reopened persistent runtime");
    let reopened = exec(&rt, "SELECT score FROM compound_materialized WHERE id = 1");
    assert_eq!(int_field(only_record(&reopened), "score"), 15);
}
