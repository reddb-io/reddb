#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use reddb::storage::engine::PAGE_SIZE;
use reddb::{RedDBError, RedDBOptions, RedDBRuntime, StorageDeployPreset};

fn persistent_operational_options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
        .expect("primary-replica production storage profile is valid")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}: {err:?}"));
}

fn corrupt_page(path: &Path, page_id: u64) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open database file");
    let offset = page_id * PAGE_SIZE as u64 + 128;
    file.seek(SeekFrom::Start(offset)).expect("seek page byte");
    let mut byte = [0u8; 1];
    file.read_exact(&mut byte).expect("read page byte");
    file.seek(SeekFrom::Start(offset))
        .expect("seek page byte again");
    file.write_all(&[byte[0] ^ 0x01]).expect("flip page byte");
    file.sync_all().expect("sync corrupted page");
}

fn page_containing(path: &Path, needle: &[u8]) -> u64 {
    let bytes = fs::read(path).expect("read database file");
    let pos = bytes
        .windows(needle.len())
        .position(|window| window == needle)
        .unwrap_or_else(|| panic!("needle {needle:?} not found in database file"));
    (pos / PAGE_SIZE) as u64
}

fn assert_integrity_error(err: RedDBError, zone: &str, id: &str, collection: Option<&str>) {
    let RedDBError::StorageIntegrity(integrity) = err else {
        panic!("expected storage integrity error, got {err:?}");
    };
    assert_eq!(integrity.zone, zone);
    assert_eq!(integrity.id, id);
    assert_eq!(integrity.collection.as_deref(), collection);
    let rendered = integrity.to_string();
    assert!(rendered.contains("refused to serve unverified bytes"));
    assert!(rendered.contains("scrub"));
    assert!(rendered.contains("salvage"));
}

fn expect_runtime_open_err(result: Result<RedDBRuntime, RedDBError>) -> RedDBError {
    match result {
        Ok(_) => panic!("corrupted store must fail closed"),
        Err(err) => err,
    }
}

#[test]
fn data_page_checksum_failure_reports_page_id_and_collection() {
    let db = support::temp_db_file("storage-integrity-page");
    {
        let rt = RedDBRuntime::with_options(persistent_operational_options(db.path()))
            .expect("runtime opens");
        exec(&rt, "CREATE TABLE bad_rows (id INT, label TEXT)");
        exec(&rt, "CREATE TABLE healthy_rows (id INT, label TEXT)");
        exec(
            &rt,
            "INSERT INTO bad_rows (id, label) VALUES (1, 'needle-row-1963')",
        );
        exec(
            &rt,
            "INSERT INTO healthy_rows (id, label) VALUES (1, 'still-ok')",
        );
        rt.checkpoint().expect("checkpoint fixture");
    }

    let page_id = page_containing(db.path(), b"needle-row-1963");
    corrupt_page(db.path(), page_id);

    let err = expect_runtime_open_err(RedDBRuntime::with_options(persistent_operational_options(
        db.path(),
    )));
    assert_integrity_error(err, "page", &page_id.to_string(), Some("bad_rows"));
}

#[test]
fn manifest_checksum_failure_reports_manifest_zone() {
    let db = support::temp_db_file("storage-integrity-manifest");
    {
        let rt = RedDBRuntime::with_options(persistent_operational_options(db.path()))
            .expect("runtime opens");
        exec(&rt, "CREATE TABLE manifest_probe (id INT)");
        rt.checkpoint().expect("checkpoint fixture");
    }

    let manifest =
        reddb_file::OperationalManifest::for_db_path(db.path()).current_manifest_path_for_test();
    let mut bytes = fs::read(&manifest).expect("read manifest");
    let checksum_pos = bytes
        .windows(b"checksum".len())
        .position(|window| window == b"checksum")
        .expect("manifest has checksum field");
    bytes[checksum_pos] ^= 0x01;
    fs::write(&manifest, bytes).expect("write corrupted manifest");

    let err = expect_runtime_open_err(RedDBRuntime::with_options(persistent_operational_options(
        db.path(),
    )));
    assert_integrity_error(err, "manifest", "current", None);
}

#[test]
fn append_only_segment_chunk_checksum_failure_is_detected_before_rows_decode() {
    let rows = vec![reddb_file::AppendOnlySegmentRow {
        primary_key: b"1".to_vec(),
        payload: b"segment-row-1963".to_vec(),
    }];
    let mut bytes =
        reddb_file::encode_append_only_segment(reddb_file::AppendOnlySegmentCodec::None, &rows)
            .expect("encode segment");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;

    let err = reddb_file::decode_append_only_segment(&bytes)
        .expect_err("corrupted segment chunk must fail before row decode");
    assert!(
        err.to_string().contains("segment") && err.to_string().contains("checksum"),
        "unexpected segment error: {err}"
    );
}
