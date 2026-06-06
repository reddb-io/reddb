//! Primary-side replication: WAL record production and snapshot serving.
//!
//! ## Logical WAL spool wire format
//!
//! ### Version 3 (current — issue #821)
//!
//! ```text
//! [magic     4 bytes  = b"RDLW"]
//! [version   1 byte   = 0x03]
//! [term      8 bytes  little-endian u64]
//! [lsn       8 bytes  little-endian u64]
//! [timestamp 8 bytes  little-endian u64 — wall-clock millis since UNIX epoch]
//! [payload_len 4 bytes little-endian u32]
//! [payload   payload_len bytes]
//! [crc32     4 bytes  little-endian u32 — crc32fast of (version || term ||
//!                                          lsn || timestamp || payload_len ||
//!                                          payload)]
//! ```
//!
//! ### Version 2 (legacy, read-only — PLAN.md Phase 2 / W2)
//!
//! ```text
//! [magic     4 bytes  = b"RDLW"]
//! [version   1 byte   = 0x02]
//! [lsn       8 bytes  little-endian u64]
//! [timestamp 8 bytes  little-endian u64 — wall-clock millis since UNIX epoch]
//! [payload_len 4 bytes little-endian u32]
//! [payload   payload_len bytes]
//! [crc32     4 bytes  little-endian u32 — crc32fast of (version || lsn ||
//!                                          timestamp || payload_len || payload)]
//! ```
//!
//! - `sync_all()` is called after every append so an acknowledged
//!   `append()` survives a power-loss event.
//! - Recovery accepts the longest valid prefix and silently truncates
//!   at the first torn header, short payload/crc, or checksum
//!   mismatch (warning logged). No partial record is ever returned to
//!   the replication subsystem.
//!
//! ### Version 1 (legacy, read-only)
//!
//! ```text
//! [magic 4][version 1=0x01][lsn 8][payload_len 8][payload]
//! ```
//!
//! No checksum, no timestamp. Read for backward compatibility on
//! existing spools; never written. A v1 record found in a spool will
//! be returned to consumers but flagged via `LogicalWalEntry::v1`.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tracing::warn;

const LOGICAL_WAL_SPOOL_MAGIC: &[u8; 4] = b"RDLW";
const LOGICAL_WAL_SPOOL_VERSION_V1: u8 = 1;
const LOGICAL_WAL_SPOOL_VERSION_V2: u8 = 2;
const LOGICAL_WAL_SPOOL_VERSION_V3: u8 = 3;
const LOGICAL_WAL_SPOOL_VERSION_CURRENT: u8 = LOGICAL_WAL_SPOOL_VERSION_V3;
/// Header size in bytes for a v3 record before the payload starts:
/// magic(4) + version(1) + term(8) + lsn(8) + timestamp(8) + payload_len(4) = 33.
const LOGICAL_WAL_V3_HEADER_LEN: u64 = 4 + 1 + 8 + 8 + 8 + 4;
/// CRC32 trailer size in bytes for logical spool records.
const LOGICAL_WAL_V2_CRC_LEN: u64 = 4;

/// Compute CRC32 over the bytes that follow the magic — version,
/// lsn, timestamp, payload_len, and payload. Magic is excluded so
/// torn-record detection at recovery time depends only on data the
/// writer covered.
///
/// Uses the same `crate::storage::engine::crc32` polynomial as the
/// physical WAL record format so checksums computed here are
/// comparable to those in `src/storage/wal/record.rs`.
fn compute_logical_v2_crc(version: u8, lsn: u64, timestamp: u64, payload: &[u8]) -> u32 {
    use crate::storage::engine::crc32::crc32_update;
    let mut crc = crc32_update(0, &[version]);
    crc = crc32_update(crc, &lsn.to_le_bytes());
    crc = crc32_update(crc, &timestamp.to_le_bytes());
    crc = crc32_update(crc, &(payload.len() as u32).to_le_bytes());
    crc = crc32_update(crc, payload);
    crc
}

fn compute_logical_v3_crc(version: u8, term: u64, lsn: u64, timestamp: u64, payload: &[u8]) -> u32 {
    use crate::storage::engine::crc32::crc32_update;
    let mut crc = crc32_update(0, &[version]);
    crc = crc32_update(crc, &term.to_le_bytes());
    crc = crc32_update(crc, &lsn.to_le_bytes());
    crc = crc32_update(crc, &timestamp.to_le_bytes());
    crc = crc32_update(crc, &(payload.len() as u32).to_le_bytes());
    crc = crc32_update(crc, payload);
    crc
}

fn term_from_payload(payload: &[u8]) -> u64 {
    crate::replication::cdc::ChangeRecord::decode(payload)
        .map(|record| record.term)
        .unwrap_or(crate::replication::DEFAULT_REPLICATION_TERM)
}

/// In-memory WAL buffer for replication.
/// Primary appends records here; replicas consume from it.
///
/// Each record payload is stored behind an `Arc<[u8]>` so fan-out to
/// multiple replicas shares a single heap allocation per record
/// (issue #832): a pull clones the `Arc` handle, never the bytes, so
/// adding replicas does not multiply the primary's send-buffer memory.
pub struct WalBuffer {
    /// Circular buffer of (lsn, ref-counted serialized record) pairs.
    records: RwLock<VecDeque<(u64, Arc<[u8]>)>>,
    /// Current write LSN.
    current_lsn: RwLock<u64>,
}

impl WalBuffer {
    pub fn new(max_size: usize) -> Self {
        Self {
            records: RwLock::new(VecDeque::with_capacity(max_size)),
            current_lsn: RwLock::new(0),
        }
    }

    /// Append a WAL record. Called by the storage engine after each write.
    pub fn append(&self, lsn: u64, data: Vec<u8>) {
        let mut records = self.records.write().unwrap_or_else(|e| e.into_inner());
        records.push_back((lsn, Arc::from(data.into_boxed_slice())));

        let mut current = self.current_lsn.write().unwrap_or_else(|e| e.into_inner());
        *current = (*current).max(lsn);
    }

    /// Read records since the given LSN (exclusive), copying each
    /// payload into an owned `Vec<u8>`. Kept for callers (WAL
    /// archiving, retention bookkeeping) that need owned bytes; the
    /// per-replica fan-out path should prefer [`Self::read_since_shared`]
    /// to avoid copying.
    pub fn read_since(&self, since_lsn: u64, max_count: usize) -> Vec<(u64, Vec<u8>)> {
        self.read_since_shared(since_lsn, max_count)
            .into_iter()
            .map(|(lsn, data)| (lsn, data.to_vec()))
            .collect()
    }

    /// Read records since the given LSN (exclusive) sharing the stored
    /// `Arc<[u8]>` payloads. Fan-out to N replicas clones only the
    /// reference-counted handles, so the buffer's bytes are never
    /// duplicated per replica (issue #832).
    pub fn read_since_shared(&self, since_lsn: u64, max_count: usize) -> Vec<(u64, Arc<[u8]>)> {
        let records = self.records.read().unwrap_or_else(|e| e.into_inner());
        records
            .iter()
            .filter(|(lsn, _)| *lsn > since_lsn)
            .take(max_count)
            .cloned()
            .collect()
    }

    /// Current LSN.
    pub fn current_lsn(&self) -> u64 {
        *self.current_lsn.read().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_current_lsn(&self, lsn: u64) {
        let mut current = self.current_lsn.write().unwrap_or_else(|e| e.into_inner());
        *current = (*current).max(lsn);
    }

    pub fn prune_through(&self, upto_lsn: u64) {
        let mut records = self.records.write().unwrap_or_else(|e| e.into_inner());
        while records
            .front()
            .map(|(lsn, _)| *lsn <= upto_lsn)
            .unwrap_or(false)
        {
            records.pop_front();
        }
    }

    /// Oldest available LSN (for gap detection).
    pub fn oldest_lsn(&self) -> Option<u64> {
        let records = self.records.read().unwrap_or_else(|e| e.into_inner());
        records.front().map(|(lsn, _)| *lsn)
    }
}

#[derive(Debug, Clone)]
struct LogicalWalEntry {
    term: u64,
    lsn: u64,
    /// Wall-clock millis at append time. `0` for legacy v1 records that
    /// did not carry a framing timestamp.
    timestamp_ms: u64,
    data: Vec<u8>,
}

impl LogicalWalEntry {
    fn data_with_framing_term(&self) -> Vec<u8> {
        match crate::replication::cdc::ChangeRecord::decode(&self.data) {
            Ok(mut record) if record.term != self.term => {
                record.term = self.term;
                record.encode()
            }
            _ => self.data.clone(),
        }
    }
}

/// One in every `SEEK_INDEX_INTERVAL` records is checkpointed into the
/// spool's in-memory seek index. A briefly-disconnected replica
/// resuming from its slot LSN binary-searches this sparse index and
/// seeks straight to the nearest preceding checkpoint, then scans
/// forward at most `SEEK_INDEX_INTERVAL` records — turning resume from
/// an O(n) full-file scan into a sub-linear seek (issue #832). The
/// index is rebuilt on `open` and extended on every `append`.
const SEEK_INDEX_INTERVAL: u64 = 64;

#[derive(Debug, Default)]
struct LogicalWalSpoolState {
    current_lsn: u64,
    /// Sparse, strictly LSN-ascending `(lsn, byte_offset)` checkpoints
    /// into the spool file. `byte_offset` is the start of the record
    /// whose LSN is `lsn`.
    seek_index: Vec<(u64, u64)>,
    /// Byte length of the spool file (offset at which the next append
    /// lands). Tracked so `append` can record a checkpoint's offset
    /// without an extra `stat`.
    write_offset: u64,
    /// Total records appended/recovered, used to space checkpoints
    /// `SEEK_INDEX_INTERVAL` records apart.
    record_count: u64,
}

impl LogicalWalSpoolState {
    /// Push a checkpoint for the record at `offset` if it falls on a
    /// `SEEK_INDEX_INTERVAL` boundary. `ordinal` is the record's
    /// zero-based position in the spool.
    fn note_record(&mut self, ordinal: u64, lsn: u64, offset: u64) {
        if ordinal.is_multiple_of(SEEK_INDEX_INTERVAL) {
            // Keep the index strictly ascending even if LSNs repeat
            // (they should not, but a defensive guard keeps the binary
            // search total).
            if self.seek_index.last().map(|(l, _)| *l) != Some(lsn) {
                self.seek_index.push((lsn, offset));
            }
        }
    }

