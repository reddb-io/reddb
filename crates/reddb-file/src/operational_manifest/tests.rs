use std::fs;

use super::*;

fn temp_db_path(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "reddb_file_operational_manifest_{name}_{}_{}",
        std::process::id(),
        nanos
    ));
    fs::create_dir_all(&dir).unwrap();
    dir.join("data.rdb")
}

#[test]
fn manifest_round_trip_validates_checksum() {
    let mut manifest = empty_manifest();
    manifest.generation = 7;
    manifest.collections.insert(
        "users".to_string(),
        CollectionEntry {
            state: CollectionState::Active,
            path: "users.rcol".to_string(),
            source: None,
        },
    );

    let bytes = manifest_to_bytes(&manifest).unwrap();
    let decoded = manifest_from_bytes(&bytes).unwrap();

    assert_eq!(decoded.generation, 7);
    assert_eq!(decoded.collections["users"].path, "users.rcol");
}

#[test]
fn checksum_rejects_corruption() {
    let mut manifest = empty_manifest();
    manifest.generation = 1;
    let mut text = String::from_utf8(manifest_to_bytes(&manifest).unwrap()).unwrap();
    text = text.replace("\"generation\": 1", "\"generation\": 9");

    let err = manifest_from_bytes(text.as_bytes()).unwrap_err();
    assert!(err.to_string().contains("checksum mismatch"));
}

#[test]
fn recover_removes_pending_drop_and_quarantines_orphans() {
    let path = temp_db_path("recover");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest
        .recover_or_bootstrap(&["live".to_string(), "gone".to_string()])
        .unwrap();
    fs::write(manifest.collection_path_for_test("orphan"), b"orphan").unwrap();
    let _drop = manifest.prepare_collection_drop("gone").unwrap();

    let completed = manifest.recover_or_bootstrap(&[]).unwrap();

    assert_eq!(completed, vec!["gone".to_string()]);
    assert!(manifest.collection_path_for_test("live").exists());
    assert!(!manifest.collection_path_for_test("gone").exists());
    assert!(!manifest.collection_path_for_test("orphan").exists());
    assert!(manifest.quarantine_path_for_test("orphan.rcol").exists());
}

#[test]
fn create_and_drop_collection_are_idempotent_and_escape_names() {
    let path = temp_db_path("create_drop");
    let manifest = OperationalManifest::for_db_path(&path);

    manifest.create_collection("tenant/a b").unwrap();
    let first_generation = manifest.read_generation_for_test().unwrap();
    manifest.create_collection("tenant/a b").unwrap();
    assert_eq!(
        manifest.read_generation_for_test().unwrap(),
        first_generation
    );
    assert!(manifest.collection_path_for_test("tenant/a b").exists());
    assert!(manifest
        .collection_path_for_test("tenant/a b")
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains("%2F"));

    let _drop = manifest.prepare_collection_drop("missing").unwrap();
    assert_eq!(
        manifest.read_generation_for_test().unwrap(),
        first_generation
    );
    manifest
        .prepare_collection_drop("tenant/a b")
        .unwrap()
        .finish()
        .unwrap();
    assert!(!manifest.collection_path_for_test("tenant/a b").exists());
    assert!(manifest.read_generation_for_test().unwrap() > first_generation);
}

#[test]
fn collection_rename_keeps_files_and_is_idempotent() {
    let path = temp_db_path("rename");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest.create_collection("red_queue_meta").unwrap();
    let old_path = manifest.collection_path_for_test("red_queue_meta");
    fs::write(&old_path, b"preserved").unwrap();
    let generation = manifest.read_generation_for_test().unwrap();

    manifest
        .rename_collections(&[("red_queue_meta", "red.queue.meta")])
        .unwrap();
    let published = manifest.load_current().unwrap().unwrap();
    assert!(!published.collections.contains_key("red_queue_meta"));
    assert_eq!(
        published.collections["red.queue.meta"].path,
        "red_queue_meta.rcol"
    );
    assert_eq!(fs::read(&old_path).unwrap(), b"preserved");
    assert_eq!(published.generation, generation + 1);
    assert_eq!(
        manifest
            .create_collection("red_queue_meta")
            .unwrap_err()
            .kind(),
        io::ErrorKind::AlreadyExists
    );
    manifest
        .recover_or_bootstrap(&["red.queue.meta".into()])
        .unwrap();
    manifest
        .rename_collections(&[("red_queue_meta", "red.queue.meta")])
        .unwrap();
    assert_eq!(manifest.read_generation_for_test().unwrap(), generation + 1);
    assert_eq!(fs::read(&old_path).unwrap(), b"preserved");
}

