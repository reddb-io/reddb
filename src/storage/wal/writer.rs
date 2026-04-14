use super::record::{WalRecord, WAL_MAGIC, WAL_VERSION};
use std::fs::{File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

/// Writer for the Write-Ahead Log
pub struct WalWriter {
    file: File,
    /// Log Sequence Number — byte offset of the next record. Advances
    /// every `append`; survives across restarts via `seek(End)`.
    current_lsn: u64,
    /// Highest LSN that has been `sync_all()`'d to disk. The WAL-first
    /// flush invariant relies on this: a page with `header.lsn = L` may
    /// only be written to its data file once `durable_lsn >= L`.
    /// See `src/storage/cache/README.md` § Invariant 2 and the Target 3
    /// section of `PLAN.md`.
    durable_lsn: u64,
}

impl WalWriter {
    /// Open a WAL file for writing. Creates it if it doesn't exist.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let exists = path.as_ref().exists();

        let mut file = OpenOptions::new()
            .read(true) // Read needed for finding EOF LSN? No, seek is enough.
            .create(true)
            .append(true)
            .open(path)?;

        let current_lsn = if !exists || file.metadata()?.len() == 0 {
            // Write header for new file
            // Format: Magic (4) + Version (1) + Reserved (3)
            let mut header = Vec::with_capacity(8);
            header.extend_from_slice(WAL_MAGIC);
            header.push(WAL_VERSION);
            header.extend_from_slice(&[0u8; 3]); // Reserved

            file.write_all(&header)?;
            file.sync_all()?;
            8
        } else {
            // Existing file, set LSN to current end
            file.seek(SeekFrom::End(0))?
        };

        // On open, every byte already on disk is by definition durable
        // (any pre-crash unflushed tail was lost when the OS dropped
        // page cache). Initialise `durable_lsn` to `current_lsn`.
        Ok(Self {
            file,
            current_lsn,
            durable_lsn: current_lsn,
        })
    }

    /// Append a record to the WAL
    /// Returns the LSN (Log Sequence Number) of the record
    pub fn append(&mut self, record: &WalRecord) -> io::Result<u64> {
        let bytes = record.encode();
        self.file.write_all(&bytes)?;

        let record_lsn = self.current_lsn;
        self.current_lsn += bytes.len() as u64;

        Ok(record_lsn)
    }

    /// Force sync to disk. Updates `durable_lsn` so subsequent
    /// `flush_until` calls become no-ops up to `current_lsn`.
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.sync_all()?;
        self.durable_lsn = self.current_lsn;
        Ok(())
    }

    /// Ensure the WAL is durable on disk at least up to byte offset
    /// `target`. No-op when `target <= durable_lsn`.
    ///
    /// This is the postgres `XLogFlush(LSN)` analogue. Pager flush
    /// paths call this with `max(dirty.header.lsn)` before writing
    /// any data page so the WAL record describing the change is
    /// guaranteed to be on disk before the page itself.
    pub fn flush_until(&mut self, target: u64) -> io::Result<()> {
        if self.durable_lsn >= target {
            return Ok(());
        }
        self.file.sync_all()?;
        self.durable_lsn = self.current_lsn;
        Ok(())
    }

    /// Highest byte offset that is durable on disk. Used by the pager
    /// to decide whether a `flush_until` call would actually need a
    /// `fsync`.
    pub fn durable_lsn(&self) -> u64 {
        self.durable_lsn
    }

    /// Get current LSN (end of file offset)
    pub fn current_lsn(&self) -> u64 {
        self.current_lsn
    }

    /// Truncate the WAL (usually after checkpoint)
    pub fn truncate(&mut self) -> io::Result<()> {
        self.file.set_len(0)?;
        self.file.seek(SeekFrom::Start(0))?;

        // Rewrite header
        let mut header = Vec::with_capacity(8);
        header.extend_from_slice(WAL_MAGIC);
        header.push(WAL_VERSION);
        header.extend_from_slice(&[0u8; 3]);

        self.file.write_all(&header)?;
        self.file.sync_all()?;

        self.current_lsn = 8;
        self.durable_lsn = 8;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct FileGuard {
        path: PathBuf,
    }

    impl Drop for FileGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    fn temp_wal(name: &str) -> (FileGuard, PathBuf) {
        let path =
            std::env::temp_dir().join(format!("rb_wal_writer_{}_{}.wal", name, std::process::id()));
        let guard = FileGuard { path: path.clone() };
        let _ = std::fs::remove_file(&path);
        (guard, path)
    }

    #[test]
    fn test_create_new_wal() {
        let (_guard, path) = temp_wal("create");
        let writer = WalWriter::open(&path).unwrap();

        // Should start at LSN 8 (after 8-byte header)
        assert_eq!(writer.current_lsn(), 8);
        assert!(path.exists());
    }

    #[test]
    fn test_append_record() {
        let (_guard, path) = temp_wal("append");
        let mut writer = WalWriter::open(&path).unwrap();

        let record = WalRecord::Begin { tx_id: 42 };
        let lsn = writer.append(&record).unwrap();

        // First record starts at LSN 8
        assert_eq!(lsn, 8);

        // Next record should start after encoded size
        // Begin record: 1 (type) + 8 (tx_id) + 4 (checksum) = 13 bytes
        assert_eq!(writer.current_lsn(), 8 + 13);
    }

    #[test]
    fn test_append_multiple_records() {
        let (_guard, path) = temp_wal("multi");
        let mut writer = WalWriter::open(&path).unwrap();

        let lsn1 = writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
        let lsn2 = writer.append(&WalRecord::Begin { tx_id: 2 }).unwrap();
        let lsn3 = writer.append(&WalRecord::Commit { tx_id: 1 }).unwrap();

        assert_eq!(lsn1, 8);
        assert_eq!(lsn2, 8 + 13);
        assert_eq!(lsn3, 8 + 13 + 13);
    }

    #[test]
    fn test_page_write_lsn() {
        let (_guard, path) = temp_wal("pagewrite");
        let mut writer = WalWriter::open(&path).unwrap();

        // First record
        let lsn1 = writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
        assert_eq!(lsn1, 8);

        // PageWrite record: 1 + 8 + 4 + 4 + data_len + 4 = 21 + data_len
        let data = vec![1, 2, 3, 4, 5];
        let lsn2 = writer
            .append(&WalRecord::PageWrite {
                tx_id: 1,
                page_id: 100,
                data: data.clone(),
            })
            .unwrap();

        assert_eq!(lsn2, 8 + 13); // after Begin

        // Next LSN = lsn2 + (1 + 8 + 4 + 4 + 5 + 4) = lsn2 + 26
        assert_eq!(writer.current_lsn(), 8 + 13 + 26);
    }

    #[test]
    fn test_sync() {
        let (_guard, path) = temp_wal("sync");
        let mut writer = WalWriter::open(&path).unwrap();

        writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
        writer.sync().unwrap();

        // File should be synced, just verify no error
        assert!(path.exists());
    }

    #[test]
    fn test_truncate() {
        let (_guard, path) = temp_wal("truncate");
        let mut writer = WalWriter::open(&path).unwrap();

        // Write some records
        writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
        writer
            .append(&WalRecord::PageWrite {
                tx_id: 1,
                page_id: 0,
                data: vec![0; 100],
            })
            .unwrap();
        writer.append(&WalRecord::Commit { tx_id: 1 }).unwrap();

        let lsn_before = writer.current_lsn();
        assert!(lsn_before > 8);

        // Truncate
        writer.truncate().unwrap();

        // LSN should be back to 8
        assert_eq!(writer.current_lsn(), 8);

        // File should be 8 bytes (just header)
        let len = std::fs::metadata(&path).unwrap().len();
        assert_eq!(len, 8);
    }

    #[test]
    fn test_reopen_existing() {
        let (_guard, path) = temp_wal("reopen");

        // Create and write
        let lsn_after_write;
        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
            writer.append(&WalRecord::Commit { tx_id: 1 }).unwrap();
            lsn_after_write = writer.current_lsn();
        }

        // Reopen
        {
            let writer = WalWriter::open(&path).unwrap();
            // Should continue from where we left off
            assert_eq!(writer.current_lsn(), lsn_after_write);
        }
    }

    #[test]
    fn test_checkpoint_record() {
        let (_guard, path) = temp_wal("checkpoint");
        let mut writer = WalWriter::open(&path).unwrap();

        // Checkpoint is same size as Begin (1 + 8 + 4 = 13)
        let lsn = writer
            .append(&WalRecord::Checkpoint { lsn: 12345 })
            .unwrap();
        assert_eq!(lsn, 8);
        assert_eq!(writer.current_lsn(), 8 + 13);
    }

    // -----------------------------------------------------------------
    // Target 3: durable_lsn / flush_until tests
    // -----------------------------------------------------------------

    #[test]
    fn fresh_wal_has_durable_lsn_at_header_end() {
        let (_guard, path) = temp_wal("durable_init");
        let writer = WalWriter::open(&path).unwrap();
        assert_eq!(writer.durable_lsn(), 8);
        assert_eq!(writer.current_lsn(), 8);
    }

    #[test]
    fn flush_until_below_durable_is_noop() {
        let (_guard, path) = temp_wal("flush_noop");
        let mut writer = WalWriter::open(&path).unwrap();
        // After open, durable_lsn == 8.
        let before = writer.durable_lsn();
        writer.flush_until(0).unwrap();
        writer.flush_until(8).unwrap();
        assert_eq!(writer.durable_lsn(), before);
    }

    #[test]
    fn flush_until_advances_durable_to_current() {
        let (_guard, path) = temp_wal("flush_advance");
        let mut writer = WalWriter::open(&path).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 7 }).unwrap();
        writer.append(&WalRecord::Commit { tx_id: 7 }).unwrap();
        let target = writer.current_lsn();
        // Before flush_until, durable still at the header.
        assert_eq!(writer.durable_lsn(), 8);
        writer.flush_until(target).unwrap();
        assert_eq!(writer.durable_lsn(), target);
    }

    #[test]
    fn flush_until_is_monotonic() {
        let (_guard, path) = temp_wal("flush_monotonic");
        let mut writer = WalWriter::open(&path).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
        let lo = writer.current_lsn();
        writer.flush_until(lo).unwrap();
        let durable_after_lo = writer.durable_lsn();
        writer.append(&WalRecord::Commit { tx_id: 1 }).unwrap();
        let hi = writer.current_lsn();
        writer.flush_until(hi).unwrap();
        assert!(writer.durable_lsn() >= durable_after_lo);
        // Calling flush_until(lo) after flush_until(hi) is a no-op.
        writer.flush_until(lo).unwrap();
        assert_eq!(writer.durable_lsn(), hi);
    }

    #[test]
    fn sync_advances_durable_lsn_too() {
        let (_guard, path) = temp_wal("sync_durable");
        let mut writer = WalWriter::open(&path).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 9 }).unwrap();
        let before = writer.durable_lsn();
        let after_append = writer.current_lsn();
        assert!(after_append > before);
        writer.sync().unwrap();
        assert_eq!(writer.durable_lsn(), after_append);
    }

    #[test]
    fn truncate_resets_durable_lsn() {
        let (_guard, path) = temp_wal("truncate_durable");
        let mut writer = WalWriter::open(&path).unwrap();
        writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
        writer.sync().unwrap();
        assert!(writer.durable_lsn() > 8);
        writer.truncate().unwrap();
        assert_eq!(writer.durable_lsn(), 8);
        assert_eq!(writer.current_lsn(), 8);
    }

    #[test]
    fn reopen_initialises_durable_to_current() {
        let (_guard, path) = temp_wal("reopen_durable");
        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
            writer.sync().unwrap();
        }
        let writer = WalWriter::open(&path).unwrap();
        // After reopen, every byte on disk is durable by definition.
        assert_eq!(writer.durable_lsn(), writer.current_lsn());
    }
}
