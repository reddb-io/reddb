use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
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

fn returned_texts(result: &reddb::runtime::RuntimeQueryResult, field: &str) -> Vec<String> {
    result
        .result
        .records
        .iter()
        .map(|record| text_field(record, field))
        .collect()
}

fn err_string(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(sql)
        .expect_err("query should fail")
        .to_string()
}

#[test]
fn document_claim_updates_ordered_subset_with_returning() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT doc_claim_tasks");
    exec(
        &rt,
        r#"INSERT INTO doc_claim_tasks DOCUMENT (body) VALUES ('{"name":"slow","priority":30,"status":"ready"}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO doc_claim_tasks DOCUMENT (body) VALUES ('{"name":"fast","priority":10,"status":"ready"}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO doc_claim_tasks DOCUMENT (body) VALUES ('{"name":"middle","priority":20,"status":"ready"}')"#,
    );

    let claimed = exec(
        &rt,
        "UPDATE doc_claim_tasks DOCUMENTS SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 2 ORDER BY priority ASC RETURNING name, status",
    );

    assert_eq!(claimed.affected_rows, 2);
    assert_eq!(returned_texts(&claimed, "name"), vec!["fast", "middle"]);
    assert_eq!(
        returned_texts(&claimed, "status"),
        vec!["claimed", "claimed"]
    );
    let remaining = exec(
        &rt,
        "SELECT name FROM doc_claim_tasks WHERE status = 'ready' ORDER BY priority ASC",
    );
    assert_eq!(returned_texts(&remaining, "name"), vec!["slow"]);
}

#[test]
fn kv_claim_uses_key_identity_for_ordered_subset_with_returning() {
    let rt = runtime();
    exec(&rt, "CREATE KV kv_claim_tasks");
    exec(
        &rt,
        "INSERT INTO kv_claim_tasks KV (key, value) VALUES ('slow', 30)",
    );
    exec(
        &rt,
        "INSERT INTO kv_claim_tasks KV (key, value) VALUES ('fast', 10)",
    );
    exec(
        &rt,
        "INSERT INTO kv_claim_tasks KV (key, value) VALUES ('middle', 20)",
    );
    let inserted = exec(&rt, "SELECT key FROM kv_claim_tasks ORDER BY value ASC");
    assert_eq!(
        returned_texts(&inserted, "key"),
        vec!["fast", "middle", "slow"]
    );

    let claimed = exec(
        &rt,
        "UPDATE kv_claim_tasks KV SET value += 100 WHERE value >= 10 \
         CLAIM LIMIT 2 ORDER BY value ASC RETURNING value",
    );

    assert_eq!(claimed.affected_rows, 2);
    let values = claimed
        .result
        .records
        .iter()
        .map(|record| int_field(record, "value"))
        .collect::<Vec<_>>();
    assert_eq!(values, vec![110, 120]);
    let updated = exec(
        &rt,
        "SELECT key FROM kv_claim_tasks WHERE value >= 100 ORDER BY value ASC",
    );
    let mut updated_keys = returned_texts(&updated, "key");
    updated_keys.sort();
    assert_eq!(updated_keys, vec!["fast", "middle"]);
    let remaining = exec(
        &rt,
        "SELECT key FROM kv_claim_tasks WHERE value < 100 ORDER BY value ASC",
    );
    assert_eq!(returned_texts(&remaining, "key"), vec!["slow"]);
}

#[test]
fn document_claim_locks_skip_and_release_on_rollback() {
    let rt = runtime();
    set_current_connection_id(145401);
    exec(&rt, "CREATE DOCUMENT doc_claim_lock_tasks");
    exec(
        &rt,
        r#"INSERT INTO doc_claim_lock_tasks DOCUMENT (body) VALUES ('{"name":"a","priority":10,"status":"ready"}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO doc_claim_lock_tasks DOCUMENT (body) VALUES ('{"name":"b","priority":20,"status":"ready"}')"#,
    );

    exec(&rt, "BEGIN");
    let first = exec(
        &rt,
        "UPDATE doc_claim_lock_tasks DOCUMENTS SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 1 ORDER BY priority ASC RETURNING name",
    );
    assert_eq!(returned_texts(&first, "name"), vec!["a"]);

    set_current_connection_id(145402);
    let second = exec(
        &rt,
        "UPDATE doc_claim_lock_tasks DOCUMENTS SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 1 ORDER BY priority ASC RETURNING name",
    );
    assert_eq!(second.affected_rows, 1);
    assert_eq!(returned_texts(&second, "name"), vec!["b"]);

    set_current_connection_id(145401);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(145402);
    let after_rollback = exec(
        &rt,
        "UPDATE doc_claim_lock_tasks DOCUMENTS SET status = 'claimed' WHERE status = 'ready' \
         CLAIM LIMIT 1 ORDER BY priority ASC RETURNING name",
    );
    assert_eq!(after_rollback.affected_rows, 1);
    assert_eq!(returned_texts(&after_rollback, "name"), vec!["a"]);
    clear_current_connection_id();
}

#[test]
fn kv_claim_locks_skip_and_release_on_rollback_by_key() {
    let rt = runtime();
    set_current_connection_id(145403);
    exec(&rt, "CREATE KV kv_claim_lock_tasks");
    exec(
        &rt,
        "INSERT INTO kv_claim_lock_tasks KV (key, value) VALUES ('a', 10)",
    );
    exec(
        &rt,
        "INSERT INTO kv_claim_lock_tasks KV (key, value) VALUES ('b', 20)",
    );

    exec(&rt, "BEGIN");
    let first = exec(
        &rt,
        "UPDATE kv_claim_lock_tasks KV SET value += 100 WHERE value >= 10 \
         CLAIM LIMIT 1 ORDER BY value ASC RETURNING value",
    );
    assert_eq!(int_field(only_record(&first), "value"), 110);

    set_current_connection_id(145404);
    let second = exec(
        &rt,
        "UPDATE kv_claim_lock_tasks KV SET value += 100 WHERE value >= 10 \
         CLAIM LIMIT 1 ORDER BY value ASC RETURNING value",
    );
    assert_eq!(second.affected_rows, 1);
    assert_eq!(int_field(only_record(&second), "value"), 120);
    let visible_claimed = exec(
        &rt,
        "SELECT key FROM kv_claim_lock_tasks WHERE value >= 100 ORDER BY value ASC",
    );
    assert_eq!(returned_texts(&visible_claimed, "key"), vec!["b"]);

    set_current_connection_id(145403);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(145404);
    let after_rollback = exec(
        &rt,
        "UPDATE kv_claim_lock_tasks KV SET value += 100 WHERE value >= 10 \
         CLAIM LIMIT 1 ORDER BY value ASC RETURNING value",
    );
    assert_eq!(after_rollback.affected_rows, 1);
    assert_eq!(int_field(only_record(&after_rollback), "value"), 110);
    let claimed_after_rollback = exec(
        &rt,
        "SELECT key FROM kv_claim_lock_tasks WHERE value >= 100 ORDER BY value ASC",
    );
    let mut claimed_after_rollback_keys = returned_texts(&claimed_after_rollback, "key");
    claimed_after_rollback_keys.sort();
    assert_eq!(claimed_after_rollback_keys, vec!["a", "b"]);
    clear_current_connection_id();
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