#[test]
fn collision_reconciliation_rejects_append_only_rows_owned_by_loser() {
    let path = temp_db_path("collision_append_only_rows");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest.create_collection("red_config").unwrap();
    manifest.create_collection("red.config").unwrap();
    let mut current = manifest.load_current().unwrap().unwrap();
    current.append_only_retired.insert(
        "red.config".to_string(),
        AppendOnlyRetiredState {
            row_count: 7,
            segment_high_water: 3,
        },
    );
    current.generation += 1;
    manifest.publish(&current).unwrap();
    let generation = current.generation;

    let error = manifest
        .reconcile_collection_collisions(&[("red_config", "red.config", "red.config")])
        .expect_err("append-only data cannot be discarded as an empty duplicate");
    assert!(error.to_string().contains("still owns 7 append-only rows"));
    let unchanged = manifest.load_current().unwrap().unwrap();
    assert_eq!(unchanged.generation, generation);
    assert!(unchanged.collections.contains_key("red_config"));
    assert!(unchanged.collections.contains_key("red.config"));
}

#[test]
fn collection_rename_collision_rejects_the_whole_batch() {
    let path = temp_db_path("rename_collision");
    let manifest = OperationalManifest::for_db_path(&path);
    for name in ["red_config", "red_queue_meta", "red.queue.meta"] {
        manifest.create_collection(name).unwrap();
    }
    let before = fs::read(manifest.root.join(MANIFEST_FILE)).unwrap();
    assert!(manifest
        .rename_collections(&[
            ("red_config", "red.config"),
            ("red_queue_meta", "red.queue.meta"),
        ])
        .is_err());
    assert_eq!(fs::read(manifest.root.join(MANIFEST_FILE)).unwrap(), before);
    let state = manifest.load_current().unwrap().unwrap();
    assert!(state.collections.contains_key("red_config"));
    assert!(!state.collections.contains_key("red.config"));
}

#[test]
fn collection_rename_preserves_a_live_shared_fork() {
    let path = temp_db_path("rename_shared_fork");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest.create_collection("red_config").unwrap();
    manifest.create_fork("shared", 1).unwrap();
    manifest
        .rename_collections(&[("red_config", "red.config")])
        .unwrap();
    let parent = manifest.load_current().unwrap().unwrap();
    assert!(parent.collections.contains_key("red.config"));
    assert!(!parent.collections.contains_key("red_config"));

    let fork = manifest.fork_handle("shared");
    let before = fork.load_current().unwrap().unwrap();
    let source = before.collections["red_config"].source.clone();
    fork.rename_collections(&[("red_config", "red.config")])
        .unwrap();
    let after = fork.load_current().unwrap().unwrap();
    assert_eq!(after.collections["red.config"].source, source);
    assert!(after.fork_origin.is_some());
}

#[test]
fn renamed_collection_keeps_fork_isolation_and_drop_uses_recorded_path() {
    let path = temp_db_path("rename_then_fork");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest.create_collection("red_config").unwrap();
    let old_path = manifest.collection_path_for_test("red_config");
    fs::write(&old_path, b"before fork").unwrap();
    manifest
        .rename_collections(&[("red_config", "red.config")])
        .unwrap();
    manifest.create_fork("child", 1).unwrap();
    manifest
        .materialize_forks_before_write("red.config")
        .unwrap();
    fs::write(&old_path, b"after fork").unwrap();
    let fork = manifest.fork_handle("child");
    let child = fork.load_current().unwrap().unwrap();
    let entry = &child.collections["red.config"];
    assert!(entry.source.is_none());
    assert_eq!(
        fs::read(fork.collections_dir().join(&entry.path)).unwrap(),
        b"before fork"
    );
    manifest
        .prepare_collection_drop("red.config")
        .unwrap()
        .finish()
        .unwrap();
    assert!(!old_path.exists());
    assert_eq!(
        fs::read(fork.collections_dir().join(&entry.path)).unwrap(),
        b"before fork"
    );
}

