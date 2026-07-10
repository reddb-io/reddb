use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use reddb_file::{
    EmbeddedRdbArtifact, EMBEDDED_RDB_MANIFEST_0_OFFSET, EMBEDDED_RDB_MANIFEST_1_OFFSET,
    EMBEDDED_RDB_MANIFEST_ZONE_END, EMBEDDED_RDB_SUPERBLOCK_0_OFFSET,
    EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
};

/// Auto-cleaning temp dir: the returned [`tempfile::TempDir`] guard removes the
/// directory and all `.rdb` artifacts under it on drop, including on panic. The
/// caller keeps the binding alive and reads paths via `dir.path()`.
fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-embedded-rdb-{label}-"))
        .tempdir()
        .expect("temp dir")
}

fn artifact_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

fn run_crash_child(test_name: &str, path: &Path, op: &str, point: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env("REDDB_EMBEDDED_RDB_CRASH_CHILD_PATH", path)
        .env("REDDB_EMBEDDED_RDB_CRASH_CHILD_OP", op)
        .env("REDDB_EMBEDDED_RDB_CRASH_AT", point)
        .output()
        .expect("run crash child");
    assert_eq!(
        output.status.code(),
        Some(173),
        "child did not crash at {point}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn create_open_embedded_rdb_uses_one_required_artifact() {
    let dir = temp_dir("create_open");
    let path = dir.path().join("data.rdb");

    let created = EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");
    assert_eq!(created.selected_superblock.copy_index, 1);
    assert_eq!(created.selected_superblock.generation, 2);
    assert_eq!(
        created.selected_superblock.manifest_offset,
        EMBEDDED_RDB_MANIFEST_0_OFFSET
    );
    assert_eq!(
        created.manifest.wal_recovery_boundary,
        created.manifest.wal_region_offset
    );

    let reopened = EmbeddedRdbArtifact::open(&path).expect("open embedded rdb");
    assert_eq!(reopened.selected_superblock.copy_index, 1);
    assert_eq!(reopened.selected_superblock.generation, 2);
    assert_eq!(reopened.manifest.checksum, created.manifest.checksum);
    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);
}

#[test]
fn open_falls_back_to_older_superblock_when_newer_copy_is_invalid() {
    let dir = temp_dir("superblock_fallback");
    let path = dir.path().join("data.rdb");
    EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("operation should succeed");
    file.seek(SeekFrom::Start(EMBEDDED_RDB_SUPERBLOCK_1_OFFSET + 64))
        .expect("operation should succeed");
    file.write_all(&[0xA5]).unwrap();
    file.sync_all().unwrap();

    let reopened = EmbeddedRdbArtifact::open(&path).expect("open falls back");
    assert_eq!(reopened.selected_superblock.copy_index, 0);
    assert_eq!(reopened.selected_superblock.generation, 1);
}

/// Flip one bit at `offset`, leaving every other byte of the file alone.
fn rot_bit(path: &Path, offset: u64) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("operation should succeed");
    file.seek(SeekFrom::Start(offset))
        .expect("seek() should succeed");
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Start(offset))
        .expect("seek() should succeed");
    file.write_all(&[byte[0] ^ 0x01]).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn manifest_bit_rot_fails_the_open_didactically_instead_of_falling_back() {
    let dir = temp_dir("manifest_checksum");
    let path = dir.path().join("data.rdb");
    let created = EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");
    assert_eq!(
        created.selected_superblock.manifest_offset,
        EMBEDDED_RDB_MANIFEST_0_OFFSET
    );

    rot_bit(&path, EMBEDDED_RDB_MANIFEST_0_OFFSET + 20);

    let err =
        EmbeddedRdbArtifact::open_strict_manifest(&path).expect_err("manifest corruption fails");
    let msg = err.to_string();
    assert!(msg.contains("manifest checksum mismatch"), "{msg}");

    // ADR 0074 §2: manifest corruption fails the open with a didactic error
    // naming the zone. Silently re-deriving the manifest from the superblock
    // would resurrect a root the checksum can no longer vouch for.
    let err = EmbeddedRdbArtifact::open(&path).expect_err("open must not paper over bit rot");
    let msg = err.to_string();
    assert!(msg.contains("manifest zone"), "{msg}");
    assert!(msg.contains("salvage"), "{msg}");
}

