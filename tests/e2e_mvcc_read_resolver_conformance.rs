//! MVCC read resolver conformance across public table-row paths.

use reddb::application::{Author, CreateCommitInput, VcsUseCases};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn ids(rt: &RedDBRuntime, sql: &str) -> Vec<i64> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    let mut ids: Vec<i64> = result
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("id") {
            Some(Value::Integer(value)) => Some(*value),
            Some(Value::UnsignedInteger(value)) => Some(*value as i64),
            _ => None,
        })
        .collect();
    ids.sort_unstable();
    ids
}

fn single_i64(rt: &RedDBRuntime, sql: &str, column: &str) -> i64 {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert_eq!(result.result.records.len(), 1, "{sql}");
    match result.result.records[0].get(column) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => *value as i64,
        other => panic!("expected integer {column}, got {other:?}"),
    }
}

fn single_u64(rt: &RedDBRuntime, sql: &str, column: &str) -> u64 {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert_eq!(result.result.records.len(), 1, "{sql}");
    match result.result.records[0].get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) => *value as u64,
        other => panic!("expected integer {column}, got {other:?}"),
    }
}

fn single_text(rt: &RedDBRuntime, sql: &str, column: &str) -> String {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert_eq!(result.result.records.len(), 1, "{sql}");
    match result.result.records[0].get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text {column}, got {other:?}"),
    }
}

fn row_count(rt: &RedDBRuntime, sql: &str) -> usize {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .result
        .records
        .len()
}

fn commit(rt: &RedDBRuntime, conn: u64, message: &str) -> String {
    VcsUseCases::new(rt)
        .commit(CreateCommitInput {
            connection_id: conn,
            message: message.to_string(),
            author: Author {
                name: "test".to_string(),
                email: "test@reddb.io".to_string(),
            },
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .expect("commit")
        .hash
}

#[test]
fn snapshot_table_scan_indexed_read_and_logical_lookup_agree() {
    let rt = rt();
    set_current_connection_id(51401);
    exec(
        &rt,
        "CREATE TABLE mvcc_resolver_read (id INT, status TEXT, marker TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO mvcc_resolver_read (id, status, marker) VALUES (1, 'old', 'stable')",
    );
    exec(
        &rt,
        "CREATE INDEX idx_mvcc_resolver_read_status ON mvcc_resolver_read (status) USING HASH",
    );
    let rid = single_u64(
        &rt,
        "SELECT rid FROM mvcc_resolver_read WHERE marker = 'stable'",
        "rid",
    );

    set_current_connection_id(51402);
    exec(&rt, "BEGIN");
    assert_eq!(
        ids(
            &rt,
            "SELECT id FROM mvcc_resolver_read WHERE status = 'old'"
        ),
        vec![1]
    );

    set_current_connection_id(51403);
    exec(
        &rt,
        "UPDATE mvcc_resolver_read SET status = 'new' WHERE id = 1",
    );

    set_current_connection_id(51402);
    let indexed_old = ids(
        &rt,
        "SELECT id FROM mvcc_resolver_read WHERE status = 'old'",
    );
    let scanned_old = ids(
        &rt,
        "SELECT id FROM mvcc_resolver_read WHERE marker = 'stable'",
    );
    let logical_status = single_text(
        &rt,
        &format!("SELECT status FROM mvcc_resolver_read WHERE rid = {rid} OFFSET 0"),
        "status",
    );

    assert_eq!(indexed_old, scanned_old);
    assert_eq!(indexed_old, vec![1]);
    assert_eq!(logical_status, "old");
    assert_eq!(
        ids(
            &rt,
            "SELECT id FROM mvcc_resolver_read WHERE status = 'new'"
        ),
        Vec::<i64>::new()
    );
    exec(&rt, "ROLLBACK");
    clear_current_connection_id();
}

#[test]
fn snapshot_select_update_and_delete_visibility_agree() {
    let rt = rt();
    set_current_connection_id(51411);
    exec(
        &rt,
        "CREATE TABLE mvcc_resolver_update (id INT, v INT, touched INT)",
    );
    exec(
        &rt,
        "INSERT INTO mvcc_resolver_update (id, v, touched) VALUES (1, 10, 0)",
    );
    exec(
        &rt,
        "CREATE INDEX idx_mvcc_resolver_update_id ON mvcc_resolver_update (id) USING HASH",
    );

    set_current_connection_id(51412);
    exec(&rt, "BEGIN");
    assert_eq!(
        single_i64(&rt, "SELECT v FROM mvcc_resolver_update WHERE id = 1", "v"),
        10
    );

    set_current_connection_id(51413);
    exec(&rt, "UPDATE mvcc_resolver_update SET v = 99 WHERE id = 1");

    set_current_connection_id(51412);
    let updated = rt
        .execute_query("UPDATE mvcc_resolver_update SET touched = 1 WHERE id = 1")
        .expect("snapshot update");
    assert_eq!(updated.affected_rows, 1);
    assert_eq!(
        single_i64(&rt, "SELECT v FROM mvcc_resolver_update WHERE id = 1", "v"),
        10
    );
    exec(&rt, "ROLLBACK");

    set_current_connection_id(51421);
    exec(&rt, "CREATE TABLE mvcc_resolver_delete (id INT, v INT)");
    exec(
        &rt,
        "INSERT INTO mvcc_resolver_delete (id, v) VALUES (1, 10)",
    );
    exec(
        &rt,
        "CREATE INDEX idx_mvcc_resolver_delete_id ON mvcc_resolver_delete (id) USING HASH",
    );

    set_current_connection_id(51422);
    exec(&rt, "BEGIN");
    assert_eq!(
        single_i64(&rt, "SELECT v FROM mvcc_resolver_delete WHERE id = 1", "v"),
        10
    );

    set_current_connection_id(51423);
    exec(&rt, "UPDATE mvcc_resolver_delete SET v = 99 WHERE id = 1");

    set_current_connection_id(51422);
    let deleted = rt
        .execute_query("DELETE FROM mvcc_resolver_delete WHERE id = 1")
        .expect("snapshot delete");
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(
        row_count(&rt, "SELECT v FROM mvcc_resolver_delete WHERE id = 1"),
        0
    );
    exec(&rt, "ROLLBACK");
    clear_current_connection_id();
}

#[test]
fn as_of_table_read_uses_the_same_snapshot_visibility_contract() {
    let rt = rt();
    exec(
        &rt,
        "CREATE TABLE mvcc_resolver_asof (id INT, status TEXT, marker TEXT)",
    );
    exec(&rt, "ALTER TABLE mvcc_resolver_asof SET VERSIONED = true");
    exec(
        &rt,
        "INSERT INTO mvcc_resolver_asof (id, status, marker) VALUES (1, 'old', 'stable')",
    );
    let before_update = commit(&rt, 51431, "before update");

    exec(
        &rt,
        "UPDATE mvcc_resolver_asof SET status = 'new' WHERE id = 1",
    );

    assert_eq!(
        single_text(
            &rt,
            "SELECT status FROM mvcc_resolver_asof WHERE marker = 'stable'",
            "status",
        ),
        "new"
    );
    let as_of_sql = format!(
        "SELECT status FROM mvcc_resolver_asof AS OF COMMIT '{before_update}' WHERE marker = 'stable'"
    );
    assert_eq!(single_text(&rt, &as_of_sql, "status"), "old");
}