#[test]
fn next_manifest_helper_writes_next_file_without_publishing() {
    let path = temp_db_path("next");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest.recover_or_bootstrap(&[]).unwrap();
    manifest.write_next_manifest_for_test("future").unwrap();

    assert!(manifest.root.join(NEXT_MANIFEST_FILE).exists());
    assert!(manifest
        .load_current()
        .unwrap()
        .unwrap()
        .collections
        .is_empty());
}

#[test]
fn manifest_parser_rejects_malformed_shapes() {
    assert!(manifest_from_bytes(b"[]").is_err());
    assert!(manifest_from_bytes(br#"{"format_version":1}"#).is_err());
    assert!(CollectionState::parse("bad").is_err());

    let mut object = Map::new();
    object.insert("format_version".to_string(), Value::from(2));
    object.insert("generation".to_string(), Value::from(0));
    object.insert("collections".to_string(), Value::Object(Map::new()));
    assert!(manifest_from_object(&object).is_err());

    object.insert("format_version".to_string(), Value::from(FORMAT_VERSION));
    object.remove("generation");
    assert!(manifest_from_object(&object).is_err());

    object.insert("generation".to_string(), Value::from(0));
    object.remove("collections");
    assert!(manifest_from_object(&object).is_err());

    let mut collections = Map::new();
    collections.insert("users".to_string(), Value::String("bad".to_string()));
    object.insert("collections".to_string(), Value::Object(collections));
    assert!(manifest_from_object(&object).is_err());

    let mut entry = Map::new();
    entry.insert("path".to_string(), Value::String("users.rcol".to_string()));
    let mut collections = Map::new();
    collections.insert("users".to_string(), Value::Object(entry));
    object.insert("collections".to_string(), Value::Object(collections));
    assert!(manifest_from_object(&object).is_err());

    let mut entry = Map::new();
    entry.insert("state".to_string(), Value::String("active".to_string()));
    let mut collections = Map::new();
    collections.insert("users".to_string(), Value::Object(entry));
    object.insert("collections".to_string(), Value::Object(collections));
    assert!(manifest_from_object(&object).is_err());
}

#[test]
fn unique_quarantine_path_adds_suffix_when_needed() {
    let path = temp_db_path("unique_quarantine");
    let manifest = OperationalManifest::for_db_path(&path);
    manifest.ensure_dirs().unwrap();
    fs::write(manifest.quarantine_dir().join("orphan.rcol"), b"one").unwrap();
    fs::write(manifest.quarantine_dir().join("orphan.rcol.1"), b"two").unwrap();

    assert_eq!(
        manifest.unique_quarantine_path("orphan.rcol"),
        manifest.quarantine_dir().join("orphan.rcol.2")
    );
}

// Write bytes straight to a collection's file so the fork isolation tests
// have observable per-collection content to diverge.
fn write_collection(manifest: &OperationalManifest, name: &str, bytes: &[u8]) {
    fs::write(manifest.collection_path_for_test(name), bytes).unwrap();
}

fn read_collection(manifest: &OperationalManifest, name: &str) -> Vec<u8> {
    fs::read(manifest.collection_path_for_test(name)).unwrap()
}

#[test]
fn fork_create_is_metadata_only_and_copies_no_data() {
    let path = temp_db_path("fork_metadata");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"parent-rows");

    parent.create_fork("exp", 42).unwrap();
    let fork = parent.fork_handle("exp");

    // The fork references the parent's collection file by path; it did not
    // materialize its own copy at create time.
    assert!(!fork.collection_path_for_test("users").exists());
    // Listing reports the parent identity and the pinned fork LSN.
    let forks = parent.list_forks().unwrap();
    assert_eq!(forks.len(), 1);
    assert_eq!(forks[0].name, "exp");
    assert_eq!(forks[0].fork_lsn, 42);
    assert_eq!(forks[0].parent_store, parent.store_identity());
    // The fork's own manifest carries the origin.
    assert_eq!(fork.fork_origin().unwrap().unwrap().fork_lsn, 42);
    // The parent is not itself a fork.
    assert!(parent.fork_origin().unwrap().is_none());
}

#[test]
fn fork_writes_are_invisible_to_parent() {
    let path = temp_db_path("fork_to_parent");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"v0");
    parent.create_fork("exp", 1).unwrap();
    let fork = parent.fork_handle("exp");

    // Fork writes: hydrate (copy-on-write) then mutate the fork's own copy.
    fork.hydrate_collection("users").unwrap();
    write_collection(&fork, "users", b"fork-only");

    assert_eq!(read_collection(&fork, "users"), b"fork-only");
    // Parent is untouched by the fork's write.
    assert_eq!(read_collection(&parent, "users"), b"v0");
    // Hydration dropped the shared reference.
    assert!(fork.fork_origin().unwrap().is_some());
}

