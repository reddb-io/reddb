//! MVCC-correct indexed table lookups.

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
        .filter_map(
            |record| match record.get("id").or_else(|| record.get("c0")) {
                Some(Value::Integer(id)) => Some(*id),
                Some(Value::UnsignedInteger(id)) => Some(*id as i64),
                _ => None,
            },
        )
        .collect();
    ids.sort_unstable();
    ids
}

#[test]
fn historical_snapshot_indexed_lookup_keeps_pre_update_index_value() {
    let rt = rt();
    set_current_connection_id(44201);
    exec(
        &rt,
        "CREATE TABLE mvcc_idx_update (id INT, status TEXT, marker TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO mvcc_idx_update (id, status, marker) VALUES (1, 'old', 'stable')",
    );
    exec(
        &rt,
        "CREATE INDEX idx_mvcc_update_status ON mvcc_idx_update (status) USING HASH",
    );

    set_current_connection_id(44202);
    exec(&rt, "BEGIN");
    assert_eq!(
        ids(&rt, "SELECT id FROM mvcc_idx_update WHERE status = 'old'"),
        vec![1]
    );

    set_current_connection_id(44203);
    exec(
        &rt,
        "UPDATE mvcc_idx_update SET status = 'new' WHERE id = 1",
    );

    set_current_connection_id(44202);
    let indexed_old = ids(&rt, "SELECT id FROM mvcc_idx_update WHERE status = 'old'");
    let scan_old = ids(
        &rt,
        "SELECT id FROM mvcc_idx_update WHERE marker = 'stable'",
    );
    assert_eq!(indexed_old, scan_old);
    assert_eq!(indexed_old, vec![1]);
    assert_eq!(
        ids(&rt, "SELECT id FROM mvcc_idx_update WHERE status = 'new'"),
        Vec::<i64>::new()
    );
    exec(&rt, "ROLLBACK");

    set_current_connection_id(44204);
    assert_eq!(
        ids(&rt, "SELECT id FROM mvcc_idx_update WHERE status = 'old'"),
        Vec::<i64>::new()
    );
    assert_eq!(
        ids(&rt, "SELECT id FROM mvcc_idx_update WHERE status = 'new'"),
        vec![1]
    );
    clear_current_connection_id();
}

#[test]
fn historical_snapshot_indexed_lookup_keeps_pre_delete_row() {
    let rt = rt();
    set_current_connection_id(44211);
    exec(
        &rt,
        "CREATE TABLE mvcc_idx_delete (id INT, status TEXT, marker TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO mvcc_idx_delete (id, status, marker) VALUES (1, 'gone', 'stable')",
    );
    exec(
        &rt,
        "CREATE INDEX idx_mvcc_delete_status ON mvcc_idx_delete (status) USING HASH",
    );

    set_current_connection_id(44212);
    exec(&rt, "BEGIN");
    assert_eq!(
        ids(&rt, "SELECT id FROM mvcc_idx_delete WHERE status = 'gone'"),
        vec![1]
    );

    set_current_connection_id(44213);
    exec(&rt, "DELETE FROM mvcc_idx_delete WHERE id = 1");

    set_current_connection_id(44212);
    let indexed_old = ids(&rt, "SELECT id FROM mvcc_idx_delete WHERE status = 'gone'");
    let scan_old = ids(
        &rt,
        "SELECT id FROM mvcc_idx_delete WHERE marker = 'stable'",
    );
    assert_eq!(indexed_old, scan_old);
    assert_eq!(indexed_old, vec![1]);
    exec(&rt, "ROLLBACK");

    set_current_connection_id(44214);
    assert_eq!(
        ids(&rt, "SELECT id FROM mvcc_idx_delete WHERE status = 'gone'"),
        Vec::<i64>::new()
    );
    clear_current_connection_id();
}
