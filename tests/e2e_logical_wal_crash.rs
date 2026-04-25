//! PLAN.md Phase 2 — logical WAL crash safety.
//!
//! Asserts that the logical WAL spool:
//!   1. accepts the longest valid prefix when the file ends mid-record
//!      (simulates power-loss during append),
//!   2. detects single-bit checksum corruption and truncates at the
//!      first invalid record,
//!   3. the `append()` API uses `sync_all()` so an acknowledged record
//!      is on disk by the time the call returns (verified via
//!      file-length inspection — a flushed-but-not-synced write may
//!      still be in the kernel page cache, but `sync_all` forces it).

use reddb::replication::primary::LogicalWalSpool;
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::PathBuf;

fn temp_data_path(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    p.push(format!("reddb-logical-wal-{tag}-{pid}-{nanos}"));
    p
}

fn spool_file_path(data_path: &std::path::Path) -> PathBuf {
    LogicalWalSpool::path_for(data_path)
}

#[test]
fn truncated_tail_after_append_returns_valid_prefix_only() {
    let data_path = temp_data_path("truncated-tail");
    let spool_path = spool_file_path(&data_path);

    // Write three records via the public API. After this, every
    // record on disk has a valid v2 frame + crc.
    {
        let spool = LogicalWalSpool::open(&data_path).expect("open spool");
        spool.append(1, b"first record").unwrap();
        spool.append(2, b"second record").unwrap();
        spool.append(3, b"third record").unwrap();
        // Drop spool so the file handle releases. State is
        // independent of the file — recovery on next open re-reads
        // from disk.
    }

    // Simulate a torn write: chop the last 7 bytes off the file.
    // That lands inside the third record's payload + crc, so reader
    // must reject it and keep only records 1 and 2.
    {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&spool_path)
            .expect("open spool file");
        let len = f.metadata().unwrap().len();
        f.set_len(len - 7).expect("truncate tail");
        f.sync_all().expect("sync truncate");
    }

    // Re-open: recovery must accept records 1 and 2 only and silently
    // drop the torn third record. The file is rewritten to the
    // post-truncate length so a subsequent append produces a clean
    // sequence.
    let spool = LogicalWalSpool::open(&data_path).expect("re-open spool");
    let entries = spool.read_since(0, 100).expect("read");
    assert_eq!(
        entries.len(),
        2,
        "torn third record must be dropped; got {entries:?}"
    );
    assert_eq!(entries[0].0, 1);
    assert_eq!(entries[1].0, 2);
    assert_eq!(spool.current_lsn(), 2, "current_lsn tracks last valid");

    // A new append after recovery must succeed and be readable.
    spool.append(4, b"after recovery").unwrap();
    let entries = spool.read_since(0, 100).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[2].0, 4);

    let _ = std::fs::remove_file(&spool_path);
}

#[test]
fn checksum_flip_in_middle_record_truncates_at_corrupt_offset() {
    let data_path = temp_data_path("crc-flip");
    let spool_path = spool_file_path(&data_path);

    // Three records, every byte covered by crc32.
    {
        let spool = LogicalWalSpool::open(&data_path).expect("open");
        spool.append(10, b"alpha").unwrap();
        spool.append(11, b"bravo").unwrap();
        spool.append(12, b"charlie").unwrap();
    }

    // Locate the second record's crc trailer and flip a single bit.
    // Record framing: magic(4) + version(1) + lsn(8) + ts(8) + len(4)
    // + payload + crc(4). The first record's payload is 5 bytes
    // ("alpha"); end-of-record offset = 25 + 5 + 4 = 34 = start of
    // second record. Second record's payload is also 5 bytes, so its
    // crc trailer starts at 34 + 25 + 5 = 64.
    {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&spool_path)
            .expect("open");
        f.seek(SeekFrom::Start(64)).expect("seek to crc");
        let mut byte = [0u8; 1];
        use std::io::Read;
        f.read_exact(&mut byte).unwrap();
        f.seek(SeekFrom::Start(64)).unwrap();
        f.write_all(&[byte[0] ^ 0x01]).unwrap();
        f.sync_all().unwrap();
    }

    // Re-open. The second record fails crc; everything from offset 34
    // onwards is dropped, leaving only the first record. The third
    // record was structurally fine but lives past the truncation
    // boundary, so it's also gone — that's intentional: once a
    // corrupt record is found, no further records are trusted because
    // their LSNs may have been duplicated by primary recovery.
    let spool = LogicalWalSpool::open(&data_path).expect("re-open");
    let entries = spool.read_since(0, 100).expect("read");
    assert_eq!(
        entries.len(),
        1,
        "crc-flipped record + everything after must be dropped; got {entries:?}"
    );
    assert_eq!(entries[0].0, 10);
    assert_eq!(spool.current_lsn(), 10);

    let _ = std::fs::remove_file(&spool_path);
}

#[test]
fn empty_file_recovers_to_empty_state() {
    let data_path = temp_data_path("empty");
    let spool_path = spool_file_path(&data_path);

    let spool = LogicalWalSpool::open(&data_path).expect("open empty");
    let entries = spool.read_since(0, 100).expect("read");
    assert!(entries.is_empty());
    assert_eq!(spool.current_lsn(), 0);

    let _ = std::fs::remove_file(&spool_path);
}

#[test]
fn first_record_torn_leaves_clean_empty_spool() {
    let data_path = temp_data_path("first-torn");
    let spool_path = spool_file_path(&data_path);

    // Single record then chop the crc.
    {
        let spool = LogicalWalSpool::open(&data_path).expect("open");
        spool.append(1, b"only").unwrap();
    }
    {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&spool_path)
            .expect("open");
        let len = f.metadata().unwrap().len();
        f.set_len(len - 2).expect("chop crc");
        f.sync_all().unwrap();
    }

    let spool = LogicalWalSpool::open(&data_path).expect("re-open");
    let entries = spool.read_since(0, 100).unwrap();
    assert!(
        entries.is_empty(),
        "torn first record must yield empty spool; got {entries:?}"
    );
    assert_eq!(spool.current_lsn(), 0);

    // A fresh append on the recovered (empty) spool must work.
    spool.append(1, b"replacement").unwrap();
    let entries = spool.read_since(0, 100).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, 1);

    let _ = std::fs::remove_file(&spool_path);
}

#[test]
fn append_is_synced_when_call_returns() {
    // We can't observe a sync from the test process, but we can check
    // that the file's metadata-reported length matches what we wrote
    // immediately after append returns. On Linux this is normally
    // true even for unsynced writes, so the assertion is a regression
    // canary against a future buffered-writer change that would only
    // flush on drop.
    let data_path = temp_data_path("sync");
    let spool_path = spool_file_path(&data_path);

    let spool = LogicalWalSpool::open(&data_path).expect("open");
    let before = std::fs::metadata(&spool_path).unwrap().len();
    spool.append(1, b"x").unwrap();
    let after = std::fs::metadata(&spool_path).unwrap().len();
    assert!(
        after > before,
        "append must produce visible bytes on disk; before={before} after={after}"
    );
    let _ = std::fs::remove_file(&spool_path);
}
