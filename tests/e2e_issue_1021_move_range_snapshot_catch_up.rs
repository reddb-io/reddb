//! Issue #1021 — move-range snapshot catch-up tracer.

use std::path::{Path, PathBuf};

use reddb::api::DurabilityMode;
use reddb::replication::{
    MoveRangeError, MoveRangeRequest, MoveRangeTargetState, MoveRangeTracer, RangeOwnership,
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
            "reddb_issue_1021_{label}_{}_{}.rdb",
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
            "reddb_issue_1021_{label}_{}_{}",
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
fn range_move_installs_snapshot_catches_up_and_then_cuts_over() {
    let source = DbPath::new("success_source");
    let target = DirPath::new("success_target");
    let rt = source.open();

    exec(&rt, "CREATE TABLE orders (id INTEGER, label TEXT)");
    exec(&rt, "INSERT INTO orders (id, label) VALUES (1, 'alpha')");

    let metadata = rt
        .db()
        .store()
        .collection_range_metadata("orders")
        .expect("orders range metadata");
    let snapshot_lsn = latest_range_lsn(&source.wal_path(), &metadata.logical_range_id);
    let source_layout = ClusterRangeLayout::new(source.support_dir());
    let snapshot = source_layout
        .export_range_snapshot(
            &metadata,
            source.support_dir().join("move-snapshots"),
            snapshot_lsn,
            7,
        )
        .expect("export physical range snapshot");

    exec(&rt, "INSERT INTO orders (id, label) VALUES (2, 'beta')");
    let required_watermark_lsn = latest_range_lsn(&source.wal_path(), &metadata.logical_range_id);
    assert!(
        required_watermark_lsn > snapshot_lsn,
        "catch-up watermark must be past snapshot boundary"
    );

    let target_layout = ClusterRangeLayout::new(&target.path);
    let installed = target_layout
        .install_range_snapshot(&snapshot)
        .expect("install range snapshot");
    assert!(installed
        .physical_dir
        .starts_with(target_layout.ranges_dir()));
    assert!(installed.physical_dir.join("range.meta").is_file());
    assert!(installed.data_file.is_file());

    let mut target_state = MoveRangeTargetState::from_installed_snapshot(&snapshot, installed);
    let request = MoveRangeRequest {
        range_id: metadata.logical_range_id.clone(),
        required_watermark_lsn,
        source_owner_epoch: 7,
        target_owner_epoch: 8,
    };

    assert!(matches!(
        MoveRangeTracer::cutover(&request, &target_state),
        Err(MoveRangeError::CatchUpRequired { .. })
    ));

    let catch_up = MoveRangeTracer::catch_up_from_wal(
        &mut target_state,
        source.wal_path(),
        required_watermark_lsn,
    )
    .expect("catch up through range-indexed stream");
    assert_eq!(catch_up.started_after_lsn, snapshot_lsn);
    assert_eq!(catch_up.applied_lsn, required_watermark_lsn);
    assert_eq!(catch_up.applied_batches, 1);

    let cutover = MoveRangeTracer::cutover(&request, &target_state).expect("cutover");
    assert_eq!(cutover.range_id, metadata.logical_range_id);
    assert_eq!(cutover.owner_epoch, 8);
    assert_eq!(cutover.reached_lsn, required_watermark_lsn);
}

#[test]
fn range_move_rejects_stale_target_snapshot_epoch() {
    let source = DbPath::new("stale_source");
    let target = DirPath::new("stale_target");
    let rt = source.open();

    exec(&rt, "CREATE TABLE accounts (id INTEGER, label TEXT)");
    exec(&rt, "INSERT INTO accounts (id, label) VALUES (1, 'open')");

    let metadata = rt
        .db()
        .store()
        .collection_range_metadata("accounts")
        .expect("accounts range metadata");
    let snapshot_lsn = latest_range_lsn(&source.wal_path(), &metadata.logical_range_id);
    let source_layout = ClusterRangeLayout::new(source.support_dir());
    let snapshot = source_layout
        .export_range_snapshot(
            &metadata,
            source.support_dir().join("move-snapshots"),
            snapshot_lsn,
            4,
        )
        .expect("export stale physical range snapshot");
    let installed = ClusterRangeLayout::new(&target.path)
        .install_range_snapshot(&snapshot)
        .expect("install stale range snapshot");
    let mut target_state = MoveRangeTargetState::from_installed_snapshot(&snapshot, installed);
    target_state.applied_lsn = snapshot_lsn;

    let request = MoveRangeRequest {
        range_id: metadata.logical_range_id,
        required_watermark_lsn: snapshot_lsn,
        source_owner_epoch: 5,
        target_owner_epoch: 6,
    };

    assert!(matches!(
        MoveRangeTracer::cutover(&request, &target_state),
        Err(MoveRangeError::StaleTargetSnapshot {
            snapshot_epoch: 4,
            required_epoch: 5,
            ..
        })
    ));
}

#[test]
fn range_owner_rejects_writes_under_stale_ownership_epoch() {
    let ownership = RangeOwnership::new("range-0000000000000042", "node-b", 9);

    assert!(ownership.check_write_epoch(9).is_ok());
    assert!(matches!(
        ownership.check_write_epoch(8),
        Err(MoveRangeError::StaleOwnerEpoch {
            attempted_epoch: 8,
            current_epoch: 9,
            ..
        })
    ));
}
