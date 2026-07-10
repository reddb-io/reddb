use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};

use reddb_file::{
    append_native_store_crc32_footer, encode_native_store_header, salvage_embedded_store,
    EmbeddedRdbArtifact, StorageSalvageMode, EMBEDDED_RDB_MANIFEST_0_OFFSET,
    EMBEDDED_RDB_MANIFEST_1_OFFSET, EMBEDDED_RDB_MANIFEST_SLOT_SIZE,
    EMBEDDED_RDB_SUPERBLOCK_0_OFFSET, EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
    EMBEDDED_RDB_SUPERBLOCK_SIZE,
};

fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-salvage-{label}-"))
        .tempdir()
        .expect("temp dir")
}

#[test]
fn salvage_healthy_store_copies_verified_payloads_into_fresh_store_without_source_writes() {
    let dir = temp_dir("healthy");
    let source = dir.path().join("source.rdb");
    let destination = dir.path().join("recovered.rdb");
    let snapshot = native_snapshot();
    let wal_payloads = vec![b"insert-events-1".to_vec(), b"insert-events-2".to_vec()];

    EmbeddedRdbArtifact::create_with_snapshot(&source, &snapshot).expect("create source");
    EmbeddedRdbArtifact::append_wal_payloads(&source, &wal_payloads).expect("append wal payloads");

    let before = fs::read(&source).expect("read source before salvage");
    let report = salvage_embedded_store(&source, &destination).expect("salvage healthy store");
    let after = fs::read(&source).expect("read source after salvage");

    assert_eq!(before, after, "salvage must never mutate the source store");
    assert_eq!(report.mode, StorageSalvageMode::Manifest);
    assert_eq!(report.skipped_regions.len(), 0);
    assert_eq!(report.collections.len(), 1);
    assert_eq!(report.collections[0].collection, "embedded_snapshot");
    assert_eq!(report.collections[0].recovered_entities, 3);
    assert_eq!(report.collections[0].skipped_entities, 0);

    let recovered = EmbeddedRdbArtifact::open(&destination).expect("open recovered store");
    assert_eq!(
        EmbeddedRdbArtifact::read_snapshot(&recovered).expect("read recovered snapshot"),
        Some(snapshot)
    );
    assert_eq!(
        EmbeddedRdbArtifact::read_wal_payloads(&recovered).expect("read recovered wal"),
        wal_payloads
    );

    let machine = serde_json::to_value(&report).expect("report serializes");
    assert_eq!(machine["schema_version"], 1);
    assert_eq!(machine["mode"], "manifest");
    assert_eq!(
        machine["collections"][0]["collection"],
        serde_json::Value::String("embedded_snapshot".to_string())
    );

    let summary = report.human_summary();
    assert!(summary.contains("embedded_snapshot"), "{summary}");
    assert!(summary.contains("recovered 3"), "{summary}");
    assert!(summary.contains("Next steps"), "{summary}");
}

#[test]
fn salvage_carves_snapshot_when_superblocks_and_manifest_are_destroyed() {
    let dir = temp_dir("carving");
    let source = dir.path().join("source.rdb");
    let destination = dir.path().join("recovered.rdb");
    let snapshot = native_snapshot();

    EmbeddedRdbArtifact::create_with_snapshot(&source, &snapshot).expect("create source");
    zero_region(
        &source,
        EMBEDDED_RDB_SUPERBLOCK_0_OFFSET,
        EMBEDDED_RDB_SUPERBLOCK_SIZE,
    );
    zero_region(
        &source,
        EMBEDDED_RDB_SUPERBLOCK_1_OFFSET,
        EMBEDDED_RDB_SUPERBLOCK_SIZE,
    );
    zero_region(
        &source,
        EMBEDDED_RDB_MANIFEST_0_OFFSET,
        EMBEDDED_RDB_MANIFEST_SLOT_SIZE,
    );
    zero_region(
        &source,
        EMBEDDED_RDB_MANIFEST_1_OFFSET,
        EMBEDDED_RDB_MANIFEST_SLOT_SIZE,
    );

    EmbeddedRdbArtifact::open(&source).expect_err("destroyed roots must not open normally");
    let before = fs::read(&source).expect("read source before salvage");
    let report = salvage_embedded_store(&source, &destination).expect("salvage by carving");
    let after = fs::read(&source).expect("read source after salvage");

    assert_eq!(
        before, after,
        "carving salvage must not mutate the source store"
    );
    assert_eq!(report.mode, StorageSalvageMode::Carving);
    assert_eq!(report.collections[0].collection, "embedded_snapshot");
    assert_eq!(report.collections[0].recovered_entities, 1);
    assert!(report
        .skipped_regions
        .iter()
        .any(|region| region.zone_kind == "superblock"));
    assert!(report
        .skipped_regions
        .iter()
        .any(|region| region.zone_kind == "manifest"));

    let recovered = EmbeddedRdbArtifact::open(&destination).expect("open recovered store");
    assert_eq!(
        EmbeddedRdbArtifact::read_snapshot(&recovered).expect("read recovered snapshot"),
        Some(snapshot)
    );

    let machine = serde_json::to_value(&report).expect("report serializes");
    assert_eq!(machine["mode"], "carving");
}

#[test]
fn salvage_skips_checksum_failed_snapshot_and_still_writes_fresh_store() {
    let dir = temp_dir("snapshot-bit-rot");
    let source = dir.path().join("source.rdb");
    let destination = dir.path().join("recovered.rdb");
    let snapshot = native_snapshot();

    let open =
        EmbeddedRdbArtifact::create_with_snapshot(&source, &snapshot).expect("create source");
    flip_byte(&source, open.manifest.snapshot_offset + 4);

    EmbeddedRdbArtifact::open(&source).expect_err("snapshot corruption must fail normal open");
    let before = fs::read(&source).expect("read source before salvage");
    let report = salvage_embedded_store(&source, &destination).expect("salvage damaged snapshot");
    let after = fs::read(&source).expect("read source after salvage");

    assert_eq!(before, after, "salvage must not mutate the damaged source");
    assert_eq!(report.mode, StorageSalvageMode::Manifest);
    assert_eq!(report.collections[0].recovered_entities, 0);
    assert_eq!(report.collections[0].skipped_entities, 1);
    assert!(report
        .skipped_regions
        .iter()
        .any(|region| region.zone_kind == "page" && region.reason == "checksum mismatch"));

    let recovered = EmbeddedRdbArtifact::open(&destination).expect("open recovered empty store");
    assert_eq!(
        EmbeddedRdbArtifact::read_snapshot(&recovered).expect("read empty snapshot"),
        None
    );
}

fn zero_region(path: &std::path::Path, offset: u64, len: u64) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open source for corruption");
    file.seek(SeekFrom::Start(offset)).expect("seek corruption");
    file.write_all(&vec![0u8; len as usize])
        .expect("zero corruption region");
    file.sync_all().expect("sync corruption");
}

fn flip_byte(path: &std::path::Path, offset: u64) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open source for corruption");
    file.seek(SeekFrom::Start(offset)).expect("seek corruption");
    file.write_all(&[0xA5]).expect("write corruption byte");
    file.sync_all().expect("sync corruption");
}

fn native_snapshot() -> Vec<u8> {
    let mut snapshot = encode_native_store_header(reddb_file::native_store::STORE_VERSION_CURRENT);
    append_native_store_crc32_footer(&mut snapshot);
    snapshot
}
