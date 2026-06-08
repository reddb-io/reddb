use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use reddb_server::storage::schema::Value;
use reddb_server::storage::{
    EmbeddedRdbArtifact, EntityId, UnifiedStore, UnifiedStoreConfig, EMBEDDED_RDB_MANIFEST_OFFSET,
    EMBEDDED_RDB_SUPERBLOCK_0_OFFSET, EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
};
use reddb_server::{RedDBOptions, RedDBRuntime};

fn temp_dir(label: &str) -> PathBuf {
    let unique = format!(
        "reddb_embedded_rdb_{label}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let dir = std::env::temp_dir().join(unique);
    fs::create_dir_all(&dir).unwrap();
    dir
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
    let path = dir.join("data.rdb");

    let created = EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");
    assert_eq!(created.selected_superblock.copy_index, 1);
    assert_eq!(created.selected_superblock.generation, 2);
    assert_eq!(
        created.selected_superblock.manifest_offset,
        EMBEDDED_RDB_MANIFEST_OFFSET
    );
    assert_eq!(
        created.manifest.wal_recovery_boundary,
        created.manifest.wal_region_offset
    );

    let reopened = EmbeddedRdbArtifact::open(&path).expect("open embedded rdb");
    assert_eq!(reopened.selected_superblock.copy_index, 1);
    assert_eq!(reopened.selected_superblock.generation, 2);
    assert_eq!(reopened.manifest.checksum, created.manifest.checksum);
    assert_eq!(artifact_names(&dir), vec!["data.rdb"]);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn open_falls_back_to_older_superblock_when_newer_copy_is_invalid() {
    let dir = temp_dir("superblock_fallback");
    let path = dir.join("data.rdb");
    EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    file.seek(SeekFrom::Start(EMBEDDED_RDB_SUPERBLOCK_1_OFFSET + 64))
        .unwrap();
    file.write_all(&[0xA5]).unwrap();
    file.sync_all().unwrap();

    let reopened = EmbeddedRdbArtifact::open(&path).expect("open falls back");
    assert_eq!(reopened.selected_superblock.copy_index, 0);
    assert_eq!(reopened.selected_superblock.generation, 1);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn open_validates_manifest_checksum_from_selected_superblock() {
    let dir = temp_dir("manifest_checksum");
    let path = dir.join("data.rdb");
    EmbeddedRdbArtifact::create(&path).expect("create embedded rdb");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    file.seek(SeekFrom::Start(EMBEDDED_RDB_MANIFEST_OFFSET + 20))
        .unwrap();
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte).unwrap();
    file.seek(SeekFrom::Start(EMBEDDED_RDB_MANIFEST_OFFSET + 20))
        .unwrap();
    file.write_all(&[byte[0] ^ 0x01]).unwrap();
    file.sync_all().unwrap();

    let err =
        EmbeddedRdbArtifact::open_strict_manifest(&path).expect_err("manifest corruption fails");
    let msg = err.to_string();
    assert!(msg.contains("manifest checksum mismatch"), "{msg}");

    let recovered = EmbeddedRdbArtifact::open(&path).expect("open recovers from superblock");
    assert_eq!(recovered.manifest.wal_region_offset, 12288);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn embedded_wal_frames_are_versioned_ordered_and_chained() {
    let dir = temp_dir("wal_chain");
    let path = dir.join("data.rdb");
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
        .unwrap();
    file.seek(SeekFrom::Start(second_previous_crc_offset))
        .unwrap();
    file.write_all(&[0xFF]).unwrap();
    file.sync_all().unwrap();

    let artifact = EmbeddedRdbArtifact::open(&path).expect("open corrupted artifact");
    let payloads =
        EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read valid wal prefix");
    assert_eq!(payloads, vec![b"first".to_vec()]);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn embedded_wal_recovery_ignores_corrupt_or_truncated_tail_frame() {
    let dir = temp_dir("wal_tail");
    let corrupt_path = dir.join("corrupt.rdb");
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
        .unwrap();
    file.seek(SeekFrom::Start(artifact.manifest.wal_recovery_boundary - 1))
        .unwrap();
    file.write_all(&[0x00]).unwrap();
    file.sync_all().unwrap();

    let artifact = EmbeddedRdbArtifact::open(&corrupt_path).expect("open corrupt tail");
    let payloads =
        EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read valid wal prefix");
    assert_eq!(payloads, vec![b"durable".to_vec()]);

    let truncated_path = dir.join("truncated.rdb");
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
        .unwrap();
    file.set_len(artifact.manifest.wal_recovery_boundary - 2)
        .unwrap();
    file.sync_all().unwrap();

    let artifact = EmbeddedRdbArtifact::open(&truncated_path).expect("open truncated tail");
    let payloads =
        EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read valid wal prefix");
    assert_eq!(payloads, vec![b"durable".to_vec()]);

    fs::remove_dir_all(dir).unwrap();
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
        let path = dir.join(format!("{point}.rdb"));
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

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn embedded_wal_serializes_concurrent_appenders() {
    let dir = temp_dir("wal_concurrent");
    let path = dir.join("data.rdb");
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

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn embedded_snapshot_checkpoint_is_copy_on_write_until_superblock_publish() {
    let dir = temp_dir("snapshot_cow");
    let path = dir.join("data.rdb");
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
        .unwrap();
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

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn embedded_open_skips_newer_superblock_when_snapshot_checksum_fails() {
    let dir = temp_dir("snapshot_checksum_fallback");
    let path = dir.join("data.rdb");
    let v1 = b"RDST-good";
    let v2 = b"RDST-bad-checkpoint";
    EmbeddedRdbArtifact::create_with_snapshot(&path, v1).expect("create snapshot");
    let checkpointed = EmbeddedRdbArtifact::write_snapshot(&path, v2).expect("write checkpoint");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    file.seek(SeekFrom::Start(checkpointed.manifest.snapshot_offset + 5))
        .unwrap();
    file.write_all(&[0xFF]).unwrap();
    file.sync_all().unwrap();

    let recovered = EmbeddedRdbArtifact::open(&path).expect("fallback to prior snapshot");
    assert_eq!(
        EmbeddedRdbArtifact::read_snapshot(&recovered)
            .expect("read prior snapshot")
            .unwrap(),
        v1
    );

    fs::remove_dir_all(dir).unwrap();
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
        "snapshot_after_superblock_write",
    ] {
        let path = dir.join(format!("{point}.rdb"));
        EmbeddedRdbArtifact::create_with_snapshot(&path, b"RDST-base")
            .expect("create base snapshot");

        run_crash_child(TEST_NAME, &path, "snapshot", point);

        let artifact = EmbeddedRdbArtifact::open(&path).expect("open after snapshot crash");
        let snapshot = EmbeddedRdbArtifact::read_snapshot(&artifact)
            .expect("read snapshot")
            .unwrap();
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

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn embedded_runtime_persists_table_data_inside_single_rdb_file() {
    let dir = temp_dir("runtime_single_file");
    let path = dir.join("data.rdb");

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("open runtime");
        rt.execute_query("CREATE TABLE users (id INT, name TEXT)")
            .expect("create table");
        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'ada'), (2, 'linus')")
            .expect("insert rows");
        rt.flush().expect("flush embedded artifact");
    }

    assert_eq!(artifact_names(&dir), vec!["data.rdb"]);
    let artifact = EmbeddedRdbArtifact::open(&path).expect("open embedded artifact");
    assert_eq!(artifact.manifest.snapshot_bytes > 0, true);
    assert!(EmbeddedRdbArtifact::read_snapshot(&artifact)
        .expect("read snapshot")
        .is_some());

    {
        let rt =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen runtime");
        let rows = rt
            .execute_query("SELECT * FROM users")
            .expect("select rows");
        assert_eq!(rows.result.records.len(), 2);
        rt.flush().expect("flush reopened artifact");
    }

    assert_eq!(artifact_names(&dir), vec!["data.rdb"]);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn embedded_runtime_replays_internal_wal_without_flush_or_drop() {
    if let Ok(path) = std::env::var("REDDB_EMBEDDED_RDB_WAL_CHILD_PATH") {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path))
            .expect("child opens runtime");
        rt.execute_query("CREATE TABLE events (id INT, body TEXT)")
            .expect("child creates table");
        rt.execute_query("INSERT INTO events (id, body) VALUES (1, 'boot'), (2, 'commit')")
            .expect("child inserts rows");
        std::process::exit(0);
    }

    let dir = temp_dir("internal_wal_replay");
    let path = dir.join("data.rdb");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("embedded_runtime_replays_internal_wal_without_flush_or_drop")
        .arg("--nocapture")
        .env("REDDB_EMBEDDED_RDB_WAL_CHILD_PATH", &path)
        .output()
        .expect("run child test process");
    assert!(
        output.status.success(),
        "child failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(artifact_names(&dir), vec!["data.rdb"]);
    let artifact = EmbeddedRdbArtifact::open(&path).expect("open embedded artifact");
    assert_eq!(artifact.manifest.snapshot_bytes, 0);
    assert!(
        artifact.manifest.wal_recovery_boundary > artifact.manifest.wal_region_offset,
        "expected committed frames in internal wal"
    );

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen runtime");
    let rows = rt
        .execute_query("SELECT * FROM events")
        .expect("select replayed rows");
    assert_eq!(rows.result.records.len(), 2);
    rt.flush().expect("checkpoint replayed state");

    let checkpointed = EmbeddedRdbArtifact::open(&path).expect("open checkpointed artifact");
    assert_eq!(
        checkpointed.manifest.wal_recovery_boundary,
        checkpointed.manifest.wal_region_offset
    );
    assert!(checkpointed.manifest.snapshot_bytes > 0);
    assert_eq!(artifact_names(&dir), vec!["data.rdb"]);

    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn embedded_runtime_checkpoints_expands_and_retries_when_internal_wal_fills() {
    let dir = temp_dir("internal_wal_expand");
    let path = dir.join("data.rdb");

    EmbeddedRdbArtifact::create(&path).expect("create embedded artifact");
    let store =
        UnifiedStore::with_config(UnifiedStoreConfig::default().with_embedded_wal_path(&path));
    store.create_collection("blobs").expect("create collection");
    let mut seed = 0xD00D_F00D_CAFE_BABEu64;
    let body: Vec<u8> = (0..100_000)
        .map(|_| {
            seed ^= seed << 7;
            seed ^= seed >> 9;
            seed ^= seed << 8;
            (seed & 0xFF) as u8
        })
        .collect();
    let entity = reddb_server::storage::UnifiedEntity::table_row(
        EntityId::new(1),
        "blobs",
        1,
        vec![Value::Blob(body)],
    );
    store
        .insert_auto("blobs", entity)
        .expect("insert large row");

    assert_eq!(artifact_names(&dir), vec!["data.rdb"]);
    let artifact = EmbeddedRdbArtifact::open(&path).expect("open embedded artifact");
    assert!(artifact.manifest.snapshot_bytes > 0);
    assert!(artifact.manifest.wal_region_bytes > 64 * 1024);
    assert!(
        artifact.manifest.wal_recovery_boundary > artifact.manifest.wal_region_offset,
        "expected retried frame after checkpoint"
    );

    let frames = EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read wal payloads");
    assert!(!frames.is_empty(), "expected retried wal frame");

    fs::remove_dir_all(dir).unwrap();
}
