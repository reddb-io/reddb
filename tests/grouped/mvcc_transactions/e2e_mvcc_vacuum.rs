//! Manual VACUUM / MVCC history reclamation.

use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::storage::EntityKind;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn vacuum_message(rt: &RedDBRuntime, sql: &str) -> String {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    match result.result.records[0].get("message") {
        Some(Value::Text(message)) => message.to_string(),
        other => panic!("expected VACUUM message, got {other:?}"),
    }
}

fn selected_i64(rt: &RedDBRuntime, sql: &str, column: &str) -> Vec<i64> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    result
        .result
        .records
        .iter()
        .filter_map(
            |record| match record.get(column).or_else(|| record.get("c0")) {
                Some(Value::Integer(value)) => Some(*value),
                Some(Value::UnsignedInteger(value)) => Some(*value as i64),
                _ => None,
            },
        )
        .collect()
}

fn physical_table_row_count(rt: &RedDBRuntime, table: &str) -> usize {
    rt.db()
        .store()
        .get_collection(table)
        .expect("collection")
        .query_all(|entity| matches!(entity.kind, EntityKind::TableRow { .. }))
        .len()
}

fn tombstoned_table_row_count(rt: &RedDBRuntime, table: &str) -> usize {
    rt.db()
        .store()
        .get_collection(table)
        .expect("collection")
        .query_all(|entity| matches!(entity.kind, EntityKind::TableRow { .. }) && entity.xmax != 0)
        .len()
}

#[test]
fn vacuum_preserves_update_history_until_old_snapshot_releases() {
    let rt = rt();
    set_current_connection_id(44301);
    exec(&rt, "CREATE TABLE mvcc_vacuum_update (id INT, value INT)");
    exec(
        &rt,
        "INSERT INTO mvcc_vacuum_update (id, value) VALUES (1, 10)",
    );

    set_current_connection_id(44302);
    exec(&rt, "BEGIN");
    assert_eq!(
        selected_i64(
            &rt,
            "SELECT value FROM mvcc_vacuum_update WHERE id = 1",
            "value"
        ),
        vec![10]
    );

    set_current_connection_id(44303);
    exec(&rt, "UPDATE mvcc_vacuum_update SET value = 20 WHERE id = 1");
    assert_eq!(physical_table_row_count(&rt, "mvcc_vacuum_update"), 2);
    assert_eq!(tombstoned_table_row_count(&rt, "mvcc_vacuum_update"), 1);

    let retained = vacuum_message(&rt, "VACUUM mvcc_vacuum_update");
    assert!(retained.contains("scanned_versions=1"), "{retained}");
    assert!(retained.contains("retained_versions=1"), "{retained}");
    assert!(retained.contains("reclaimed_versions=0"), "{retained}");
    assert_eq!(physical_table_row_count(&rt, "mvcc_vacuum_update"), 2);

    set_current_connection_id(44302);
    assert_eq!(
        selected_i64(
            &rt,
            "SELECT value FROM mvcc_vacuum_update WHERE id = 1",
            "value"
        ),
        vec![10],
        "old snapshot must still read the pre-update version"
    );
    exec(&rt, "ROLLBACK");

    set_current_connection_id(44304);
    let reclaimed = vacuum_message(&rt, "VACUUM mvcc_vacuum_update");
    assert!(reclaimed.contains("scanned_versions=1"), "{reclaimed}");
    assert!(reclaimed.contains("reclaimed_versions=1"), "{reclaimed}");
    assert!(
        reclaimed.contains("reclaimed_history_versions=1"),
        "{reclaimed}"
    );
    assert_eq!(physical_table_row_count(&rt, "mvcc_vacuum_update"), 1);
    assert_eq!(
        selected_i64(
            &rt,
            "SELECT value FROM mvcc_vacuum_update WHERE id = 1",
            "value"
        ),
        vec![20]
    );
    clear_current_connection_id();
}

#[test]
fn vacuum_preserves_delete_tombstone_until_old_snapshot_releases() {
    let rt = rt();
    set_current_connection_id(44311);
    exec(&rt, "CREATE TABLE mvcc_vacuum_delete (id INT, value INT)");
    exec(
        &rt,
        "INSERT INTO mvcc_vacuum_delete (id, value) VALUES (1, 10)",
    );

    set_current_connection_id(44312);
    exec(&rt, "BEGIN");
    assert_eq!(
        selected_i64(&rt, "SELECT id FROM mvcc_vacuum_delete WHERE id = 1", "id"),
        vec![1]
    );

    set_current_connection_id(44313);
    exec(&rt, "DELETE FROM mvcc_vacuum_delete WHERE id = 1");
    assert_eq!(physical_table_row_count(&rt, "mvcc_vacuum_delete"), 1);
    assert_eq!(tombstoned_table_row_count(&rt, "mvcc_vacuum_delete"), 1);

    let retained = vacuum_message(&rt, "VACUUM mvcc_vacuum_delete");
    assert!(retained.contains("retained_versions=1"), "{retained}");
    assert!(retained.contains("retained_tombstones=1"), "{retained}");
    assert_eq!(physical_table_row_count(&rt, "mvcc_vacuum_delete"), 1);

    set_current_connection_id(44312);
    assert_eq!(
        selected_i64(&rt, "SELECT id FROM mvcc_vacuum_delete WHERE id = 1", "id"),
        vec![1],
        "old snapshot must still read the deleted row"
    );
    exec(&rt, "ROLLBACK");

    set_current_connection_id(44314);
    let reclaimed = vacuum_message(&rt, "VACUUM mvcc_vacuum_delete");
    assert!(reclaimed.contains("reclaimed_versions=1"), "{reclaimed}");
    assert!(reclaimed.contains("reclaimed_tombstones=1"), "{reclaimed}");
    assert_eq!(physical_table_row_count(&rt, "mvcc_vacuum_delete"), 0);
    assert_eq!(
        selected_i64(&rt, "SELECT id FROM mvcc_vacuum_delete WHERE id = 1", "id"),
        Vec::<i64>::new()
    );
    clear_current_connection_id();
}