    /// Byte offset to start a forward scan from when resuming at
    /// `since_lsn` (exclusive). Returns the offset of the latest
    /// checkpoint whose LSN is `<= since_lsn`, or `0` when no such
    /// checkpoint exists.
    fn seek_floor_offset(&self, since_lsn: u64) -> u64 {
        match self
            .seek_index
            .binary_search_by(|(lsn, _)| lsn.cmp(&since_lsn))
        {
            Ok(idx) => self.seek_index[idx].1,
            Err(0) => 0,
            Err(idx) => self.seek_index[idx - 1].1,
        }
    }
}

/// Durable append-only logical WAL spool kept beside the main `.rdb` file.
///
/// This is not the storage-engine WAL; it is a structured replication/PITR log.
pub struct LogicalWalSpool {
    path: PathBuf,
    state: Mutex<LogicalWalSpoolState>,
}

impl LogicalWalSpool {
    pub fn path_for(data_path: &Path) -> PathBuf {
        let file_name = data_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("reddb.rdb");
        let spool_name = format!("{file_name}.logical.wal");
        match data_path.parent() {
            Some(parent) => parent.join(spool_name),
            None => PathBuf::from(spool_name),
        }
    }

    pub fn open(data_path: &Path) -> io::Result<Self> {
        let path = Self::path_for(data_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            File::create(&path)?;
        }
        // Recover-or-truncate to the longest valid prefix. A torn tail
        // from the previous process exit (power loss, OOM kill, ENOSPC
        // mid-write) is silently dropped; the warning surfaces to the
        // operator log but the spool stays open.
        let entries = read_and_repair_entries(&path)?;
        let current_lsn = entries.last().map(|entry| entry.lsn).unwrap_or(0);
        // Rebuild the sparse seek index from the (now repaired) file so
        // a post-restart resume is sub-linear from the first pull.
        let (seek_index, write_offset, record_count) = build_seek_index(&path)?;
        Ok(Self {
            path,
            state: Mutex::new(LogicalWalSpoolState {
                current_lsn,
                seek_index,
                write_offset,
                record_count,
            }),
        })
    }

    pub fn append(&self, lsn: u64, data: &[u8]) -> io::Result<()> {
        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        self.append_with_timestamp(lsn, timestamp_ms, data)
    }

    /// Append a record with an explicit framing timestamp. Used in
    /// tests to produce deterministic timestamps; production callers
    /// should use `append`.
    pub fn append_with_timestamp(
        &self,
        lsn: u64,
        timestamp_ms: u64,
        data: &[u8],
    ) -> io::Result<()> {
        self.append_with_term_and_timestamp(term_from_payload(data), lsn, timestamp_ms, data)
    }

    pub fn append_with_term_and_timestamp(
        &self,
        term: u64,
        lsn: u64,
        timestamp_ms: u64,
        data: &[u8],
    ) -> io::Result<()> {
        if data.len() > u32::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "logical WAL payload of {} bytes exceeds 4 GiB framing limit",
                    data.len()
                ),
            ));
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        // Pre-build the record in memory so a single write_all keeps
        // the on-disk record contiguous. Two side-effects:
        //   (a) crash recovery sees either a complete record or a torn
        //       header, never an interleaved partial frame from two
        //       writers (the spool is not multi-writer today, but the
        //       single-write semantics make that future-safe);
        //   (b) crc32 is computed exactly once over the same bytes the
        //       reader will checksum, with zero risk of header/payload
        //       drift from a partial flush.
        let mut frame = Vec::with_capacity(
            LOGICAL_WAL_V3_HEADER_LEN as usize + data.len() + LOGICAL_WAL_V2_CRC_LEN as usize,
        );
        frame.extend_from_slice(LOGICAL_WAL_SPOOL_MAGIC);
        frame.push(LOGICAL_WAL_SPOOL_VERSION_CURRENT);
        frame.extend_from_slice(&term.to_le_bytes());
        frame.extend_from_slice(&lsn.to_le_bytes());
        frame.extend_from_slice(&timestamp_ms.to_le_bytes());
        frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
        frame.extend_from_slice(data);
        let crc = compute_logical_v3_crc(
            LOGICAL_WAL_SPOOL_VERSION_CURRENT,
            term,
            lsn,
            timestamp_ms,
            data,
        );
        frame.extend_from_slice(&crc.to_le_bytes());

        file.write_all(&frame)?;
        // PLAN.md Phase 2 mandates `sync_all` for logical WAL durability.
        // `flush()` only drains the std::io userspace buffer; without
        // `sync_all` the kernel page cache may still be dirty when an
        // acknowledged write supposedly committed.
        file.sync_all()?;

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current_lsn = state.current_lsn.max(lsn);
        // The record we just wrote starts at the prior end-of-file.
        // Checkpoint it into the seek index if it lands on an interval
        // boundary, then advance the tracked write offset.
        let record_start = state.write_offset;
        let ordinal = state.record_count;
        state.note_record(ordinal, lsn, record_start);
        state.write_offset = record_start + frame.len() as u64;
        state.record_count = ordinal + 1;
        Ok(())
    }

    pub fn read_since(&self, since_lsn: u64, max_count: usize) -> io::Result<Vec<(u64, Vec<u8>)>> {
        // Seek straight to the nearest indexed checkpoint at or before
        // `since_lsn` instead of scanning the whole spool from offset 0
        // (issue #832). The file was already repaired on `open`, so the
        // forward scan from the checkpoint is non-repairing and simply
        // stops at the first torn tail (left for the next `open` to fix).
        let start_offset = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.seek_floor_offset(since_lsn)
        };
        let entries = read_entries_from(&self.path, start_offset)?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.lsn > since_lsn)
            .take(max_count)
            .map(|entry| (entry.lsn, entry.data_with_framing_term()))
            .collect())
    }

    /// Byte offset a resume at `since_lsn` would seek to before
    /// forward-scanning. Exposed for tests asserting the resume is
    /// sub-linear (starts past offset 0 for a mid-spool LSN).
    #[cfg(test)]
    fn seek_floor_offset(&self, since_lsn: u64) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .seek_floor_offset(since_lsn)
    }

    pub fn current_lsn(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current_lsn
    }

    pub fn oldest_lsn(&self) -> io::Result<Option<u64>> {
        Ok(read_and_repair_entries(&self.path)?
            .into_iter()
            .next()
            .map(|entry| entry.lsn))
    }

    pub fn prune_through(&self, upto_lsn: u64) -> io::Result<()> {
        let previous_lsn = self.current_lsn();
        let retained: Vec<_> = read_and_repair_entries(&self.path)?
            .into_iter()
            .filter(|entry| entry.lsn > upto_lsn)
            .collect();
        let temp_path = self.path.with_extension("logical.wal.tmp");
        let mut temp = File::create(&temp_path)?;
        let mut current_lsn = 0;
        for entry in retained {
            // Re-frame as v3 so the spool only ever contains current records
            // after a prune. Legacy v1 records are upgraded by carrying
            // their original LSN and default term forward; the framing timestamp is
            // re-stamped to wall-clock-now because the original v1
            // record didn't carry one — downstream consumers that need
            // the operation's logical timestamp continue to use the
            // payload's own ChangeRecord::timestamp field.
            let timestamp_ms = if entry.timestamp_ms > 0 {
                entry.timestamp_ms
            } else {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            };
            let crc = compute_logical_v3_crc(
                LOGICAL_WAL_SPOOL_VERSION_CURRENT,
                entry.term,
                entry.lsn,
                timestamp_ms,
                &entry.data,
            );
            temp.write_all(LOGICAL_WAL_SPOOL_MAGIC)?;
            temp.write_all(&[LOGICAL_WAL_SPOOL_VERSION_CURRENT])?;
            temp.write_all(&entry.term.to_le_bytes())?;
            temp.write_all(&entry.lsn.to_le_bytes())?;
            temp.write_all(&timestamp_ms.to_le_bytes())?;
            temp.write_all(&(entry.data.len() as u32).to_le_bytes())?;
            temp.write_all(&entry.data)?;
            temp.write_all(&crc.to_le_bytes())?;
            current_lsn = current_lsn.max(entry.lsn);
        }
        temp.sync_all()?;
        fs::rename(&temp_path, &self.path)?;

        // The rewrite shifted every record's byte offset, so the old
        // seek index is stale — rebuild it from the compacted file.
        let (seek_index, write_offset, record_count) = build_seek_index(&self.path)?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current_lsn = previous_lsn.max(current_lsn).max(upto_lsn);
        state.seek_index = seek_index;
        state.write_offset = write_offset;
        state.record_count = record_count;
        Ok(())
    }
}

/// Reads every logical-WAL record from `path`, accepting the longest
/// valid prefix and *truncating* the file at the first torn or
/// corrupt record. Designed for crash recovery: a process killed
/// mid-write leaves a partial frame that this function silently drops
/// so the spool can resume appending without ambiguity.
///
/// Detection of "stop here" cases:
///   1. `UnexpectedEof` while reading any header field, payload, or
///      crc → torn write at end of file.
///   2. Magic mismatch (any 4 bytes that aren't `RDLW`) → corrupt or
///      foreign data; treated as if the file ended at the start of
///      this record.
///   3. v2 record with unsupported version byte → same.
///   4. v2 CRC mismatch → record corrupt; truncated.
///
/// The truncation only fires when at least one valid record precedes
/// the corrupt region (or when the corrupt region is the very first
/// record — in which case the spool becomes empty). Either way the
/// invariant that callers see only fully-checksummed payloads is
/// preserved.
///
/// v1 records (legacy, no checksum) are accepted for read-only
/// compatibility. They never receive a checksum; a v1 read that hits
/// `UnexpectedEof` mid-payload also triggers truncation.
fn read_and_repair_entries(path: &Path) -> io::Result<Vec<LogicalWalEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut entries = Vec::new();
    let mut last_good_offset: u64 = 0;
    let mut corrupt_reason: Option<String> = None;

    loop {
        let record_start = file.stream_position()?;

        let mut magic = [0u8; 4];
        match file.read_exact(&mut magic) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err),
        }
        if &magic != LOGICAL_WAL_SPOOL_MAGIC {
            corrupt_reason = Some(format!(
                "bad magic at offset {record_start}: got {magic:02x?}"
            ));
            break;
        }

        let mut version = [0u8; 1];
        if let Err(err) = file.read_exact(&mut version) {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                corrupt_reason = Some(format!("torn header at offset {record_start}"));
                break;
            }
            return Err(err);
        }

        let entry_result = match version[0] {
            LOGICAL_WAL_SPOOL_VERSION_V3 => read_one_v3(&mut file, record_start),
            LOGICAL_WAL_SPOOL_VERSION_V2 => read_one_v2(&mut file, record_start),
            LOGICAL_WAL_SPOOL_VERSION_V1 => read_one_v1(&mut file, record_start),
            other => {
                corrupt_reason = Some(format!(
                    "unsupported version {other} at offset {record_start}"
                ));
                break;
            }
        };

        match entry_result {
            Ok(entry) => {
                entries.push(entry);
                last_good_offset = file.stream_position()?;
            }
            Err(reason) => {
                corrupt_reason = Some(reason);
                break;
            }
        }
    }

    if let Some(reason) = corrupt_reason {
        let total_len = file.metadata()?.len();
        if last_good_offset < total_len {
            warn!(
                target: "reddb::replication::logical_wal",
                path = %path.display(),
                reason = %reason,
                truncating_from = last_good_offset,
                truncating_to = total_len,
                kept_records = entries.len(),
                "truncating logical-WAL spool to last valid record"
            );
            file.set_len(last_good_offset)?;
            file.sync_all()?;
        }
    }

    Ok(entries)
}

