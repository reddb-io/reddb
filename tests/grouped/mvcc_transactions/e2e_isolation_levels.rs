//! Transaction isolation level acceptance tests.
//!
//! reddb accepts PG isolation-level syntax and routes SERIALIZABLE to SSI.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn try_exec(rt: &RedDBRuntime, sql: &str) -> Result<(), String> {
    rt.execute_query(sql).map(|_| ()).map_err(|e| e.to_string())
}

#[test]
fn begin_accepts_read_committed() {
    let rt = rt();
    set_current_connection_id(9901);
    try_exec(&rt, "BEGIN TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .expect("READ COMMITTED should be accepted");
    try_exec(&rt, "COMMIT").expect("COMMIT should close the tx");
    clear_current_connection_id();
}

#[test]
fn begin_accepts_repeatable_read() {
    let rt = rt();
    set_current_connection_id(9902);
    try_exec(&rt, "BEGIN ISOLATION LEVEL REPEATABLE READ")
        .expect("REPEATABLE READ should be accepted");
    try_exec(&rt, "COMMIT").unwrap();
    clear_current_connection_id();
}

#[test]
fn begin_accepts_snapshot() {
    let rt = rt();
    set_current_connection_id(9903);
    try_exec(&rt, "BEGIN TRANSACTION ISOLATION LEVEL SNAPSHOT")
        .expect("SNAPSHOT should be accepted");
    try_exec(&rt, "COMMIT").unwrap();
    clear_current_connection_id();
}

#[test]
fn begin_accepts_serializable() {
    let rt = rt();
    set_current_connection_id(9904);
    try_exec(&rt, "BEGIN TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .expect("SERIALIZABLE should be accepted");
    try_exec(&rt, "COMMIT").expect("COMMIT should close the tx");
    clear_current_connection_id();
}

#[test]
fn start_transaction_isolation_level_is_accepted() {
    let rt = rt();
    set_current_connection_id(9905);
    try_exec(&rt, "START TRANSACTION ISOLATION LEVEL READ UNCOMMITTED")
        .expect("READ UNCOMMITTED should be accepted (upgraded to snapshot)");
    try_exec(&rt, "COMMIT").unwrap();
    clear_current_connection_id();
}

// ─────────────────────────────────────────────────────────────────────
// MVCC reads — issue #29.
//
// Verifies that the autocommit `SELECT` path consults the snapshot
// manager so concurrent uncommitted writes from another transaction
// stay invisible. Connections are simulated by toggling the per-thread
// connection-id, mirroring the pattern in e2e_cross_model_tx.rs.
// ─────────────────────────────────────────────────────────────────────

fn select_count(rt: &RedDBRuntime, sql: &str) -> usize {
    rt.execute_query(sql).expect("select").result.records.len()
}

fn text_cell(row: &reddb::storage::query::unified::UnifiedRecord, column: &str) -> String {
    match row.get(column) {
        Some(Value::Text(value)) => value.to_string(),
        other => panic!("expected text in {column}, got {other:?}"),
    }
}

fn int_cell(row: &reddb::storage::query::unified::UnifiedRecord, column: &str) -> i64 {
    match row.get(column) {
        Some(Value::Integer(value)) => *value,
        Some(Value::UnsignedInteger(value)) => *value as i64,
        other => panic!("expected integer in {column}, got {other:?}"),
    }
}

#[test]
fn begin_isolation_level_round_trips_to_transaction_status() {
    let rt = rt();

    for (offset, (sql, requested, effective)) in [
        (
            "BEGIN ISOLATION LEVEL READ UNCOMMITTED",
            "read_uncommitted",
            "snapshot_isolation",
        ),
        (
            "BEGIN ISOLATION LEVEL READ COMMITTED",
            "read_committed",
            "read_committed",
        ),
        (
            "BEGIN ISOLATION LEVEL REPEATABLE READ",
            "snapshot_isolation",
            "snapshot_isolation",
        ),
        (
            "BEGIN ISOLATION LEVEL SNAPSHOT",
            "snapshot_isolation",
            "snapshot_isolation",
        ),
        (
            "BEGIN ISOLATION LEVEL SERIALIZABLE",
            "serializable",
            "serializable",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        set_current_connection_id(9960 + offset as u64);
        try_exec(&rt, sql).expect("BEGIN with isolation level should be accepted");

        let status = rt
            .execute_query("SELECT isolation_level, effective_isolation_level FROM red.status")
            .expect("red.status should expose transaction isolation");
        assert_eq!(status.result.records.len(), 1, "one red.status row");
        let row = &status.result.records[0];
        assert_eq!(text_cell(row, "isolation_level"), requested, "{sql}");
        assert_eq!(
            text_cell(row, "effective_isolation_level"),
            effective,
            "{sql}"
        );

        try_exec(&rt, "COMMIT").expect("COMMIT should close the tx");
    }

    clear_current_connection_id();
}

#[test]
fn read_committed_refreshes_snapshot_per_statement_but_snapshot_does_not() {
    let rt = rt();
    set_current_connection_id(9970);
    try_exec(&rt, "CREATE TABLE rc_statement_snap (id INT, label TEXT)").unwrap();
    try_exec(
        &rt,
        "INSERT INTO rc_statement_snap (id, label) VALUES (1, 'base')",
    )
    .unwrap();

    try_exec(&rt, "BEGIN ISOLATION LEVEL READ COMMITTED").unwrap();
    let rc_first = select_count(&rt, "SELECT * FROM rc_statement_snap");
    assert_eq!(rc_first, 1, "RC first statement sees initial row");

    set_current_connection_id(9971);
    try_exec(&rt, "BEGIN").unwrap();
    try_exec(
        &rt,
        "INSERT INTO rc_statement_snap (id, label) VALUES (2, 'concurrent')",
    )
    .unwrap();
    try_exec(&rt, "COMMIT").unwrap();

    set_current_connection_id(9970);
    let rc_second = select_count(&rt, "SELECT * FROM rc_statement_snap");
    assert_eq!(rc_second, 2, "RC next statement sees concurrent commit");
    try_exec(&rt, "COMMIT").unwrap();

    set_current_connection_id(9972);
    try_exec(
        &rt,
        "CREATE TABLE snapshot_statement_snap (id INT, label TEXT)",
    )
    .unwrap();
    try_exec(
        &rt,
        "INSERT INTO snapshot_statement_snap (id, label) VALUES (1, 'base')",
    )
    .unwrap();
    try_exec(&rt, "BEGIN ISOLATION LEVEL SNAPSHOT").unwrap();
    let snapshot_first = select_count(&rt, "SELECT * FROM snapshot_statement_snap");
    assert_eq!(
        snapshot_first, 1,
        "SNAPSHOT first statement sees initial row"
    );

    set_current_connection_id(9973);
    try_exec(&rt, "BEGIN").unwrap();
    try_exec(
        &rt,
        "INSERT INTO snapshot_statement_snap (id, label) VALUES (2, 'concurrent')",
    )
    .unwrap();
    try_exec(&rt, "COMMIT").unwrap();

    set_current_connection_id(9972);
    let snapshot_second = select_count(&rt, "SELECT * FROM snapshot_statement_snap");
    assert_eq!(
        snapshot_second, 1,
        "SNAPSHOT keeps the transaction-begin snapshot"
    );
    try_exec(&rt, "COMMIT").unwrap();
    clear_current_connection_id();
}

#[test]
fn writer_sees_own_uncommitted_row() {
    // Read-your-own-writes inside a single transaction. The writer's
    // own xid lands in `own_xids` so its uncommitted row is visible
    // to subsequent SELECTs on the same connection.
    let rt = rt();
    set_current_connection_id(9910);
    try_exec(&rt, "CREATE TABLE ryo (id INT, val TEXT)").unwrap();

    try_exec(&rt, "BEGIN").unwrap();
    try_exec(&rt, "INSERT INTO ryo (id, val) VALUES (1, 'self')").unwrap();
    let own = select_count(&rt, "SELECT * FROM ryo WHERE id = 1");
    assert_eq!(own, 1, "writer must see its own uncommitted row");
    try_exec(&rt, "ROLLBACK").unwrap();

    let after = select_count(&rt, "SELECT * FROM ryo WHERE id = 1");
    assert_eq!(after, 0, "rollback must hide the writer's own row");
    clear_current_connection_id();
}

#[test]
fn read_committed_keeps_own_writes_visible_across_statement_snapshots() {
    let rt = rt();
    set_current_connection_id(9980);
    try_exec(&rt, "CREATE TABLE rc_own_writes (id INT, label TEXT)").unwrap();
    try_exec(&rt, "BEGIN ISOLATION LEVEL READ COMMITTED").unwrap();
    try_exec(
        &rt,
        "INSERT INTO rc_own_writes (id, label) VALUES (1, 'parent')",
    )
    .unwrap();
    let first = rt
        .execute_query("SELECT id FROM rc_own_writes")
        .expect("select own write");
    assert_eq!(first.result.records.len(), 1);
    assert_eq!(int_cell(&first.result.records[0], "id"), 1);

    set_current_connection_id(9981);
    try_exec(&rt, "BEGIN").unwrap();
    try_exec(
        &rt,
        "INSERT INTO rc_own_writes (id, label) VALUES (2, 'other')",
    )
    .unwrap();
    try_exec(&rt, "COMMIT").unwrap();

    set_current_connection_id(9980);
    let second = rt
        .execute_query("SELECT id FROM rc_own_writes")
        .expect("select own and concurrent writes");
    let mut ids: Vec<i64> = second
        .result
        .records
        .iter()
        .map(|row| int_cell(row, "id"))
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 2]);
    try_exec(&rt, "ROLLBACK").unwrap();
    clear_current_connection_id();
}

#[test]
fn other_connection_does_not_see_uncommitted_row() {
    // Cross-connection visibility — the load-bearing #29 assertion.
    // Conn A writes inside a tx; Conn B autocommit SELECT must not
    // see the row; after Conn A commits, Conn B sees it.
    let rt = rt();

    set_current_connection_id(9920);
    try_exec(&rt, "CREATE TABLE iso_check (id INT, val TEXT)").unwrap();

    // Conn A: open tx, write, do not commit.
    try_exec(&rt, "BEGIN").unwrap();
    try_exec(
        &rt,
        "INSERT INTO iso_check (id, val) VALUES (42, 'pending')",
    )
    .unwrap();
    let writer_view = select_count(&rt, "SELECT * FROM iso_check WHERE id = 42");
    assert_eq!(writer_view, 1, "writer sees own uncommitted row");

    // Conn B: switch connection, autocommit SELECT — must not see it.
    set_current_connection_id(9921);
    let outsider_view = select_count(&rt, "SELECT * FROM iso_check WHERE id = 42");
    assert_eq!(
        outsider_view, 0,
        "other connection must not see pre-commit row"
    );

    // Conn A: commit. Now Conn B sees it.
    set_current_connection_id(9920);
    try_exec(&rt, "COMMIT").unwrap();

    set_current_connection_id(9921);
    let after_commit = select_count(&rt, "SELECT * FROM iso_check WHERE id = 42");
    assert_eq!(after_commit, 1, "post-commit row must be visible");

    clear_current_connection_id();
}

#[test]
fn autocommit_insert_stamps_xmin_greater_than_zero() {
    // #30: every freshly-written entity must carry xmin > 0. Pre-fix
    // autocommit INSERTs left xmin at 0 ("pre-MVCC, always visible").
    // Post-fix: a fresh xid is allocated from the coordinator and
    // committed up-front so the row's xmin is meaningful.
    let rt = rt();
    rt.execute_query("CREATE TABLE xmin_check (id INT, name TEXT)")
        .unwrap();
    rt.execute_query("INSERT INTO xmin_check (id, name) VALUES (1, 'fresh')")
        .unwrap();

    let store = rt.db().store();
    let mgr = store.get_collection("xmin_check").expect("collection");
    let entities = mgr.query_all(|_| true);
    assert_eq!(entities.len(), 1, "one row expected");
    assert!(
        entities[0].xmin > 0,
        "autocommit INSERT must stamp xmin > 0, got {}",
        entities[0].xmin
    );
}

#[test]
fn snapshot_isolation_blocks_read_skew() {
    // #31 read-skew assertion: two reads inside the same SNAPSHOT tx
    // must return the same value even when a concurrent autocommit
    // write commits between them. The tx's snapshot was captured at
    // BEGIN; later commits stay invisible to it.
    let rt = rt();
    set_current_connection_id(9940);
    try_exec(&rt, "CREATE TABLE skew_check (id INT, v INT)").unwrap();
    try_exec(&rt, "INSERT INTO skew_check (id, v) VALUES (1, 10)").unwrap();
    let inserted = rt
        .execute_query("SELECT rid FROM skew_check WHERE id = 1")
        .unwrap();
    let rid = match inserted.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(id)) => *id,
        Some(Value::Integer(id)) => *id as u64,
        other => panic!("expected rid, got {other:?}"),
    };

    // Conn A: BEGIN — captures snapshot, sees v=10.
    try_exec(&rt, "BEGIN").unwrap();
    let res1 = rt
        .execute_query("SELECT v FROM skew_check WHERE id = 1")
        .unwrap();
    assert_eq!(res1.result.records.len(), 1, "first read sees row");
    assert_eq!(res1.result.records[0].get("v"), Some(&Value::Integer(10)));

    // Conn B: autocommit UPDATE, commits.
    set_current_connection_id(9941);
    try_exec(&rt, "UPDATE skew_check SET v = 99 WHERE id = 1").unwrap();

    // Conn A: second read in same tx — snapshot pinned at BEGIN, so
    // the new value is invisible. The row count must stay stable;
    // the pinned-snapshot guarantee is exactly that the tx never
    // sees writes committed after it started.
    set_current_connection_id(9940);
    let res2 = rt
        .execute_query("SELECT v FROM skew_check WHERE id = 1")
        .unwrap();
    assert_eq!(
        res2.result.records.len(),
        res1.result.records.len(),
        "snapshot tx must see same row count both reads"
    );
    assert_eq!(
        res2.result.records[0].get("v"),
        Some(&Value::Integer(10)),
        "snapshot tx must keep reading the pre-update value"
    );
    try_exec(&rt, "COMMIT").unwrap();

    let res3 = rt
        .execute_query(&format!("SELECT v FROM skew_check WHERE rid = {rid}"))
        .unwrap();
    assert_eq!(res3.result.records.len(), 1, "new snapshot sees row");
    assert_eq!(
        res3.result.records[0].get("v"),
        Some(&Value::Integer(99)),
        "new snapshot must see the updated value"
    );
    clear_current_connection_id();
}

