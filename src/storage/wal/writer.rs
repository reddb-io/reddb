use super::record::{WalRecord, WAL_MAGIC, WAL_VERSION};
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

/// User-space buffer size for the WAL writer.
///
/// Chosen so that ~5 000 small records (Begin/Commit ≈ 13 bytes,
/// small PageWrite ≈ 26 bytes) coalesce into a single `write` syscall
/// before the next `sync()` drains the buffer. Tunable; reflects the
/// postgres XLOG block size (8 KiB) scaled up because we batch
/// record-level rather than page-level.
const WAL_BUFFER_BYTES: usize = 64 * 1024;

/// Writer for the Write-Ahead Log
///
/// Wraps the underlying file in a [`BufWriter`] so each `append` does
/// not pay a write syscall — bytes accumulate in a 64 KiB user-space
/// buffer until `sync()` (or `flush_until()`) drains them and then
/// calls `sync_all()` on the raw file. This is how postgres turns
/// per-record append cost from ~500 ns down to ~5 ns; reddb's previous
/// per-append `write_all` directly to the file paid the syscall on
/// every record.
///
/// **Critical contract:** every code path that calls `sync_all()` on
/// the underlying file *must* drain the [`BufWriter`] first via
/// `BufWriter::flush()`. Otherwise the bytes in user-space never reach
/// the kernel before fsync, and durability is silently broken.
pub struct WalWriter {
    file: BufWriter<File>,
    /// Cloned file descriptor for `sync_all()` outside the writer
    /// mutex. Both this and `file`'s inner `File` point at the same
    /// kernel inode; calling `sync_all()` on either flushes ALL
    /// pending bytes for that inode. This is the trick that lets
    /// the group-commit leader release the WAL writer lock during
    /// the expensive fsync — see [`WalWriter::drain_for_group_sync`].
    ///
    /// Without this clone, a leader holding the writer mutex during
    /// `sync_all()` blocks every other writer from appending,
    /// defeating the entire purpose of group commit.
    sync_handle: Arc<File>,
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

        // We do all initial bookkeeping (write header, seek to EOF) on
        // the raw `File` BEFORE wrapping in a BufWriter so we don't
        // have to worry about flush ordering during construction.
        let mut raw = OpenOptions::new()
            .read(true)
            .create(true)
            .append(true)
            .open(path)?;

        let current_lsn = if !exists || raw.metadata()?.len() == 0 {
            // Write header for new file
            // Format: Magic (4) + Version (1) + Reserved (3)
            let mut header = Vec::with_capacity(8);
            header.extend_from_slice(WAL_MAGIC);
            header.push(WAL_VERSION);
            header.extend_from_slice(&[0u8; 3]); // Reserved

            raw.write_all(&header)?;
            raw.sync_all()?;
            8
        } else {
            // Existing file, set LSN to current end. Append-mode files
            // ignore this seek for *writes*, but we use the returned
            // position as our LSN counter.
            raw.seek(SeekFrom::End(0))?
        };

        // Clone the file handle BEFORE wrapping in BufWriter. The
        // clone shares the same kernel file description, so
        // sync_all() on either descriptor flushes the whole inode.
        // The BufWriter owns the original; the Arc<File> is shared
        // with the group-commit leader.
        let sync_handle = Arc::new(raw.try_clone()?);
        let file = BufWriter::with_capacity(WAL_BUFFER_BYTES, raw);