/// Read the single record whose magic byte begins at the current file
/// cursor. Returns `Ok(Some(entry))` on a valid record, `Ok(None)`
/// when the cursor is at a clean EOF or a torn/corrupt frame (the
/// caller stops scanning), and `Err` only on an unexpected I/O fault.
///
/// Unlike [`read_and_repair_entries`] this never truncates: it is the
/// read-time forward-scan primitive used after a seek, where the file
/// was already repaired at `open`.
fn read_frame(file: &mut File, record_start: u64) -> io::Result<Option<LogicalWalEntry>> {
    let mut magic = [0u8; 4];
    match file.read_exact(&mut magic) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    if &magic != LOGICAL_WAL_SPOOL_MAGIC {
        return Ok(None);
    }
    let mut version = [0u8; 1];
    match file.read_exact(&mut version) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let entry = match version[0] {
        LOGICAL_WAL_SPOOL_VERSION_V3 => read_one_v3(file, record_start),
        LOGICAL_WAL_SPOOL_VERSION_V2 => read_one_v2(file, record_start),
        LOGICAL_WAL_SPOOL_VERSION_V1 => read_one_v1(file, record_start),
        _ => return Ok(None),
    };
    Ok(entry.ok())
}

/// Forward-scan valid records starting at `start_offset`, stopping at
/// the first clean EOF or torn/corrupt frame. Non-repairing — used by
/// the seek-based [`LogicalWalSpool::read_since`] resume path.
fn read_entries_from(path: &Path, start_offset: u64) -> io::Result<Vec<LogicalWalEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut file = OpenOptions::new().read(true).open(path)?;
    file.seek(SeekFrom::Start(start_offset))?;
    let mut entries = Vec::new();
    loop {
        let record_start = file.stream_position()?;
        match read_frame(&mut file, record_start)? {
            Some(entry) => entries.push(entry),
            None => break,
        }
    }
    Ok(entries)
}

/// Build the sparse seek index by walking the (repaired) spool once,
/// checkpointing every `SEEK_INDEX_INTERVAL`-th record. Returns the
/// index, the byte offset just past the last valid record, and the
/// total record count.
fn build_seek_index(path: &Path) -> io::Result<(Vec<(u64, u64)>, u64, u64)> {
    if !path.exists() {
        return Ok((Vec::new(), 0, 0));
    }
    let mut file = OpenOptions::new().read(true).open(path)?;
    let mut index = Vec::new();
    let mut ordinal: u64 = 0;
    let mut write_offset: u64 = 0;
    loop {
        let record_start = file.stream_position()?;
        match read_frame(&mut file, record_start)? {
            Some(entry) => {
                if ordinal.is_multiple_of(SEEK_INDEX_INTERVAL)
                    && index.last().map(|(l, _)| *l) != Some(entry.lsn)
                {
                    index.push((entry.lsn, record_start));
                }
                ordinal += 1;
                write_offset = file.stream_position()?;
            }
            None => break,
        }
    }
    Ok((index, write_offset, ordinal))
}

/// Read a v3 record assuming the magic + version byte have already
/// been consumed and the file cursor sits at the term field.
fn read_one_v3(file: &mut File, record_start: u64) -> Result<LogicalWalEntry, String> {
    let mut term = [0u8; 8];
    if let Err(err) = file.read_exact(&mut term) {
        return Err(format!("torn term at offset {record_start}: {err}"));
    }
    let mut lsn = [0u8; 8];
    if let Err(err) = file.read_exact(&mut lsn) {
        return Err(format!("torn lsn at offset {record_start}: {err}"));
    }
    let mut timestamp = [0u8; 8];
    if let Err(err) = file.read_exact(&mut timestamp) {
        return Err(format!("torn timestamp at offset {record_start}: {err}"));
    }
    let mut len_bytes = [0u8; 4];
    if let Err(err) = file.read_exact(&mut len_bytes) {
        return Err(format!(
            "torn payload length at offset {record_start}: {err}"
        ));
    }
    let payload_len = u32::from_le_bytes(len_bytes) as usize;
    const MAX_PLAUSIBLE_PAYLOAD: usize = 256 * 1024 * 1024;
    if payload_len > MAX_PLAUSIBLE_PAYLOAD {
        return Err(format!(
            "implausible payload_len {payload_len} at offset {record_start}"
        ));
    }
    let mut payload = vec![0u8; payload_len];
    if let Err(err) = file.read_exact(&mut payload) {
        return Err(format!(
            "torn payload at offset {record_start} (expected {payload_len} bytes): {err}"
        ));
    }
    let mut crc_bytes = [0u8; 4];
    if let Err(err) = file.read_exact(&mut crc_bytes) {
        return Err(format!("torn crc at offset {record_start}: {err}"));
    }
    let stored_crc = u32::from_le_bytes(crc_bytes);
    let term = u64::from_le_bytes(term);
    let lsn = u64::from_le_bytes(lsn);
    let timestamp = u64::from_le_bytes(timestamp);
    let expected_crc =
        compute_logical_v3_crc(LOGICAL_WAL_SPOOL_VERSION_V3, term, lsn, timestamp, &payload);
    if stored_crc != expected_crc {
        return Err(format!(
            "crc mismatch at offset {record_start}: stored {stored_crc:#010x}, expected {expected_crc:#010x}"
        ));
    }
    Ok(LogicalWalEntry {
        term,
        lsn,
        timestamp_ms: timestamp,
        data: payload,
    })
}

/// Read a v2 record assuming the magic + version byte have already
/// been consumed and the file cursor sits at the LSN field. Returns
/// `Err(reason)` for any condition that should trigger truncation.
fn read_one_v2(file: &mut File, record_start: u64) -> Result<LogicalWalEntry, String> {
    let mut lsn = [0u8; 8];
    if let Err(err) = file.read_exact(&mut lsn) {
        return Err(format!("torn lsn at offset {record_start}: {err}"));
    }
    let mut timestamp = [0u8; 8];
    if let Err(err) = file.read_exact(&mut timestamp) {
        return Err(format!("torn timestamp at offset {record_start}: {err}"));
    }
    let mut len_bytes = [0u8; 4];
    if let Err(err) = file.read_exact(&mut len_bytes) {
        return Err(format!(
            "torn payload length at offset {record_start}: {err}"
        ));
    }
    let payload_len = u32::from_le_bytes(len_bytes) as usize;
    // Sanity guard against a runaway length encoded by a partially-
    // corrupted header. 256 MiB is well above any plausible single
    // ChangeRecord and well below memory we'd allocate from a torn
    // header that happens to look like a real frame.
    const MAX_PLAUSIBLE_PAYLOAD: usize = 256 * 1024 * 1024;
    if payload_len > MAX_PLAUSIBLE_PAYLOAD {
        return Err(format!(
            "implausible payload_len {payload_len} at offset {record_start}"
        ));
    }
    let mut payload = vec![0u8; payload_len];
    if let Err(err) = file.read_exact(&mut payload) {
        return Err(format!(
            "torn payload at offset {record_start} (expected {payload_len} bytes): {err}"
        ));
    }
    let mut crc_bytes = [0u8; 4];
    if let Err(err) = file.read_exact(&mut crc_bytes) {
        return Err(format!("torn crc at offset {record_start}: {err}"));
    }
    let stored_crc = u32::from_le_bytes(crc_bytes);
    let expected_crc = compute_logical_v2_crc(
        LOGICAL_WAL_SPOOL_VERSION_V2,
        u64::from_le_bytes(lsn),
        u64::from_le_bytes(timestamp),
        &payload,
    );
    if stored_crc != expected_crc {
        return Err(format!(
            "crc mismatch at offset {record_start}: stored {stored_crc:#010x}, expected {expected_crc:#010x}"
        ));
    }
    let term = term_from_payload(&payload);
    Ok(LogicalWalEntry {
        term,
        lsn: u64::from_le_bytes(lsn),
        timestamp_ms: u64::from_le_bytes(timestamp),
        data: payload,
    })
}

