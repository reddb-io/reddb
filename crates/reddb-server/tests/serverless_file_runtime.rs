use std::fs;
use std::path::PathBuf;
use std::process::{Command, ExitCode};

use reddb_server::{RedDBOptions, RedDBRuntime};

#[path = "support/primary_replica_file.rs"]
mod primary_replica_file;

const SERVERLESS_CRASH_CHILD_ENV: &str = "REDDB_SERVERLESS_RUNTIME_CRASH_CHILD";
const SERVERLESS_CRASH_DATA_PATH_ENV: &str = "REDDB_SERVERLESS_RUNTIME_CRASH_DATA_PATH";
const SERVERLESS_CRASH_AT_ENV: &str = "REDDB_SERVERLESS_CRASH_AT";

#[test]
fn runtime_publishes_serverless_generation_with_complete_packs() {
    let data_path = primary_replica_file::temp_data_path("serverless_generation");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .execute_query("INSERT INTO serverless_items (id, name) VALUES (1, 'alpha')")
        .expect("insert row");
    runtime
        .execute_query("INSERT INTO serverless_other (id, name) VALUES (2, 'beta')")
        .expect("insert other row");

    let pointer = runtime
        .publish_serverless_generation()
        .expect("publish serverless generation")
        .expect("generation pointer");
    let plan = runtime.serverless_file_plan().expect("serverless plan");
    assert_eq!(
        runtime
            .read_current_serverless_generation_verified()
            .expect("read verified serverless current")
            .expect("verified serverless pointer"),
        pointer
    );
    assert!(plan.manifest_path().exists());
    assert!(plan.extent_index_path().exists());
    assert!(plan.collection_data_path().exists());
    assert!(plan.secondary_index_path().exists());

    let manifest = reddb_file::ServerlessManifest::read_from_path(plan.manifest_path())
        .expect("read manifest");
    plan.validate_complete_generation(&manifest)
        .expect("serverless generation validates");
    let extent_index = reddb_file::ServerlessExtentIndex::read_from_path(plan.extent_index_path())
        .expect("read extent index");
    let item_extent = extent_index
        .extents
        .iter()
        .find(|extent| extent.collection == "serverless_items")
        .expect("serverless_items extent");
    let other_extent = extent_index
        .extents
        .iter()
        .find(|extent| extent.collection == "serverless_other")
        .expect("serverless_other extent");
    assert_eq!(item_extent.relative_path, other_extent.relative_path);
    assert_ne!(
        item_extent.offset, other_extent.offset,
        "collections should hydrate from distinct byte ranges"
    );
    let collection_data_len = fs::metadata(plan.collection_data_path())
        .expect("collection-data metadata")
        .len();
    assert!(item_extent.bytes < collection_data_len);
    assert!(other_extent.bytes < collection_data_len);

    let secondary =
        reddb_file::ServerlessSecondaryIndex::read_from_path(plan.secondary_index_path())
            .expect("read secondary index pack");
    assert_eq!(secondary.generation, plan.generation);
    assert_eq!(
        secondary.entries_for_collection("serverless_items").len(),
        1,
        "secondary index should catalog serverless_items extent"
    );
    assert_eq!(
        secondary.entries_for_collection("serverless_other").len(),
        1,
        "secondary index should catalog serverless_other extent"
    );
    let secondary_hydration = secondary.hydration_plan_for_collection("serverless_items");
    assert_eq!(secondary_hydration.requests.len(), 1);
    assert_eq!(secondary_hydration.requests[0].offset, item_extent.offset);
    assert_eq!(secondary_hydration.requests[0].bytes, item_extent.bytes);

    let hydration = extent_index.hydration_plan_for_key("serverless_items", b"1");
    assert!(!hydration.is_empty());
    let hydrated = plan
        .hydrate_local_plan(&hydration)
        .expect("hydrate serverless range");
    assert!(!hydrated[0].payload.is_empty());
    assert_eq!(hydrated[0].payload.len() as u64, item_extent.bytes);
    let hydrated_path = data_path.with_extension("serverless-items-hydrated.rdb");
    reddb_server::storage::EmbeddedRdbArtifact::create_with_snapshot(
        &hydrated_path,
        &hydrated[0].payload,
    )
    .expect("write hydrated embedded snapshot");
    let hydrated_runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&hydrated_path))
        .expect("hydrated runtime opens");
    assert_eq!(
        hydrated_runtime
            .execute_query("SELECT id, name FROM serverless_items WHERE id = 1")
            .expect("query hydrated collection")
            .result
            .len(),
        1
    );
    assert!(
        hydrated_runtime
            .execute_query("SELECT id, name FROM serverless_other WHERE id = 2")
            .is_err(),
        "hydrated range should not contain unrelated collection"
    );

    let _ = fs::remove_file(&hydrated_path);
    let _ = fs::remove_dir_all(data_path.with_extension("serverless"));
    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_serverless_publish_survives_pack_and_current_crash_points() {
    if std::env::var(SERVERLESS_CRASH_CHILD_ENV).ok().as_deref() == Some("1") {
        return;
    }

    for point in [
        "serverless_pack_after_tmp_write",
        "serverless_pack_after_tmp_sync",
        "serverless_pack_after_rename",
        "serverless_pack_after_dir_sync",
        "current_pointer_after_tmp_write",
        "current_pointer_after_tmp_sync",
        "current_pointer_after_rename",
        "current_pointer_after_dir_sync",
    ] {
        let data_path = primary_replica_file::temp_data_path(&format!(
            "serverless_runtime_publish_crash_{point}"
        ));
        primary_replica_file::cleanup(&data_path);
        let (first_pointer, current_plan) = {
            let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&data_path))
                .expect("runtime boots");
            runtime
                .execute_query("INSERT INTO serverless_items (id, name) VALUES (1, 'alpha')")
                .expect("insert row");
            let first_pointer = runtime
                .publish_serverless_generation()
                .expect("publish first serverless generation")
                .expect("first generation pointer");
            let plan = runtime.serverless_file_plan().expect("serverless plan");
            let current_plan = reddb_file::ServerlessFilePlan::new(
                plan.root.clone(),
                plan.namespace.clone(),
                first_pointer.generation,
            );
            (first_pointer, current_plan)
        };

        let child = Command::new(std::env::current_exe().expect("current test exe"))
            .arg("--exact")
            .arg("serverless_runtime_publish_crash_child")
            .arg("--nocapture")
            .env(SERVERLESS_CRASH_CHILD_ENV, "1")
            .env(SERVERLESS_CRASH_DATA_PATH_ENV, &data_path)
            .env(SERVERLESS_CRASH_AT_ENV, point)
            .status()
            .expect("run crash child");
        assert_eq!(
            child.code(),
            Some(173),
            "child should crash at {point}, status={child:?}"
        );

        let current = current_plan
            .read_current_pointer_verified()
            .expect("CURRENT must remain verified after runtime publish crash");
        if point.starts_with("serverless_pack_") {
            assert_eq!(
                current, first_pointer,
                "pack crash at {point} must not advance CURRENT"
            );
        } else if current.generation == first_pointer.generation {
            assert_eq!(current, first_pointer);
        } else {
            let advanced_plan = reddb_file::ServerlessFilePlan::new(
                current_plan.root.clone(),
                current_plan.namespace.clone(),
                current.generation,
            );
            let manifest =
                reddb_file::ServerlessManifest::read_from_path(advanced_plan.manifest_path())
                    .expect("read advanced manifest");
            advanced_plan
                .validate_complete_generation(&manifest)
                .expect("advanced CURRENT must reference a complete generation");
        }

        let _ = fs::remove_dir_all(data_path.with_extension("serverless"));
        primary_replica_file::cleanup(&data_path);
    }
}

