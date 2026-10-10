#![cfg(feature = "backend-s3")]

use reddb_server::storage::backend::{RemoteBackend, S3Backend, S3Config};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveFixture {
    endpoint: String,
    bucket: String,
    prefix: String,
    first_credentials_file: PathBuf,
    second_credentials_file: PathBuf,
}

/// Deliberately opt-in: requires newly minted, single-prefix staging credentials.
/// Writes only a unique synthetic object, preserving it for independent review.
#[test]
#[ignore = "requires REDDB_S3_ROTATION_FIXTURE with scoped staging credentials"]
fn rotating_credentials_put_get_list_without_restart() {
    let fixture_path = std::env::var("REDDB_S3_ROTATION_FIXTURE").expect("explicit fixture path");
    let fixture: LiveFixture =
        serde_json::from_slice(&fs::read(fixture_path).expect("fixture")).expect("fixture schema");
    assert!(fixture.endpoint.starts_with("https://"));
    assert_eq!(fixture.bucket, "rdb-lair-stg-storage-qualification");
    assert!(fixture.prefix.starts_with("qualification/") && fixture.prefix.ends_with('/'));
    let directory = tempfile::tempdir().expect("private local directory");
    let path = directory.path().join("credentials.json");
    fs::copy(&fixture.first_credentials_file, &path).expect("first scoped credentials");
    let config = S3Config::generic(&fixture.endpoint, &fixture.bucket, "", "")
        .with_region("auto")
        .with_prefix(&fixture.prefix);
    let backend = S3Backend::new(config).with_credentials_file(&path);
    let body = b"native S3 temporary credential rotation: synthetic payload";
    let source = directory.path().join("payload");
    fs::write(&source, body).expect("synthetic source");
    backend
        .upload(&source, "before-rotation.txt")
        .expect("first upload");
    let destination = directory.path().join("first-readback");
    backend
        .download("before-rotation.txt", &destination)
        .expect("first download");
    assert_eq!(fs::read(&destination).expect("readback"), body);
    assert!(backend.exists("before-rotation.txt").expect("HEAD"));
    let first: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).expect("first file")).expect("first JSON");
    let second: serde_json::Value =
        serde_json::from_slice(&fs::read(&fixture.second_credentials_file).expect("second file"))
            .expect("second JSON");
    assert!(
        first["session_token"] != second["session_token"]
            && first["secret_access_key"] != second["secret_access_key"]
    );
    let replacement = directory.path().join("replacement.json");
    fs::copy(&fixture.second_credentials_file, &replacement).expect("replacement credentials");
    fs::rename(&replacement, &path).expect("atomic rotation");
    backend
        .upload(&source, "after-rotation.txt")
        .expect("rotated upload");
    backend
        .download("before-rotation.txt", &destination)
        .expect("rotated old-object download");
    assert_eq!(fs::read(&destination).expect("readback"), body);
    backend
        .download("after-rotation.txt", &destination)
        .expect("rotated new-object download");
    assert_eq!(fs::read(&destination).expect("readback"), body);
    let listed = backend.list("").expect("scoped listing");
    assert!(listed
        .iter()
        .any(|key| key.ends_with("before-rotation.txt")));
    assert!(listed.iter().any(|key| key.ends_with("after-rotation.txt")));
    fs::remove_file(&path).expect("remove only local test credentials");
    assert!(backend.upload(&source, "must-not-exist.txt").is_err());
    assert!(backend
        .download("before-rotation.txt", &destination)
        .is_err());
}
