// Regression coverage for issue #1363 — RQL document WHERE matches by id
// and body field.
//
// Each test maps to one bullet in the issue's `## Acceptance criteria`:
//   - `UPDATE … DOCUMENTS … WHERE id = <v>` matches by logical id.
//   - `UPDATE … DOCUMENTS … WHERE <body-field> = <v>` matches by a field
//     inside the document body.
//   - `SELECT` and `DELETE` over documents filter correctly by id and by
//     body field.
//   - A genuine no-match returns `record_count: 0` (the `affected_rows` /
//     row-count surface), distinct from an "unsupported" error.
//
// Before #1363 these predicates resolved against nothing, so every
// `UPDATE/DELETE/SELECT … WHERE id = <v>` reported zero matches even when
// the document existed. `id` now aliases the entity's logical id as a
// last resort, while a real `id` column or a body-promoted `id` field
// still wins.

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

fn u64_field(record: &UnifiedRecord, field: &str) -> u64 {
    match record.get(field) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => u64::try_from(*value).expect("i64 should fit u64"),
        other => panic!("expected {field} integer field, got {other:?} in {record:?}"),
    }
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

// Seed two documents and return the logical id of the `alpha` document so
// the id-based predicates have a real value to match.
fn seed(rt: &RedDBRuntime, collection: &str) -> u64 {
    exec(&rt, &format!("CREATE DOCUMENT {collection}"));
    for body in [
        r#"{"category":"active","name":"alpha","score":10}"#,
        r#"{"category":"inactive","name":"beta","score":100}"#,
    ] {
        exec(
            &rt,
            &format!("INSERT INTO {collection} DOCUMENT VALUES ({body})"),
        );
    }
    let page = exec(
        &rt,
        &format!("SELECT rid FROM {collection} WHERE name = 'alpha'"),
    );
    assert_eq!(page.result.records.len(), 1, "alpha should exist once");
    u64_field(&page.result.records[0], "rid")
}

// Bullet 1: `UPDATE … DOCUMENTS … WHERE id = <v>` matches by logical id.
#[test]
fn update_documents_where_id_matches_by_logical_id() {
    let rt = runtime();
    let alpha_id = seed(&rt, "issue1363_update_id");

    let updated = exec(
        &rt,
        &format!(
            "UPDATE issue1363_update_id DOCUMENTS SET score += 5 \
             WHERE id = {alpha_id} RETURNING name, score"
        ),
    );
    assert_eq!(
        updated.affected_rows, 1,
        "WHERE id = <alpha> should match exactly the alpha document"
    );
    let returned = &updated.result.records[0];
    assert_eq!(text_field(returned, "name"), "alpha");
    assert_eq!(int_field(returned, "score"), 15);

    // The other document is untouched.
    let beta = exec(
        &rt,
        "SELECT score FROM issue1363_update_id WHERE name = 'beta'",
    );
    assert_eq!(int_field(&beta.result.records[0], "score"), 100);
}

// Bullet 2: `UPDATE … DOCUMENTS … WHERE <body-field> = <v>` matches by a
// field inside the document body, addressed with the explicit `body.`
// prefix.
#[test]
fn update_documents_where_body_field_matches() {
    let rt = runtime();
    seed(&rt, "issue1363_update_body");

    let updated = exec(
        &rt,
        "UPDATE issue1363_update_body DOCUMENTS SET score += 1 \
         WHERE body.category = 'active' RETURNING name, score",
    );
    assert_eq!(
        updated.affected_rows, 1,
        "WHERE body.category should match the active document"
    );
    assert_eq!(text_field(&updated.result.records[0], "name"), "alpha");
    assert_eq!(int_field(&updated.result.records[0], "score"), 11);
}

