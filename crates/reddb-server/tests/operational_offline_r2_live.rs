#![cfg(all(feature = "backend-s3", target_os = "linux"))]

use reddb_server::storage::backend::{RemoteBackend, S3Backend, S3Config};
use reddb_server::{RedDBOptions, RedDBRuntime};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OfflineFixture {
    endpoint: String,
    bucket: String,
    prefix: String,
    first_credentials_file: PathBuf,
    second_credentials_file: PathBuf,
    snapshot_archive: PathBuf,
    expected_archive_sha256: String,
    db_relative_path: String,
}

/// Requires an immutable, complete archive from a stopped and independently
/// fenced synthetic staging writer. Does not qualify online checkpoints/PITR.
#[test]
#[ignore = "requires REDDB_OFFLINE_R2_FIXTURE and a fenced synthetic staging archive"]
fn complete_offline_layout_survives_native_r2_transport_and_restore() {
    let fixture_path = std::env::var("REDDB_OFFLINE_R2_FIXTURE").expect("explicit fixture path");
    let fixture: OfflineFixture =
        serde_json::from_slice(&fs::read(fixture_path).expect("private fixture"))
            .expect("fixture schema");
    assert!(fixture.endpoint.starts_with("https://"));
    assert!(fixture.bucket.contains("-stg-") && fixture.bucket.ends_with("-storage-qualification"));
    assert!(fixture.prefix.starts_with("qualification/") && fixture.prefix.ends_with('/'));
    assert_eq!(fixture.db_relative_path, "data.rdb");
    let source_hash =
        reddb_file::SnapshotManifest::compute_snapshot_sha256(&fixture.snapshot_archive)
            .expect("source archive digest");
    assert_eq!(source_hash, fixture.expected_archive_sha256);
    let config = S3Config::generic(&fixture.endpoint, &fixture.bucket, "", "")
        .with_region("auto")
        .with_prefix(&fixture.prefix);
    let backend = S3Backend::new(config).with_credentials_file(&fixture.first_credentials_file);
    backend
        .upload(&fixture.snapshot_archive, "offline-layout/full-store.tar")
        .expect("native upload of complete fenced layout");
    assert!(backend
        .exists("offline-layout/full-store.tar")
        .expect("remote HEAD"));
    let restore_config = S3Config::generic(&fixture.endpoint, &fixture.bucket, "", "")
        .with_region("auto")
        .with_prefix(&fixture.prefix);
    // Independent client and credentials; no dependence on a process cache.
    let restore_backend =
        S3Backend::new(restore_config).with_credentials_file(&fixture.second_credentials_file);
    let directory = tempfile::tempdir().expect("independent private restore directory");
    let archive = directory.path().join("download.tar");
    restore_backend
        .download("offline-layout/full-store.tar", &archive)
        .expect("native independent download");
    assert_eq!(
        reddb_file::SnapshotManifest::compute_snapshot_sha256(&archive).unwrap(),
        source_hash,
    );
    let destination = directory.path().join("restored");
    fs::create_dir(&destination).unwrap();
    let extract = Command::new("python3")
        .args(["-c", "import pathlib,sys,tarfile; a=tarfile.open(sys.argv[1]); members=a.getmembers(); assert all(m.isfile() or m.isdir() for m in members); assert all(not pathlib.PurePosixPath(m.name).is_absolute() and '..' not in pathlib.PurePosixPath(m.name).parts for m in members); a.extractall(sys.argv[2],filter='data')"])
        .arg(&archive)
        .arg(&destination)
        .output()
        .expect("safe archive extractor (Python 3.12+)");
    assert!(extract.status.success(), "archive extraction failed");
    let database = destination.join(&fixture.db_relative_path);
    assert!(database
        .with_extension("rdb.ops")
        .join("collections")
        .is_dir());
    let mut options = RedDBOptions::persistent(&database);
    options.storage_profile.packaging =
        reddb_server::storage::StoragePackaging::OperationalDirectory;
    let restored = RedDBRuntime::with_options(options).expect("independent operational runtime");
    let result = restored
        .execute_query("SELECT id, value FROM standard_persistence ORDER BY id")
        .expect("restored native data")
        .result;
    assert_eq!(result.len(), 125);
    for (index, record) in result.records.iter().enumerate() {
        assert_eq!(
            record.get("id").unwrap().as_integer(),
            Some((index + 1) as i64)
        );
        assert_eq!(
            record.get("value").unwrap().as_text(),
            Some(format!("standard-value-{}", index + 1).as_str())
        );
    }
}
