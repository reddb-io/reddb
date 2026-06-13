//! Atomic TxCommitBatch WAL recovery for autocommit table mutations.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::path::{Path, PathBuf};

use reddb::api::DurabilityMode;
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::storage::wal::{WalReader, WalRecord};
use reddb::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

fn db_open(db: &support::TempDbFile) -> RedDBRuntime {
    RedDBRuntime::with_options(
        RedDBOptions::persistent(db.path())
            .with_durability_mode(DurabilityMode::WalDurableGrouped)
            .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
            .expect("primary-replica operational profile"),
    )
    .expect("persistent runtime")
}

fn db_wal_path(db: &support::TempDbFile) -> PathBuf {
    reddb_file::unified_wal_path(db.path())
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn red_entity_id(rt: &RedDBRuntime, table: &str, id: i64) -> u64 {
    let result = rt
        .execute_query(&format!(
            "SELECT red_entity_id FROM {table} WHERE id = {id}"
        ))
        .expect("select red_entity_id");
    match result.result.records[0].get("red_entity_id") {
        Some(Value::UnsignedInteger(id)) => *id,
        Some(Value::Integer(id)) => *id as u64,
        other => panic!("expected red_entity_id, got {other:?}"),
    }
}

fn label_for(rt: &RedDBRuntime, table: &str, red_entity_id: u64) -> Option<String> {
    let result = rt
        .execute_query(&format!("SELECT red_entity_id, label FROM {table}"))
        .expect("select label");
    result.result.records.iter().find_map(|record| {
        // Full-scan SELECT projects `red_entity_id` under the `rid` key
        // (the projection-only path keeps the `red_entity_id` alias).
        let row_id = match record.get("red_entity_id").or_else(|| record.get("rid")) {
            Some(Value::UnsignedInteger(id)) => *id,
            Some(Value::Integer(id)) => *id as u64,
            _ => return None,
        };
        if row_id != red_entity_id {
            return None;
        }
        match record.get("label").or_else(|| record.get("c0")) {
            Some(Value::Text(value)) => Some(value.to_string()),
            _ => None,
        }
    })
}

fn has_user_id(rt: &RedDBRuntime, table: &str, id: i64) -> bool {
    let result = rt
        .execute_query(&format!("SELECT id FROM {table}"))
        .expect("select ids");
    result.result.records.iter().any(|record| {
        matches!(
            record.get("id").or_else(|| record.get("c1")),
            Some(Value::Integer(value)) if *value == id
        )
    })
}

fn truncate_wal_tail(path: &Path, bytes: u64) {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open wal");
    let len = file.metadata().expect("wal metadata").len();
    file.set_len(len - bytes).expect("truncate wal");
    file.sync_all().expect("sync truncated wal");
}

#[test]
fn autocommit_insert_update_delete_recover_from_commit_batches() {
    let db = support::temp_db_file("txcommitbatch-wal-recover");

    let (keep, deleted) = {
        let rt = db_open(&db);
        set_current_connection_id(44001);
        exec(&rt, "CREATE TABLE txb_recover (id INT, label TEXT)");
        exec(
            &rt,
            "INSERT INTO txb_recover (id, label) VALUES (1, 'inserted'), (2, 'delete-me')",
        );
        let keep = red_entity_id(&rt, "txb_recover", 1);
        let deleted = red_entity_id(&rt, "txb_recover", 2);
        exec(
            &rt,
            &format!("UPDATE txb_recover SET label = 'updated' WHERE red_entity_id = {keep}"),
        );
        exec(
            &rt,
            &format!("DELETE FROM txb_recover WHERE red_entity_id = {deleted}"),
        );
        clear_current_connection_id();
        (keep, deleted)
    };

    {
        let rt = db_open(&db);
        set_current_connection_id(44002);
        assert_eq!(
            label_for(&rt, "txb_recover", keep).as_deref(),
            Some("updated")
        );
        assert_eq!(label_for(&rt, "txb_recover", deleted), None);

        let store = rt.db().store();
        let manager = store
            .get_collection("txb_recover")
            .expect("txb_recover collection");
        let versions =
            manager.query_all(|entity| entity.logical_id() == reddb::storage::EntityId::new(keep));
        assert_eq!(
            versions.len(),
            2,
            "UPDATE replay should keep old and new versions"
        );
        assert!(
            versions.iter().any(|entity| entity.xmax != 0),
            "old UPDATE version should replay as tombstoned history"
        );
        clear_current_connection_id();
    }

    {
        let rt = db_open(&db);
        set_current_connection_id(44003);
        assert_eq!(
            label_for(&rt, "txb_recover", keep).as_deref(),
            Some("updated"),
            "replaying the same WAL batches twice must be idempotent"
        );
        clear_current_connection_id();
    }
}

#[test]
fn truncated_commit_batch_is_absent_after_recovery() {
    let db = support::temp_db_file("txcommitbatch-wal-truncated");
    let stable_db_image = db.path().with_extension("stable-copy");

    {
        let rt = db_open(&db);
        set_current_connection_id(44011);
        exec(&rt, "CREATE TABLE txb_truncated (id INT, label TEXT)");
        exec(
            &rt,
            "INSERT INTO txb_truncated (id, label) VALUES (1, 'base')",
        );
        rt.checkpoint().expect("checkpoint stable prefix");
        std::fs::copy(db.path(), &stable_db_image).expect("copy stable db image");
        exec(
            &rt,
            "INSERT INTO txb_truncated (id, label) VALUES (2, 'torn')",
        );
        clear_current_connection_id();
    }

    std::fs::copy(&stable_db_image, db.path()).expect("restore stable db image");
    let _ = std::fs::remove_file(&stable_db_image);
    truncate_wal_tail(&db_wal_path(&db), 1);

    {
        let rt = db_open(&db);
        set_current_connection_id(44012);
        let base = red_entity_id(&rt, "txb_truncated", 1);
        assert_eq!(
            label_for(&rt, "txb_truncated", base).as_deref(),
            Some("base")
        );
        assert!(
            !has_user_id(&rt, "txb_truncated", 2),
            "torn commit batch must not replay"
        );
        clear_current_connection_id();
    }
}

#[test]
fn autocommit_table_mutations_write_tx_commit_batch_records() {
    let db = support::temp_db_file("txcommitbatch-wal-record-shape");

    {
        let rt = db_open(&db);
        set_current_connection_id(44021);
        exec(&rt, "CREATE TABLE txb_shape (id INT, label TEXT)");
        exec(
            &rt,
            "INSERT INTO txb_shape (id, label) VALUES (1, 'a'), (2, 'b')",
        );
        let updated = red_entity_id(&rt, "txb_shape", 1);
        let deleted = red_entity_id(&rt, "txb_shape", 2);
        exec(
            &rt,
            &format!("UPDATE txb_shape SET label = 'aa' WHERE red_entity_id = {updated}"),
        );
        exec(
            &rt,
            &format!("DELETE FROM txb_shape WHERE red_entity_id = {deleted}"),
        );
        clear_current_connection_id();
    }

    let reader = WalReader::open(db_wal_path(&db)).expect("open wal");
    let records: Vec<WalRecord> = reader
        .iter()
        .map(|entry| entry.expect("valid wal record").1)
        .collect();
    let batches = records
        .iter()
        .filter(|record| matches!(record, WalRecord::TxCommitBatch { .. }))
        .count();
    let page_writes = records
        .iter()
        .filter(|record| matches!(record, WalRecord::PageWrite { .. }))
        .count();

    assert!(
        batches >= 4,
        "CREATE TABLE plus INSERT/UPDATE/DELETE should use commit batches, got {batches}"
    );
    assert_eq!(
        page_writes, 0,
        "store WAL should not split batches into PageWrite records"
    );
}