#[test]
fn serverless_runtime_publish_crash_child() -> ExitCode {
    if std::env::var(SERVERLESS_CRASH_CHILD_ENV).ok().as_deref() != Some("1") {
        return ExitCode::SUCCESS;
    }
    let data_path =
        PathBuf::from(std::env::var(SERVERLESS_CRASH_DATA_PATH_ENV).expect("data path env"));
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .execute_query("INSERT INTO serverless_items (id, name) VALUES (99, 'crash')")
        .expect("insert crash row");
    let _ = runtime.publish_serverless_generation();
    ExitCode::from(1)
}

#[test]
fn runtime_hydrates_current_serverless_collection_from_verified_pointer() {
    let data_path = primary_replica_file::temp_data_path("serverless_generation_current_hydrate");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .execute_query("INSERT INTO serverless_items (id, name) VALUES (1, 'alpha')")
        .expect("insert row");
    runtime
        .execute_query("INSERT INTO serverless_other (id, name) VALUES (2, 'beta')")
        .expect("insert other row");
    runtime
        .publish_serverless_generation()
        .expect("publish serverless generation")
        .expect("generation pointer");

    let collection_ranges = runtime
        .hydrate_current_serverless_collection("serverless_items")
        .expect("hydrate current collection")
        .expect("current serverless generation");
    let key_ranges = runtime
        .hydrate_current_serverless_key("serverless_items", b"1")
        .expect("hydrate current key")
        .expect("current serverless generation");
    let range_ranges = runtime
        .hydrate_current_serverless_range("serverless_items", b"", b"")
        .expect("hydrate current range")
        .expect("current serverless generation");
    let cached_ranges = runtime
        .hydrate_current_serverless_key_cached("serverless_items", b"1")
        .expect("hydrate current key cached")
        .expect("current serverless generation");
    assert_eq!(collection_ranges.len(), 1);
    assert_eq!(key_ranges.len(), 1);
    assert_eq!(range_ranges.len(), 1);
    assert_eq!(cached_ranges.len(), 1);
    assert_eq!(collection_ranges[0].payload, key_ranges[0].payload);
    assert_eq!(range_ranges[0].payload, key_ranges[0].payload);
    assert_eq!(cached_ranges[0].payload, key_ranges[0].payload);

    let pointer = runtime
        .read_current_serverless_generation_verified()
        .expect("read verified current")
        .expect("current pointer");
    let current_plan = {
        let plan = runtime.serverless_file_plan().expect("serverless plan");
        reddb_file::ServerlessFilePlan::new(
            plan.root.clone(),
            plan.namespace.clone(),
            pointer.generation,
        )
    };
    let extent_index =
        reddb_file::ServerlessExtentIndex::read_from_path(current_plan.extent_index_path())
            .expect("read extent index");
    let hydration = extent_index.hydration_plan_for_key("serverless_items", b"1");
    let cache = reddb_file::ServerlessLocalCache::new(
        current_plan
            .root
            .join(&current_plan.namespace)
            .join("cache"),
        pointer.generation,
    );
    let cache_path = cache.path_for_request(&hydration.requests[0]);
    assert!(
        cache_path.exists(),
        "cached hydrate should persist a validated range"
    );
    fs::write(&cache_path, b"corrupt cache entry").expect("corrupt cache entry");
    let healed_cached_ranges = runtime
        .hydrate_current_serverless_key_cached("serverless_items", b"1")
        .expect("cached hydrate should self-heal corrupt cache")
        .expect("current serverless generation");
    assert_eq!(healed_cached_ranges[0].payload, key_ranges[0].payload);

    let hot_ranges = runtime
        .prefetch_current_serverless_hot_extents_cached()
        .expect("prefetch hot extents")
        .expect("current serverless generation");
    assert!(
        hot_ranges.len() >= 2,
        "published serverless collections should be marked hot for prefetch"
    );

    let hydrated_path = data_path.with_extension("serverless-current-hydrated.rdb");
    reddb_server::storage::EmbeddedRdbArtifact::create_with_snapshot(
        &hydrated_path,
        &key_ranges[0].payload,
    )
    .expect("write hydrated embedded snapshot");
    let hydrated_runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&hydrated_path))
        .expect("hydrated runtime opens");
    assert_eq!(
        hydrated_runtime
            .execute_query("SELECT id, name FROM serverless_items WHERE id = 1")
            .expect("query hydrated collection")
            .result
            .len(),
        1
    );
    assert!(
        hydrated_runtime
            .execute_query("SELECT id, name FROM serverless_other WHERE id = 2")
            .is_err(),
        "hydrated current key should not include unrelated collection"
    );

    let _ = fs::remove_file(&hydrated_path);
    let _ = fs::remove_dir_all(data_path.with_extension("serverless"));
    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_allows_missing_serverless_current_generation() {
    let data_path = primary_replica_file::temp_data_path("serverless_generation_missing_current");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    assert!(
        runtime
            .read_current_serverless_generation_verified()
            .expect("missing current pointer is not corrupt")
            .is_none(),
        "missing serverless CURRENT pointer should not block serverless attach"
    );

    let _ = fs::remove_dir_all(data_path.with_extension("serverless"));
    primary_replica_file::cleanup(&data_path);
}

