//! Native read-only recovery must never publish to the source tree.
use reddb_server::api::DurabilityMode;
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn options(path: &Path, bounded: bool) -> RedDBOptions {
    let options = RedDBOptions::persistent(path);
    let options = if bounded {
        options
    } else {
        options
            .with_storage_profile(StorageDeployPreset::PrimaryReplicaDev.selection())
            .expect("native paged profile")
    };
    options
        .with_memory_budget(64 * 1024 * 1024)
        .with_bounded_disk_residency(bounded)
        .with_durability_mode(DurabilityMode::WalDurableGrouped)
}

fn tree_bytes(root: &Path) -> BTreeMap<PathBuf, ([u8; 32], std::time::SystemTime)> {
    fn visit(
        root: &Path,
        path: &Path,
        out: &mut BTreeMap<PathBuf, ([u8; 32], std::time::SystemTime)>,
    ) {
        for entry in std::fs::read_dir(path).expect("source directory") {
            let entry = entry.expect("source entry");
            let metadata = entry.metadata().expect("source metadata");
            let path = entry.path();
            let bytes = if metadata.is_dir() {
                Vec::new()
            } else {
                std::fs::read(&path).expect("source bytes")
            };
            out.insert(
                path.strip_prefix(root)
                    .expect("under fixture")
                    .to_path_buf(),
                (
                    *blake3::hash(&bytes).as_bytes(),
                    metadata.modified().expect("mtime"),
                ),
            );
            if metadata.is_dir() {
                visit(root, &path, out);
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out);
    out
}

fn seed(path: &Path, bounded: bool) -> RedDBRuntime {
    let runtime = RedDBRuntime::with_options(options(path, bounded)).expect("native writer");
    runtime
        .execute_query("CREATE TABLE samples (id INT PRIMARY KEY, payload TEXT)")
        .expect("schema");
    runtime
        .execute_query("INSERT INTO samples (id, payload) VALUES (1, 'old'), (2, 'removed')")
        .expect("seed rows");
    runtime
        .execute_query("CREATE INDEX samples_payload ON samples (payload) USING HASH")
        .expect("index");
    runtime.checkpoint().expect("baseline checkpoint");
    runtime
}

fn verify(runtime: &RedDBRuntime, recovered: bool) {
    let predicate = if recovered { "new" } else { "old" };
    let result = runtime
        .execute_query(&format!(
            "SELECT id FROM samples WHERE payload = '{predicate}'"
        ))
        .expect("indexed query");
    let mut ids: Vec<_> = result
        .result
        .records
        .iter()
        .map(|row| row.get("id").expect("id").display_string())
        .collect();
    ids.sort();
    assert_eq!(ids, if recovered { vec!["1", "3"] } else { vec!["1"] });
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM samples WHERE id = 2")
            .expect("point lookup")
            .result
            .records
            .len(),
        usize::from(!recovered)
    );
    assert_eq!(
        runtime
            .execute_query("SELECT id FROM samples")
            .expect("scan")
            .result
            .records
            .len(),
        2
    );
    assert!(runtime
        .execute_query("INSERT INTO samples (id, payload) VALUES (4, 'denied')")
        .is_err());
    assert!(runtime
        .execute_query("DELETE FROM samples WHERE id = 1")
        .is_err());
    assert!(runtime
        .execute_query("CREATE TABLE forbidden (id INT)")
        .is_err());
    runtime.flush().expect("read-only flush");
    assert!(runtime.db().remove_collection_contract("samples").is_err());
}

#[test]
fn native_paged_read_only_cold_boot_preserves_every_source_file() {
    for bounded in [false, true] {
        let directory = tempfile::tempdir().expect("fixture");
        let path = directory.path().join("data.rdb");
        drop(seed(&path, bounded));
        let before = tree_bytes(directory.path());
        for reader_bounded in [false, true, bounded] {
            let runtime =
                RedDBRuntime::with_options(options(&path, reader_bounded).with_read_only(true))
                    .expect("native read-only cold boot");
            assert!(runtime
                .db()
                .store()
                .pager()
                .expect("native pager")
                .is_read_only());
            verify(&runtime, false);
            drop(runtime);
            assert_eq!(
                tree_bytes(directory.path()),
                before,
                "read-only open/query/close changed source"
            );
        }
        let writer =
            RedDBRuntime::with_options(options(&path, bounded)).expect("writer after readers");
        assert_eq!(
            writer
                .execute_query("SELECT id FROM samples")
                .expect("unchanged rows")
                .result
                .records
                .len(),
            2
        );
    }
}

const CRASH_PATH: &str = "REDDB_NATIVE_READ_ONLY_CRASH_PATH";
const CRASH_BOUNDED: &str = "REDDB_NATIVE_READ_ONLY_CRASH_BOUNDED";

#[test]
fn native_paged_crash_fixture() {
    let Some(path) = std::env::var_os(CRASH_PATH) else {
        return;
    };
    let bounded = std::env::var(CRASH_BOUNDED).expect("fixture mode") == "true";
    let runtime = seed(Path::new(&path), bounded);
    runtime
        .execute_query("UPDATE samples SET payload = 'new' WHERE id = 1")
        .expect("committed update");
    runtime
        .execute_query("DELETE FROM samples WHERE id = 2")
        .expect("committed delete");
    runtime
        .execute_query("INSERT INTO samples (id, payload) VALUES (3, 'new')")
        .expect("committed insert");
    // Grouped durability has acknowledged the WAL. Skip Drop/checkpoint.
    std::process::exit(0);
}

#[test]
fn native_paged_read_only_recovers_wal_without_consuming_it() {
    for bounded in [false, true] {
        let directory = tempfile::tempdir().expect("crash fixture");
        let path = directory.path().join("data.rdb");
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", "native_paged_crash_fixture", "--nocapture"])
            .env(CRASH_PATH, &path)
            .env(CRASH_BOUNDED, bounded.to_string())
            .status()
            .expect("crash writer");
        assert!(status.success());
        let before = tree_bytes(directory.path());
        for _ in 0..2 {
            let runtime = RedDBRuntime::with_options(options(&path, bounded).with_read_only(true))
                .expect("read-only WAL recovery");
            verify(&runtime, true);
            drop(runtime);
            assert_eq!(
                tree_bytes(directory.path()),
                before,
                "recovery consumed or rewrote source"
            );
        }
        let writer = RedDBRuntime::with_options(options(&path, bounded))
            .expect("ordinary recovery remains possible");
        assert_eq!(
            writer
                .execute_query("SELECT id FROM samples WHERE payload = 'new'")
                .expect("writer sees committed WAL")
                .result
                .records
                .len(),
            2
        );
    }
}

#[test]
fn native_paged_read_only_refuses_active_writer_and_allows_shared_readers() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let writer = seed(&path, true);
    assert!(RedDBRuntime::with_options(options(&path, true).with_read_only(true)).is_err());
    drop(writer);
    let before = tree_bytes(directory.path());
    let first = RedDBRuntime::with_options(options(&path, true).with_read_only(true))
        .expect("first reader");
    let second = RedDBRuntime::with_options(options(&path, true).with_read_only(true))
        .expect("second reader");
    assert!(RedDBRuntime::with_options(options(&path, true)).is_err());
    verify(&first, false);
    verify(&second, false);
    drop((first, second));
    assert_eq!(tree_bytes(directory.path()), before);
}

