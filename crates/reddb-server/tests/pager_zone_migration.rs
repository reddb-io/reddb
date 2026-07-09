//! ADR 0038 §4 phase 1, exit criterion (c): a legacy sidecar-backed store is
//! read *only* through the explicit offline migration path, and that path is
//! reversible.

use std::fs;
use std::path::Path;

use reddb_file::layout::retired;
use reddb_server::pager_zone_migration::{
    backup_path_for, migrate_to_zoned, revert_to_sidecars, ZoneMigrationError,
};
use reddb_server::storage::engine::page::PageType;
use reddb_server::storage::engine::pager::{Pager, PagerError};

fn temp_dir(label: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("reddb-test-zone-migration-{label}-"))
        .tempdir()
        .expect("temp dir")
}

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