#[test]
fn runtime_rejects_corrupt_serverless_current_generation() {
    let data_path = primary_replica_file::temp_data_path("serverless_generation_corrupt");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .execute_query("INSERT INTO serverless_items (id, name) VALUES (1, 'alpha')")
        .expect("insert row");
    runtime
        .publish_serverless_generation()
        .expect("publish serverless generation")
        .expect("generation pointer");
    runtime
        .read_current_serverless_generation_verified()
        .expect("verified generation before corruption")
        .expect("verified serverless pointer");

    let plan = runtime.serverless_file_plan().expect("serverless plan");
    fs::write(plan.collection_data_path(), b"corrupt collection data")
        .expect("corrupt collection-data pack");

    let err = runtime
        .read_current_serverless_generation_verified()
        .expect_err("corrupt serverless generation must fail closed");
    let message = err.to_string();
    assert!(
        message.contains("serverless")
            || message.contains("checksum")
            || message.contains("content hash"),
        "error should identify corrupt serverless generation, got {message}"
    );
    let hydrate_err = runtime
        .hydrate_current_serverless_key("serverless_items", b"1")
        .expect_err("corrupt serverless generation must block hydrate");
    assert!(
        hydrate_err
            .to_string()
            .contains("corrupt serverless generation"),
        "hydrate should identify corrupt serverless generation, got {hydrate_err}"
    );

    let _ = fs::remove_dir_all(data_path.with_extension("serverless"));
    primary_replica_file::cleanup(&data_path);
}

#[test]
fn native_use_cases_reject_corrupt_serverless_current_generation() {
    let data_path = primary_replica_file::temp_data_path("serverless_generation_native_corrupt");
    primary_replica_file::cleanup(&data_path);

    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(&data_path)).expect("runtime boots");
    runtime
        .execute_query("INSERT INTO serverless_items (id, name) VALUES (1, 'alpha')")
        .expect("insert row");
    runtime
        .publish_serverless_generation()
        .expect("publish serverless generation")
        .expect("generation pointer");

    let plan = runtime.serverless_file_plan().expect("serverless plan");
    fs::write(plan.collection_data_path(), b"corrupt collection data")
        .expect("corrupt collection-data pack");

    let err = reddb_server::application::NativeUseCases::new(&runtime)
        .validate_current_serverless_generation()
        .expect_err("native use case must fail closed on corrupt serverless generation");
    assert!(
        err.to_string().contains("corrupt serverless generation"),
        "error should identify corrupt serverless generation, got {err}"
    );

    let _ = fs::remove_dir_all(data_path.with_extension("serverless"));
    primary_replica_file::cleanup(&data_path);
}
