//! Issue gh-478: fold DWB into WAL via FullPageImage (FPI) records.
//!
//! Acceptance for this slice:
//!  - flag `fold_dwb_into_wal` controls behaviour;
//!  - OFF (default): legacy `-dwb` sidecar created next to the datafile;
//!  - ON: `-dwb` sidecar is not opened, and any pre-existing `-dwb`
//!    file is removed at open time;
//!  - WAL `FullPageImage` records round-trip through encode/decode and
//!    are recoverable via `WalReader::collect_full_page_images`.
//!
//! Tier auto-enable + checkpoint-cycle FPI emission are deferred (see
//! issue notes).

use reddb::storage::wal::reader::WalReader;
use reddb::storage::wal::record::WalRecord;
use reddb::storage::wal::writer::WalWriter;
use reddb::{fold_dwb_into_wal_enabled, set_fold_dwb_into_wal_enabled, RedDBOptions, RedDBRuntime};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// Serialise tests that flip the process-global toggle.
static POLICY_GUARD: Mutex<()> = Mutex::new(());

fn persistent_path(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb_{prefix}_{unique}.rdb"))
}

fn dwb_path(data: &Path) -> PathBuf {
    let mut p = data.to_path_buf().into_os_string();
    p.push("-dwb");
    PathBuf::from(p)
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(dwb_path(path));
    let mut hdr = path.to_path_buf().into_os_string();
    hdr.push("-hdr");
    let _ = std::fs::remove_file(PathBuf::from(hdr));
    let mut meta = path.to_path_buf().into_os_string();
    meta.push("-meta");
    let _ = std::fs::remove_file(PathBuf::from(meta));
    let mut wal = path.to_path_buf();
    wal.set_extension("wal");
    let _ = std::fs::remove_file(&wal);
}

fn open_persistent(path: &Path) -> RedDBRuntime {
    let mut last_error = String::new();
    for _ in 0..20 {
        match RedDBRuntime::with_options(RedDBOptions::persistent(path)) {
            Ok(rt) => return rt,
            Err(err) if err.to_string().contains("Database is locked") => {
                last_error = err.to_string();
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(err) => panic!("persistent runtime opens: {err}"),
        }
    }
    panic!("persistent runtime opens after lock retry: {last_error}");
}

#[test]
fn fold_dwb_off_default_preserves_dwb_sidecar() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_fold_dwb_into_wal_enabled(false);
    std::env::remove_var("REDDB_FOLD_DWB_INTO_WAL");
    assert!(!fold_dwb_into_wal_enabled(), "default policy must be OFF",);

    let path = persistent_path("fold_dwb_off");
    cleanup(&path);

    {
        let rt = open_persistent(&path);
        rt.execute_query("CREATE TABLE dwb_off_a (name TEXT)")
            .expect("ddl");
        rt.execute_query("INSERT INTO dwb_off_a (name) VALUES ('a')")
            .expect("insert");
        rt.checkpoint().expect("flush");
    }

    let dwb = dwb_path(&path);
    assert!(
        dwb.exists(),
        "legacy -dwb sidecar must be present when fold is OFF: {dwb:?}",
    );

    cleanup(&path);
}

#[test]
fn fold_dwb_on_suppresses_and_removes_sidecar() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());

    let path = persistent_path("fold_dwb_on");
    cleanup(&path);

    // First, create a database with the flag OFF so a -dwb sidecar lands
    // on disk. Then flip ON and reopen: the sidecar must be cleaned up.
    set_fold_dwb_into_wal_enabled(false);
    {
        let rt = open_persistent(&path);
        rt.execute_query("CREATE TABLE dwb_on_a (name TEXT)")
            .expect("ddl");
        rt.checkpoint().expect("flush");
    }
    assert!(dwb_path(&path).exists(), "fixture must produce -dwb");

    set_fold_dwb_into_wal_enabled(true);
    {
        let rt = open_persistent(&path);
        rt.execute_query("INSERT INTO dwb_on_a (name) VALUES ('alpha')")
            .expect("insert");
        rt.checkpoint().expect("flush");
    }

    assert!(
        !dwb_path(&path).exists(),
        "-dwb sidecar must be removed when fold-DWB-into-WAL is ON",
    );

    // Reopen ON and verify data still readable (no DWB needed).
    {
        let rt = open_persistent(&path);
        let _ = rt
            .execute_query("SELECT name FROM dwb_on_a")
            .expect("select");
    }

    cleanup(&path);
    set_fold_dwb_into_wal_enabled(false);
}

#[test]
fn full_page_image_record_roundtrips_through_wal_file() {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let wal = std::env::temp_dir().join(format!("reddb_fpi_roundtrip_{unique}.wal"));
    let _ = std::fs::remove_file(&wal);

    let payload: Vec<u8> = (0..4096).map(|i| (i % 251) as u8).collect();

    {
        let mut w = WalWriter::open(&wal).expect("open wal writer");
        w.append(&WalRecord::FullPageImage {
            tx_id: 1,
            page_id: 17,
            ckpt_epoch: 5,
            data: payload.clone(),
        })
        .expect("append fpi #1");
        // Later FPI for same page id with newer epoch — recovery must
        // prefer the most recent image.
        let mut newer = payload.clone();
        newer[0] = 0xAA;
        w.append(&WalRecord::FullPageImage {
            tx_id: 2,
            page_id: 17,
            ckpt_epoch: 6,
            data: newer.clone(),
        })
        .expect("append fpi #2");
        // Distinct page id — must appear independently.
        w.append(&WalRecord::FullPageImage {
            tx_id: 3,
            page_id: 99,
            ckpt_epoch: 6,
            data: vec![0xCD; 4096],
        })
        .expect("append fpi #3");
        w.sync().expect("sync");
    }

    let images = WalReader::collect_full_page_images(&wal).expect("scan wal");
    assert_eq!(images.len(), 2, "two distinct page ids expected");
    let (_lsn, image) = images.get(&17).expect("page 17 image present");
    assert_eq!(image[0], 0xAA, "latest FPI must win for page 17");
    assert_eq!(image.len(), 4096);
    let (_lsn99, image99) = images.get(&99).expect("page 99 image present");
    assert_eq!(image99, &vec![0xCD; 4096]);

    let _ = std::fs::remove_file(&wal);
}