// Bullet 3a: `SELECT` over documents filters correctly by id and by body
// field.
#[test]
fn select_documents_filters_by_id_and_body_field() {
    let rt = runtime();
    let alpha_id = seed(&rt, "issue1363_select");

    let by_id = exec(
        &rt,
        &format!("SELECT name FROM issue1363_select WHERE id = {alpha_id}"),
    );
    assert_eq!(by_id.result.records.len(), 1, "id select should match one");
    assert_eq!(text_field(&by_id.result.records[0], "name"), "alpha");

    let by_body = exec(
        &rt,
        "SELECT name FROM issue1363_select WHERE body.category = 'inactive'",
    );
    assert_eq!(
        by_body.result.records.len(),
        1,
        "body-field select should match one"
    );
    assert_eq!(text_field(&by_body.result.records[0], "name"), "beta");
}

// Bullet 3b: `DELETE` over documents filters correctly by id and by body
// field.
#[test]
fn delete_documents_filters_by_id() {
    let rt = runtime();
    let alpha_id = seed(&rt, "issue1363_delete_id");

    let deleted = exec(
        &rt,
        &format!("DELETE FROM issue1363_delete_id WHERE id = {alpha_id}"),
    );
    assert_eq!(deleted.affected_rows, 1, "id delete should remove one");

    let remaining = exec(&rt, "SELECT name FROM issue1363_delete_id");
    assert_eq!(remaining.result.records.len(), 1);
    assert_eq!(text_field(&remaining.result.records[0], "name"), "beta");
}

#[test]
fn delete_documents_filters_by_body_field() {
    let rt = runtime();
    seed(&rt, "issue1363_delete_body");

    let deleted = exec(
        &rt,
        "DELETE FROM issue1363_delete_body WHERE body.category = 'active'",
    );
    assert_eq!(deleted.affected_rows, 1, "body delete should remove one");

    let remaining = exec(&rt, "SELECT name FROM issue1363_delete_body");
    assert_eq!(remaining.result.records.len(), 1);
    assert_eq!(text_field(&remaining.result.records[0], "name"), "beta");
}

// Bullet 4: a genuine no-match returns zero rows affected — not an
// "unsupported" error.
#[test]
fn no_match_reports_zero_not_error() {
    let rt = runtime();
    seed(&rt, "issue1363_nomatch");

    let update = exec(
        &rt,
        "UPDATE issue1363_nomatch DOCUMENTS SET score += 1 WHERE id = 99999999",
    );
    assert_eq!(update.affected_rows, 0, "no id match → zero, not error");

    let delete = exec(
        &rt,
        "DELETE FROM issue1363_nomatch WHERE body.category = 'missing'",
    );
    assert_eq!(delete.affected_rows, 0, "no body match → zero, not error");

    let select = exec(
        &rt,
        "SELECT name FROM issue1363_nomatch WHERE id = 99999999",
    );
    assert_eq!(select.result.records.len(), 0, "no id match → no rows");

    // Both documents survived the no-op statements.
    let all = exec(&rt, "SELECT name FROM issue1363_nomatch");
    assert_eq!(all.result.records.len(), 2);
}

// Guard: a body-promoted `id` field still wins over the logical-id alias,
// so collections that carry their own `id` are unaffected.
#[test]
fn body_id_field_wins_over_logical_id_alias() {
    let rt = runtime();
    exec(&rt, "CREATE DOCUMENT issue1363_own_id");
    exec(
        &rt,
        r#"INSERT INTO issue1363_own_id DOCUMENT VALUES ({"id":7,"name":"seven"})"#,
    );
    exec(
        &rt,
        r#"INSERT INTO issue1363_own_id DOCUMENT VALUES ({"id":8,"name":"eight"})"#,
    );

    let page = exec(&rt, "SELECT name FROM issue1363_own_id WHERE id = 7");
    assert_eq!(
        page.result.records.len(),
        1,
        "the body-promoted id field must win over the logical-id alias"
    );
    assert_eq!(text_field(&page.result.records[0], "name"), "seven");
}