/// Read a v1 record (legacy, no checksum). Layout after magic+version:
/// [lsn 8][payload_len 8][payload]. v1 spools were written before
/// PLAN.md Phase 2 hardened the format; we read them so existing dev
/// installs don't drop history on upgrade.
fn read_one_v1(file: &mut File, record_start: u64) -> Result<LogicalWalEntry, String> {
    let mut lsn = [0u8; 8];
    if let Err(err) = file.read_exact(&mut lsn) {
        return Err(format!("v1 torn lsn at offset {record_start}: {err}"));
    }
    let mut len_bytes = [0u8; 8];
    if let Err(err) = file.read_exact(&mut len_bytes) {
        return Err(format!(
            "v1 torn payload length at offset {record_start}: {err}"
        ));
    }
    let payload_len = u64::from_le_bytes(len_bytes) as usize;
    if payload_len > 256 * 1024 * 1024 {
        return Err(format!(
            "v1 implausible payload_len {payload_len} at offset {record_start}"
        ));
    }
    let mut payload = vec![0u8; payload_len];
    if let Err(err) = file.read_exact(&mut payload) {
        return Err(format!("v1 torn payload at offset {record_start}: {err}"));
    }
    let term = term_from_payload(&payload);
    Ok(LogicalWalEntry {
        term,
        lsn: u64::from_le_bytes(lsn),
        timestamp_ms: 0,
        data: payload,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotInvalidationCause {
    WalRemoved,
    Horizon,
    IdleTimeout,
}

impl SlotInvalidationCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WalRemoved => "wal-removed",
            Self::Horizon => "horizon",
            Self::IdleTimeout => "idle-timeout",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "wal-removed" => Some(Self::WalRemoved),
            "horizon" => Some(Self::Horizon),
            "idle-timeout" => Some(Self::IdleTimeout),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReplicationSlot {
    pub id: String,
    pub restart_lsn: u64,
    pub confirmed_lsn: u64,
    pub last_seen_at_unix_ms: u128,
    pub invalidation_reason: Option<SlotInvalidationCause>,
    pub invalidated_at_unix_ms: Option<u128>,
}

fn load_replication_slots(path: Option<&Path>, now_ms: u128) -> BTreeMap<String, ReplicationSlot> {
    let Some(path) = path else {
        return BTreeMap::new();
    };
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(err) => {
            warn!(
                target: "reddb::replication::slots",
                path = %path.display(),
                error = %err,
                "failed to read replication slot store"
            );
            return BTreeMap::new();
        }
    };
    match crate::serde_json::from_slice::<crate::serde_json::Value>(&bytes) {
        Ok(value) => value
            .get("slots")
            .and_then(crate::serde_json::Value::as_array)
            .unwrap_or(&[])
            .iter()
            .filter_map(|value| {
                let object = value.as_object()?;
                let id = object.get("id")?.as_str()?.to_string();
                let restart_lsn = object.get("restart_lsn")?.as_u64()?;
                let confirmed_lsn = object.get("confirmed_lsn")?.as_u64()?;
                let last_seen_at_unix_ms = object
                    .get("last_seen_at_unix_ms")
                    .and_then(crate::serde_json::Value::as_u64)
                    .map(u128::from)
                    .unwrap_or(now_ms);
                let invalidation_reason = object
                    .get("invalidation_reason")
                    .and_then(crate::serde_json::Value::as_str)
                    .and_then(SlotInvalidationCause::from_str);
                let invalidated_at_unix_ms = object
                    .get("invalidated_at_unix_ms")
                    .and_then(crate::serde_json::Value::as_u64)
                    .map(u128::from);
                Some((
                    id.clone(),
                    ReplicationSlot {
                        id,
                        restart_lsn,
                        confirmed_lsn,
                        last_seen_at_unix_ms,
                        invalidation_reason,
                        invalidated_at_unix_ms,
                    },
                ))
            })
            .collect(),
        Err(err) => {
            warn!(
                target: "reddb::replication::slots",
                path = %path.display(),
                error = %err,
                "failed to decode replication slot store"
            );
            BTreeMap::new()
        }
    }
}

fn persist_replication_slots(
    path: Option<&Path>,
    slots: &BTreeMap<String, ReplicationSlot>,
) -> io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp_path = path.with_extension("logical.slots.tmp");
    let slots_json = slots
        .values()
        .map(|slot| {
            let mut object = crate::serde_json::Map::new();
            object.insert(
                "id".to_string(),
                crate::serde_json::Value::String(slot.id.clone()),
            );
            object.insert(
                "restart_lsn".to_string(),
                crate::serde_json::Value::Number(slot.restart_lsn as f64),
            );
            object.insert(
                "confirmed_lsn".to_string(),
                crate::serde_json::Value::Number(slot.confirmed_lsn as f64),
            );
            object.insert(
                "last_seen_at_unix_ms".to_string(),
                crate::serde_json::Value::Number(slot.last_seen_at_unix_ms as f64),
            );
            if let Some(reason) = slot.invalidation_reason {
                object.insert(
                    "invalidation_reason".to_string(),
                    crate::serde_json::Value::String(reason.as_str().to_string()),
                );
            }
            if let Some(invalidated_at) = slot.invalidated_at_unix_ms {
                object.insert(
                    "invalidated_at_unix_ms".to_string(),
                    crate::serde_json::Value::Number(invalidated_at as f64),
                );
            }
            crate::serde_json::Value::Object(object)
        })
        .collect();
    let mut root = crate::serde_json::Map::new();
    root.insert(
        "slots".to_string(),
        crate::serde_json::Value::Array(slots_json),
    );
    let value = crate::serde_json::Value::Object(root);
    let bytes = crate::serde_json::to_string_pretty(&value)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    let mut temp = File::create(&temp_path)?;
    temp.write_all(bytes.as_bytes())?;
    temp.sync_all()?;
    fs::rename(&temp_path, path)?;
    Ok(())
}

/// State of a connected replica. PLAN.md Phase 11.4 fields:
/// `last_seen_at_unix_ms` updates on every interaction (pull or ack);
/// `last_sent_lsn` updates when the primary serves a `pull_wal_records`
/// batch; `last_durable_lsn` updates when the replica reports its WAL
/// is durably written via `ack_replica_lsn`.
#[derive(Debug, Clone)]
pub struct ReplicaState {
    pub id: String,
    pub last_acked_lsn: u64,
    pub last_sent_lsn: u64,
    pub last_durable_lsn: u64,
    pub apply_error_count: u64,
    pub divergence_count: u64,
    pub connected_at_unix_ms: u128,
    pub last_seen_at_unix_ms: u128,
    /// Region identifier declared by the replica at handshake time
    /// (Phase 2.6 multi-region PG parity). `None` until the replica
    /// handshake extension lands in 2.6.2; the quorum coordinator's
    /// region-binding map covers the in-process case meanwhile.
    pub region: Option<String>,
    /// `true` while this replica is re-bootstrapping — loading a fresh
    /// snapshot to replace its current dataset (issue #837). It keeps
    /// serving non-causal reads from the old data, but the advertiser
    /// surfaces this flag so a causal reader routes bookmark reads
    /// elsewhere: the replica's `last_acked_lsn` describes data it is
    /// about to discard. Cleared atomically when the swap completes.
    pub rebootstrapping: bool,
}

/// Primary-side replication progress derived from the replica registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplicationProgress {
    pub lag_lsn: u64,
    pub safe_replay_lsn: u64,
}

impl ReplicationProgress {
    pub fn from_replicas(replicas: &[ReplicaState]) -> Option<Self> {
        let max_sent_lsn = replicas.iter().map(|replica| replica.last_sent_lsn).max()?;
        let min_acked_lsn = replicas
            .iter()
            .map(|replica| replica.last_acked_lsn)
            .min()?;
        let safe_replay_lsn = replicas
            .iter()
            .map(|replica| replica.last_durable_lsn)
            .min()?;

        Some(Self {
            lag_lsn: max_sent_lsn.saturating_sub(min_acked_lsn),
            safe_replay_lsn,
        })
    }
}

/// Primary replication manager.
pub struct PrimaryReplication {
    pub wal_buffer: Arc<WalBuffer>,
    pub logical_wal_spool: Option<Arc<LogicalWalSpool>>,
    pub replicas: RwLock<Vec<ReplicaState>>,
    wal_appended: (Mutex<u64>, Condvar),
    slot_path: Option<PathBuf>,
    slots: RwLock<BTreeMap<String, ReplicationSlot>>,
    slot_retention_max_lag_lsn: u64,
    slot_idle_timeout_ms: u64,
    /// PLAN.md Phase 11.4 — ack-driven commit synchronization. Always
    /// allocated so the policy enum can flip from `Local` to
    /// `AckN`/`Quorum` without touching this struct's shape.
    pub commit_waiter: Arc<crate::replication::commit_waiter::CommitWaiter>,
    /// Monotonic registry-change counter consumed by the
    /// `TopologyAdvertiser` (issue #167). Bumps on register,
    /// unregister, and the periodic health sweep when a replica
    /// flips between healthy/unhealthy. Clients use the epoch to
    /// detect stale advertisements without comparing the full
    /// replica list element-wise.
    topology_epoch: std::sync::atomic::AtomicU64,
    /// Count of pulls served as a partial resync — a replica resuming
    /// incrementally from its retained slot position rather than
    /// triggering a full re-bootstrap (issue #832). Surfaced as a
    /// replication metric so a brief disconnect that recovers via
    /// partial resync is observable.
    partial_resync_count: std::sync::atomic::AtomicU64,
    /// Count of pulls that forced a full re-bootstrap — the replica's
    /// retained WAL no longer covers its requested position, so it must
    /// discard its dataset and reload a fresh snapshot (issue #839).
    /// This is the primary alert signal: a healthy cluster re-bootstraps
    /// rarely, so any sustained rise means slots are being invalidated
    /// faster than replicas can keep up.
    full_resync_count: std::sync::atomic::AtomicU64,
}

/// How a replica's pull should be served, decided from its slot state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeMode {
    /// Resume incrementally from `resume_lsn` (the replica's slot
    /// position, never behind it). The retained WAL still covers the
    /// gap, so a brief disconnect costs only a partial resync.
    PartialResync { resume_lsn: u64 },
    /// The slot is past the retention cap (or otherwise invalidated);
    /// the replica must discard and re-bootstrap from a fresh snapshot.
    FullRebootstrap { cause: SlotInvalidationCause },
}

impl PrimaryReplication {
    pub fn slot_path_for(data_path: &Path) -> PathBuf {
        let file_name = data_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("reddb.rdb");
        let slot_name = format!("{file_name}.logical.slots.json");
        match data_path.parent() {
            Some(parent) => parent.join(slot_name),
            None => PathBuf::from(slot_name),
        }
    }

    pub fn new(data_path: Option<&Path>) -> Self {
        Self::new_with_config(data_path, &crate::replication::ReplicationConfig::primary())
    }

