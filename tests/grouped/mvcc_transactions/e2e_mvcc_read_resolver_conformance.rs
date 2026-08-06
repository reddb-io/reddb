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

/// Run `sql` through the prepared seam: parse to a shape once, then execute
/// the bound expression together with its text, exactly as the gRPC and
/// stdio `execute_prepared` handlers do.
fn prepared_text(rt: &RedDBRuntime, sql: &str, column: &str) -> String {
    let expr = reddb::storage::query::modes::parse_multi(sql).expect("prepare statement");
    let result = rt
        .execute_prepared_query(sql, expr)
        .unwrap_or_else(|err| panic!("prepared {sql}: {err:?}"));
    assert_eq!(result.result.records.len(), 1, "prepared {sql}");
    match result.result.records[0].get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected prepared text {column}, got {other:?}"),
    }
}

#[test]
fn prepared_runtime_seam_matches_text_snapshot_during_concurrent_write() {
    let rt = rt();
    let sql = "SELECT status FROM mvcc_prepared_snapshot WHERE id = 1";

    set_current_connection_id(51404);
    exec(
        &rt,
        "CREATE TABLE mvcc_prepared_snapshot (id INT, status TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO mvcc_prepared_snapshot (id, status) VALUES (1, 'old')",
    );

    set_current_connection_id(51405);
    exec(&rt, "BEGIN");
    assert_eq!(single_text(&rt, sql, "status"), "old");

    set_current_connection_id(51406);
    exec(
        &rt,
        "UPDATE mvcc_prepared_snapshot SET status = 'new' WHERE id = 1",
    );

    set_current_connection_id(51405);
    let text_status = single_text(&rt, sql, "status");
    let prepared_status = prepared_text(&rt, sql, "status");

    assert_eq!(text_status, "old");
    assert_eq!(prepared_status, text_status);
    exec(&rt, "ROLLBACK");
    clear_current_connection_id();
}

/// Autocommit is the isolation level where the prepared and text paths are
/// easiest to drift apart: each statement mints its own snapshot, so a
/// prepared execution that skipped the frame would resolve visibility from
/// the frameless fallback rather than a real statement snapshot.
#[test]
fn prepared_autocommit_read_matches_text_after_concurrent_commit() {
    let rt = rt();
    let sql = "SELECT status FROM mvcc_prepared_autocommit WHERE id = 1";

    set_current_connection_id(51407);
    exec(
        &rt,
        "CREATE TABLE mvcc_prepared_autocommit (id INT, status TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO mvcc_prepared_autocommit (id, status) VALUES (1, 'old')",
    );

    // No BEGIN anywhere: both reads run in autocommit.
    assert_eq!(prepared_text(&rt, sql, "status"), "old");
    assert_eq!(single_text(&rt, sql, "status"), "old");

    set_current_connection_id(51408);
    exec(
        &rt,
        "UPDATE mvcc_prepared_autocommit SET status = 'new' WHERE id = 1",
    );

    set_current_connection_id(51407);
    assert_eq!(prepared_text(&rt, sql, "status"), "new");
    assert_eq!(
        prepared_text(&rt, sql, "status"),
        single_text(&rt, sql, "status")
    );
    clear_current_connection_id();
}

/// A prepared `AS OF` read must answer with the historical rows, not with
/// live rows presented as history. The frame resolves the floor from the
/// statement text that travels with the prepared shape.
#[test]
fn prepared_as_of_read_returns_historical_rows() {
    let rt = rt();
    exec(
        &rt,
        "CREATE TABLE mvcc_prepared_asof (id INT, status TEXT, marker TEXT)",
    );
    exec(&rt, "ALTER TABLE mvcc_prepared_asof SET VERSIONED = true");
    exec(
        &rt,
        "INSERT INTO mvcc_prepared_asof (id, status, marker) VALUES (1, 'old', 'stable')",
    );
    let before_update = commit(&rt, 51432, "before update");
    exec(
        &rt,
        "UPDATE mvcc_prepared_asof SET status = 'new' WHERE id = 1",
    );

    let live_sql = "SELECT status FROM mvcc_prepared_asof WHERE marker = 'stable'";
    let as_of_sql = format!(
        "SELECT status FROM mvcc_prepared_asof AS OF COMMIT '{before_update}' \
         WHERE marker = 'stable'"
    );

    assert_eq!(prepared_text(&rt, live_sql, "status"), "new");
    assert_eq!(prepared_text(&rt, &as_of_sql, "status"), "old");
    assert_eq!(
        prepared_text(&rt, &as_of_sql, "status"),
        single_text(&rt, &as_of_sql, "status")
    );
}

/// The expression-only entry cannot resolve `AS OF` — `TableQuery::as_of` is
/// consumed nowhere in the runtime or scan path — so it must refuse rather
/// than install a live snapshot and answer with current rows.
#[test]
fn expression_only_entry_refuses_as_of_instead_of_answering_live() {
    let rt = rt();
    exec(
        &rt,
        "CREATE TABLE mvcc_expr_asof (id INT, status TEXT, marker TEXT)",
    );
    exec(&rt, "ALTER TABLE mvcc_expr_asof SET VERSIONED = true");
    exec(
        &rt,
        "INSERT INTO mvcc_expr_asof (id, status, marker) VALUES (1, 'old', 'stable')",
    );
    let before_update = commit(&rt, 51433, "before update");
    exec(&rt, "UPDATE mvcc_expr_asof SET status = 'new' WHERE id = 1");

    let as_of_sql = format!(
        "SELECT status FROM mvcc_expr_asof AS OF COMMIT '{before_update}' \
         WHERE marker = 'stable'"
    );
    let expr = reddb::storage::query::modes::parse_multi(&as_of_sql).expect("parse AS OF");
    let err = rt
        .execute_query_expr(expr)
        .expect_err("AS OF without statement text must be refused, not answered live");
    assert!(
        err.to_string()
            .contains("AS OF cannot be resolved from a pre-parsed expression"),
        "expected a loud AS OF refusal, got: {err}"
    );
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
