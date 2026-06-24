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
    // Operational telemetry sinks (audit log, slow-query log, …) resolve next
    // to the data path and end in `.log`. They are not part of the embedded
    // single-file DB *storage* contract this asserts, so filter them to keep
    // the assertion focused on the `.rdb` data artifact. (Whether embedded mode
    // should emit these sidecars at all is a separate operational-policy call.)
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| !name.ends_with(".log"))
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
        checkpointed.manifest.wal_region_offset
    );
    assert!(checkpointed.manifest.snapshot_bytes > 0);
    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);
}

#[test]
fn embedded_runtime_checkpoints_expands_and_retries_when_internal_wal_fills() {
    let dir = temp_dir("internal_wal_expand");
    let path = dir.path().join("data.rdb");

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

    assert_eq!(artifact_names(dir.path()), vec!["data.rdb"]);
    let artifact = EmbeddedRdbArtifact::open(&path).expect("open embedded artifact");
    assert!(artifact.manifest.snapshot_bytes > 0);
    assert!(artifact.manifest.wal_region_bytes > 64 * 1024);
    assert!(
        artifact.manifest.wal_recovery_boundary > artifact.manifest.wal_region_offset,
        "expected retried frame after checkpoint"
    );

    let frames = EmbeddedRdbArtifact::read_wal_payloads(&artifact).expect("read wal payloads");
    assert!(!frames.is_empty(), "expected retried wal frame");
}
