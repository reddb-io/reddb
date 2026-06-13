use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn text(record: &reddb::storage::query::unified::UnifiedRecord, field: &str) -> Option<String> {
    record.get(field).and_then(|value| match value {
        Value::Text(value) => Some(value.to_string()),
        _ => None,
    })
}

#[test]
fn select_without_from_searches_matching_attributes_across_models() {
    let rt = runtime();

    exec(&rt, "CREATE TABLE travelers (passport TEXT, name TEXT)");
    exec(&rt, "CREATE TABLE pets (tag TEXT, name TEXT)");
    exec(&rt, "CREATE VECTOR embeddings DIM 2 METRIC cosine");
    exec(&rt, "CREATE TIMESERIES metrics RETENTION 7 d");
    exec(&rt, "CREATE QUEUE jobs");
    exec(
        &rt,
        "INSERT INTO travelers (passport, name) VALUES ('ABC123123', 'Ada')",
    );
    exec(
        &rt,
        "INSERT INTO pets (tag, name) VALUES ('ABC123123', 'Otto')",
    );
    exec(
        &rt,
        "INSERT INTO social NODE (label, node_type, passport, name) \
         VALUES ('person', 'Person', 'ABC123123', 'Grace')",
    );
    exec(
        &rt,
        "INSERT INTO places NODE (label, node_type, name) \
         VALUES ('city', 'Place', 'Paris')",
    );
    exec(
        &rt,
        "INSERT INTO embeddings VECTOR (dense, content) VALUES ([1.0, 0.0], 'passport note')",
    );
    exec(
        &rt,
        "INSERT INTO metrics (metric, value, tags, timestamp) \
         VALUES ('passport.lookup', 1.0, {passport: 'ABC123123'}, 1704067200000000000)",
    );
    exec(
        &rt,
        "QUEUE PUSH jobs {passport: 'ABC123123', name: 'Queue'}",
    );

    let result = rt
        .execute_query("SELECT * WHERE passport = 'ABC123123'")
        .expect("global select should execute");

    let mut names: Vec<String> = result
        .result
        .records
        .iter()
        .filter_map(|record| text(record, "name"))
        .collect();
    names.sort();

    assert_eq!(names, vec!["Ada".to_string(), "Grace".to_string()]);
    assert!(
        result
            .result
            .records
            .iter()
            .all(|record| record.get("passport") == Some(&Value::text("ABC123123"))),
        "every row should satisfy the global passport predicate: {:?}",
        result.result.records
    );

    let vector = rt
        .execute_query("SELECT content WHERE content = 'passport note'")
        .expect("global select should find vector content");
    assert_eq!(vector.result.records.len(), 1);
    assert_eq!(
        vector.result.records[0].get("content"),
        Some(&Value::text("passport note"))
    );

    let timeseries = rt
        .execute_query("SELECT metric WHERE tags.passport = 'ABC123123'")
        .expect("global select should find timeseries tags");
    assert_eq!(timeseries.result.records.len(), 1);
    assert_eq!(
        timeseries.result.records[0].get("metric"),
        Some(&Value::text("passport.lookup"))
    );

    let queue = rt
        .execute_query("SELECT payload WHERE payload.passport = 'ABC123123'")
        .expect("global select should find queue payload");
    assert_eq!(queue.result.records.len(), 1);
    assert!(queue.result.records[0].get("payload").is_some());
}
