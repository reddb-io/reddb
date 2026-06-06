//! Issue #1020 — cluster range-directory layout tracer.

use std::path::PathBuf;

use reddb::api::DurabilityMode;
use reddb::storage::schema::Value;
use reddb::storage::wal::{WalReader, WalRecord};
use reddb::storage::StorageDeployPreset;
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
            "reddb_issue_1020_{label}_{}_{}.rdb",
            std::process::id(),
            nanos
        ));
        let db = Self { path };
        db.cleanup();
        db
    }

    fn open(&self) -> RedDBRuntime {
        let options = RedDBOptions::persistent(&self.path)
            .with_durability_mode(DurabilityMode::WalDurableGrouped)
            .with_storage_profile(StorageDeployPreset::Cluster.selection())
            .expect("cluster storage profile");
        RedDBRuntime::with_options(options).expect("cluster runtime")
    }

    fn support_dir(&self) -> PathBuf {
        let file = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("data path file name");
        self.path.with_file_name(format!("{file}.red"))
    }

    fn wal_path(&self) -> PathBuf {
        self.path.with_extension("rdb-uwal")
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.wal_path());
        let _ = std::fs::remove_dir_all(self.support_dir());
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

#[test]
fn cluster_profile_creates_range_directory_metadata_and_range_wal_identity() {
    let db = DbPath::new("range_layout");
    let rt = db.open();

    exec(&rt, "CREATE TABLE orders (id INTEGER, label TEXT)");
    exec(&rt, "INSERT INTO orders (id, label) VALUES (1, 'alpha')");

    let metadata = rt
        .db()
        .store()
        .collection_range_metadata("orders")
        .expect("orders range metadata");
    assert_eq!(metadata.collection, "orders");
    let expected_range_id = format!("range-{:016x}", metadata.collection_id);
    let expected_physical_dir_id = format!("collection-{:016x}-range", metadata.collection_id);
    assert_eq!(metadata.logical_range_id, expected_range_id);
    assert_eq!(metadata.physical_range_dir_id, expected_physical_dir_id);
    assert!(metadata.physical_dir.is_dir());
    assert!(metadata.data_file.is_file());
    assert!(metadata.index_file.is_file());
    assert!(metadata.append_segment_file.is_file());
    assert!(metadata.physical_dir.join("range.meta").is_file());

    let wal_records = WalReader::open(db.wal_path())
        .expect("open store wal")
        .iter()
        .collect::<std::io::Result<Vec<_>>>()
        .expect("read store wal");
    let range_batches: Vec<_> = wal_records
        .iter()
        .filter_map(|(_, record)| match record {
            WalRecord::RangeCommitBatch {
                range_id, actions, ..
            } => Some((range_id, actions.len())),
            _ => None,
        })
        .collect();
    assert!(
        range_batches
            .iter()
            .any(|(range_id, _)| range_id.as_str() == metadata.logical_range_id),
        "expected range-stamped WAL batch for {}",
        metadata.logical_range_id
    );

    drop(rt);
    let reopened = db.open();
    let reopened_metadata = reopened
        .db()
        .store()
        .collection_range_metadata("orders")
        .expect("orders range metadata after replay");
    assert_eq!(reopened_metadata, metadata);

    let selected = reopened
        .execute_query("SELECT label FROM orders WHERE id = 1")
        .expect("select through range-owned collection");
    assert_eq!(
        selected.result.records[0].get("label"),
        Some(&Value::text("alpha"))
    );
}
