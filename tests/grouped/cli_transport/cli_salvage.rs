use std::path::PathBuf;
use std::process::{Command, Stdio};

fn red_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_red"))
}

#[test]
fn salvage_json_writes_fresh_store_and_reports_zero_loss_for_healthy_source() {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("source.rdb");
    let destination = dir.path().join("recovered.rdb");
    let mut snapshot =
        reddb_file::encode_native_store_header(reddb_file::native_store::STORE_VERSION_CURRENT);
    reddb_file::append_native_store_crc32_footer(&mut snapshot);

    reddb_file::EmbeddedRdbArtifact::create_with_snapshot(&source, &snapshot)
        .expect("create source");

    let output = Command::new(red_binary())
        .args([
            "salvage",
            "--source",
            source.to_str().unwrap(),
            "--destination",
            destination.to_str().unwrap(),
            "--json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run red salvage");

    assert!(
        output.status.success(),
        "salvage failed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["command"], "salvage");
    assert_eq!(body["data"]["schema_version"], 1);
    assert_eq!(body["data"]["mode"], "manifest");
    assert_eq!(body["data"]["skipped_regions"].as_array().unwrap().len(), 0);

    let recovered = reddb_file::EmbeddedRdbArtifact::open(&destination).expect("open recovered");
    assert_eq!(
        reddb_file::EmbeddedRdbArtifact::read_snapshot(&recovered)
            .expect("read recovered snapshot"),
        Some(snapshot)
    );
}
