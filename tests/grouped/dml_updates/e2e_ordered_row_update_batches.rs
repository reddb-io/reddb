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

fn int_field(record: &UnifiedRecord, field: &str) -> i64 {
    match record.get(field) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => *value as i64,
        other => panic!("expected {field} integer field, got {other:?} in {record:?}"),
    }
}

fn selected_ids(rt: &RedDBRuntime, table: &str) -> Vec<i64> {
    exec(
        rt,
        &format!("SELECT id FROM {table} WHERE touched = 1 ORDER BY id ASC"),
    )
    .result
    .records
    .iter()
    .map(|record| int_field(record, "id"))
    .collect()
}

fn err_string(rt: &RedDBRuntime, sql: &str) -> String {
    rt.execute_query(sql)
        .expect_err("query should fail")
        .to_string()
}

#[test]
fn update_order_by_desc_limit_updates_expected_batch() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE ordered_updates (id INT, rank INT, touched INT)",
    );
    exec(
        &rt,
        "INSERT INTO ordered_updates (id, rank, touched) VALUES \
         (1, 10, 0), (2, 30, 0), (3, 20, 0), (4, 40, 0)",
    );

    let updated = exec(
        &rt,
        "UPDATE ordered_updates SET touched = 1 ORDER BY rank DESC LIMIT 2",
    );

    assert_eq!(updated.affected_rows, 2);
    assert_eq!(selected_ids(&rt, "ordered_updates"), vec![2, 4]);
}

#[test]
fn update_order_by_requires_limit_and_top_level_fields() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE ordered_update_rejections (id INT, rank INT, touched INT)",
    );
    exec(
        &rt,
        "INSERT INTO ordered_update_rejections (id, rank, touched) VALUES (1, 10, 0)",
    );

    let without_limit = err_string(
        &rt,
        "UPDATE ordered_update_rejections SET touched = 1 ORDER BY rank",
    );
    assert!(without_limit.contains("ORDER BY requires LIMIT"));

    let expression = err_string(
        &rt,
        "UPDATE ordered_update_rejections SET touched = 1 ORDER BY rank + 1 LIMIT 1",
    );
    assert!(expression.contains("top-level fields"));

    let nested = err_string(
        &rt,
        "UPDATE ordered_update_rejections SET touched = 1 ORDER BY payload.rank LIMIT 1",
    );
    assert!(nested.contains("top-level fields"));
}

#[test]
fn update_order_by_limit_breaks_ties_by_implicit_rid_asc() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE ordered_update_ties (id INT, rank INT, touched INT)",
    );
    exec(
        &rt,
        "INSERT INTO ordered_update_ties (id, rank, touched) VALUES \
         (30, 7, 0), (10, 7, 0), (20, 7, 0)",
    );

    let updated = exec(
        &rt,
        "UPDATE ordered_update_ties SET touched = 1 ORDER BY rank ASC LIMIT 2",
    );

    assert_eq!(updated.affected_rows, 2);
    assert_eq!(selected_ids(&rt, "ordered_update_ties"), vec![10, 30]);
}
