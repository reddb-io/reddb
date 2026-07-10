use std::fs;
use std::path::Path;

use reddb_file::EmbeddedRdbArtifact;
use reddb_server::storage::schema::Value;
use reddb_server::storage::{EntityId, UnifiedStore, UnifiedStoreConfig};
use reddb_server::{RedDBOptions, RedDBRuntime};

/// Auto-cleaning temp dir: the returned [`tempfile::TempDir`] guard removes the
/// directory and the `.rdb` artifact (incl. internal WAL) on drop, including on
/// panic. The caller keeps the binding alive and reads paths via `dir.path()`.
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

#[test]
fn embedded_runtime_persists_table_data_inside_single_rdb_file() {
    let dir = temp_dir("runtime_single_file");
    let path = dir.path().join("data.rdb");

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("open runtime");
        rt.execute_query("CREATE TABLE users (id INT, name TEXT)")
            .expect("create table");
        rt.execute_query("INSERT INTO users (id, name) VALUES (1, 'ada'), (2, 'linus')")
            .expect("insert rows");
        rt.flush().expect("flush embedded artifact");
    }

    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);
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

    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);
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
    let path = dir.path().join("data.rdb");
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

    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);
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
        checkpointed.manifest.wal_checkpoint_boundary
    );
    assert!(checkpointed.manifest.snapshot_bytes > 0);
    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);
}

#[test]
fn embedded_runtime_checkpoints_and_retries_when_internal_wal_fills() {
    let dir = temp_dir("internal_wal_checkpoint_retry");
    let path = dir.path().join("data.rdb");

    EmbeddedRdbArtifact::create(&path).expect("create embedded artifact");
    let store =
        UnifiedStore::with_config(UnifiedStoreConfig::default().with_embedded_wal_path(&path));
    store.create_collection("blobs").expect("create collection");
    let mut seed = 0xD00D_F00D_CAFE_BABEu64;
    let body: Vec<u8> = (0..40_000)
        .map(|_| {
            seed ^= seed << 7;
            seed ^= seed >> 9;
            seed ^= seed << 8;
            (seed & 0xFF) as u8
        })
        .collect();
    for id in 1..=2 {
        let entity = reddb_server::storage::UnifiedEntity::table_row(
            EntityId::new(id),
            "blobs",
            id as u64,
            vec![Value::Blob(body.clone())],
        );
        store
            .insert_auto("blobs", entity)
            .expect("insert large row");
    }

    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);
    let artifact = EmbeddedRdbArtifact::open(&path).expect("open embedded artifact");
    assert!(artifact.manifest.snapshot_bytes > 0);
    assert_eq!(artifact.manifest.wal_region_bytes, 64 * 1024);
    assert!(
        artifact.manifest.wal_recovery_boundary > artifact.manifest.wal_region_offset,
        "expected retried frame after checkpoint"
    );

    let frames = EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read wal payloads");
    assert!(!frames.is_empty(), "expected retried wal frame");
}

#[test]
fn embedded_runtime_wraps_internal_wal_without_sidecars() {
    let dir = temp_dir("internal_wal_wrap");
    let path = dir.path().join("data.rdb");

    EmbeddedRdbArtifact::create(&path).expect("create embedded artifact");
    let store =
        UnifiedStore::with_config(UnifiedStoreConfig::default().with_embedded_wal_path(&path));
    store
        .create_collection("events")
        .expect("create collection");

    let mut seed = 0xA11C_E5ED_1234_5678u64;
    let body: Vec<u8> = (0..40 * 1024)
        .map(|_| {
            seed ^= seed << 7;
            seed ^= seed >> 9;
            seed ^= seed << 8;
            (seed & 0xFF) as u8
        })
        .collect();
    for id in 1..=2 {
        let entity = reddb_server::storage::UnifiedEntity::table_row(
            EntityId::new(id),
            "events",
            id as u64,
            vec![Value::Blob(body.clone())],
        );
        store.insert_auto("events", entity).expect("insert row");
    }

    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);
    let artifact = EmbeddedRdbArtifact::open(&path).expect("open embedded artifact");
    assert!(
        artifact.manifest.wal_recovery_boundary
            > artifact.manifest.wal_region_offset + artifact.manifest.wal_region_bytes,
        "expected logical wal boundary to wrap past the physical region"
    );

    let replayed =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen runtime");
    let rows = replayed
        .execute_query("SELECT * FROM events")
        .expect("select replayed rows");
    assert_eq!(rows.result.records.len(), 2);
    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);
}

#[test]
fn embedded_runtime_reports_region_size_when_internal_wal_remains_full_after_checkpoint() {
    let dir = temp_dir("internal_wal_full");
    let path = dir.path().join("data.rdb");

    EmbeddedRdbArtifact::create(&path).expect("create embedded artifact");
    let err = EmbeddedRdbArtifact::append_wal_payloads(&path, &[vec![b'z'; 80 * 1024]])
        .expect_err("oversized write should not silently grow the wal region");
    let msg = err.to_string();
    assert!(msg.contains("embedded circular wal region full"), "{msg}");
    assert!(msg.contains("region size 65536 bytes"), "{msg}");
    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);

    EmbeddedRdbArtifact::open(&path).expect("store remains readable after full wal failure");
}
