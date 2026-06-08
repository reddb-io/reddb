use std::path::PathBuf;
use std::process::{Command, ExitCode};

use reddb_file::{
    BaseBackupChunkRef, BaseBackupPlan, PrimaryReplicaBaseBackupManifest, PrimaryReplicaFilePlan,
    TimelineId,
};

const CHILD_ENV: &str = "REDDB_PRIMARY_REPLICA_BASEBACKUP_CRASH_CHILD";
const ROOT_ENV: &str = "REDDB_PRIMARY_REPLICA_BASEBACKUP_CRASH_ROOT";
const MODE_ENV: &str = "REDDB_PRIMARY_REPLICA_BASEBACKUP_CRASH_MODE";
const CRASH_ENV: &str = "REDDB_PRIMARY_REPLICA_CRASH_AT";

#[test]
fn basebackup_parts_publish_survives_crash_without_manifest() {
    if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in [
        "atomic_after_tmp_write",
        "atomic_after_tmp_sync",
        "atomic_after_rename",
        "atomic_after_dir_sync",
        "basebackup_after_parts_dir_rename",
    ] {
        let root = temp_root(point);
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1));
        let backup = backup_plan();

        let child = Command::new(std::env::current_exe().expect("current test exe"))
            .arg("--exact")
            .arg("primary_replica_basebackup_crash_child")
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .env(ROOT_ENV, &root)
            .env(CRASH_ENV, point)
            .status()
            .expect("run crash child");
        assert_eq!(
            child.code(),
            Some(173),
            "child should crash at {point}, status={child:?}"
        );

        assert!(
            !plan.basebackup_path(&backup).exists(),
            "basebackup manifest must not be published after crash at {point}"
        );
        if point != "basebackup_after_parts_dir_rename" {
            assert!(
                !plan.basebackup_parts_dir(&backup).exists(),
                "final parts dir must not be visible before directory publish at {point}"
            );
        }

        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn basebackup_retry_crash_preserves_existing_parts() {
    if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    let root = temp_root("retry");
    let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1));
    let backup = backup_plan();
    let manifest = plan
        .write_basebackup_snapshot_parts(backup.clone(), b"original-snapshot", 8)
        .expect("write original parts");
    manifest
        .write_to_path(plan.basebackup_path(&backup))
        .expect("write original manifest");

    let retried = plan
        .write_basebackup_snapshot_parts(backup, b"original-snapshot", 8)
        .expect("retry with matching snapshot reuses existing parts");
    assert_eq!(retried, manifest);

    let mismatch = plan
        .write_basebackup_snapshot_parts(backup, b"replacement-snapshot", 8)
        .expect_err("retry with mismatched snapshot must not replace published parts");
    assert!(
        mismatch.to_string().contains("checksum") || mismatch.to_string().contains("base backup"),
        "mismatch should identify invalid existing parts, got {mismatch}"
    );

    let restored = manifest
        .read_snapshot_parts(plan.basebackup_dir())
        .expect("existing parts still verify");
    assert_eq!(restored, b"original-snapshot");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn basebackup_manifest_publish_survives_atomic_crash_points() {
    if std::env::var(CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in [
        "atomic_after_tmp_write",
        "atomic_after_tmp_sync",
        "atomic_after_rename",
        "atomic_after_dir_sync",
    ] {
        let root = temp_root(&format!("manifest-{point}"));
        let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1));
        let backup = backup_plan();
        let manifest = plan
            .write_basebackup_snapshot_parts(backup.clone(), b"manifest-snapshot", 8)
            .expect("write parts before manifest crash");

        let child = Command::new(std::env::current_exe().expect("current test exe"))
            .arg("--exact")
            .arg("primary_replica_basebackup_crash_child")
            .arg("--nocapture")
            .env(CHILD_ENV, "1")
            .env(ROOT_ENV, &root)
            .env(MODE_ENV, "manifest")
            .env(CRASH_ENV, point)
            .status()
            .expect("run crash child");
        assert_eq!(
            child.code(),
            Some(173),
            "child should crash at {point}, status={child:?}"
        );

        if plan.basebackup_path(&backup).exists() {
            let published =
                PrimaryReplicaBaseBackupManifest::read_from_path(plan.basebackup_path(&backup))
                    .expect("published basebackup manifest decodes");
            assert_eq!(published, manifest);
            published
                .verify_snapshot_parts(plan.basebackup_dir())
                .expect("published basebackup chunks verify");
        } else {
            manifest
                .verify_snapshot_parts(plan.basebackup_dir())
                .expect("unpublished basebackup chunks remain valid");
        }

        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn primary_replica_basebackup_crash_child() -> ExitCode {
    if std::env::var(CHILD_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let root = PathBuf::from(std::env::var(ROOT_ENV).expect("root env"));
    let plan = PrimaryReplicaFilePlan::new(&root, TimelineId(1));
    match std::env::var(MODE_ENV).ok().as_deref() {
        Some("manifest") => {
            let manifest = expected_manifest(&plan, b"manifest-snapshot", 8);
            let _ = manifest.write_to_path(plan.basebackup_path(&backup_plan()));
        }
        _ => {
            let _ = plan.write_basebackup_snapshot_parts(backup_plan(), b"replacement-snapshot", 8);
        }
    }
    ExitCode::from(1)
}

fn backup_plan() -> BaseBackupPlan {
    BaseBackupPlan::new(TimelineId(1), 10, 50)
}

fn expected_manifest(
    plan: &PrimaryReplicaFilePlan,
    snapshot: &[u8],
    chunk_bytes: usize,
) -> PrimaryReplicaBaseBackupManifest {
    let backup = backup_plan();
    let chunk_bytes = chunk_bytes.max(1);
    let mut chunks = Vec::new();
    for (index, part) in snapshot.chunks(chunk_bytes).enumerate() {
        let ordinal = u32::try_from(index).expect("chunk ordinal");
        chunks.push(BaseBackupChunkRef::new(
            ordinal,
            (index * chunk_bytes) as u64,
            part.len() as u64,
            crc32(part),
            plan.basebackup_chunk_relative_path(&backup, ordinal),
        ));
    }
    if chunks.is_empty() {
        chunks.push(BaseBackupChunkRef::new(
            0,
            0,
            0,
            crc32(&[]),
            plan.basebackup_chunk_relative_path(&backup, 0),
        ));
    }
    let snapshot_relative_path = PathBuf::from(format!(
        "base-{:020}-{:020}",
        backup.start_lsn, backup.checkpoint_lsn
    ))
    .with_extension("snapshot");
    PrimaryReplicaBaseBackupManifest::incremental(
        backup,
        snapshot_relative_path,
        snapshot.len() as u64,
        crc32(snapshot),
        chunks,
    )
    .expect("expected manifest")
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn temp_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "reddb-file-primary-basebackup-crash-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}