#[test]
fn fork_recovery_quarantines_partial_hydration_artifact() {
    let path = temp_db_path("fork_partial_hydration");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"complete-parent-copy");
    parent.create_fork("exp", 1).unwrap();
    let fork = parent.fork_handle("exp");

    write_collection(&fork, "users", b"partial");

    fork.recover_or_bootstrap(&[]).unwrap();

    assert!(
        !fork.collection_path_for_test("users").exists(),
        "shared-by-reference fork recovery must not keep half-hydrated local artifacts"
    );
    assert_eq!(read_collection(&parent, "users"), b"complete-parent-copy");
    let forks = parent.list_forks().unwrap();
    assert_eq!(forks.len(), 1);
    assert_eq!(
        forks[0].hydration_state,
        ForkHydrationState::SharedByReference
    );
}

#[test]
fn recovery_rejects_manifest_referencing_missing_artifact() {
    let path = temp_db_path("missing_artifact");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    fs::remove_file(parent.collection_path_for_test("users")).unwrap();

    let err = parent.recover_or_bootstrap(&[]).unwrap_err();

    assert!(err.to_string().contains("missing collection artifact"));
}

#[test]
fn parent_writes_after_fork_are_invisible_to_fork() {
    let path = temp_db_path("parent_to_fork");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"as-of-fork");
    parent.create_fork("exp", 5).unwrap();
    let fork = parent.fork_handle("exp");

    // Parent write: snapshot referencing forks first (copy-on-write on the
    // parent side), then mutate the parent's file in place.
    parent.materialize_forks_before_write("users").unwrap();
    write_collection(&parent, "users", b"parent-moved-on");

    // Fork still sees the as-of-fork snapshot; parent's later write is invisible.
    assert_eq!(read_collection(&fork, "users"), b"as-of-fork");
    assert_eq!(read_collection(&parent, "users"), b"parent-moved-on");
}

#[test]
fn drop_fork_removes_private_files_and_leaves_parent_intact() {
    let path = temp_db_path("drop_fork");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"keep-me");
    parent.create_fork("exp", 9).unwrap();
    let fork = parent.fork_handle("exp");
    fork.hydrate_collection("users").unwrap();
    write_collection(&fork, "users", b"fork-private");
    let fork_root = fork.root.clone();

    assert!(parent.drop_fork("exp").unwrap());
    // Fork-private files are gone; the parent's data survives untouched.
    assert!(!fork_root.exists());
    assert_eq!(read_collection(&parent, "users"), b"keep-me");
    assert!(parent.list_forks().unwrap().is_empty());
    // Dropping again is idempotent.
    assert!(!parent.drop_fork("exp").unwrap());
}

#[test]
fn parent_recovery_retains_dropped_collection_file_referenced_by_live_fork() {
    let path = temp_db_path("fork_retains_parent_drop");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"as-of-fork");
    parent.create_fork("exp", 12).unwrap();

    let _drop = parent.prepare_collection_drop("users").unwrap();
    let completed = parent.recover_or_bootstrap(&[]).unwrap();

    assert_eq!(completed, vec!["users".to_string()]);
    assert!(
        parent.collection_path_for_test("users").exists(),
        "live fork source must keep the parent artifact available"
    );
    let fork = parent.fork_handle("exp");
    fork.hydrate_collection("users").unwrap();
    assert_eq!(read_collection(&fork, "users"), b"as-of-fork");
}

#[test]
fn dropping_last_referencing_fork_releases_parent_file_to_retention() {
    let path = temp_db_path("fork_drop_releases_parent_file");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"as-of-fork");
    parent.create_fork("exp", 12).unwrap();
    let _drop = parent.prepare_collection_drop("users").unwrap();
    parent.recover_or_bootstrap(&[]).unwrap();

    assert!(parent.drop_fork("exp").unwrap());

    assert!(!parent.collection_path_for_test("users").exists());
    assert!(parent.quarantine_path_for_test("users.rcol").exists());
}

