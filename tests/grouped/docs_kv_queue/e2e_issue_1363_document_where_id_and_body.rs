// Regression coverage for issue #1363 — document WHERE matches by id and body field.
//
// Acceptance criteria:
//   - UPDATE … DOCUMENTS … WHERE id = <v> matches and reports record_count > 0.
//   - UPDATE … DOCUMENTS … WHERE <body-field> = <v> matches by body field.
//   - SELECT and DELETE over documents filter correctly by id and by body field.
//   - A genuine no-match returns record_count = 0 (not an error).

use reddb::storage::schema::Value;
use reddb::RedDBRuntime;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

fn uint_field(record: &reddb::storage::query::UnifiedRecord, field: &str) -> u64 {
    match record.get(field) {
        Some(Value::UnsignedInteger(v)) => *v,
        Some(Value::Integer(v)) => *v as u64,
        other => panic!("expected uint {field}, got {other:?} in {record:?}"),
    }
}

fn text_field<'a>(record: &'a reddb::storage::query::UnifiedRecord, field: &str) -> &'a str {
    match record.get(field) {
        Some(Value::Text(v)) => v.as_ref(),
        other => panic!("expected text {field}, got {other:?} in {record:?}"),
    }
}

#[test]
fn update_documents_where_id_matches_and_reports_record_count() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT id_filter_docs");
    let insert = exec(
        &rt,
        r#"INSERT INTO id_filter_docs DOCUMENT (body) VALUES ('{"name":"target","score":1}') RETURNING *"#,
    );
    exec(
        &rt,
        r#"INSERT INTO id_filter_docs DOCUMENT (body) VALUES ('{"name":"other","score":2}')"#,
    );

    let inserted_rid = insert.result.records[0]
        .get("rid")
        .and_then(|v| if let Value::UnsignedInteger(n) = v { Some(*n) } else { None })
        .expect("rid on INSERT result");

    let updated = rt
        .execute_query(&format!(
            "UPDATE id_filter_docs DOCUMENTS SET score = 99 WHERE id = {inserted_rid}"
        ))
        .expect("UPDATE WHERE id should succeed");

    assert_eq!(
        updated.affected_rows, 1,
        "WHERE id = <entity_id> should match exactly one document; got {updated:?}"
    );

    let check = exec(
        &rt,
        &format!("SELECT score FROM id_filter_docs WHERE id = {inserted_rid}"),
    );
    assert_eq!(check.result.records.len(), 1);
    assert_eq!(uint_field(&check.result.records[0], "score"), 99);
}

#[test]
fn update_documents_where_body_field_matches_and_reports_record_count() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT body_filter_docs");
    exec(
        &rt,
        r#"INSERT INTO body_filter_docs DOCUMENT (body) VALUES ('{"category":"active","score":1}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO body_filter_docs DOCUMENT (body) VALUES ('{"category":"inactive","score":2}')"#,
    );

    let updated = exec(
        &rt,
        "UPDATE body_filter_docs DOCUMENTS SET score = 50 WHERE category = 'active'",
    );
    assert_eq!(
        updated.affected_rows, 1,
        "WHERE <body-field> = <v> should match one document"
    );

    let check = exec(
        &rt,
        "SELECT score FROM body_filter_docs WHERE category = 'active'",
    );
    assert_eq!(check.result.records.len(), 1);
    assert_eq!(uint_field(&check.result.records[0], "score"), 50);
}

#[test]
fn update_documents_where_body_dot_field_matches() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT body_dot_docs");
    exec(
        &rt,
        r#"INSERT INTO body_dot_docs DOCUMENT (body) VALUES ('{"level":"warn","score":1}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO body_dot_docs DOCUMENT (body) VALUES ('{"level":"info","score":2}')"#,
    );

    let updated = exec(
        &rt,
        "UPDATE body_dot_docs DOCUMENTS SET score = 77 WHERE body.level = 'warn'",
    );
    assert_eq!(
        updated.affected_rows, 1,
        "WHERE body.<field> = <v> should match one document"
    );
}