#[test]
fn native_paged_read_only_needs_no_writable_files_or_wal() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    drop(seed(&path, false));
    let wal = reddb_file::layout::unified_wal_path(&path);
    if wal.exists() {
        std::fs::remove_file(&wal).expect("remove checkpointed WAL");
    }
    let before = tree_bytes(directory.path());
    let mut permissions = Vec::new();
    for relative in before.keys() {
        let file = directory.path().join(relative);
        if file.is_file() {
            let original = std::fs::metadata(&file).expect("metadata").permissions();
            let mut read_only = original.clone();
            read_only.set_readonly(true);
            std::fs::set_permissions(&file, read_only).expect("read-only file");
            permissions.push((file, original));
        }
    }
    let mut config = options(&path, false).with_read_only(true);
    // A read-only copy must not start a replication spool even if the writer's
    // primary configuration was reused by the operator.
    config.replication = reddb_server::replication::ReplicationConfig::primary();
    let opened = RedDBRuntime::with_options(config);
    for (file, original) in permissions {
        std::fs::set_permissions(file, original).expect("restore fixture permissions");
    }
    let runtime = opened.expect("read-only source without WAL");
    verify(&runtime, false);
    drop(runtime);
    assert_eq!(tree_bytes(directory.path()), before);
}

#[test]
fn native_paged_read_only_applies_pending_manifest_drop_without_cleanup() {
    use reddb_server::storage::{UnifiedStore, UnifiedStoreConfig};
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    let writer = UnifiedStore::open(&path).expect("writer");
    writer.create_collection("retired").expect("collection");
    writer.persist().expect("checkpoint");
    drop(writer);
    let manifest = reddb_file::OperationalManifest::for_db_path(&path);
    drop(
        manifest
            .prepare_collection_drop("retired")
            .expect("durable pending drop"),
    );
    let before = tree_bytes(directory.path());
    for _ in 0..2 {
        let reader = UnifiedStore::open_read_only_with_config(&path, UnifiedStoreConfig::default())
            .expect("read-only manifest recovery");
        assert!(reader.get_collection("retired").is_none());
        reader.persist().expect("read-only checkpoint is a no-op");
        drop(reader);
        assert_eq!(tree_bytes(directory.path()), before);
    }
    let writer = UnifiedStore::open(&path).expect("ordinary manifest recovery");
    assert!(writer.get_collection("retired").is_none());
}

#[test]
fn native_paged_read_only_does_not_start_replica_apply() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("data.rdb");
    drop(seed(&path, false));
    let before = tree_bytes(directory.path());
    let mut config = options(&path, false).with_read_only(true);
    config.replication = reddb_server::replication::ReplicationConfig::replica("127.0.0.1:1");
    config.replica_loop_enabled = true;
    let reader = RedDBRuntime::with_options(config).expect("read-only replica inspection");
    verify(&reader, false);
    drop(reader);
    assert_eq!(tree_bytes(directory.path()), before);
    // A spawned replica loop would retain the runtime and its shared file lock.
    let writer =
        RedDBRuntime::with_options(options(&path, false)).expect("reader released its lock");
    drop(writer);
}