#[test]
fn detach_fork_hydrates_and_survives_parent_removal() {
    let path = temp_db_path("detach_fork");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"successful-experiment");
    parent.create_fork("exp", 21).unwrap();

    let detached = parent.detach_fork("exp").unwrap().unwrap();

    assert!(parent.list_forks().unwrap().is_empty());
    assert!(detached.fork_origin().unwrap().is_none());
    assert_eq!(
        read_collection(&detached, "users"),
        b"successful-experiment"
    );

    fs::remove_dir_all(&parent.root).unwrap();
    assert_eq!(
        detached.recover_or_bootstrap(&[]).unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        read_collection(&detached, "users"),
        b"successful-experiment"
    );
}

#[test]
fn promote_fork_replaces_primary_and_archives_parent() {
    let path = temp_db_path("promote_fork");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"old-primary");
    parent.create_fork("exp", 21).unwrap();
    let fork = parent.fork_handle("exp");
    fork.hydrate_collection("users").unwrap();
    write_collection(&fork, "users", b"new-primary");

    let outcome = parent.promote_fork("exp").unwrap().unwrap();

    assert_eq!(outcome.name, "exp");
    assert_eq!(outcome.fork_lsn, 21);
    assert_eq!(read_collection(&parent, "users"), b"new-primary");
    assert!(parent.fork_origin().unwrap().is_none());
    assert!(parent.list_forks().unwrap().is_empty());
    assert_eq!(
        read_collection(&outcome.archived_parent, "users"),
        b"old-primary"
    );
}

#[test]
fn promotion_recovery_resumes_after_fork_moves_to_staging() {
    let path = temp_db_path("promote_fork_resume_staging");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"old-primary");
    parent.create_fork("exp", 21).unwrap();
    let fork = parent.fork_handle("exp");
    fork.hydrate_collection("users").unwrap();
    write_collection(&fork, "users", b"new-primary");
    let staging = parent.promoting_fork_handle("exp");
    fs::rename(&fork.root, &staging.root).unwrap();

    parent.recover_or_bootstrap(&[]).unwrap();

    assert_eq!(read_collection(&parent, "users"), b"new-primary");
    assert!(parent.fork_origin().unwrap().is_none());
    assert!(parent.list_forks().unwrap().is_empty());
    assert_eq!(
        read_collection(&parent.archived_parent_handle("exp"), "users"),
        b"old-primary"
    );
}

#[test]
fn detach_fork_resumes_after_move_before_origin_clear() {
    let path = temp_db_path("detach_fork_resume");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"as-of-fork");
    parent.create_fork("exp", 34).unwrap();

    let fork = parent.fork_handle("exp");
    fork.hydrate_shared_collections().unwrap();
    let detached = parent.detached_fork_handle("exp");
    fs::rename(&fork.root, &detached.root).unwrap();

    let resumed = parent.detach_fork("exp").unwrap().unwrap();

    assert_eq!(resumed.root, detached.root);
    assert!(parent.list_forks().unwrap().is_empty());
    assert!(resumed.fork_origin().unwrap().is_none());
    assert_eq!(read_collection(&resumed, "users"), b"as-of-fork");
}

#[test]
fn detach_fork_releases_parent_retention_pin() {
    let path = temp_db_path("detach_releases_parent_pin");
    let parent = OperationalManifest::for_db_path(&path);
    parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
    write_collection(&parent, "users", b"as-of-fork");
    parent.create_fork("exp", 55).unwrap();
    let _drop = parent.prepare_collection_drop("users").unwrap();
    parent.recover_or_bootstrap(&[]).unwrap();
    assert!(parent.collection_path_for_test("users").exists());

    let detached = parent.detach_fork("exp").unwrap().unwrap();
    parent.recover_or_bootstrap(&[]).unwrap();

    assert!(!parent.collection_path_for_test("users").exists());
    assert_eq!(read_collection(&detached, "users"), b"as-of-fork");
}

#[test]
fn fork_creation_rejects_duplicate_and_missing_parent() {
    let path = temp_db_path("fork_errors");
    let parent = OperationalManifest::for_db_path(&path);
    // A store with no manifest cannot be forked.
    assert_eq!(
        parent.create_fork("exp", 0).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    parent.recover_or_bootstrap(&[]).unwrap();
    parent.create_fork("exp", 0).unwrap();
    assert_eq!(
        parent.create_fork("exp", 0).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
}
