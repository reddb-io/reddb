//! Exercise a torn home-page write with its durable in-file DWB image intact.
//! Corruption after a completed checkpoint has no recovery image and must fail
//! closed. Select a live payload page rather than the file tail, which may be
//! free after tree replacement or live after sparse checkpoint reuse.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use reddb::storage::engine::{Page, PAGE_SIZE};
use reddb::types::Value;
use reddb::{
    set_fold_dwb_into_wal_enabled, RedDBError, RedDBOptions, RedDBRuntime, StorageDeployPreset,
};
use reddb_engine::storage::pager_io::dwb_zone_offset;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Mutex;

const ROWS: usize = 25;

// Serialise tests that flip the process-global fold-DWB toggle.
static POLICY_GUARD: Mutex<()> = Mutex::new(());

fn operational_options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
        .expect("primary-replica operational profile")
}

/// Populate a fresh DB with `ROWS` rows in its own table and run a
/// checkpoint so the pages reach the main data file. Returns the
/// auto-cleaning guard for the caller to keep alive while corrupting.
fn populate(prefix: &str) -> support::TempDbFile {
    let db = support::temp_db_file(prefix);
    let path = db.path();

    let rt =
        RedDBRuntime::with_options(operational_options(path)).expect("persistent runtime opens");
    rt.execute_query("CREATE TABLE crash_rows (n INTEGER, label TEXT)")
        .expect("ddl");
    for i in 0..ROWS {
        rt.execute_query(&format!(
            "INSERT INTO crash_rows (n, label) VALUES ({i}, 'row-{i}')"
        ))
        .expect("insert");
    }
    rt.checkpoint().expect("flush");
    drop(rt);
    db
}

/// Select the page containing an acknowledged row and optionally stage its
/// checked image exactly where the pager stages a batch before home writes.
fn corrupt_live_page(path: &Path, stage_recovery_image: bool) -> u32 {
    let bytes = std::fs::read(path).expect("read populated file");
    let needle = b"row-24";
    let offset = bytes
        .windows(needle.len())
        .position(|part| part == needle)
        .expect("live row payload is present");
    let page_id = u32::try_from(offset / PAGE_SIZE).expect("fixture page ID fits");
    let page_offset = u64::from(page_id) * PAGE_SIZE as u64;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open fixture");
    file.seek(SeekFrom::Start(page_offset))
        .expect("seek live page");
    let mut image = [0; PAGE_SIZE];
    file.read_exact(&mut image).expect("read live page");
    let page = Page::from_bytes(image);
    page.verify_checksum().expect("original page checksum");
    assert_eq!(page.page_id(), page_id);
    if stage_recovery_image {
        let frame = reddb_file::encode_paged_dwb_frame([(page_id, page.as_bytes())]);
        file.seek(SeekFrom::Start(dwb_zone_offset()))
            .expect("seek DWB");
        file.write_all(&frame).expect("stage recovery image");
        file.sync_all().expect("DWB durable before torn home write");
    }
    file.seek(SeekFrom::Start(page_offset + 128))
        .expect("seek payload");
    file.write_all(&vec![0xA5; PAGE_SIZE / 2])
        .expect("tear home page");
    file.sync_all().expect("sync torn page");
    page_id
}

fn reopen_and_verify(path: &Path) {
    // Repeat the reopen to check that consuming the recovery frame stays durable.
    for _ in 0..2 {
        let rt = RedDBRuntime::with_options(operational_options(path))
            .expect("runtime reopens after torn write");
        let result = rt
            .execute_query("SELECT n, label FROM crash_rows ORDER BY n")
            .expect("select after recovery");
        assert_eq!(result.result.records.len(), ROWS);
        for (index, record) in result.result.records.iter().enumerate() {
            assert_eq!(
                record.get("n"),
                Some(&Value::Integer(i64::try_from(index).expect("row ID")))
            );
            assert_eq!(
                record.get("label"),
                Some(&Value::text(format!("row-{index}")))
            );
        }
    }
}

fn corruption_without_recovery_image_fails_closed(fold: bool) {
    let _guard = POLICY_GUARD
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    set_fold_dwb_into_wal_enabled(fold);
    std::env::remove_var("REDDB_FOLD_DWB_INTO_WAL");
    let db = populate("corruption_without_dwb");
    let page_id = corrupt_live_page(db.path(), false);
    match RedDBRuntime::with_options(operational_options(db.path())) {
        Err(RedDBError::StorageIntegrity(error)) => {
            assert_eq!(error.zone, "page");
            assert_eq!(error.id, page_id.to_string());
            assert_eq!(error.collection.as_deref(), Some("crash_rows"));
        }
        Err(error) => panic!("expected page integrity error, got {error:?}"),
        Ok(_) => panic!("corruption without a durable image must fail closed"),
    }
    set_fold_dwb_into_wal_enabled(false);
}

#[test]
fn committed_page_corruption_fails_closed_with_fold_off() {
    corruption_without_recovery_image_fails_closed(false);
}

#[test]
fn committed_page_corruption_fails_closed_with_fold_on() {
    corruption_without_recovery_image_fails_closed(true);
}

#[test]
fn crash_during_page_write_recovers_with_fold_off() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_fold_dwb_into_wal_enabled(false);
    std::env::remove_var("REDDB_FOLD_DWB_INTO_WAL");

    let db = populate("crash_fold_off");
    let path = db.path();
    corrupt_live_page(path, true);
    reopen_and_verify(path);
}

#[test]
fn crash_during_page_write_recovers_with_fold_on() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_fold_dwb_into_wal_enabled(true);
    std::env::remove_var("REDDB_FOLD_DWB_INTO_WAL");

    let db = populate("crash_fold_on");
    let path = db.path();
    corrupt_live_page(path, true);
    reopen_and_verify(path);
    set_fold_dwb_into_wal_enabled(false);
}