#[test]
fn a_checkpoint_publishes_the_manifest_into_the_inactive_slot() {
    let dir = temp_dir("manifest_pingpong");
    let path = dir.path().join("data.rdb");
    let created = EmbeddedRdbArtifact::create_with_snapshot(&path, b"RDST-v1").expect("create");
    assert_eq!(
        created.selected_superblock.manifest_offset,
        EMBEDDED_RDB_MANIFEST_0_OFFSET
    );

    let checkpointed = EmbeddedRdbArtifact::write_snapshot(&path, b"RDST-v2").expect("checkpoint");
    assert_eq!(
        checkpointed.selected_superblock.manifest_offset, EMBEDDED_RDB_MANIFEST_1_OFFSET,
        "a checkpoint must not overwrite the manifest the live superblock roots through"
    );

    // Slot 0 still holds the pre-checkpoint manifest, byte for byte: that is
    // what makes the update atomic from a reader's point of view.
    let mut file = OpenOptions::new()
        .read(true)
        .open(&path)
        .expect("open() should succeed");
    let mut slot0 = vec![0u8; 32];
    file.seek(SeekFrom::Start(EMBEDDED_RDB_MANIFEST_0_OFFSET))
        .expect("operation should succeed");
    file.read_exact(&mut slot0)
        .expect("read_exact() should succeed");
    assert_eq!(&slot0[0..8], b"RDBMNFS1");

    let next = EmbeddedRdbArtifact::write_snapshot(&path, b"RDST-v3").expect("second checkpoint");
    assert_eq!(
        next.selected_superblock.manifest_offset, EMBEDDED_RDB_MANIFEST_0_OFFSET,
        "consecutive checkpoints alternate manifest slots"
    );
}

#[test]
fn a_superblock_naming_a_manifest_outside_the_zone_is_refused() {
    let dir = temp_dir("manifest_misdirected");
    let path = dir.path().join("data.rdb");
    EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");

    // Point both superblocks' manifest_offset at the WAL region. A pointer
    // outside the zone is a misdirected or forged write; never chase it.
    for copy_offset in [
        EMBEDDED_RDB_SUPERBLOCK_0_OFFSET,
        EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
    ] {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("operation should succeed");
        // manifest_offset sits at magic(8) + version(4) + copy_index(1) +
        // generation(8) + format_version(4) = 25 bytes into the slot.
        file.seek(SeekFrom::Start(copy_offset + 25))
            .expect("operation should succeed");
        file.write_all(&EMBEDDED_RDB_MANIFEST_ZONE_END.to_le_bytes())
            .expect("operation should succeed");
        file.sync_all().expect("sync_all() should succeed");
    }

    // The superblock CRC now fails too, so this lands as a rootless store —
    // either way the pointer is never followed.
    let err = EmbeddedRdbArtifact::open(&path).expect_err("a forged pointer must not be chased");
    let msg = err.to_string();
    assert!(msg.contains("zone of"), "{msg}");
}

#[test]
fn embedded_wal_frames_are_versioned_ordered_and_chained() {
    let dir = temp_dir("wal_chain");
    let path = dir.path().join("data.rdb");
    EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");

    EmbeddedRdbArtifact::append_wal_payloads(
        &path,
        &[b"first".to_vec(), b"second".to_vec(), b"third".to_vec()],
    )
    .expect("append wal frames");

    let artifact = EmbeddedRdbArtifact::open(&path).expect("open embedded artifact");
    let payloads = EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read wal payloads");
    assert_eq!(
        payloads,
        vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec()]
    );

    let first_frame_len =
        EmbeddedRdbArtifact::wal_payloads_encoded_len(&[b"first".to_vec()]).unwrap();
    let second_previous_crc_offset = artifact.manifest.wal_region_offset + first_frame_len + 28;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("operation should succeed");
    file.seek(SeekFrom::Start(second_previous_crc_offset))
        .expect("operation should succeed");
    file.write_all(&[0xFF]).unwrap();
    file.sync_all().unwrap();

    let artifact = EmbeddedRdbArtifact::open(&path).expect("open corrupted artifact");
    let payloads =
        EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read valid wal prefix");
    assert_eq!(payloads, vec![b"first".to_vec()]);
}

