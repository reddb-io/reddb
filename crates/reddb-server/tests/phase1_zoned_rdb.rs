//! ADR 0038 §4 phase 1 acceptance suite for the zoned `.rdb`.
//!
//! Exit criterion (a) — the **fresh-store sidecar census**: a store on the
//! promoted embedded profile goes through DDL, DML, checkpoint and reopen, and
//! at *every* step a directory glob asserts that no retired phase-1 artifact —
//! `rdb-hdr`, `rdb-meta`, or their shadow-suffix companions — exists. A retired
//! extension reappearing is a test failure, not a review comment.
//!
//! Exit criterion (c) — a legacy sidecar-backed store is read *only* through
//! the explicit offline migration path, and that path is reversible.
//!
//! The census is deliberately scoped to the phase-1 family. The WAL region
//! (phase 2) and the double-write buffer (phase 3) keep their sidecars for now,
//! so a blanket "one file only" assertion would fail for the wrong reason and
//! would silently start passing once those phases land.

use std::fs;
use std::path::{Path, PathBuf};

use reddb_file::layout::retired;
use reddb_server::pager_zone_migration::{
    backup_path_for, migrate_to_zoned, revert_to_sidecars, ZoneMigrationError,
};
use reddb_server::storage::engine::page::PageType;
use reddb_server::storage::engine::pager::{Pager, PagerError};
use reddb_server::{RedDBOptions, RedDBRuntime};

/// Suffixes the phase-1 retirement removed. Written out here rather than
/// derived from `reddb_file::layout::retired` on purpose: the census must fail
/// if a future change quietly redefines what "retired" means.
const RETIRED_PHASE1_SUFFIXES: [&str; 4] = ["rdb-hdr", "rdb-meta", "-hdr", "-meta"];

fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-phase1-{label}-"))
        .tempdir()
        .expect("temp dir")
}

/// Every retired phase-1 artifact anywhere under `dir`, by glob over the names.
fn census(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if RETIRED_PHASE1_SUFFIXES
                .iter()
                .any(|suffix| name.ends_with(suffix))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

fn assert_no_phase1_sidecars(dir: &Path, after: &str) {
    let found = census(dir);
    assert!(
        found.is_empty(),
        "phase-1 sidecar reappeared after {after}: {found:?}\n\
         The superblock pair and the internal manifest live inside the .rdb \
         (ADR 0038 §2); nothing may recreate these files."
    );
}

#[test]
fn a_promoted_embedded_store_never_creates_a_phase_one_sidecar() {
    let dir = temp_dir("lifecycle");
    let path = dir.path().join("data.rdb");

    assert_no_phase1_sidecars(dir.path(), "an empty directory");

    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("open runtime");
        assert_no_phase1_sidecars(dir.path(), "store creation");

        runtime
            .execute_query("CREATE TABLE users (id INT, name TEXT)")
            .expect("create table");
        assert_no_phase1_sidecars(dir.path(), "DDL");

        runtime
            .execute_query("INSERT INTO users (id, name) VALUES (1, 'ada'), (2, 'linus')")
            .expect("insert rows");
        assert_no_phase1_sidecars(dir.path(), "DML");

        runtime.flush().expect("checkpoint");
        assert_no_phase1_sidecars(dir.path(), "checkpoint");
    }
    assert_no_phase1_sidecars(dir.path(), "close");

    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen runtime");
        assert_no_phase1_sidecars(dir.path(), "reopen");

        let rows = runtime
            .execute_query("SELECT * FROM users")
            .expect("select rows");
        assert_eq!(
            rows.result.records.len(),
            2,
            "the round trip must survive the zoned layout"
        );

        runtime
            .execute_query("INSERT INTO users (id, name) VALUES (3, 'grace')")
            .expect("insert after reopen");
        runtime.flush().expect("checkpoint after reopen");
        assert_no_phase1_sidecars(dir.path(), "DML + checkpoint after reopen");
    }
    assert_no_phase1_sidecars(dir.path(), "final close");
}

