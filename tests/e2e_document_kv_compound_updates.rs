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
    assert_eq!(
        result.result.records.len(),
        1,
        "{:?}",
        result.result.records
    );
    &result.result.records[0]
}

fn int_field(record: &UnifiedRecord, field: &str) -> i64 {
    match record.get(field) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => i64::try_from(*value).expect("u64 should fit i64"),
        other => panic!("expected {field} integer field, got {other:?} in {record:?}"),
    }
}

fn text_field(record: &UnifiedRecord, field: &str) -> String {
    match record.get(field) {
        Some(Value::Text(value)) => value.as_ref().to_string(),
        other => panic!("expected {field} text field, got {other:?} in {record:?}"),
    }
}

fn err_string(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(sql)
        .expect_err("query should fail")
        .to_string()
}

#[test]
fn document_compound_update_uses_top_level_where_and_post_image_returning() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT doc_scores");
    exec(
        &rt,
        r#"INSERT INTO doc_scores DOCUMENT (body) VALUES ('{"category":"active","name":"alpha","score":10}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO doc_scores DOCUMENT (body) VALUES ('{"category":"inactive","name":"beta","score":100}')"#,
    );

    let updated = exec(
        &rt,
        "UPDATE doc_scores DOCUMENTS SET score += 5 WHERE category = 'active' RETURNING name, score",
    );
    assert_eq!(updated.affected_rows, 1);
    let returned = only_record(&updated);
    assert_eq!(text_field(returned, "name"), "alpha");
    assert_eq!(int_field(returned, "score"), 15);

    let active = exec(&rt, "SELECT score FROM doc_scores WHERE name = 'alpha'");
    assert_eq!(int_field(only_record(&active), "score"), 15);
    let inactive = exec(&rt, "SELECT score FROM doc_scores WHERE name = 'beta'");
    assert_eq!(int_field(only_record(&inactive), "score"), 100);
}

#[test]
fn kv_compound_update_uses_key_where_and_post_image_returning() {
    let rt = runtime();
    exec(&rt, "CREATE KV counters");
    exec(
        &rt,
        "INSERT INTO counters KV (key, value) VALUES ('hits', 10)",
    );
    exec(
        &rt,
        "INSERT INTO counters KV (key, value) VALUES ('misses', 3)",
    );

    let updated = exec(
        &rt,
        "UPDATE counters KV SET value += 7 WHERE key = 'hits' RETURNING value",
    );
    assert_eq!(updated.affected_rows, 1);
    let returned = only_record(&updated);
    assert_eq!(int_field(returned, "value"), 17);

    let hits = exec(&rt, "SELECT value FROM counters WHERE key = 'hits'");
    assert_eq!(int_field(only_record(&hits), "value"), 17);
    let misses = exec(&rt, "SELECT value FROM counters WHERE key = 'misses'");
    assert_eq!(int_field(only_record(&misses), "value"), 3);
}

#[test]
fn document_and_kv_compound_failures_abort_without_partial_write() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT doc_invalid");
    exec(
        &rt,
        r#"INSERT INTO doc_invalid DOCUMENT (body) VALUES ('{"category":"batch","name":"ok","score":10}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO doc_invalid DOCUMENT (body) VALUES ('{"category":"batch","name":"bad","score":"text"}')"#,
    );

    let doc_err = err_string(
        &rt,
        "UPDATE doc_invalid DOCUMENTS SET score += 1 WHERE category = 'batch'",
    );
    assert!(doc_err.contains("numeric field 'score'"));
    let ok_doc = exec(&rt, "SELECT score FROM doc_invalid WHERE name = 'ok'");
    assert_eq!(int_field(only_record(&ok_doc), "score"), 10);

    exec(&rt, "CREATE KV kv_invalid");
    exec(
        &rt,
        "INSERT INTO kv_invalid KV (key, value) VALUES ('ok', 10)",
    );
    exec(
        &rt,
        "INSERT INTO kv_invalid KV (key, value) VALUES ('bad', 0)",
    );
    exec(
        &rt,
        "INSERT INTO kv_invalid KV (key, value) VALUES ('nullish', NULL)",
    );
    exec(
        &rt,
        "INSERT INTO kv_invalid KV (key, value) VALUES ('max', 9223372036854775807)",
    );

    let kv_err = err_string(&rt, "UPDATE kv_invalid KV SET value /= 0 WHERE key = 'ok'");
    assert!(kv_err.contains("division by zero"));
    let modulo_err = err_string(&rt, "UPDATE kv_invalid KV SET value %= 0 WHERE key = 'ok'");
    assert!(modulo_err.contains("modulo by zero"));
    let null_err = err_string(
        &rt,
        "UPDATE kv_invalid KV SET value += 1 WHERE key = 'nullish'",
    );
    assert!(null_err.contains("non-null numeric field 'value'"));
    let overflow_err = err_string(&rt, "UPDATE kv_invalid KV SET value += 1 WHERE key = 'max'");
    assert!(overflow_err.contains("numeric overflow"));
    let ok_kv = exec(&rt, "SELECT value FROM kv_invalid WHERE key = 'ok'");
    assert_eq!(int_field(only_record(&ok_kv), "value"), 10);
    let max_kv = exec(&rt, "SELECT value FROM kv_invalid WHERE key = 'max'");
    assert_eq!(
        int_field(only_record(&max_kv), "value"),
        9_223_372_036_854_775_807
    );

    let missing_err = err_string(
        &rt,
        "UPDATE kv_invalid KV SET missing += 1 WHERE key = 'ok'",
    );
    assert!(missing_err.contains("existing numeric field 'missing'"));
}

#[test]
fn explicit_targets_keep_tenant_rls_scoped_to_matching_item_category() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT tenant_docs");
    exec(
        &rt,
        r#"INSERT INTO tenant_docs DOCUMENT (body) VALUES ('{"tenant_id":"acme","name":"a","score":10}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO tenant_docs DOCUMENT (body) VALUES ('{"tenant_id":"globex","name":"g","score":20}')"#,
    );
    exec(
        &rt,
        "CREATE POLICY tenant_update ON tenant_docs FOR UPDATE USING (tenant_id = CURRENT_TENANT())",
    );
    exec(&rt, "ALTER TABLE tenant_docs ENABLE ROW LEVEL SECURITY");

    let updated = exec(
        &rt,
        "WITHIN TENANT 'acme' UPDATE tenant_docs DOCUMENTS SET score += 1 RETURNING name, score",
    );
    assert_eq!(updated.affected_rows, 1);
    let returned = only_record(&updated);
    assert_eq!(text_field(returned, "name"), "a");
    assert_eq!(int_field(returned, "score"), 11);
}