#[test]
fn embedded_wal_recovery_ignores_corrupt_or_truncated_tail_frame() {
    let dir = temp_dir("wal_tail");
    let corrupt_path = dir.path().join("corrupt.rdb");
    EmbeddedRdbArtifact::create(&corrupt_path).expect("create corrupt artifact");
    EmbeddedRdbArtifact::append_wal_payloads(
        &corrupt_path,
        &[b"durable".to_vec(), b"tail".to_vec()],
    )
    .expect("append wal frames");

    let artifact = EmbeddedRdbArtifact::open(&corrupt_path).expect("open artifact");
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&corrupt_path)
        .expect("operation should succeed");
    file.seek(SeekFrom::Start(artifact.manifest.wal_recovery_boundary - 1))
        .expect("operation should succeed");
    file.write_all(&[0x00]).unwrap();
    file.sync_all().unwrap();

    let artifact = EmbeddedRdbArtifact::open(&corrupt_path).expect("open corrupt tail");
    let payloads =
        EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read valid wal prefix");
    assert_eq!(payloads, vec![b"durable".to_vec()]);

    let truncated_path = dir.path().join("truncated.rdb");
    EmbeddedRdbArtifact::create(&truncated_path).expect("create truncated artifact");
    EmbeddedRdbArtifact::append_wal_payloads(
        &truncated_path,
        &[b"durable".to_vec(), b"tail".to_vec()],
    )
    .expect("append wal frames");
    let artifact = EmbeddedRdbArtifact::open(&truncated_path).expect("open artifact");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&truncated_path)
        .expect("operation should succeed");
    file.set_len(artifact.manifest.wal_recovery_boundary - 2)
        .expect("operation should succeed");
    file.sync_all().unwrap();

    let artifact = EmbeddedRdbArtifact::open(&truncated_path).expect("open truncated tail");
    let payloads =
        EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read valid wal prefix");
    assert_eq!(payloads, vec![b"durable".to_vec()]);
}

#[test]
fn embedded_wal_checkpoint_fence_allows_wrap_after_quiescing_region() {
    let dir = temp_dir("wal_wrap_after_checkpoint");
    let path = dir.path().join("data.rdb");
    let created = EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");

    let empty_frame_len = EmbeddedRdbArtifact::wal_payloads_encoded_len(&[Vec::new()]).unwrap();
    let prefill_payload_len = created.manifest.wal_region_bytes - empty_frame_len - 8;
    let prefill = vec![0xA5; prefill_payload_len as usize];
    EmbeddedRdbArtifact::append_wal_payloads(&path, &[prefill]).expect("append prefill frame");

    let pre_checkpoint = EmbeddedRdbArtifact::open(&path).expect("open before checkpoint");
    assert_eq!(
        pre_checkpoint.manifest.wal_recovery_boundary,
        pre_checkpoint.manifest.wal_region_offset + pre_checkpoint.manifest.wal_region_bytes - 8
    );

    let checkpointed =
        EmbeddedRdbArtifact::write_snapshot(&path, b"RDST-checkpoint").expect("checkpoint");
    assert_eq!(
        checkpointed.manifest.wal_recovery_boundary,
        pre_checkpoint.manifest.wal_recovery_boundary,
        "checkpointing proves the old frames reclaimable but keeps the append fence at the tail"
    );
    assert_eq!(checkpointed.manifest.wal_live_bytes, 0);

    let wrapped = b"after-wrap".to_vec();
    EmbeddedRdbArtifact::append_wal_payloads(&path, std::slice::from_ref(&wrapped))
        .expect("append wrapped frame");

    let artifact = EmbeddedRdbArtifact::open(&path).expect("open after wrapped append");
    assert!(
        artifact.manifest.wal_recovery_boundary < artifact.manifest.wal_region_offset + 64,
        "the append boundary should wrap to the start of the region"
    );
    assert_eq!(
        artifact.manifest.wal_live_bytes,
        EmbeddedRdbArtifact::wal_payloads_encoded_len(std::slice::from_ref(&wrapped)).unwrap()
    );
    let payloads = EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read wrapped wal");
    assert_eq!(payloads, vec![wrapped]);
}

