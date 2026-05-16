//! Issue gh-478 iter 2: crash injection during page write — recovery
//! must reconstruct corrupt data pages via WAL replay (and, once the
//! pager wire-up lands, via FullPageImage records). The harness here
//! corrupts the trailing 4 KiB of the data file after a clean
//! shutdown and asserts the runtime reopens cleanly with all
//! committed rows reachable.
//!
//! Deterministic: no sleeps, no race-windows. The "crash" is modelled
//! by post-hoc file mutation, which is the same shape as
//! `e2e_seqn_journal_policy::recovery_handles_present_absent_and_corrupt_binary`.

use reddb::{set_fold_dwb_into_wal_enabled, RedDBOptions, RedDBRuntime};
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const PAGE_BYTES: u64 = 4096;
const ROWS: usize = 25;

// Serialise tests that flip the process-global fold-DWB toggle.
static POLICY_GUARD: Mutex<()> = Mutex::new(());

fn persistent_path(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb_{prefix}_{unique}.rdb"))
}

fn cleanup(path: &Path) {
    let _ = std::fs::remove_file(path);
    let mut p = path.to_path_buf().into_os_string();
    p.push("-dwb");
    let _ = std::fs::remove_file(PathBuf::from(p));
    let mut p = path.to_path_buf().into_os_string();
    p.push("-hdr");
    let _ = std::fs::remove_file(PathBuf::from(p));
    let mut p = path.to_path_buf().into_os_string();
    p.push("-meta");
    let _ = std::fs::remove_file(PathBuf::from(p));
    let mut wal = path.to_path_buf();
    wal.set_extension("wal");
    let _ = std::fs::remove_file(&wal);
}

/// Populate a fresh DB with `ROWS` rows in its own table and run a
/// checkpoint so the pages reach the main data file. Returns the
/// path on disk for the caller to corrupt.
fn populate(prefix: &str) -> PathBuf {
    let path = persistent_path(prefix);
    cleanup(&path);

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
        .expect("persistent runtime opens");
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
    path
}

/// Overwrite the trailing 4 KiB of the data file with deterministic
/// garbage. Models a torn write to the last page that landed
/// half-written before the process crashed.
fn corrupt_last_page(path: &Path) {
    let mut f = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open data file");
    let len = f.metadata().expect("metadata").len();
    assert!(
        len >= PAGE_BYTES,
        "data file must be at least one page long: got {len} bytes",
    );
    f.seek(SeekFrom::Start(len - PAGE_BYTES))
        .expect("seek last page");
    let garbage = vec![0xA5u8; PAGE_BYTES as usize];
    f.write_all(&garbage).expect("write garbage");
    f.sync_all().expect("sync");
}

fn reopen_and_count(path: &Path) -> usize {
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(path))
        .expect("runtime reopens after corruption");
    let result = rt
        .execute_query("SELECT n FROM crash_rows")
        .expect("select after recovery");
    result.result.records.len()
}

#[test]
fn crash_during_page_write_recovers_with_fold_off() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_fold_dwb_into_wal_enabled(false);
    std::env::remove_var("REDDB_FOLD_DWB_INTO_WAL");

    let path = populate("crash_fold_off");
    corrupt_last_page(&path);
    let recovered = reopen_and_count(&path);
    assert_eq!(
        recovered, ROWS,
        "WAL replay must reconstruct all committed rows when DWB is on",
    );
    cleanup(&path);
}

#[test]
fn crash_during_page_write_recovers_with_fold_on() {
    let _g = POLICY_GUARD.lock().unwrap_or_else(|err| err.into_inner());
    set_fold_dwb_into_wal_enabled(true);
    std::env::remove_var("REDDB_FOLD_DWB_INTO_WAL");

    let path = populate("crash_fold_on");
    corrupt_last_page(&path);
    let recovered = reopen_and_count(&path);
    assert_eq!(
        recovered, ROWS,
        "WAL replay must reconstruct all committed rows when fold-DWB-into-WAL is on",
    );
    cleanup(&path);
    set_fold_dwb_into_wal_enabled(false);
}
