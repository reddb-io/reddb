//! Issue gh-472: tiers minimal/standard/performance no longer auto-write
//! `<data>.meta.json`. The binary `<data>.meta.rdbx` is always written;
//! the legacy JSON sidecar is gated behind [`set_meta_json_sidecar_enabled`]
//! (which a future tier-wiring slice flips on for `Max`) or the
//! `REDDB_META_JSON_SIDECAR=1` env escape hatch.

#[allow(dead_code)]
mod support;

use reddb::{set_meta_json_sidecar_enabled, PhysicalMetadataFile, RedDBOptions, RedDBRuntime};
use std::sync::Mutex;

// Serialises the two tests because they mutate a process-global toggle.
static POLICY_GUARD: Mutex<()> = Mutex::new(());

fn persistent_path(prefix: &str) -> support::TempDbFile {
    support::temp_db_file(prefix)
}

#[test]
fn standard_tier_default_does_not_write_json_sidecar() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_meta_json_sidecar_enabled(false);
    std::env::remove_var("REDDB_META_JSON_SIDECAR");

    let path = persistent_path("meta_sidecar_off");

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path.path()))
            .expect("persistent runtime opens");
        rt.execute_query("CREATE TABLE sidecar_off (name TEXT)")
            .expect("ddl");
        rt.checkpoint().expect("flush");
    }

    let json_path = PhysicalMetadataFile::metadata_path_for(&path);
    let binary_path = PhysicalMetadataFile::metadata_binary_path_for(&path);
    assert!(
        binary_path.exists(),
        "binary metadata must always be written: {binary_path:?}",
    );
    assert!(
        !json_path.exists(),
        "JSON sidecar must be absent by default: {json_path:?}",
    );

    // Loader still serves catalog from binary metadata (the substrate used
    // by `red inspect catalog` for the current-state path).
    let loaded =
        PhysicalMetadataFile::load_for_data_path(&path).expect("loader resolves from binary");
    assert!(
        loaded
            .collection_contracts
            .iter()
            .any(|c| c.name == "sidecar_off"),
        "catalog should include the new collection",
    );
}

#[test]
fn max_opt_in_writes_json_sidecar() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_meta_json_sidecar_enabled(true);

    let path = persistent_path("meta_sidecar_on");

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path.path()))
            .expect("persistent runtime opens");
        rt.execute_query("CREATE TABLE sidecar_on (name TEXT)")
            .expect("ddl");
        rt.checkpoint().expect("flush");
    }

    let json_path = PhysicalMetadataFile::metadata_path_for(&path);
    assert!(
        json_path.exists(),
        "JSON sidecar must be written when opt-in is on: {json_path:?}",
    );

    set_meta_json_sidecar_enabled(false);
}
