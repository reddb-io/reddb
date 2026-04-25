//! Chaos test: missing middle WAL segment (PLAN.md Phase 11.3 / 8 slice).
//!
//! Archives three segments, then deletes segment 2's payload + sidecar.
//! Restore enumerates segments 1 and 3 (segment 2 is gone from the
//! bucket); segment 3's `prev_hash` points to segment 2's sha256, not
//! segment 1's, so the chain check fires.

use reddb::storage::backend::LocalBackend;
use reddb::storage::wal::{
    archive_change_records, publish_snapshot_manifest, wal_segment_manifest_key,
    PointInTimeRecovery, SnapshotManifest,
};
use reddb::storage::RedDB;
use std::path::PathBuf;
use std::sync::Arc;

#[allow(dead_code)]
mod support;

fn temp_dir(prefix: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "reddb-chaos-{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn record(lsn: u64, payload: &[u8]) -> reddb::replication::cdc::ChangeRecord {
    support::logical_insert_record("users", lsn, 100 + lsn, payload)
}

#[test]
fn restore_fails_closed_on_missing_middle_segment() {
    let work = temp_dir("missing");
    let snapshot_dir = work.join("snapshots");
    let wal_dir = work.join("wal");
    let restore_path = work.join("restore").join("data.rdb");
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    std::fs::create_dir_all(&wal_dir).unwrap();

    let snapshot = snapshot_dir.join("1-100.snapshot");
    RedDB::open(&snapshot).unwrap().flush().unwrap();
    publish_snapshot_manifest(
        &LocalBackend,
        &SnapshotManifest {
            timeline_id: "main".to_string(),
            snapshot_key: snapshot.to_string_lossy().to_string(),
            snapshot_id: 1,
            snapshot_time: 100,
            base_lsn: 0,
            schema_version: reddb::api::REDDB_FORMAT_VERSION,
            format_version: reddb::api::REDDB_FORMAT_VERSION,
            snapshot_sha256: None,
        },
    )
    .unwrap();

    let wal_prefix = format!("{}/", wal_dir.to_string_lossy());
    let mut prev: Option<String> = None;
    let mut metas = Vec::new();
    for lsn in [1u64, 2, 3] {
        let r = record(lsn, format!("payload-{lsn}").as_bytes());
        let m = archive_change_records(
            &LocalBackend,
            &wal_prefix,
            &[(r.lsn, r.encode())],
            prev.clone(),
        )
        .unwrap()
        .expect("archived");
        prev = m.sha256.clone();
        metas.push(m);
    }

    // Delete segment 2's payload AND sidecar — gone from the bucket
    // entirely. The LocalBackend stores keys as filesystem paths, so
    // `metas[1].key` is the on-disk path.
    let seg2_path = std::path::Path::new(&metas[1].key);
    let seg2_sidecar = wal_segment_manifest_key(&metas[1].key);
    let _ = std::fs::remove_file(seg2_path);
    let _ = std::fs::remove_file(&seg2_sidecar);

    let recovery = PointInTimeRecovery::new(
        Arc::new(LocalBackend),
        snapshot_dir.to_string_lossy().to_string(),
        wal_prefix,
    );
    let err = recovery
        .restore_to(0, &restore_path)
        .expect_err("missing middle segment must fail closed");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("chain"),
        "error must mention chain; got: {msg}"
    );

    let _ = std::fs::remove_dir_all(&work);
}