#[test]
fn select_documents_where_id_filters_correctly() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT select_id_docs");
    let ins = exec(
        &rt,
        r#"INSERT INTO select_id_docs DOCUMENT (body) VALUES ('{"name":"alice"}') RETURNING *"#,
    );
    exec(
        &rt,
        r#"INSERT INTO select_id_docs DOCUMENT (body) VALUES ('{"name":"bob"}')"#,
    );

    let rid = ins.result.records[0]
        .get("rid")
        .and_then(|v| if let Value::UnsignedInteger(n) = v { Some(*n) } else { None })
        .expect("rid on INSERT");

    let result = rt
        .execute_query(&format!(
            "SELECT name FROM select_id_docs WHERE id = {rid}"
        ))
        .expect("SELECT WHERE id should succeed");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(text_field(&result.result.records[0], "name"), "alice");
}

#[test]
fn delete_documents_where_id_removes_correct_document() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT delete_id_docs");
    let ins = exec(
        &rt,
        r#"INSERT INTO delete_id_docs DOCUMENT (body) VALUES ('{"name":"to-delete"}') RETURNING *"#,
    );
    exec(
        &rt,
        r#"INSERT INTO delete_id_docs DOCUMENT (body) VALUES ('{"name":"keep"}')"#,
    );

    let rid = ins.result.records[0]
        .get("rid")
        .and_then(|v| if let Value::UnsignedInteger(n) = v { Some(*n) } else { None })
        .expect("rid on INSERT");

    let deleted = rt
        .execute_query(&format!(
            "DELETE FROM delete_id_docs WHERE id = {rid}"
        ))
        .expect("DELETE WHERE id should succeed");
    assert_eq!(deleted.affected_rows, 1, "DELETE WHERE id should remove one document");

    let remaining = exec(&rt, "SELECT name FROM delete_id_docs");
    assert_eq!(remaining.result.records.len(), 1);
    assert_eq!(text_field(&remaining.result.records[0], "name"), "keep");
}

#[test]
fn delete_documents_where_body_field_removes_correct_document() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT delete_body_docs");
    exec(
        &rt,
        r#"INSERT INTO delete_body_docs DOCUMENT (body) VALUES ('{"status":"archived"}')"#,
    );
    exec(
        &rt,
        r#"INSERT INTO delete_body_docs DOCUMENT (body) VALUES ('{"status":"active"}')"#,
    );

    let deleted = exec(
        &rt,
        "DELETE FROM delete_body_docs WHERE status = 'archived'",
    );
    assert_eq!(deleted.affected_rows, 1);

    let remaining = exec(&rt, "SELECT status FROM delete_body_docs");
    assert_eq!(remaining.result.records.len(), 1);
    assert_eq!(text_field(&remaining.result.records[0], "status"), "active");
}

#[test]
fn no_match_where_id_returns_record_count_zero() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT nomatch_id_docs");
    exec(
        &rt,
        r#"INSERT INTO nomatch_id_docs DOCUMENT (body) VALUES ('{"x":1}')"#,
    );

    let updated = rt
        .execute_query("UPDATE nomatch_id_docs DOCUMENTS SET x = 0 WHERE id = 999999")
        .expect("UPDATE with no-match WHERE id should succeed (not error)");
    assert_eq!(
        updated.affected_rows, 0,
        "WHERE id = <non-existent> should yield record_count=0, not an error"
    );
}

#[test]
fn no_match_where_body_field_returns_record_count_zero() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT nomatch_body_docs");
    exec(
        &rt,
        r#"INSERT INTO nomatch_body_docs DOCUMENT (body) VALUES ('{"label":"real"}')"#,
    );

    let updated = rt
        .execute_query(
            "UPDATE nomatch_body_docs DOCUMENTS SET label = 'nope' WHERE label = 'nonexistent'",
        )
        .expect("UPDATE with no-match WHERE body-field should succeed (not error)");
    assert_eq!(
        updated.affected_rows, 0,
        "WHERE body-field = <non-existent-value> should yield record_count=0"
    );
}
