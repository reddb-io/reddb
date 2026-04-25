//! Chaos test: WAL segment payload corruption (PLAN.md Phase 2.4 / 8 slice).
//!
//! Archives three WAL segments with valid sha256 sidecars, then
//! tampers segment 2's recorded sha256 to a value that doesn't match
//! the on-disk payload. Restore must fail closed before applying any
//! record from the corrupted segment.

use reddb::storage::backend::LocalBackend;
use reddb::storage::wal::{
    archive_change_records, load_wal_segment_manifest, publish_snapshot_manifest,
    publish_wal_segment_manifest, PointInTimeRecovery, SnapshotManifest,
};
use reddb::storage::RedDB;
use std::path::PathBuf;
use std::sync::Arc;

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
    use reddb::replication::cdc::{ChangeOperation, ChangeRecord};
    use reddb::storage::{EntityId, UnifiedEntity, UnifiedStore};
    let entity = UnifiedEntity::new(EntityId::new(lsn), payload.to_vec());
    ChangeRecord {
        lsn,
        timestamp: 100 + lsn,
        operation: ChangeOperation::Insert,
        collection: "users".to_string(),
        entity_id: lsn,
        entity_kind: "row".to_string(),
        entity_bytes: Some(UnifiedStore::serialize_entity(
            &entity,
            reddb::api::REDDB_FORMAT_VERSION,
        )),
        metadata: None,
    }
}

#[test]
fn restore_fails_closed_on_segment_sha256_mismatch() {
    let work = temp_dir("sha-corrupt");
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
        let m = archive_change_records(&LocalBackend, &wal_prefix, &[(r.lsn, r.encode())], prev.clone())
            .unwrap()
            .expect("archived");
        prev = m.sha256.clone();
        metas.push(m);
    }

    // Tamper segment 2's recorded sha256 — payload bytes are intact,
    // but the sidecar lies about their digest.
    let mut sidecar2 = load_wal_segment_manifest(&LocalBackend, &metas[1].key)
        .unwrap()
        .expect("segment 2 sidecar");
    sidecar2.sha256 = Some("ff".repeat(32));
    publish_wal_segment_manifest(&LocalBackend, &sidecar2).unwrap();

    let recovery = PointInTimeRecovery::new(
        Arc::new(LocalBackend),
        snapshot_dir.to_string_lossy().to_string(),
        wal_prefix,
    );
    let err = recovery
        .restore_to(0, &restore_path)
        .expect_err("sha256 mismatch must fail closed");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("integrity") || msg.contains("sha256"),
        "error must mention integrity/sha256; got: {msg}"
    );

    let _ = std::fs::remove_dir_all(&work);
}
