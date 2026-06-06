use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use reddb_server::storage::{
    EmbeddedRdbArtifact, EMBEDDED_RDB_MANIFEST_OFFSET, EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
};

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

    let err = EmbeddedRdbArtifact::open(&path).expect_err("manifest corruption fails");
    let msg = err.to_string();
    assert!(msg.contains("manifest checksum mismatch"), "{msg}");

    fs::remove_dir_all(dir).unwrap();
}