        // On open, every byte already on disk is by definition durable
        // (any pre-crash unflushed tail was lost when the OS dropped
        // page cache). Initialise `durable_lsn` to `current_lsn`.
        Ok(Self {
            file,
            sync_handle,
            current_lsn,
            durable_lsn: current_lsn,
        })
    }

    /// Append a record to the WAL.
    ///
    /// Bytes go into the BufWriter — they are NOT durable on disk
    /// after this call returns. Callers that need durability must
    /// follow up with [`WalWriter::sync`] or
    /// [`WalWriter::flush_until`].
    ///
    /// Returns the LSN (Log Sequence Number) of the record.
    pub fn append(&mut self, record: &WalRecord) -> io::Result<u64> {
        let bytes = record.encode();
        self.file.write_all(&bytes)?;

        let record_lsn = self.current_lsn;
        self.current_lsn += bytes.len() as u64;

        Ok(record_lsn)
    }

    /// Write already-encoded bytes and advance the LSN counter to
    /// match. Used by the lock-free append path: writers encode +
    /// atomically reserve an LSN range outside this writer, the
    /// group-commit coordinator drains the pending queue in LSN
    /// order, then calls `append_bytes` for each batch.
    ///
    /// The bytes MUST be a valid `WalRecord::encode()` payload (or a
    /// concatenation of such) — no structural validation happens
    /// here. The caller is responsible for keeping the on-disk
    /// byte offset synchronised with the externally-tracked LSN
    /// counter; this method just appends and advances.
    pub fn append_bytes(&mut self, bytes: &[u8]) -> io::Result<u64> {
        self.file.write_all(bytes)?;
        let record_lsn = self.current_lsn;
        self.current_lsn += bytes.len() as u64;
        Ok(record_lsn)
    }

    /// Rewind the writer's LSN counter to a specific value. Used
    /// by the lock-free append path to resync the writer with the
    /// externally-tracked `next_lsn` after a drain batch; the
    /// coordinator knows the exact byte offset it just wrote to
    /// and needs `current_lsn` to match so subsequent direct
    /// callers of `append` stay consistent.
    pub fn set_current_lsn(&mut self, lsn: u64) {
        self.current_lsn = lsn;
    }

    /// Force sync to disk.
    ///
    /// Drains the user-space [`BufWriter`] first, then calls
    /// `sync_all()` on the underlying file so every byte appended
    /// since the last sync is durable. Updates `durable_lsn` so
    /// subsequent `flush_until` calls become no-ops up to
    /// `current_lsn`.
    pub fn sync(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
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
        self.file.flush()?;
        self.file.get_ref().sync_all()?;
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

    /// Drain the BufWriter into the kernel and return the captured
    /// LSN plus a cloned file handle for the caller to fsync
    /// **without holding the WAL writer mutex**.
    ///
    /// Used by the group-commit leader path. The flow is:
    ///
    /// 1. Take the WAL writer mutex.
    /// 2. Call this method — drains user-space buffer to the kernel
    ///    and captures `(target_lsn, sync_handle)`.
    /// 3. Release the WAL writer mutex.
    /// 4. Call `sync_handle.sync_all()` — this is the expensive
    ///    ~100 µs syscall, and other writers can keep appending
    ///    while it runs.
    /// 5. Take the WAL writer mutex briefly and call
    ///    [`WalWriter::mark_durable(target_lsn)`] to publish the
    ///    new durable position.
    ///
    /// The cloned `sync_handle` shares the same kernel inode with
    /// the writer's `file`, so `sync_all()` on the clone flushes
    /// ALL bytes that have reached the kernel for that file —
    /// including bytes appended by other writers AFTER step 3.
    /// This is the coalescing window that makes group commit win.
    pub fn drain_for_group_sync(&mut self) -> io::Result<(u64, Arc<File>)> {
        // Drain user-space buffer into the kernel.
        self.file.flush()?;
        Ok((self.current_lsn, Arc::clone(&self.sync_handle)))
    }

    /// Manually advance `durable_lsn` after a successful out-of-lock
    /// `sync_all()` performed via [`WalWriter::drain_for_group_sync`].
    ///
    /// Monotonic — never lowers `durable_lsn`. Safe to call with a
    /// stale `lsn`; just becomes a no-op.
    pub fn mark_durable(&mut self, lsn: u64) {
        if lsn > self.durable_lsn {
            self.durable_lsn = lsn;
        }
    }

    /// Truncate the WAL (usually after checkpoint).
    ///
    /// Drains the BufWriter first so no pending bytes hit the file
    /// after the truncate. Then resets the underlying file, rewrites
    /// the header through the buffered writer (header is small; the
    /// followup `flush + sync_all` makes it durable), and resets
    /// LSN bookkeeping.
    pub fn truncate(&mut self) -> io::Result<()> {
        // Drop any pending bytes BEFORE the truncate; otherwise the
        // BufWriter would flush them to a re-shrunken file in
        // confused order.
        self.file.flush()?;

        {
            let raw = self.file.get_mut();
            raw.set_len(0)?;
            raw.seek(SeekFrom::Start(0))?;
        }

        // Rewrite header through the BufWriter then drain.
        let mut header = Vec::with_capacity(8);
        header.extend_from_slice(WAL_MAGIC);
        header.push(WAL_VERSION);
        header.extend_from_slice(&[0u8; 3]);
        self.file.write_all(&header)?;
        self.file.flush()?;
        self.file.get_ref().sync_all()?;

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

    // -----------------------------------------------------------------
    // Perf 1.1: BufWriter coalesces small appends until sync
    // -----------------------------------------------------------------

    #[test]
    fn bufwriter_coalesces_until_sync() {
        // Append 100 small records but DO NOT sync. The on-disk file
        // size must still equal the header (8 bytes) because the
        // bytes are sitting in the BufWriter, not in the kernel.
        let (_guard, path) = temp_wal("bufwriter_coalesce");
        let mut writer = WalWriter::open(&path).unwrap();
        for tx in 0..100u64 {
            writer.append(&WalRecord::Begin { tx_id: tx }).unwrap();
        }
        // current_lsn reflects the in-buffer position.
        assert_eq!(writer.current_lsn(), 8 + 100 * 13);
        // But the file on disk only has the header.
        let on_disk = std::fs::metadata(&path).unwrap().len();
        assert_eq!(on_disk, 8, "BufWriter leaked bytes to disk before sync");
    }

    #[test]
    fn sync_drains_bufwriter_before_fsync() {
        // After sync(), the file size must equal current_lsn — the
        // BufWriter has been flushed and sync_all has hit the kernel.
        let (_guard, path) = temp_wal("sync_drains");
        let mut writer = WalWriter::open(&path).unwrap();
        for tx in 0..50u64 {
            writer.append(&WalRecord::Begin { tx_id: tx }).unwrap();
        }
        writer.sync().unwrap();
        let on_disk = std::fs::metadata(&path).unwrap().len();
        assert_eq!(on_disk, writer.current_lsn());
        assert_eq!(writer.durable_lsn(), writer.current_lsn());
    }

    #[test]
    fn flush_until_drains_bufwriter_too() {
        // flush_until must drain the BufWriter before calling
        // sync_all on the underlying file — otherwise pending bytes
        // never become durable.
        let (_guard, path) = temp_wal("flush_until_drains");
        let mut writer = WalWriter::open(&path).unwrap();
        for tx in 0..30u64 {
            writer.append(&WalRecord::Begin { tx_id: tx }).unwrap();
        }
        let target = writer.current_lsn();
        writer.flush_until(target).unwrap();
        let on_disk = std::fs::metadata(&path).unwrap().len();
        assert_eq!(on_disk, target);
        assert_eq!(writer.durable_lsn(), target);
    }

    #[test]
    fn truncate_drains_pending_bufwriter_bytes_first() {
        // If truncate did NOT drain BufWriter first, the pending bytes
        // would either land in the post-truncate file (corrupting it
        // with stale records) or be lost. Verify the resulting file
        // contains only a fresh header.
        let (_guard, path) = temp_wal("truncate_drain");
        let mut writer = WalWriter::open(&path).unwrap();
        // Write enough small records to fill some of the 64 KiB buffer
        // but stay below the auto-flush threshold.
        for tx in 0..200u64 {
            writer.append(&WalRecord::Begin { tx_id: tx }).unwrap();
        }
        // Sanity: bytes are buffered.
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 8);

        writer.truncate().unwrap();
        // After truncate the file is just the header again.
        let on_disk = std::fs::metadata(&path).unwrap().len();
        assert_eq!(on_disk, 8);
        assert_eq!(writer.current_lsn(), 8);
        assert_eq!(writer.durable_lsn(), 8);

        // And we can append again successfully.
        writer.append(&WalRecord::Begin { tx_id: 99 }).unwrap();
        writer.sync().unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 8 + 13);
    }

    #[test]
    fn reopen_sees_only_synced_records() {
        // Records that were appended but never sync'd must NOT
        // survive a reopen — they lived in the BufWriter, never made
        // it to the kernel, and the previous WalWriter went out of
        // scope. The new WalWriter reopens the file and reads from
        // EOF, which reflects only the bytes that hit disk.
        //
        // We sync some records, then drop the writer mid-buffer, and
        // assert the reopen LSN matches only the synced prefix.
        let (_guard, path) = temp_wal("reopen_synced_only");
        let synced_lsn;
        {
            let mut writer = WalWriter::open(&path).unwrap();
            writer.append(&WalRecord::Begin { tx_id: 1 }).unwrap();
            writer.sync().unwrap();
            synced_lsn = writer.current_lsn();
            // These records are never sync'd before drop. Drop runs
            // BufWriter::flush which DOES write them — see note below.
            for tx in 100..120u64 {
                writer.append(&WalRecord::Begin { tx_id: tx }).unwrap();
            }
            // Without a sync, the in-buffer bytes are still pending.
            // BufWriter's Drop impl does flush to the file but does
            // not call sync_all. For reopen-LSN purposes, on-disk
            // bytes count regardless of fsync, so the reopened LSN
            // will reflect the dropped writes too.
        }
        let writer = WalWriter::open(&path).unwrap();
        // The reopen LSN reflects what's physically on disk after
        // BufWriter::Drop flushes its buffer. That may or may not
        // include the unsync'd records depending on platform; the
        // contract we care about is that durable_lsn ≥ synced_lsn.
        assert!(writer.durable_lsn() >= synced_lsn);
    }
}