#[test]
fn embedded_wal_crash_injection_preserves_last_published_prefix() {
    const TEST_NAME: &str = "embedded_wal_crash_injection_preserves_last_published_prefix";
    if std::env::var("REDDB_EMBEDDED_RDB_CRASH_CHILD_OP")
        .ok()
        .as_deref()
        == Some("wal")
    {
        let path = PathBuf::from(std::env::var("REDDB_EMBEDDED_RDB_CRASH_CHILD_PATH").unwrap());
        EmbeddedRdbArtifact::append_wal_payloads(&path, &[b"crash".to_vec()])
            .expect("child appends crash frame");
        std::process::exit(0);
    }

    let dir = temp_dir("wal_crash_inject");
    for point in [
        "wal_after_frame_write",
        "wal_after_frame_sync",
        "wal_after_superblock_write",
    ] {
        let path = dir.path().join(format!("{point}.rdb"));
        EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");
        EmbeddedRdbArtifact::append_wal_payloads(&path, &[b"base".to_vec()])
            .expect("append base frame");

        run_crash_child(TEST_NAME, &path, "wal", point);

        let artifact = EmbeddedRdbArtifact::open(&path).expect("open after wal crash");
        let payloads =
            EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read valid wal prefix");
        assert!(
            payloads == vec![b"base".to_vec()]
                || payloads == vec![b"base".to_vec(), b"crash".to_vec()],
            "unexpected payloads after {point}: {payloads:?}"
        );

        EmbeddedRdbArtifact::append_wal_payloads(&path, &[b"after".to_vec()])
            .expect("append after crash");
        let artifact = EmbeddedRdbArtifact::open(&path).expect("open after follow-up append");
        let payloads =
            EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read follow-up wal");
        assert_eq!(payloads.last(), Some(&b"after".to_vec()));
        assert!(
            payloads.starts_with(&[b"base".to_vec()]),
            "base frame lost after {point}: {payloads:?}"
        );
    }
}

#[test]
fn embedded_wal_serializes_concurrent_appenders() {
    let dir = temp_dir("wal_concurrent");
    let path = dir.path().join("data.rdb");
    EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");

    let mut handles = Vec::new();
    for writer in 0..8u8 {
        let path = path.clone();
        handles.push(std::thread::spawn(move || {
            for seq in 0..25u8 {
                EmbeddedRdbArtifact::append_wal_payloads(&path, &[vec![writer, seq]])
                    .expect("append wal payload");
            }
        }));
    }
    for handle in handles {
        handle.join().expect("writer thread");
    }

    let artifact = EmbeddedRdbArtifact::open(&path).expect("open embedded artifact");
    let payloads = EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read wal payloads");
    assert_eq!(payloads.len(), 200);
    let mut seen = payloads;
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 200);
}

