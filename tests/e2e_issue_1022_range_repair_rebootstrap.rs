//! Issue #1022 — range repair as full rebootstrap tracer.

use std::path::{Path, PathBuf};

use reddb::api::DurabilityMode;
use reddb::replication::{
    RangeRepairError, RangeRepairRequest, RangeRepairTracer, RangeReplicaHealth,
};
use reddb::storage::wal::{WalReader, WalRecord};
use reddb::storage::{ClusterRangeLayout, StorageDeployPreset};
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
            "reddb_issue_1022_{label}_{}_{}.rdb",
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

struct DirPath {
    path: PathBuf,
}

impl DirPath {
    fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "reddb_issue_1022_{label}_{}_{}",
            std::process::id(),
            nanos
        ));
        let dir = Self { path };
        dir.cleanup();
        dir
    }

    fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl Drop for DirPath {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn latest_range_lsn(wal_path: &Path, range_id: &str) -> u64 {
    WalReader::open(wal_path)
        .expect("open WAL")
        .iter()
        .filter_map(|entry| {
            let (lsn, record) = entry.expect("read WAL record");
            match record {
                WalRecord::RangeCommitBatch {
                    range_id: record_range_id,
                    ..
                } if record_range_id == range_id => Some(lsn),
                _ => None,
            }
        })
        .max()
        .expect("range WAL record")
}

#[test]
fn corrupt_range_repair_quarantines_installs_snapshot_catches_up_and_marks_healthy() {
    let source = DbPath::new("corrupt_source");
    let target = DirPath::new("corrupt_target");
    let rt = source.open();

    exec(&rt, "CREATE TABLE orders (id INTEGER, label TEXT)");
    exec(&rt, "INSERT INTO orders (id, label) VALUES (1, 'alpha')");

    let source_metadata = rt
        .db()
        .store()
        .collection_range_metadata("orders")
        .expect("orders range metadata");
    let snapshot_lsn = latest_range_lsn(&source.wal_path(), &source_metadata.logical_range_id);
    let source_layout = ClusterRangeLayout::new(source.support_dir());
    let snapshot = source_layout
        .export_range_snapshot(
            &source_metadata,
            source.support_dir().join("repair-snapshots"),
            snapshot_lsn,
            11,
        )
        .expect("export physical range snapshot");

    let target_layout = ClusterRangeLayout::new(&target.path);
    let local_metadata = target_layout
        .install_range_snapshot(&snapshot)
        .expect("seed stale local range copy");
    std::fs::write(&local_metadata.data_file, b"corrupt local bytes").expect("mark local data");

    exec(&rt, "INSERT INTO orders (id, label) VALUES (2, 'beta')");
    let required_watermark_lsn =
        latest_range_lsn(&source.wal_path(), &source_metadata.logical_range_id);
    assert!(required_watermark_lsn > snapshot_lsn);

    let mut state =
        RangeRepairTracer::mark_corrupt(local_metadata, 11, snapshot_lsn, "checksum mismatch");
    assert!(matches!(
        state.health,
        RangeReplicaHealth::RepairRequired { .. }
    ));

    let outcome = RangeRepairTracer::repair_from_healthy_owner(
        &target_layout,
        &mut state,
        &snapshot,
        source.wal_path(),
        RangeRepairRequest {
            range_id: source_metadata.logical_range_id.clone(),
            required_watermark_lsn,
            source_owner_epoch: 11,
            repaired_owner_epoch: 12,
        },
    )
    .expect("repair range from healthy owner");

    assert!(outcome.quarantine.quarantine_dir.is_dir());
    assert_eq!(
        std::fs::read(outcome.quarantine.quarantine_dir.join("data.rdb")).expect("quarantine data"),
        b"corrupt local bytes"
    );
    assert!(outcome.installed_metadata.data_file.is_file());
    assert_eq!(outcome.catch_up.started_after_lsn, snapshot_lsn);
    assert_eq!(outcome.catch_up.applied_lsn, required_watermark_lsn);
    assert_eq!(outcome.catch_up.applied_batches, 1);
    assert_eq!(
        state.health,
        RangeReplicaHealth::Healthy {
            owner_epoch: 12,
            applied_lsn: required_watermark_lsn
        }
    );
}

#[test]
fn too_stale_range_can_be_marked_for_rebootstrap() {
    let source = DbPath::new("stale_mark_source");
    let target = DirPath::new("stale_mark_target");
    let rt = source.open();

    exec(&rt, "CREATE TABLE accounts (id INTEGER, label TEXT)");
    exec(&rt, "INSERT INTO accounts (id, label) VALUES (1, 'open')");

    let source_metadata = rt
        .db()
        .store()
        .collection_range_metadata("accounts")
        .expect("accounts range metadata");
    let snapshot_lsn = latest_range_lsn(&source.wal_path(), &source_metadata.logical_range_id);
    let snapshot = ClusterRangeLayout::new(source.support_dir())
        .export_range_snapshot(
            &source_metadata,
            source.support_dir().join("repair-snapshots"),
            snapshot_lsn,
            3,
        )
        .expect("export snapshot");
    let local_metadata = ClusterRangeLayout::new(&target.path)
        .install_range_snapshot(&snapshot)
        .expect("seed local range copy");

    let state = RangeRepairTracer::mark_too_stale(local_metadata, 3, 7, 42);

    assert!(matches!(
        state.health,
        RangeReplicaHealth::RepairRequired { .. }
    ));
    assert_eq!(state.applied_lsn, 7);
}

#[test]
fn failed_repair_leaves_quarantined_data_available_for_inspection() {
    let source = DbPath::new("failed_source");
    let target = DirPath::new("failed_target");
    let rt = source.open();

    exec(&rt, "CREATE TABLE invoices (id INTEGER, label TEXT)");
    exec(&rt, "INSERT INTO invoices (id, label) VALUES (1, 'sent')");

    let source_metadata = rt
        .db()
        .store()
        .collection_range_metadata("invoices")
        .expect("invoices range metadata");
    let snapshot_lsn = latest_range_lsn(&source.wal_path(), &source_metadata.logical_range_id);
    let source_layout = ClusterRangeLayout::new(source.support_dir());
    let snapshot = source_layout
        .export_range_snapshot(
            &source_metadata,
            source.support_dir().join("repair-snapshots"),
            snapshot_lsn,
            5,
        )
        .expect("export snapshot");

    let target_layout = ClusterRangeLayout::new(&target.path);
    let local_metadata = target_layout
        .install_range_snapshot(&snapshot)
        .expect("seed local range copy");
    std::fs::write(&local_metadata.data_file, b"inspect me").expect("local inspection marker");
    std::fs::remove_dir_all(&snapshot.checkpoint_dir)
        .expect("break healthy-owner snapshot transfer");

    let mut state = RangeRepairTracer::mark_corrupt(local_metadata, 5, snapshot_lsn, "bad page");
    let err = RangeRepairTracer::repair_from_healthy_owner(
        &target_layout,
        &mut state,
        &snapshot,
        source.wal_path(),
        RangeRepairRequest {
            range_id: source_metadata.logical_range_id,
            required_watermark_lsn: snapshot_lsn,
            source_owner_epoch: 5,
            repaired_owner_epoch: 6,
        },
    )
    .expect_err("broken snapshot install must fail");

    let quarantine = match err {
        RangeRepairError::InstallSnapshot { quarantine, .. } => quarantine,
        other => panic!("expected install failure, got {other:?}"),
    };
    assert!(quarantine.quarantine_dir.is_dir());
    assert_eq!(
        std::fs::read(quarantine.quarantine_dir.join("data.rdb")).expect("quarantined data"),
        b"inspect me"
    );
    assert!(!quarantine.original_dir.exists());
    assert!(matches!(
        state.health,
        RangeReplicaHealth::RepairRequired { .. }
    ));
}