#[test]
fn transaction_update_write_set_rolls_back_cleanly() {
    let rt = rt();

    set_current_connection_id(9950);
    try_exec(&rt, "CREATE TABLE tx_update_ws (id INT, v INT)").unwrap();
    try_exec(&rt, "INSERT INTO tx_update_ws (id, v) VALUES (1, 10)").unwrap();
    let inserted = rt
        .execute_query("SELECT rid FROM tx_update_ws WHERE id = 1")
        .unwrap();
    let rid = match inserted.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(id)) => *id,
        Some(Value::Integer(id)) => *id as u64,
        other => panic!("expected rid, got {other:?}"),
    };

    try_exec(&rt, "BEGIN").unwrap();
    try_exec(
        &rt,
        &format!("UPDATE tx_update_ws SET v = 20 WHERE rid = {rid}"),
    )
    .unwrap();
    let writer = rt
        .execute_query(&format!("SELECT v FROM tx_update_ws WHERE rid = {rid}"))
        .unwrap();
    assert_eq!(writer.result.records[0].get("v"), Some(&Value::Integer(20)));

    set_current_connection_id(9951);
    let outsider = rt
        .execute_query(&format!("SELECT v FROM tx_update_ws WHERE rid = {rid}"))
        .unwrap();
    assert_eq!(
        outsider.result.records[0].get("v"),
        Some(&Value::Integer(10)),
        "other connection must not see uncommitted UPDATE"
    );

    set_current_connection_id(9950);
    try_exec(&rt, "ROLLBACK").unwrap();
    let after_rollback = rt
        .execute_query(&format!("SELECT v FROM tx_update_ws WHERE rid = {rid}"))
        .unwrap();
    assert_eq!(
        after_rollback.result.records[0].get("v"),
        Some(&Value::Integer(10)),
        "ROLLBACK must restore the committed pre-update value"
    );

    try_exec(&rt, "BEGIN").unwrap();
    try_exec(
        &rt,
        &format!("UPDATE tx_update_ws SET v = 30 WHERE rid = {rid}"),
    )
    .unwrap();
    try_exec(&rt, "COMMIT").unwrap();
    let after_commit = rt
        .execute_query(&format!("SELECT v FROM tx_update_ws WHERE rid = {rid}"))
        .unwrap();
    assert_eq!(
        after_commit.result.records[0].get("v"),
        Some(&Value::Integer(30)),
        "COMMIT must publish the transaction-local UPDATE"
    );

    clear_current_connection_id();
}

#[test]
fn rolled_back_writer_row_stays_invisible_to_other_connection() {
    // ROLLBACK must hide the row from every other connection — same
    // mechanism as commit-aware visibility, but exercises the
    // is_aborted gate in the visibility predicate.
    let rt = rt();
    set_current_connection_id(9930);
    try_exec(&rt, "CREATE TABLE rollback_check (id INT)").unwrap();

    try_exec(&rt, "BEGIN").unwrap();
    try_exec(&rt, "INSERT INTO rollback_check (id) VALUES (7)").unwrap();
    try_exec(&rt, "ROLLBACK").unwrap();

    set_current_connection_id(9931);
    let view = select_count(&rt, "SELECT * FROM rollback_check WHERE id = 7");
    assert_eq!(view, 0, "rolled-back row stays invisible cross-connection");
    clear_current_connection_id();
}
