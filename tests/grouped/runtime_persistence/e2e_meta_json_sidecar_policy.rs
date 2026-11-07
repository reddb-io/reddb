//! Legacy sidecar toggles cannot override the runtime single-database contract.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use reddb::{
    set_meta_json_sidecar_enabled, PhysicalMetadataFile, RedDBOptions, RedDBRuntime,
    StorageDeployPreset,
};
use std::sync::Mutex;

// Serialises the two tests because they mutate a process-global toggle.
static POLICY_GUARD: Mutex<()> = Mutex::new(());

fn persistent_path(prefix: &str) -> support::TempDbFile {
    support::temp_db_file(prefix)
}

fn sidecar_options(path: &std::path::Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
        .expect("production HA profile")
}

#[test]
fn standard_tier_default_does_not_write_json_sidecar() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_meta_json_sidecar_enabled(false);
    std::env::remove_var("REDDB_META_JSON_SIDECAR");

    let path = persistent_path("meta_sidecar_off");

    {
        let rt = RedDBRuntime::with_options(sidecar_options(path.path()))
            .expect("persistent runtime opens");
        rt.execute_query("CREATE TABLE sidecar_off (name TEXT)")
            .expect("ddl");
        rt.checkpoint().expect("flush");
    }

    let json_path = PhysicalMetadataFile::metadata_path_for(&path);
    let binary_path = PhysicalMetadataFile::metadata_binary_path_for(&path);
    assert!(
        !binary_path.exists(),
        "runtime must not write binary metadata sidecars: {binary_path:?}",
    );
    assert!(
        !json_path.exists(),
        "JSON sidecar must be absent by default: {json_path:?}",
    );

    let reopened = RedDBRuntime::with_options(sidecar_options(path.path())).expect("cold boot");
    let loaded = reopened
        .db()
        .physical_metadata()
        .expect("internal metadata");
    assert!(
        loaded
            .collection_contracts
            .iter()
            .any(|c| c.name == "sidecar_off"),
        "catalog should include the new collection",
    );
}

#[test]
fn legacy_opt_in_does_not_enable_runtime_json_sidecars() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_meta_json_sidecar_enabled(true);

    let path = persistent_path("meta_sidecar_on");

    {
        let rt = RedDBRuntime::with_options(sidecar_options(path.path()))
            .expect("persistent runtime opens");
        rt.execute_query("CREATE TABLE sidecar_on (name TEXT)")
            .expect("ddl");
        rt.checkpoint().expect("flush");
    }

    let json_path = PhysicalMetadataFile::metadata_path_for(&path);
    assert!(
        !json_path.exists(),
        "legacy opt-in must not create runtime JSON sidecars: {json_path:?}",
    );

    set_meta_json_sidecar_enabled(false);
}
