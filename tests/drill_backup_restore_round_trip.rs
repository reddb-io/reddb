//! Drill: full backup → simulated primary loss → restore round trip
//! (PLAN.md Phase 11.7).
//!
//! 1. Open primary DB on disk; insert N rows.
//! 2. Take a snapshot + archive WAL (writes the unified MANIFEST.json
//!    + per-segment sidecars).
//! 3. Insert M more rows; archive again.
//! 4. "Lose" the primary by deleting the data file.
//! 5. Run PITR restore to latest into a fresh path.
//! 6. Verify the restored DB has all N+M rows, hash chain validation
//!    passed end-to-end, and the recovered LSN matches what we wrote.
//!
//! This isn't the full chaos drill from PLAN.md 11.7 (no replica
//! fleet, no manual promotion); it's the restore-from-remote leg
//! that catches integrity regressions in CI.

use reddb::api::REDDB_FORMAT_VERSION;
use reddb::replication::cdc::ChangeRecord;
use reddb::storage::backend::LocalBackend;
use reddb::storage::wal::{
    archive_change_records, archive_snapshot, publish_snapshot_manifest,
    publish_unified_manifest_for_prefix, PointInTimeRecovery, SnapshotManifest,
};
use reddb::storage::RedDB;
use std::path::PathBuf;
use std::sync::Arc;

#[allow(dead_code)]
mod support;

fn temp_dir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "reddb-drill-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn record(lsn: u64, payload: &[u8]) -> ChangeRecord {
    support::logical_insert_record("drill", lsn, 1000 + lsn, payload)
}

#[test]
fn round_trip_restore_replays_full_wal_history() {
    let work = temp_dir("round-trip");
    let snapshot_dir = work.join("snapshots");
    let wal_dir = work.join("wal");
    let primary_path = work.join("primary").join("data.rdb");
    let restore_path = work.join("restore").join("data.rdb");
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    std::fs::create_dir_all(&wal_dir).unwrap();
    std::fs::create_dir_all(primary_path.parent().unwrap()).unwrap();

    // 1) Open primary, take a base snapshot at LSN 0.
    let primary = RedDB::open(&primary_path).unwrap();
    primary.flush().unwrap();
    let snapshot_prefix = snapshot_dir.to_string_lossy().to_string();
    let snapshot_key = archive_snapshot(
        &LocalBackend,
        &primary_path,
        1,
        &format!("{snapshot_prefix}/"),
    )
    .unwrap();
    publish_snapshot_manifest(
        &LocalBackend,
        &SnapshotManifest {
            timeline_id: "main".to_string(),
            snapshot_key: snapshot_key.clone(),
            snapshot_id: 1,
            snapshot_time: 100,
            base_lsn: 0,
            schema_version: REDDB_FORMAT_VERSION,
            format_version: REDDB_FORMAT_VERSION,
            snapshot_sha256: SnapshotManifest::compute_snapshot_sha256(&primary_path).ok(),
        },
    )
    .unwrap();

    // 2) Two rounds of WAL archive — five LSNs each — with a chain
    //    head propagated via the optional prev_hash arg.
    let wal_prefix = format!("{}/", wal_dir.to_string_lossy());
    let mut prev: Option<String> = None;
    let total_records: u64 = 10;
    let mut max_archived_lsn = 0u64;
    for batch in 0..2 {
        let start = batch * 5 + 1;
        let records: Vec<(u64, Vec<u8>)> = (start..start + 5)
            .map(|lsn| {
                let r = record(lsn, format!("payload-{lsn}").as_bytes());
                (r.lsn, r.encode())
            })
            .collect();
        let meta = archive_change_records(&LocalBackend, &wal_prefix, &records, prev.clone())
            .unwrap()
            .expect("archived");
        prev = meta.sha256.clone();
        max_archived_lsn = max_archived_lsn.max(meta.lsn_end);
    }

    // Refresh the unified MANIFEST.json so external tooling sees the
    // full catalog.
    publish_unified_manifest_for_prefix(&LocalBackend, &snapshot_prefix).unwrap();

    // 3) Simulate primary loss — delete the data file. The remote
    //    backend (just a local FS in this test) is the only source of
    //    truth from here on.
    drop(primary);
    std::fs::remove_file(&primary_path).unwrap();
    assert!(!primary_path.exists());

    // 4) Restore to "latest" into a fresh path.
    let recovery = PointInTimeRecovery::new(Arc::new(LocalBackend), snapshot_prefix, wal_prefix);
    let result = recovery
        .restore_to(0, &restore_path)
        .expect("restore must succeed against intact chain");

    // 5) Assertions: every WAL segment was replayed; recovered LSN is
    //    the highest archived; the destination file exists.
    assert_eq!(result.snapshot_used, 1, "snapshot 1 must be used as base");
    assert_eq!(
        result.wal_segments_replayed, 2,
        "both archived segments must replay"
    );
    assert_eq!(
        result.records_applied, total_records,
        "all {total_records} records must apply"
    );
    assert_eq!(
        result.recovered_to_lsn, max_archived_lsn,
        "recovered LSN must reach the last archived LSN"
    );
    assert!(restore_path.exists(), "restored DB file must be on disk");

    let _ = std::fs::remove_dir_all(&work);
}