    pub fn new_with_config(
        data_path: Option<&Path>,
        config: &crate::replication::ReplicationConfig,
    ) -> Self {
        let now_ms = crate::utils::now_unix_millis() as u128;
        let slot_path = data_path.map(Self::slot_path_for);
        let slots = load_replication_slots(slot_path.as_deref(), now_ms);
        let logical_wal_spool = data_path
            .and_then(|path| LogicalWalSpool::open(path).ok())
            .map(Arc::new);
        let current_lsn = logical_wal_spool
            .as_ref()
            .map(|spool| spool.current_lsn())
            .unwrap_or(0);
        Self {
            wal_buffer: Arc::new(WalBuffer::new(100_000)),
            logical_wal_spool,
            replicas: RwLock::new(Vec::new()),
            wal_appended: (Mutex::new(current_lsn), Condvar::new()),
            slot_path,
            slots: RwLock::new(slots),
            slot_retention_max_lag_lsn: config.slot_retention_max_lag_lsn,
            slot_idle_timeout_ms: config.slot_idle_timeout_ms,
            commit_waiter: Arc::new(crate::replication::commit_waiter::CommitWaiter::new()),
            topology_epoch: std::sync::atomic::AtomicU64::new(0),
            partial_resync_count: std::sync::atomic::AtomicU64::new(0),
            full_resync_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn append_logical_record(&self, lsn: u64, encoded: Vec<u8>) {
        self.wal_buffer.append(lsn, encoded.clone());
        if let Some(spool) = &self.logical_wal_spool {
            let _ = spool.append(lsn, &encoded);
        }
        let (lock, cvar) = &self.wal_appended;
        let mut latest = lock.lock().unwrap_or_else(|e| e.into_inner());
        *latest = (*latest).max(lsn);
        cvar.notify_all();
    }

    pub fn wait_for_logical_lsn_after(&self, since_lsn: u64, timeout: Duration) -> bool {
        if self.current_logical_lsn() > since_lsn {
            return true;
        }
        let deadline = Instant::now() + timeout;
        let (lock, cvar) = &self.wal_appended;
        let mut latest = lock.lock().unwrap_or_else(|e| e.into_inner());
        while *latest <= since_lsn {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let remaining = deadline.saturating_duration_since(now);
            let (guard, result) = cvar
                .wait_timeout(latest, remaining)
                .unwrap_or_else(|e| e.into_inner());
            latest = guard;
            if result.timed_out() && *latest <= since_lsn {
                return false;
            }
        }
        true
    }

    pub fn register_replica(&self, id: String) -> u64 {
        self.register_replica_with_region(id, None)
    }

    /// Register a replica with an explicit region tag (Phase 2.6 multi-region).
    ///
    /// Preferred when the replica handshake declares a region — the quorum
    /// coordinator uses this field to decide whether the replica counts
    /// toward a `QuorumMode::Regions` commit.
    ///
    /// Idempotent on reconnect (issue #812): if a replica with `id` is
    /// already registered, the existing entry is *updated in place* rather
    /// than duplicated — progress LSNs (`last_acked_lsn`, `last_sent_lsn`,
    /// `last_durable_lsn`) are preserved so a reconnecting replica is not
    /// rewound, only `last_seen_at_unix_ms` is refreshed (and `region` when
    /// a non-`None` value is supplied). A re-registration is not a
    /// registry-shape change, so it does **not** bump the topology epoch.
    /// Returns the slot `restart_lsn` the replica should resume streaming from:
    /// the current WAL LSN for a fresh registration, or the durable slot
    /// restart point for a reconnect.
    pub fn register_replica_with_region(&self, id: String, region: Option<String>) -> u64 {
        let now_ms = crate::utils::now_unix_millis() as u128;
        let resume_lsn = self.ensure_slot(&id, self.current_logical_lsn());
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = replicas.iter_mut().find(|r| r.id == id) {
            existing.last_seen_at_unix_ms = now_ms;
            if region.is_some() {
                existing.region = region;
            }
            return resume_lsn;
        }
        replicas.push(ReplicaState {
            id,
            last_acked_lsn: resume_lsn,
            last_sent_lsn: resume_lsn,
            last_durable_lsn: resume_lsn,
            apply_error_count: 0,
            divergence_count: 0,
            connected_at_unix_ms: now_ms,
            last_seen_at_unix_ms: now_ms,
            region,
            rebootstrapping: false,
        });
        drop(replicas);
        self.bump_topology_epoch();
        resume_lsn
    }

    /// Mark (or clear) a replica's re-bootstrap state (issue #837).
    ///
    /// While `rebootstrapping` is `true` the replica keeps serving
    /// non-causal reads from its existing data, but the advertiser
    /// surfaces the flag so causal (bookmark) reads route to a
    /// caught-up peer instead — the rebuilding replica's applied
    /// frontier describes data it is about to discard. The primary
    /// flips this back to `false` when the replica reports its atomic
    /// snapshot swap complete.
    ///
    /// A change to the flag is a registry-shape change for routing
    /// purposes, so it bumps the topology epoch to force consumers to
    /// re-read the advertisement. Returns `true` when a replica with
    /// `id` was present and updated.
    pub fn set_replica_rebootstrapping(&self, id: &str, rebootstrapping: bool) -> bool {
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        let Some(state) = replicas.iter_mut().find(|r| r.id == id) else {
            return false;
        };
        if state.rebootstrapping == rebootstrapping {
            return true;
        }
        state.rebootstrapping = rebootstrapping;
        drop(replicas);
        self.bump_topology_epoch();
        true
    }

    /// Ensure a replica identifying itself with `id` is present in the
    /// registry (issue #812). This is the production self-registration hook
    /// used by the `pull_wal_records` path: the first time a replica sends
    /// its `replica_id` on a pull, the primary registers it so it is no
    /// longer blind to that replica's existence; subsequent pulls are
    /// idempotent no-ops. Returns `true` when a new registration was
    /// created. Delegates to `register_replica_with_region`, so reconnects
    /// preserve progress and do not bump the topology epoch.
    pub fn ensure_replica_registered(&self, id: &str) -> bool {
        let already = self
            .replicas
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|r| r.id == id);
        if already {
            return false;
        }
        self.register_replica(id.to_string());
        true
    }

    /// Unregister a replica by id. Returns `true` when the replica
    /// was present (and removed). Bumps the topology epoch so a
    /// pending advertisement reflects the new fleet size.
    pub fn unregister_replica(&self, id: &str) -> bool {
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        let before = replicas.len();
        replicas.retain(|r| r.id != id);
        let removed = replicas.len() != before;
        drop(replicas);
        if removed {
            self.commit_waiter.drop_replica(id);
            self.bump_topology_epoch();
        }
        removed
    }

    /// Current topology epoch. Strictly monotonic, bumps on every
    /// registry-shape change consumed by `TopologyAdvertiser`.
    pub fn topology_epoch(&self) -> u64 {
        self.topology_epoch
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Advance the topology epoch. Call sites: register, unregister,
    /// and the health-sweep tick that flips a replica between
    /// healthy/unhealthy. Wrapping is not a concern in practice
    /// (`u64::MAX` events would take centuries at any realistic ack
    /// rate) but `fetch_add` saturates implicitly via wrap-around;
    /// the consumer treats epoch as opaque so a wrap is still
    /// strictly "different" from the previous value.
    pub fn bump_topology_epoch(&self) {
        self.topology_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn ack_replica(&self, id: &str, lsn: u64) {
        let now_ms = crate::utils::now_unix_millis() as u128;
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = replicas.iter_mut().find(|r| r.id == id) {
            r.last_acked_lsn = r.last_acked_lsn.max(lsn);
            r.last_durable_lsn = r.last_durable_lsn.max(lsn);
            r.last_seen_at_unix_ms = now_ms;
        }
        drop(replicas);
        self.commit_waiter.record_replica_ack(id, lsn);
    }

    /// PLAN.md Phase 11.4 — replica reports applied + durable LSN
    /// after persisting a batch. Idempotent: only advances LSNs
    /// monotonically. `last_seen_at_unix_ms` always refreshes.
    /// Also signals `commit_waiter` so any thread blocked on
    /// `ack_n` / `quorum` can wake and re-check its threshold.
    pub fn ack_replica_lsn(&self, id: &str, applied_lsn: u64, durable_lsn: u64) {
        self.ack_replica_lsn_with_observability(id, applied_lsn, durable_lsn, 0, 0);
    }

    pub fn ack_replica_lsn_with_observability(
        &self,
        id: &str,
        applied_lsn: u64,
        durable_lsn: u64,
        apply_error_count: u64,
        divergence_count: u64,
    ) {
        let now_ms = crate::utils::now_unix_millis() as u128;
        self.advance_slot(id, applied_lsn, durable_lsn, now_ms);
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = replicas.iter_mut().find(|r| r.id == id) {
            r.last_acked_lsn = r.last_acked_lsn.max(applied_lsn);
            r.last_durable_lsn = r.last_durable_lsn.max(durable_lsn);
            r.apply_error_count = r.apply_error_count.max(apply_error_count);
            r.divergence_count = r.divergence_count.max(divergence_count);
            r.last_seen_at_unix_ms = now_ms;
        }
        // Drop the write lock before signaling so a waiter that
        // wakes immediately can read replica state without
        // contending against us.
        drop(replicas);
        self.commit_waiter.record_replica_ack(id, durable_lsn);
    }

    /// PLAN.md Phase 11.4 — primary records the LSN it last sent to a
    /// replica via pull_wal_records. Helpful for `lag_records =
    /// last_sent_lsn - last_acked_lsn` to distinguish pull-side delay
    /// from apply-side delay.
    pub fn note_replica_pull(&self, id: &str, last_sent_lsn: u64) {
        let now_ms = crate::utils::now_unix_millis() as u128;
        self.touch_slot(id, now_ms);
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = replicas.iter_mut().find(|r| r.id == id) {
            r.last_sent_lsn = r.last_sent_lsn.max(last_sent_lsn);
            r.last_seen_at_unix_ms = now_ms;
        }
    }

    /// Snapshot of all currently registered replicas, for /metrics +
    /// /admin/status. Returns owned clones so callers don't hold the
    /// lock during serialization.
    pub fn replica_snapshots(&self) -> Vec<ReplicaState> {
        self.replicas
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn replication_progress(&self) -> Option<ReplicationProgress> {
        let replicas = self.replicas.read().unwrap_or_else(|e| e.into_inner());
        ReplicationProgress::from_replicas(&replicas)
    }

    pub fn slot_snapshots(&self) -> Vec<ReplicationSlot> {
        self.slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn retention_floor_lsn(&self) -> Option<u64> {
        self.slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|slot| slot.invalidation_reason.is_none())
            .map(|slot| slot.restart_lsn)
            .min()
    }

    pub fn prune_retained_wal_through(&self, archived_lsn: u64) -> io::Result<u64> {
        self.prune_retained_wal(crate::storage::wal::WalPruneBoundary::new(archived_lsn))
    }

    pub fn prune_retained_wal(
        &self,
        boundary: crate::storage::wal::WalPruneBoundary,
    ) -> io::Result<u64> {
        self.enforce_retention_limits(crate::utils::now_unix_millis() as u128);
        let prune_lsn = boundary
            .with_replica_restart_lsn(self.retention_floor_lsn())
            .prune_through_lsn();
        if prune_lsn > 0 {
            if let Some(spool) = &self.logical_wal_spool {
                spool.prune_through(prune_lsn)?;
            }
            self.wal_buffer.prune_through(prune_lsn);
        }
        Ok(prune_lsn)
    }

    pub fn replica_count(&self) -> usize {
        self.replicas
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Current primary write position (logical WAL LSN, falling back to
    /// the in-memory WAL buffer). Used as the reference point for
    /// per-replica lag — including issue #826 flow control.
    pub fn current_logical_lsn(&self) -> u64 {
        self.logical_wal_spool
            .as_ref()
            .map(|spool| spool.current_lsn())
            .unwrap_or_else(|| self.wal_buffer.current_lsn())
    }

    fn ensure_slot(&self, id: &str, initial_lsn: u64) -> u64 {
        let now_ms = crate::utils::now_unix_millis() as u128;
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = slots.get_mut(id) {
            slot.last_seen_at_unix_ms = now_ms;
            let restart_lsn = slot.restart_lsn;
            self.persist_slots_locked(&slots);
            return restart_lsn;
        }
        slots.insert(
            id.to_string(),
            ReplicationSlot {
                id: id.to_string(),
                restart_lsn: initial_lsn,
                confirmed_lsn: initial_lsn,
                last_seen_at_unix_ms: now_ms,
                invalidation_reason: None,
                invalidated_at_unix_ms: None,
            },
        );
        let restart_lsn = initial_lsn;
        self.persist_slots_locked(&slots);
        restart_lsn
    }

    fn advance_slot(&self, id: &str, confirmed_lsn: u64, restart_lsn: u64, now_ms: u128) {
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        let slot = slots
            .entry(id.to_string())
            .or_insert_with(|| ReplicationSlot {
                id: id.to_string(),
                restart_lsn: 0,
                confirmed_lsn: 0,
                last_seen_at_unix_ms: now_ms,
                invalidation_reason: None,
                invalidated_at_unix_ms: None,
            });
        if slot.invalidation_reason.is_some() {
            return;
        }
        slot.confirmed_lsn = slot.confirmed_lsn.max(confirmed_lsn).max(restart_lsn);
        slot.restart_lsn = slot.restart_lsn.max(restart_lsn);
        slot.last_seen_at_unix_ms = now_ms;
        self.persist_slots_locked(&slots);
    }

    pub fn touch_slot(&self, id: &str, now_ms: u128) {
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        let mut changed = false;
        if let Some(slot) = slots.get_mut(id) {
            if slot.invalidation_reason.is_none() {
                slot.last_seen_at_unix_ms = now_ms;
                changed = true;
            }
        }
        if changed {
            self.persist_slots_locked(&slots);
        }
    }

    pub fn enforce_retention_limits(&self, now_ms: u128) -> Vec<(String, SlotInvalidationCause)> {
        let current_lsn = self.current_logical_lsn();
        let mut invalidated = Vec::new();
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        for slot in slots.values_mut() {
            if slot.invalidation_reason.is_some() {
                continue;
            }
            let reason = if self.slot_retention_max_lag_lsn > 0
                && current_lsn.saturating_sub(slot.restart_lsn) > self.slot_retention_max_lag_lsn
            {
                Some(SlotInvalidationCause::Horizon)
            } else if self.slot_idle_timeout_ms > 0
                && now_ms.saturating_sub(slot.last_seen_at_unix_ms)
                    > u128::from(self.slot_idle_timeout_ms)
            {
                Some(SlotInvalidationCause::IdleTimeout)
            } else {
                None
            };
            if let Some(reason) = reason {
                slot.invalidation_reason = Some(reason);
                slot.invalidated_at_unix_ms = Some(now_ms);
                invalidated.push((slot.id.clone(), reason));
            }
        }
        if !invalidated.is_empty() {
            self.persist_slots_locked(&slots);
        }
        invalidated
    }

    pub fn slot_rebootstrap_reason(
        &self,
        id: &str,
        requested_since_lsn: u64,
        oldest_available_lsn: Option<u64>,
    ) -> Option<SlotInvalidationCause> {
        let now_ms = crate::utils::now_unix_millis() as u128;
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        let slot = slots.get_mut(id)?;
        if let Some(reason) = slot.invalidation_reason {
            return Some(reason);
        }
        let slot_floor = slot.restart_lsn.max(requested_since_lsn);
        if oldest_available_lsn
            .map(|oldest| oldest > slot_floor.saturating_add(1))
            .unwrap_or(false)
        {
            slot.invalidation_reason = Some(SlotInvalidationCause::WalRemoved);
            slot.invalidated_at_unix_ms = Some(now_ms);
            self.persist_slots_locked(&slots);
            return Some(SlotInvalidationCause::WalRemoved);
        }
        None
    }

    /// Decide how a reconnecting replica's pull should be served
    /// (issue #832). If the slot is invalidated or the requested
    /// position has fallen behind the retained WAL floor, the replica
    /// must re-bootstrap; otherwise it resumes via a partial resync
    /// from its slot position (never rewound behind it). Every
    /// partial-resync decision bumps the `partial_resync_count` metric
    /// so a brief disconnect that recovers without a full re-bootstrap
    /// is observable.
    pub fn plan_replica_resume(
        &self,
        id: &str,
        requested_since_lsn: u64,
        oldest_available_lsn: Option<u64>,
    ) -> ResumeMode {
        if let Some(cause) =
            self.slot_rebootstrap_reason(id, requested_since_lsn, oldest_available_lsn)
        {
            self.full_resync_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return ResumeMode::FullRebootstrap { cause };
        }
        let resume_lsn = self
            .slot_snapshots()
            .into_iter()
            .find(|slot| slot.id == id)
            .map(|slot| requested_since_lsn.max(slot.restart_lsn))
            .unwrap_or(requested_since_lsn);
        self.partial_resync_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        ResumeMode::PartialResync { resume_lsn }
    }

    /// Number of pulls served as a partial resync since process start.
    /// Surfaced in the replication metrics/status payload (issue #832).
    pub fn partial_resync_count(&self) -> u64 {
        self.partial_resync_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Number of pulls that forced a full re-bootstrap since process
    /// start (issue #839). Surfaced as `reddb_replication_full_resync_total`
    /// and in `/replication/status` — the primary operator alert signal.
    pub fn full_resync_count(&self) -> u64 {
        self.full_resync_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn persist_slots_locked(&self, slots: &BTreeMap<String, ReplicationSlot>) {
        if let Err(err) = persist_replication_slots(self.slot_path.as_deref(), slots) {
            warn!(
                target: "reddb::replication::slots",
                error = %err,
                "failed to persist replication slots"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replication::cdc::{ChangeOperation, ChangeRecord};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_data_path(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("reddb_{name}_{suffix}.rdb"))
    }

    #[test]
    fn logical_wal_spool_roundtrip_and_prune() {
        let data_path = temp_data_path("logical_spool");
        let spool_path = LogicalWalSpool::path_for(&data_path);
        let spool = LogicalWalSpool::open(&data_path).expect("open spool");

        let record1 = ChangeRecord {
            term: 2,
            lsn: 7,
            timestamp: 1,
            operation: ChangeOperation::Insert,
            collection: "users".to_string(),
            entity_id: 10,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1, 2, 3]),
            metadata: None,
            refresh_records: None,
        };
        let record2 = ChangeRecord {
            term: 2,
            lsn: 8,
            timestamp: 2,
            operation: ChangeOperation::Update,
            collection: "users".to_string(),
            entity_id: 10,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![4, 5, 6]),
            metadata: None,
            refresh_records: None,
        };

        spool
            .append_with_term_and_timestamp(record1.term, record1.lsn, 11, &record1.encode())
            .expect("append 1");
        spool
            .append_with_term_and_timestamp(record2.term, record2.lsn, 12, &record2.encode())
            .expect("append 2");

        let entries = spool.read_since(0, usize::MAX).expect("read");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, 7);
        assert_eq!(entries[1].0, 8);
        assert_eq!(ChangeRecord::decode(&entries[0].1).unwrap().term, 2);

        let framed = read_and_repair_entries(&spool_path).expect("read framed entries");
        assert_eq!(framed[0].term, 2);
        assert_eq!(framed[0].timestamp_ms, 11);

        spool.prune_through(7).expect("prune");
        let retained = spool.read_since(0, usize::MAX).expect("read retained");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].0, 8);
        assert_eq!(ChangeRecord::decode(&retained[0].1).unwrap().term, 2);

        let _ = fs::remove_file(spool_path);
    }

    #[test]
    fn logical_wal_spool_reads_v2_without_term() {
        let data_path = temp_data_path("logical_spool_v2");
        let spool_path = LogicalWalSpool::path_for(&data_path);
        let payload = br#"{"lsn":3,"timestamp":44,"operation":"delete","collection":"users","rid":9,"kind":"row"}"#;
        let lsn = 3u64;
        let timestamp = 44u64;
        let crc = compute_logical_v2_crc(LOGICAL_WAL_SPOOL_VERSION_V2, lsn, timestamp, payload);

        let mut file = File::create(&spool_path).expect("create v2 spool");
        file.write_all(LOGICAL_WAL_SPOOL_MAGIC).unwrap();
        file.write_all(&[LOGICAL_WAL_SPOOL_VERSION_V2]).unwrap();
        file.write_all(&lsn.to_le_bytes()).unwrap();
        file.write_all(&timestamp.to_le_bytes()).unwrap();
        file.write_all(&(payload.len() as u32).to_le_bytes())
            .unwrap();
        file.write_all(payload).unwrap();
        file.write_all(&crc.to_le_bytes()).unwrap();
        file.sync_all().unwrap();

        let spool = LogicalWalSpool::open(&data_path).expect("open v2 spool");
        let records = spool.read_since(0, usize::MAX).expect("read v2 spool");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, 3);
        let decoded = ChangeRecord::decode(&records[0].1).expect("decode v2 payload");
        assert_eq!(decoded.term, crate::replication::DEFAULT_REPLICATION_TERM);
        assert_eq!(decoded.lsn, 3);

        let framed = read_and_repair_entries(&spool_path).expect("read framed v2 entries");
        assert_eq!(framed[0].term, crate::replication::DEFAULT_REPLICATION_TERM);

        let _ = fs::remove_file(spool_path);
    }

    #[test]
    fn topology_epoch_monotonic_on_register_and_unregister() {
        // Issue #167 acceptance: the epoch consumed by
        // TopologyAdvertiser is strictly monotonic across registry
        // shape changes. Pin it here so a future refactor doesn't
        // accidentally swallow the bump.
        let primary = PrimaryReplication::new(None);
        let e0 = primary.topology_epoch();
        primary.register_replica("r1".to_string());
        let e1 = primary.topology_epoch();
        primary.register_replica("r2".to_string());
        let e2 = primary.topology_epoch();
        assert!(e1 > e0, "register must bump epoch ({e0} -> {e1})");
        assert!(e2 > e1, "second register must bump epoch ({e1} -> {e2})");

        let removed = primary.unregister_replica("r1");
        assert!(removed);
        let e3 = primary.topology_epoch();
        assert!(e3 > e2, "unregister must bump epoch ({e2} -> {e3})");

        // Unknown id is a no-op and does not bump the epoch — keep
        // the monotonicity tied to actual registry shape changes.
        let absent = primary.unregister_replica("ghost");
        assert!(!absent);
        assert_eq!(
            primary.topology_epoch(),
            e3,
            "unregistering a missing replica must not bump the epoch"
        );
    }

    #[test]
    fn register_replica_is_idempotent_on_reconnect() {
        // Issue #812 acceptance: registration is the production foundation
        // for per-replica progress tracking. A reconnect must update the
        // existing registry entry rather than create a duplicate or rewind
        // progress. Uses the `None` data-path fake — no engine boot.
        let primary = PrimaryReplication::new(None);

        // First registration creates exactly one entry and bumps the epoch.
        primary.register_replica("r1".to_string());
        assert_eq!(
            primary.replica_count(),
            1,
            "first register creates an entry"
        );
        let epoch_after_first = primary.topology_epoch();

        // Advance the replica's progress as a real pull/ack would.
        primary.note_replica_pull("r1", 42);
        primary.ack_replica_lsn("r1", 40, 40);
        let before = primary
            .replica_snapshots()
            .into_iter()
            .find(|r| r.id == "r1")
            .expect("r1 present");
        assert_eq!(before.last_sent_lsn, 42);
        assert_eq!(before.last_acked_lsn, 40);
        assert_eq!(before.last_durable_lsn, 40);

        // Reconnect: re-register the same id.
        let resume_lsn = primary.register_replica("r1".to_string());

        // No duplicate entry.
        assert_eq!(
            primary.replica_count(),
            1,
            "reconnect must not create a duplicate registry entry"
        );
        // Re-registration is not a registry-shape change — epoch is untouched.
        assert_eq!(
            primary.topology_epoch(),
            epoch_after_first,
            "reconnect must not bump the topology epoch"
        );
        // Progress preserved, not rewound to the current WAL LSN.
        let after = primary
            .replica_snapshots()
            .into_iter()
            .find(|r| r.id == "r1")
            .expect("r1 still present");
        assert_eq!(after.last_sent_lsn, 42, "last_sent_lsn preserved");
        assert_eq!(after.last_acked_lsn, 40, "last_acked_lsn preserved");
        assert_eq!(after.last_durable_lsn, 40, "last_durable_lsn preserved");
        // Reconnect returns the slot restart point, not the last sent LSN.
        assert_eq!(resume_lsn, 40, "reconnect returns the slot restart LSN");
    }

    #[test]
    fn replica_slot_persists_and_reconnect_resumes_from_restart_lsn() {
        let data_path = temp_data_path("replication_slots");
        let spool_path = LogicalWalSpool::path_for(&data_path);
        let slot_path = PrimaryReplication::slot_path_for(&data_path);

        {
            let primary = PrimaryReplication::new(Some(&data_path));
            primary.register_replica("r1".to_string());
            primary.note_replica_pull("r1", 12);
            primary.ack_replica_lsn("r1", 10, 8);

            let slot = primary
                .slot_snapshots()
                .into_iter()
                .find(|slot| slot.id == "r1")
                .expect("r1 slot present");
            assert_eq!(slot.restart_lsn, 8);
            assert_eq!(slot.confirmed_lsn, 10);
        }

        let reopened = PrimaryReplication::new(Some(&data_path));
        let slot = reopened
            .slot_snapshots()
            .into_iter()
            .find(|slot| slot.id == "r1")
            .expect("r1 slot loaded after reopen");
        assert_eq!(slot.restart_lsn, 8);
        assert_eq!(slot.confirmed_lsn, 10);
        assert_eq!(
            reopened.register_replica("r1".to_string()),
            8,
            "reconnect resumes from the durable slot restart LSN"
        );

        let _ = fs::remove_file(spool_path);
        let _ = fs::remove_file(slot_path);
    }

    #[test]
    fn retention_floor_follows_slowest_slot_and_prunes_wal() {
        let primary = PrimaryReplication::new(None);
        primary.register_replica("fast".to_string());
        primary.register_replica("slow".to_string());

        for lsn in 1..=6 {
            primary.wal_buffer.append(lsn, vec![lsn as u8]);
        }

        primary.ack_replica_lsn("fast", 5, 5);
        primary.ack_replica_lsn("slow", 3, 2);

        assert_eq!(
            primary.retention_floor_lsn(),
            Some(2),
            "slowest slot restart_lsn sets the retention floor"
        );
        assert_eq!(primary.prune_retained_wal_through(6).unwrap(), 2);
        let retained: Vec<_> = primary
            .wal_buffer
            .read_since(0, usize::MAX)
            .into_iter()
            .map(|(lsn, _)| lsn)
            .collect();
        assert_eq!(retained, vec![3, 4, 5, 6]);

        primary.ack_replica_lsn("slow", 6, 6);
        assert_eq!(
            primary.retention_floor_lsn(),
            Some(5),
            "slot confirmation advances the retention floor"
        );
        assert_eq!(primary.prune_retained_wal_through(6).unwrap(), 5);
        let retained: Vec<_> = primary
            .wal_buffer
            .read_since(0, usize::MAX)
            .into_iter()
            .map(|(lsn, _)| lsn)
            .collect();
        assert_eq!(retained, vec![6]);
    }

    #[test]
    fn backup_floor_limits_wal_pruning_without_replicas() {
        let primary = PrimaryReplication::new(None);
        for lsn in 1..=6 {
            primary.wal_buffer.append(lsn, vec![lsn as u8]);
        }

        let boundary = crate::storage::wal::WalPruneBoundary::new(6).with_backup_floor_lsn(Some(3));

        assert_eq!(primary.prune_retained_wal(boundary).unwrap(), 3);
        let retained: Vec<_> = primary
            .wal_buffer
            .read_since(0, usize::MAX)
            .into_iter()
            .map(|(lsn, _)| lsn)
            .collect();
        assert_eq!(
            retained,
            vec![4, 5, 6],
            "backup/PITR floor keeps WAL needed after the retained checkpoint"
        );
    }

    #[test]
    fn combined_backup_and_replica_floors_choose_conservative_boundary() {
        let primary = PrimaryReplication::new(None);
        primary.register_replica("fast".to_string());
        primary.register_replica("slow".to_string());
        for lsn in 1..=8 {
            primary.wal_buffer.append(lsn, vec![lsn as u8]);
        }
        primary.ack_replica_lsn("fast", 8, 8);
        primary.ack_replica_lsn("slow", 3, 2);

        let backup_floor =
            crate::storage::wal::WalPruneBoundary::new(8).with_backup_floor_lsn(Some(6));
        assert_eq!(
            primary.prune_retained_wal(backup_floor).unwrap(),
            2,
            "slowest replica restart LSN beats the newer backup floor"
        );

        primary.ack_replica_lsn("slow", 8, 8);
        let older_backup =
            crate::storage::wal::WalPruneBoundary::new(8).with_backup_floor_lsn(Some(4));
        assert_eq!(
            primary.prune_retained_wal(older_backup).unwrap(),
            4,
            "older retained backup checkpoint beats caught-up replicas"
        );
        let retained: Vec<_> = primary
            .wal_buffer
            .read_since(0, usize::MAX)
            .into_iter()
            .map(|(lsn, _)| lsn)
            .collect();
        assert_eq!(retained, vec![5, 6, 7, 8]);
    }

    #[test]
    fn bootstrap_slot_pin_prevents_wal_removed_rebootstrap_after_prune() {
        let primary = PrimaryReplication::new(None);
        for lsn in 1..=5 {
            primary.wal_buffer.append(lsn, vec![lsn as u8]);
        }

        let slot_lsn = primary.register_replica("bootstrapping".to_string());
        assert_eq!(slot_lsn, 5, "bootstrap pins the current frontier");

        for lsn in 6..=8 {
            primary.wal_buffer.append(lsn, vec![lsn as u8]);
        }

        assert_eq!(
            primary.prune_retained_wal_through(8).unwrap(),
            5,
            "bootstrap slot keeps the frontier retained"
        );
        assert_eq!(
            primary.slot_rebootstrap_reason("bootstrapping", 0, primary.wal_buffer.oldest_lsn()),
            None,
            "a caller resuming from its slot must not see wal-removed after slot-aware pruning"
        );
    }

    #[test]
    fn default_config_enables_finite_slot_retention_cap() {
        let config = crate::replication::ReplicationConfig::primary();

        assert!(
            config.slot_retention_max_lag_lsn > 0,
            "primary replication must default to a finite slot retention cap"
        );
    }

    #[test]
    fn retention_cap_invalidates_slow_slot_and_releases_wal_floor() {
        let primary = PrimaryReplication::new_with_config(
            None,
            &crate::replication::ReplicationConfig::primary().with_slot_retention_max_lag_lsn(3),
        );
        primary.register_replica("fast".to_string());
        primary.register_replica("slow".to_string());

        for lsn in 1..=6 {
            primary.wal_buffer.append(lsn, vec![lsn as u8]);
        }
        primary.ack_replica_lsn("fast", 6, 6);

        assert_eq!(primary.prune_retained_wal_through(6).unwrap(), 6);

        let slow = primary
            .slot_snapshots()
            .into_iter()
            .find(|slot| slot.id == "slow")
            .expect("slow slot present");
        assert_eq!(
            slow.invalidation_reason,
            Some(SlotInvalidationCause::Horizon)
        );

        let retained: Vec<_> = primary
            .wal_buffer
            .read_since(0, usize::MAX)
            .into_iter()
            .map(|(lsn, _)| lsn)
            .collect();
        assert!(
            retained.is_empty(),
            "invalidated slow slot must not pin WAL"
        );
    }

    #[test]
    fn slot_invalidation_cause_codes_cover_wal_removed_horizon_and_idle_timeout() {
        let wal_removed = PrimaryReplication::new_with_config(
            None,
            &crate::replication::ReplicationConfig::primary()
                .with_slot_retention_max_lag_lsn(3)
                .with_slot_idle_timeout_ms(10),
        );
        wal_removed.register_replica("wal".to_string());
        assert_eq!(
            wal_removed.slot_rebootstrap_reason("wal", 0, Some(2)),
            Some(SlotInvalidationCause::WalRemoved)
        );

        let horizon = PrimaryReplication::new_with_config(
            None,
            &crate::replication::ReplicationConfig::primary().with_slot_retention_max_lag_lsn(3),
        );
        horizon.register_replica("horizon".to_string());
        for lsn in 1..=4 {
            horizon.wal_buffer.append(lsn, vec![lsn as u8]);
        }
        horizon.enforce_retention_limits(0);
        assert_eq!(
            horizon
                .slot_snapshots()
                .into_iter()
                .find(|slot| slot.id == "horizon")
                .and_then(|slot| slot.invalidation_reason),
            Some(SlotInvalidationCause::Horizon)
        );

        let idle = PrimaryReplication::new_with_config(
            None,
            &crate::replication::ReplicationConfig::primary().with_slot_idle_timeout_ms(10),
        );
        idle.register_replica("idle".to_string());
        idle.touch_slot("idle", 1);
        idle.enforce_retention_limits(12);
        assert_eq!(
            idle.slot_snapshots()
                .into_iter()
                .find(|slot| slot.id == "idle")
                .and_then(|slot| slot.invalidation_reason),
            Some(SlotInvalidationCause::IdleTimeout)
        );
    }

    #[test]
    fn wal_buffer_fan_out_shares_refcounted_payload() {
        // Issue #832 acceptance: fan-out to multiple replicas must share
        // one ref-counted buffer — a second reader gets the *same*
        // allocation, not a per-replica byte copy.
        let buffer = WalBuffer::new(8);
        buffer.append(1, vec![0xDE, 0xAD, 0xBE, 0xEF]);

        let replica_a = buffer.read_since_shared(0, usize::MAX);
        let replica_b = buffer.read_since_shared(0, usize::MAX);
        assert_eq!(replica_a.len(), 1);
        assert_eq!(replica_b.len(), 1);

        assert!(
            Arc::ptr_eq(&replica_a[0].1, &replica_b[0].1),
            "two replicas must share one ref-counted payload allocation"
        );
        assert_eq!(&*replica_a[0].1, &[0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(
            Arc::strong_count(&replica_a[0].1) >= 3,
            "buffer + both replica handles reference the same payload"
        );

        // The owned-bytes compatibility path still yields the payload.
        let owned = buffer.read_since(0, usize::MAX);
        assert_eq!(owned, vec![(1u64, vec![0xDE, 0xAD, 0xBE, 0xEF])]);
    }

    #[test]
    fn spool_seek_index_resume_is_sublinear() {
        // Issue #832 acceptance: a replica resuming from a mid-spool slot
        // position seeks to the nearest checkpoint rather than scanning
        // the whole spool from offset 0 (partial-resync seek).
        let data_path = temp_data_path("seek_index");
        let spool_path = LogicalWalSpool::path_for(&data_path);
        let spool = LogicalWalSpool::open(&data_path).expect("open spool");

        for lsn in 1..=200u64 {
            spool
                .append_with_term_and_timestamp(1, lsn, lsn, &[(lsn % 251) as u8, 0xAB])
                .expect("append");
        }

        // Full scan from the start covers every record and seeks to 0.
        assert_eq!(spool.read_since(0, usize::MAX).expect("full").len(), 200);
        assert_eq!(spool.seek_floor_offset(0), 0);

        // Resuming from a mid LSN returns exactly the tail and seeks past
        // offset 0 — proof the scan started from a checkpoint, not byte 0.
        let resumed = spool.read_since(130, usize::MAX).expect("resume");
        assert_eq!(resumed.first().map(|(lsn, _)| *lsn), Some(131));
        assert_eq!(resumed.last().map(|(lsn, _)| *lsn), Some(200));
        assert_eq!(resumed.len(), 70);
        assert!(
            spool.seek_floor_offset(130) > 0,
            "mid-spool resume must seek past offset 0"
        );

        // Reopening rebuilds the seek index from disk, so resume stays
        // sub-linear across a restart.
        drop(spool);
        let reopened = LogicalWalSpool::open(&data_path).expect("reopen spool");
        assert!(reopened.seek_floor_offset(130) > 0);
        assert_eq!(
            reopened
                .read_since(130, usize::MAX)
                .expect("resume reopen")
                .len(),
            70
        );

        let _ = fs::remove_file(spool_path);
    }

    #[test]
    fn plan_replica_resume_partial_within_window_full_past_cap() {
        // Issue #832 acceptance: within the retention window a brief blip
        // resumes via partial resync (counted as a metric); only a slot
        // past the retention cap forces a full re-bootstrap.
        let within = PrimaryReplication::new(None);
        within.register_replica("blip".to_string());
        for lsn in 1..=5 {
            within.wal_buffer.append(lsn, vec![lsn as u8]);
        }
        let before = within.partial_resync_count();
        match within.plan_replica_resume("blip", 2, within.wal_buffer.oldest_lsn()) {
            ResumeMode::PartialResync { resume_lsn } => assert_eq!(resume_lsn, 2),
            other => panic!("brief blip must resume via partial resync, got {other:?}"),
        }
        assert_eq!(
            within.partial_resync_count(),
            before + 1,
            "partial resync must be observable via the metric"
        );
        assert_eq!(
            within.full_resync_count(),
            0,
            "a partial resync must not bump the full-resync counter"
        );

        // A slot driven past the retention cap is invalidated and must
        // re-bootstrap — and that decision must NOT count as a partial
        // resync.
        let past_cap = PrimaryReplication::new_with_config(
            None,
            &crate::replication::ReplicationConfig::primary().with_slot_retention_max_lag_lsn(3),
        );
        past_cap.register_replica("slow".to_string());
        for lsn in 1..=6 {
            past_cap.wal_buffer.append(lsn, vec![lsn as u8]);
        }
        past_cap.enforce_retention_limits(0);
        let before_full = past_cap.partial_resync_count();
        let before_full_count = past_cap.full_resync_count();
        match past_cap.plan_replica_resume("slow", 0, past_cap.wal_buffer.oldest_lsn()) {
            ResumeMode::FullRebootstrap { cause } => {
                assert_eq!(cause, SlotInvalidationCause::Horizon)
            }
            other => panic!("slot past the cap must re-bootstrap, got {other:?}"),
        }
        assert_eq!(
            past_cap.partial_resync_count(),
            before_full,
            "a full re-bootstrap must not be counted as a partial resync"
        );
        assert_eq!(
            past_cap.full_resync_count(),
            before_full_count + 1,
            "a full re-bootstrap must bump the full-resync alert counter (issue #839)"
        );
    }

    #[test]
    fn ensure_replica_registered_self_registers_then_is_a_noop() {
        // Issue #812 acceptance: the production pull path auto-registers a
        // replica the first time it identifies itself, then advances its
        // per-replica state on subsequent pulls without duplicating it.
        let primary = PrimaryReplication::new(None);

        // First pull-with-id self-registers.
        assert!(
            primary.ensure_replica_registered("r1"),
            "first identification registers the replica"
        );
        assert_eq!(primary.replica_count(), 1);
        let epoch_after_register = primary.topology_epoch();

        // Per-replica state advances on pull for the now-registered replica.
        primary.note_replica_pull("r1", 7);
        assert_eq!(
            primary
                .replica_snapshots()
                .into_iter()
                .find(|r| r.id == "r1")
                .map(|r| r.last_sent_lsn),
            Some(7),
            "primary tracks last_sent_lsn for a registered replica's pull"
        );

        // Subsequent identification is an idempotent no-op: no duplicate,
        // no epoch bump, progress preserved.
        assert!(
            !primary.ensure_replica_registered("r1"),
            "already-registered replica is not re-registered"
        );
        assert_eq!(primary.replica_count(), 1);
        assert_eq!(primary.topology_epoch(), epoch_after_register);
        assert_eq!(
            primary
                .replica_snapshots()
                .into_iter()
                .find(|r| r.id == "r1")
                .map(|r| r.last_sent_lsn),
            Some(7),
            "no-op registration preserves progress"
        );
    }

    #[test]
    fn replication_progress_uses_sent_applied_and_durable_registry_lsns() {
        let now = crate::utils::now_unix_millis() as u128;
        let replicas = vec![
            ReplicaState {
                id: "fast".to_string(),
                last_acked_lsn: 90,
                last_sent_lsn: 120,
                last_durable_lsn: 80,
                apply_error_count: 0,
                divergence_count: 0,
                connected_at_unix_ms: now,
                last_seen_at_unix_ms: now,
                region: None,
                rebootstrapping: false,
            },
            ReplicaState {
                id: "slow".to_string(),
                last_acked_lsn: 70,
                last_sent_lsn: 100,
                last_durable_lsn: 60,
                apply_error_count: 0,
                divergence_count: 0,
                connected_at_unix_ms: now,
                last_seen_at_unix_ms: now,
                region: None,
                rebootstrapping: false,
            },
        ];

        let progress = ReplicationProgress::from_replicas(&replicas).expect("registered replicas");

        assert_eq!(progress.lag_lsn, 50);
        assert_eq!(progress.safe_replay_lsn, 60);
    }
}