#[test]
fn the_promoted_embedded_profile_keeps_everything_in_one_file() {
    // The census above proves the phase-1 family is gone. This pins the wider
    // promise for the profile the ADR binds: one operator-visible artifact.
    let dir = temp_dir("single_file");
    let path = dir.path().join("data.rdb");

    {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("open runtime");
        runtime
            .execute_query("CREATE TABLE t (id INT)")
            .expect("create table");
        runtime
            .execute_query("INSERT INTO t (id) VALUES (1)")
            .expect("insert row");
        runtime.flush().expect("checkpoint");
    }

    let mut names: Vec<String> = fs::read_dir(dir.path())
        .expect("read dir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into())
        .collect();
    names.sort();
    assert_eq!(names, vec!["data.rdb".to_string()]);
}

// ── Exit criterion (c): the offline migration path ─────────────────────────

/// Build a zoned store holding `rows` distinct cells on a data page, and return
/// the page id so a later read can prove the payload survived the round trip.
fn populate(path: &Path, rows: usize) -> u32 {
    let pager = Pager::open_default(path).expect("create store");
    let mut page = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate page");
    let page_id = page.page_id();
    for row in 0..rows {
        page.insert_cell(
            format!("key-{row}").as_bytes(),
            format!("value-{row}").as_bytes(),
        )
        .expect("insert cell");
    }
    pager.write_page(page_id, page).expect("write page");
    pager.sync().expect("sync");
    drop(pager);
    page_id
}

fn assert_rows_readable(path: &Path, page_id: u32, rows: usize) {
    let pager = Pager::open_default(path).expect("open store");
    let page = pager.read_page(page_id).expect("read data page");
    assert_eq!(usize::from(page.cell_count()), rows);
    for row in 0..rows {
        let (key, value) = page.read_cell(row).expect("read cell");
        assert_eq!(key, format!("key-{row}").as_bytes());
        assert_eq!(value, format!("value-{row}").as_bytes());
    }
    drop(pager);
}

fn sidecars_present(path: &Path) -> bool {
    retired::first_present_phase1_sidecar(path).is_some()
}

#[test]
fn migrate_and_revert_round_trip_preserves_a_populated_store_byte_for_byte() {
    let dir = temp_dir("round_trip");
    let path = dir.path().join("data.rdb");
    let page_id = populate(&path, 8);

    // De-zone into the legacy shape. This is the inverse the migration undoes.
    revert_to_sidecars(&path).expect("revert to sidecars");
    assert!(sidecars_present(&path), "revert must write the sidecars");
    let legacy_bytes = fs::read(&path).expect("read legacy data file");
    let legacy_hdr =
        fs::read(retired::pager_header_shadow_path_v0(&path)).expect("read header shadow");
    let legacy_meta =
        fs::read(retired::pager_meta_shadow_path_v0(&path)).expect("read meta shadow");

    // The engine refuses the legacy store rather than reading it silently.
    match Pager::open_default(&path) {
        Ok(_) => panic!("a legacy sidecar store must not open"),
        Err(err) => {
            assert!(matches!(err, PagerError::LegacySidecarStore { .. }));
            assert!(err.to_string().contains("migration tool"));
        }
    }

    // Forward: legacy -> zoned. The rows are intact and the sidecars are gone.
    let report = migrate_to_zoned(&path).expect("migrate to zoned");
    assert!(!report.removed_sidecars.is_empty());
    assert!(!report.header_recovered_from_shadow);
    assert!(!sidecars_present(&path), "migration must drop the sidecars");
    assert!(
        report.backup_path.exists(),
        "migration must retain a rollback point"
    );
    assert_rows_readable(&path, page_id, 8);

    // Backward: zoned -> legacy, byte for byte.
    revert_to_sidecars(&path).expect("revert again");
    assert_eq!(
        fs::read(&path).expect("read reverted data file"),
        legacy_bytes,
        "the data file must round-trip byte for byte"
    );
    assert_eq!(
        fs::read(retired::pager_header_shadow_path_v0(&path)).expect("read header shadow"),
        legacy_hdr
    );
    assert_eq!(
        fs::read(retired::pager_meta_shadow_path_v0(&path)).expect("read meta shadow"),
        legacy_meta
    );
    assert!(
        !backup_path_for(&path).exists(),
        "a completed revert consumes the rollback point"
    );
}

#[test]
fn migration_recovers_the_header_from_the_shadow_when_page_zero_is_torn() {
    let dir = temp_dir("torn_page_zero");
    let path = dir.path().join("data.rdb");
    let page_id = populate(&path, 3);
    revert_to_sidecars(&path).expect("revert to sidecars");

    // Tear page 0 the way an interrupted whole-page write did. The `rdb-hdr`
    // shadow is precisely the copy that existed to survive this.
    let mut torn = fs::read(&path).expect("read data file");
    torn[32..36].copy_from_slice(&[0, 0, 0, 0]); // clobber the RDDB magic
    fs::write(&path, &torn).expect("write torn page 0");

    let report = migrate_to_zoned(&path).expect("migrate a torn legacy store");
    assert!(
        report.header_recovered_from_shadow,
        "a torn page 0 must be rebuilt from the header shadow"
    );
    assert_rows_readable(&path, page_id, 3);
}

#[test]
fn migrating_a_zoned_store_is_refused_rather_than_discarding_its_superblocks() {
    let dir = temp_dir("already_zoned");
    let path = dir.path().join("data.rdb");
    populate(&path, 2);

    match migrate_to_zoned(&path) {
        Ok(_) => panic!("a zoned store must not be migrated again"),
        Err(err) => assert!(matches!(err, ZoneMigrationError::NotALegacyStore(_))),
    }

    // Even with a stray sidecar beside it, a valid superblock zone wins: the
    // migration would otherwise overwrite live generations with stale bytes.
    fs::write(retired::pager_header_shadow_path_v0(&path), [0u8; 16_384]).expect("stray sidecar");
    match migrate_to_zoned(&path) {
        Ok(_) => panic!("a zoned store must not be migrated again"),
        Err(err) => {
            assert!(matches!(err, ZoneMigrationError::AlreadyZoned(_)));
            assert!(err
                .to_string()
                .contains("already has a valid superblock zone"));
        }
    }
}
