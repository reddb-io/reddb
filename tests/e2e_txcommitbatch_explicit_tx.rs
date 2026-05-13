//! Atomic TxCommitBatch WAL recovery for explicit table transactions.

use std::path::{Path, PathBuf};

use reddb::api::DurabilityMode;
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::storage::schema::Value;
use reddb::storage::wal::{WalReader, WalRecord};
use reddb::{RedDBOptions, RedDBRuntime};

struct DbPath {
    path: PathBuf,
}

impl DbPath {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "reddb_txcommitbatch_explicit_{label}_{}_{}.rdb",
            std::process::id(),
            nanos
        ));
        let db = Self { path };
        db.cleanup();
        db
    }

    fn open(&self) -> RedDBRuntime {
        RedDBRuntime::with_options(
            RedDBOptions::persistent(&self.path)
                .with_durability_mode(DurabilityMode::WalDurableGrouped),
        )
        .expect("persistent runtime")
    }

    fn wal_path(&self) -> PathBuf {
        self.path.with_extension("rdb-uwal")
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.wal_path());
    }
}

impl Drop for DbPath {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn label_for_id(rt: &RedDBRuntime, table: &str, id: i64) -> Option<String> {
    let result = rt
        .execute_query(&format!("SELECT id, label FROM {table}"))
        .expect("select rows");
    result.result.records.iter().find_map(|record| {
        let row_id = match record
            .get("id")
            .or_else(|| record.get("c0"))
            .or_else(|| record.get("c1"))
        {
            Some(Value::Integer(value)) => *value,
            Some(Value::UnsignedInteger(value)) => *value as i64,
            _ => return None,
        };
        if row_id != id {
            return None;
        }
        match record
            .get("label")
            .or_else(|| record.get("c1"))
            .or_else(|| record.get("c2"))
        {
            Some(Value::Text(value)) => Some(value.to_string()),
            _ => None,
        }
    })
}

fn has_user_id(rt: &RedDBRuntime, table: &str, id: i64) -> bool {
    label_for_id(rt, table, id).is_some()
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

fn wal_records(path: &Path) -> Vec<WalRecord> {
    WalReader::open(path)
        .expect("open wal")
        .iter()
        .map(|entry| entry.expect("valid wal record").1)
        .collect()
}

#[test]
fn committed_explicit_transaction_recovers_all_mutations_idempotently() {
    let db = DbPath::new("recover");

    {
        let rt = db.open();
        set_current_connection_id(44101);
        exec(&rt, "CREATE TABLE txb_explicit (id INT, label TEXT)");
        exec(
            &rt,
            "INSERT INTO txb_explicit (id, label) VALUES (2, 'delete-me')",
        );
        exec(&rt, "BEGIN");
        exec(
            &rt,
            "INSERT INTO txb_explicit (id, label) VALUES (1, 'inserted')",
        );
        exec(
            &rt,
            "UPDATE txb_explicit SET label = 'updated' WHERE id = 1",
        );
        exec(&rt, "DELETE FROM txb_explicit WHERE id = 2");
        exec(&rt, "COMMIT");
        clear_current_connection_id();
    }

    {
        let rt = db.open();
        set_current_connection_id(44102);
        assert_eq!(
            label_for_id(&rt, "txb_explicit", 1).as_deref(),
            Some("updated")
        );
        assert!(!has_user_id(&rt, "txb_explicit", 2));
        clear_current_connection_id();
    }

    {
        let rt = db.open();
        set_current_connection_id(44103);
        assert_eq!(
            label_for_id(&rt, "txb_explicit", 1).as_deref(),
            Some("updated"),
            "replaying the committed transaction twice must be idempotent"
        );
        assert!(!has_user_id(&rt, "txb_explicit", 2));
        clear_current_connection_id();
    }
}

#[test]
fn torn_explicit_transaction_commit_batch_recovers_no_partial_state() {
    let db = DbPath::new("torn");
    let stable_db_image = db.path.with_extension("stable-copy");

    {
        let rt = db.open();
        set_current_connection_id(44111);
        exec(&rt, "CREATE TABLE txb_explicit_torn (id INT, label TEXT)");
        exec(
            &rt,
            "INSERT INTO txb_explicit_torn (id, label) VALUES (1, 'base'), (2, 'delete-me')",
        );
        rt.checkpoint().expect("checkpoint stable prefix");
        std::fs::copy(&db.path, &stable_db_image).expect("copy stable db image");
        exec(&rt, "BEGIN");
        exec(
            &rt,
            "INSERT INTO txb_explicit_torn (id, label) VALUES (3, 'inserted')",
        );
        exec(
            &rt,
            "UPDATE txb_explicit_torn SET label = 'updated' WHERE id = 1",
        );
        exec(&rt, "DELETE FROM txb_explicit_torn WHERE id = 2");
        exec(&rt, "COMMIT");
        clear_current_connection_id();
    }

    std::fs::copy(&stable_db_image, &db.path).expect("restore stable db image");
    let _ = std::fs::remove_file(&stable_db_image);
    truncate_wal_tail(&db.wal_path(), 1);

    {
        let rt = db.open();
        set_current_connection_id(44112);
        assert_eq!(
            label_for_id(&rt, "txb_explicit_torn", 1).as_deref(),
            Some("base")
        );
        assert_eq!(
            label_for_id(&rt, "txb_explicit_torn", 2).as_deref(),
            Some("delete-me")
        );
        assert!(
            !has_user_id(&rt, "txb_explicit_torn", 3),
            "torn commit batch must not expose inserted transaction rows"
        );
        clear_current_connection_id();
    }
}

#[test]
fn explicit_transaction_writes_one_tx_commit_batch_for_staged_table_mutations() {
    let db = DbPath::new("shape");

    {
        let rt = db.open();
        set_current_connection_id(44121);
        exec(&rt, "CREATE TABLE txb_explicit_shape (id INT, label TEXT)");
        exec(
            &rt,
            "INSERT INTO txb_explicit_shape (id, label) VALUES (2, 'delete-me')",
        );
        exec(&rt, "BEGIN");
        exec(
            &rt,
            "INSERT INTO txb_explicit_shape (id, label) VALUES (1, 'inserted')",
        );
        exec(
            &rt,
            "UPDATE txb_explicit_shape SET label = 'updated' WHERE id = 1",
        );
        exec(&rt, "DELETE FROM txb_explicit_shape WHERE id = 2");
        exec(&rt, "COMMIT");
        clear_current_connection_id();
    }

    let records = wal_records(&db.wal_path());
    let transaction_batches = records
        .iter()
        .filter(|record| matches!(record, WalRecord::TxCommitBatch { actions, .. } if actions.len() >= 3))
        .count();
    let page_writes = records
        .iter()
        .filter(|record| matches!(record, WalRecord::PageWrite { .. }))
        .count();

    assert_eq!(
        transaction_batches, 1,
        "explicit INSERT/UPDATE/DELETE should be staged into one commit batch"
    );
    assert_eq!(
        page_writes, 0,
        "explicit transaction WAL should not split staged table mutations into PageWrite records"
    );
}
