//! Drill: PITR target-time semantics (PLAN.md Phase 11.7).
//!
//! Archives 10 records spanning a known timestamp range, then runs
//! restore with `target_time = T_mid`. Asserts:
//! - Records with timestamp <= T_mid are applied.
//! - Records with timestamp > T_mid are NOT applied.
//! - The recovered LSN reflects the latest applied record, not the
//!   archive tip.

use reddb::api::REDDB_FORMAT_VERSION;
use reddb::replication::cdc::ChangeRecord;
use reddb::storage::backend::LocalBackend;
use reddb::storage::wal::{
    archive_change_records, archive_snapshot, publish_snapshot_manifest, PointInTimeRecovery,
    SnapshotManifest,
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

fn record_at(lsn: u64, ts: u64, payload: &[u8]) -> ChangeRecord {
    support::logical_insert_record("drill", lsn, ts, payload)
}

#[test]
fn restore_to_target_time_skips_records_after_t() {
    let work = temp_dir("pitr-target");
    let snapshot_dir = work.join("snapshots");
    let wal_dir = work.join("wal");
    let primary_path = work.join("primary").join("data.rdb");
    let restore_path = work.join("restore").join("data.rdb");
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    std::fs::create_dir_all(&wal_dir).unwrap();
    std::fs::create_dir_all(primary_path.parent().unwrap()).unwrap();

    // Snapshot at t=100, base_lsn=0.
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
            snapshot_key,
            snapshot_id: 1,
            snapshot_time: 100,
            base_lsn: 0,
            schema_version: REDDB_FORMAT_VERSION,
            format_version: REDDB_FORMAT_VERSION,
            snapshot_sha256: SnapshotManifest::compute_snapshot_sha256(&primary_path).ok(),
        },
    )
    .unwrap();
    drop(primary);

    // 10 records: lsn=1..10, timestamps 200..1100 in steps of 100.
    // Pick T_mid = 600 → records with ts <= 600 (lsn 1..5) apply,
    // records with ts > 600 (lsn 6..10) skip.
    let wal_prefix = format!("{}/", wal_dir.to_string_lossy());
    let mut prev: Option<String> = None;
    let mut all_records: Vec<(u64, Vec<u8>)> = Vec::with_capacity(10);
    for i in 1u64..=10 {
        let ts = 100 + i * 100; // 200, 300, ..., 1100
        let r = record_at(i, ts, format!("p-{i}").as_bytes());
        all_records.push((r.lsn, r.encode()));
    }
    // Archive in two segments to also exercise the chain across the
    // PITR cutoff.
    let m1 = archive_change_records(&LocalBackend, &wal_prefix, &all_records[..5], prev.clone())
        .unwrap()
        .expect("seg1");
    prev = m1.sha256;
    let _m2 = archive_change_records(&LocalBackend, &wal_prefix, &all_records[5..], prev)
        .unwrap()
        .expect("seg2");

    let target_time = 600u64; // up to and including lsn=5 (ts=600)

    let recovery = PointInTimeRecovery::new(Arc::new(LocalBackend), snapshot_prefix, wal_prefix);
    let result = recovery
        .restore_to(target_time, &restore_path)
        .expect("restore");

    // 5 records had ts <= 600, those must apply. The post-T records
    // are read but skipped by the timestamp filter inside
    // execute_restore.
    assert_eq!(
        result.records_applied, 5,
        "only records with timestamp <= {target_time} must apply"
    );
    assert_eq!(
        result.recovered_to_lsn, 5,
        "recovered LSN reflects last applied record, not archive tip"
    );
    assert!(
        result.recovered_to_time <= target_time,
        "recovered_to_time {} must not exceed target {}",
        result.recovered_to_time,
        target_time
    );

    let _ = std::fs::remove_dir_all(&work);
}
