//! Issue gh-473: seq-N catalog journal is opt-in. Default off outside the
//! `Max` tier (which the tier-wiring slice will flip on). When enabled,
//! retention follows [`seqn_journal_retention`] — 32 for `Max`, 4 for
//! opt-in elsewhere. Recovery handles all three substrate states:
//! present, absent, corrupt (passive — load falls through to the next
//! viable source without panicking).

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use reddb::{
    seqn_journal_enabled, seqn_journal_retention, set_seqn_journal_enabled,
    set_seqn_journal_retention, PhysicalMetadataFile, RedDBOptions, RedDBRuntime,
    StorageDeployPreset,
};
use std::path::Path;
use std::sync::Mutex;

// Process-globals — serialise tests.
static POLICY_GUARD: Mutex<()> = Mutex::new(());

fn persistent_path(prefix: &str) -> support::TempDbFile {
    support::temp_db_file(prefix)
}

fn reset_policy() {
    set_seqn_journal_enabled(false);
    set_seqn_journal_retention(0);
    std::env::remove_var("REDDB_SEQN_JOURNAL");
    std::env::remove_var("REDDB_SEQN_JOURNAL_RETENTION");
}

fn sidecar_options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
        .expect("production HA profile should write physical metadata sidecars")
}

fn run_a_few_saves(path: &Path, table: &str, n: usize) {
    let rt = RedDBRuntime::with_options(sidecar_options(path)).expect("persistent runtime opens");
    rt.execute_query(&format!("CREATE TABLE {table} (name TEXT)"))
        .expect("ddl");
    for i in 0..n {
        rt.execute_query(&format!("INSERT INTO {table} (name) VALUES ('row-{i}')"))
            .expect("insert");
        rt.checkpoint().expect("flush");
    }
}

#[test]
fn journal_absent_by_default_outside_max_tier() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    reset_policy();

    let path = persistent_path("seqn_off");

    run_a_few_saves(&path, "seqn_off", 3);

    let journals = PhysicalMetadataFile::journal_paths_for_data_path(&path).expect("list journals");
    assert!(
        journals.is_empty(),
        "seq-N journal must be absent when policy is off: {journals:?}",
    );
    let binary = PhysicalMetadataFile::metadata_binary_path_for(&path);
    assert!(binary.exists(), "binary metadata is always written");
}

#[test]
fn journal_written_when_opt_in_with_bounded_retention() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    reset_policy();
    set_seqn_journal_enabled(true);
    set_seqn_journal_retention(4);

    assert!(seqn_journal_enabled());
    assert_eq!(seqn_journal_retention(), 4);

    let path = persistent_path("seqn_on");

    run_a_few_saves(&path, "seqn_on", 10);

    let journals = PhysicalMetadataFile::journal_paths_for_data_path(&path).expect("list journals");
    assert!(
        !journals.is_empty(),
        "seq-N journal must appear when opt-in is on",
    );
    assert!(
        journals.len() <= 4,
        "retention bound (4) must be enforced: got {} entries",
        journals.len(),
    );

    reset_policy();
}

#[test]
fn recovery_handles_present_absent_and_corrupt_binary() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    reset_policy();
    set_seqn_journal_enabled(true);
    set_seqn_journal_retention(4);

    // === Case 1: PRESENT — binary intact, journals present, loader uses binary.
    // === Case 2: CORRUPT — overwrite the binary with garbage; loader heals from journal.
    {
        let path = persistent_path("seqn_present");
        run_a_few_saves(&path, "present", 3);
        let (_, source) = PhysicalMetadataFile::load_for_data_path_with_source(&path)
            .expect("present binary loads");
        assert_eq!(source.as_str(), "binary");

        let binary = PhysicalMetadataFile::metadata_binary_path_for(&path);
        std::fs::write(&binary, b"not-valid-metadata-bytes").expect("corrupt binary");
        let (_, source) = PhysicalMetadataFile::load_for_data_path_with_source(&path)
            .expect("corrupt binary heals from journal");
        assert_eq!(
            source.as_str(),
            "binary_journal",
            "corrupt binary must heal from seq-N journal entry",
        );
    }

    // === Case 3: ABSENT — no binary, no journal, no JSON sidecar. Loader returns Err.
    let path = persistent_path("seqn_absent");
    let result = PhysicalMetadataFile::load_for_data_path_with_source(&path);
    assert!(
        result.is_err(),
        "absent metadata must surface a load error: {result:?}",
    );

    reset_policy();
}