#[test]
fn embedded_snapshot_checkpoint_is_copy_on_write_until_superblock_publish() {
    let dir = temp_dir("snapshot_cow");
    let path = dir.path().join("data.rdb");
    let v1 = b"RDST-v1";
    let v2 = b"RDST-v2-new-checkpoint";
    let created = EmbeddedRdbArtifact::create_with_snapshot(&path, v1).expect("create snapshot");
    let old_offset = created.manifest.snapshot_offset;

    let checkpointed = EmbeddedRdbArtifact::write_snapshot(&path, v2).expect("write checkpoint");
    assert_ne!(checkpointed.manifest.snapshot_offset, old_offset);
    assert_eq!(
        EmbeddedRdbArtifact::read_snapshot(&checkpointed)
            .expect("read new snapshot")
            .unwrap(),
        v2
    );

    let newer_copy_offset = if checkpointed.selected_superblock.copy_index == 0 {
        EMBEDDED_RDB_SUPERBLOCK_0_OFFSET
    } else {
        EMBEDDED_RDB_SUPERBLOCK_1_OFFSET
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("operation should succeed");
    file.seek(SeekFrom::Start(newer_copy_offset + 64)).unwrap();
    file.write_all(&[0xA5]).unwrap();
    file.sync_all().unwrap();

    let recovered = EmbeddedRdbArtifact::open(&path).expect("fallback to prior superblock");
    assert_eq!(
        EmbeddedRdbArtifact::read_snapshot(&recovered)
            .expect("read old snapshot")
            .unwrap(),
        v1
    );
}

#[test]
fn embedded_open_skips_newer_superblock_when_snapshot_checksum_fails() {
    let dir = temp_dir("snapshot_checksum_fallback");
    let path = dir.path().join("data.rdb");
    let v1 = b"RDST-good";
    let v2 = b"RDST-bad-checkpoint";
    EmbeddedRdbArtifact::create_with_snapshot(&path, v1).expect("create snapshot");
    let checkpointed = EmbeddedRdbArtifact::write_snapshot(&path, v2).expect("write checkpoint");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("operation should succeed");
    file.seek(SeekFrom::Start(checkpointed.manifest.snapshot_offset + 5))
        .expect("operation should succeed");
    file.write_all(&[0xFF]).unwrap();
    file.sync_all().unwrap();

    let recovered = EmbeddedRdbArtifact::open(&path).expect("fallback to prior snapshot");
    assert_eq!(
        EmbeddedRdbArtifact::read_snapshot(&recovered)
            .expect("read prior snapshot")
            .unwrap(),
        v1
    );
}

#[test]
fn embedded_snapshot_crash_injection_preserves_published_snapshot() {
    const TEST_NAME: &str = "embedded_snapshot_crash_injection_preserves_published_snapshot";
    if std::env::var("REDDB_EMBEDDED_RDB_CRASH_CHILD_OP")
        .ok()
        .as_deref()
        == Some("snapshot")
    {
        let path = PathBuf::from(std::env::var("REDDB_EMBEDDED_RDB_CRASH_CHILD_PATH").unwrap());
        EmbeddedRdbArtifact::write_snapshot(&path, b"RDST-crash-checkpoint")
            .expect("child writes crash snapshot");
        std::process::exit(0);
    }

    let dir = temp_dir("snapshot_crash_inject");
    for point in [
        "snapshot_after_image_write",
        "snapshot_after_image_sync",
        "snapshot_after_manifest_write",
        "snapshot_after_manifest_sync",
        "snapshot_after_superblock_write",
    ] {
        let path = dir.path().join(format!("{point}.rdb"));
        EmbeddedRdbArtifact::create_with_snapshot(&path, b"RDST-base")
            .expect("create base snapshot");

        run_crash_child(TEST_NAME, &path, "snapshot", point);

        let artifact = EmbeddedRdbArtifact::open(&path).expect("open after snapshot crash");
        let snapshot = EmbeddedRdbArtifact::read_snapshot(&artifact)
            .expect("read snapshot")
            .expect("operation should succeed");
        assert!(
            snapshot == b"RDST-base".to_vec() || snapshot == b"RDST-crash-checkpoint".to_vec(),
            "unexpected snapshot after {point}: {snapshot:?}"
        );

        EmbeddedRdbArtifact::write_snapshot(&path, b"RDST-after-crash").expect("write after crash");
        let artifact = EmbeddedRdbArtifact::open(&path).expect("open after follow-up checkpoint");
        assert_eq!(
            EmbeddedRdbArtifact::read_snapshot(&artifact)
                .expect("read follow-up snapshot")
                .unwrap(),
            b"RDST-after-crash".to_vec()
        );
    }
}
