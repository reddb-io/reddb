//! Primary-side replication: WAL record production and snapshot serving.
//!
//! ## Logical WAL spool wire format
//!
//! ### Version 2 (current — PLAN.md Phase 2 / W2)
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

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::warn;

const LOGICAL_WAL_SPOOL_MAGIC: &[u8; 4] = b"RDLW";
const LOGICAL_WAL_SPOOL_VERSION_V1: u8 = 1;
const LOGICAL_WAL_SPOOL_VERSION_V2: u8 = 2;
const LOGICAL_WAL_SPOOL_VERSION_CURRENT: u8 = LOGICAL_WAL_SPOOL_VERSION_V2;
/// Header size in bytes for a v2 record before the payload starts:
/// magic(4) + version(1) + lsn(8) + timestamp(8) + payload_len(4) = 25.
const LOGICAL_WAL_V2_HEADER_LEN: u64 = 4 + 1 + 8 + 8 + 4;
/// CRC32 trailer size in bytes for a v2 record.
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

/// In-memory WAL buffer for replication.
/// Primary appends records here; replicas consume from it.
pub struct WalBuffer {
    /// Circular buffer of (lsn, serialized_record) pairs.
    records: RwLock<VecDeque<(u64, Vec<u8>)>>,
    /// Maximum records to keep in buffer.
    max_size: usize,
    /// Current write LSN.
    current_lsn: RwLock<u64>,
}

impl WalBuffer {
    pub fn new(max_size: usize) -> Self {
        Self {
            records: RwLock::new(VecDeque::with_capacity(max_size)),
            max_size,
            current_lsn: RwLock::new(0),
        }
    }

    /// Append a WAL record. Called by the storage engine after each write.
    pub fn append(&self, lsn: u64, data: Vec<u8>) {
        let mut records = self.records.write().unwrap_or_else(|e| e.into_inner());
        records.push_back((lsn, data));
        while records.len() > self.max_size {
            records.pop_front();
        }

        let mut current = self.current_lsn.write().unwrap_or_else(|e| e.into_inner());
        *current = (*current).max(lsn);
    }

    /// Read records since the given LSN (exclusive).
    pub fn read_since(&self, since_lsn: u64, max_count: usize) -> Vec<(u64, Vec<u8>)> {
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

    /// Oldest available LSN (for gap detection).
    pub fn oldest_lsn(&self) -> Option<u64> {
        let records = self.records.read().unwrap_or_else(|e| e.into_inner());
        records.front().map(|(lsn, _)| *lsn)
    }
}

#[derive(Debug, Clone)]
struct LogicalWalEntry {
    lsn: u64,
    /// Wall-clock millis at append time. `0` for legacy v1 records that
    /// did not carry a framing timestamp.
    timestamp_ms: u64,
    data: Vec<u8>,
}

#[derive(Debug, Default)]
struct LogicalWalSpoolState {
    current_lsn: u64,
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
        Ok(Self {
            path,
            state: Mutex::new(LogicalWalSpoolState { current_lsn }),
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
        let mut frame =
            Vec::with_capacity(LOGICAL_WAL_V2_HEADER_LEN as usize + data.len() + LOGICAL_WAL_V2_CRC_LEN as usize);
        frame.extend_from_slice(LOGICAL_WAL_SPOOL_MAGIC);
        frame.push(LOGICAL_WAL_SPOOL_VERSION_CURRENT);
        frame.extend_from_slice(&lsn.to_le_bytes());
        frame.extend_from_slice(&timestamp_ms.to_le_bytes());
        frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
        frame.extend_from_slice(data);
        let crc = compute_logical_v2_crc(LOGICAL_WAL_SPOOL_VERSION_CURRENT, lsn, timestamp_ms, data);
        frame.extend_from_slice(&crc.to_le_bytes());

        file.write_all(&frame)?;
        // PLAN.md Phase 2 mandates `sync_all` for logical WAL durability.
        // `flush()` only drains the std::io userspace buffer; without
        // `sync_all` the kernel page cache may still be dirty when an
        // acknowledged write supposedly committed.
        file.sync_all()?;

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current_lsn = state.current_lsn.max(lsn);
        Ok(())
    }

    pub fn read_since(&self, since_lsn: u64, max_count: usize) -> io::Result<Vec<(u64, Vec<u8>)>> {
        let entries = read_and_repair_entries(&self.path)?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.lsn > since_lsn)
            .take(max_count)
            .map(|entry| (entry.lsn, entry.data))
            .collect())
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
            // Re-frame as v2 so the spool only ever contains v2 records
            // after a prune. Legacy v1 records are upgraded by carrying
            // their original LSN forward; the framing timestamp is
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
            let crc = compute_logical_v2_crc(
                LOGICAL_WAL_SPOOL_VERSION_CURRENT,
                entry.lsn,
                timestamp_ms,
                &entry.data,
            );
            temp.write_all(LOGICAL_WAL_SPOOL_MAGIC)?;
            temp.write_all(&[LOGICAL_WAL_SPOOL_VERSION_CURRENT])?;
            temp.write_all(&entry.lsn.to_le_bytes())?;
            temp.write_all(&timestamp_ms.to_le_bytes())?;
            temp.write_all(&(entry.data.len() as u32).to_le_bytes())?;
            temp.write_all(&entry.data)?;
            temp.write_all(&crc.to_le_bytes())?;
            current_lsn = current_lsn.max(entry.lsn);
        }
        temp.sync_all()?;
        fs::rename(&temp_path, &self.path)?;

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current_lsn = previous_lsn.max(current_lsn).max(upto_lsn);
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
        return Err(format!("torn payload length at offset {record_start}: {err}"));
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
    Ok(LogicalWalEntry {
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
        return Err(format!(
            "v1 torn payload at offset {record_start}: {err}"
        ));
    }
    Ok(LogicalWalEntry {
        lsn: u64::from_le_bytes(lsn),
        timestamp_ms: 0,
        data: payload,
    })
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
    pub connected_at_unix_ms: u128,
    pub last_seen_at_unix_ms: u128,
    /// Region identifier declared by the replica at handshake time
    /// (Phase 2.6 multi-region PG parity). `None` until the replica
    /// handshake extension lands in 2.6.2; the quorum coordinator's
    /// region-binding map covers the in-process case meanwhile.
    pub region: Option<String>,
}

/// Primary replication manager.
pub struct PrimaryReplication {
    pub wal_buffer: Arc<WalBuffer>,
    pub logical_wal_spool: Option<Arc<LogicalWalSpool>>,
    pub replicas: RwLock<Vec<ReplicaState>>,
    /// PLAN.md Phase 11.4 — ack-driven commit synchronization. Always
    /// allocated so the policy enum can flip from `Local` to
    /// `AckN`/`Quorum` without touching this struct's shape.
    pub commit_waiter: Arc<crate::replication::commit_waiter::CommitWaiter>,
}

impl PrimaryReplication {
    pub fn new(data_path: Option<&Path>) -> Self {
        Self {
            wal_buffer: Arc::new(WalBuffer::new(100_000)),
            logical_wal_spool: data_path
                .and_then(|path| LogicalWalSpool::open(path).ok())
                .map(Arc::new),
            replicas: RwLock::new(Vec::new()),
            commit_waiter: Arc::new(crate::replication::commit_waiter::CommitWaiter::new()),
        }
    }

    pub fn register_replica(&self, id: String) -> u64 {
        self.register_replica_with_region(id, None)
    }

    /// Register a replica with an explicit region tag (Phase 2.6 multi-region).
    ///
    /// Preferred when the replica handshake declares a region — the quorum
    /// coordinator uses this field to decide whether the replica counts
    /// toward a `QuorumMode::Regions` commit.
    pub fn register_replica_with_region(&self, id: String, region: Option<String>) -> u64 {
        let lsn = self.wal_buffer.current_lsn();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let state = ReplicaState {
            id,
            last_acked_lsn: lsn,
            last_sent_lsn: lsn,
            last_durable_lsn: lsn,
            connected_at_unix_ms: now_ms,
            last_seen_at_unix_ms: now_ms,
            region,
        };
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        replicas.push(state);
        lsn
    }

    pub fn ack_replica(&self, id: &str, lsn: u64) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = replicas.iter_mut().find(|r| r.id == id) {
            r.last_acked_lsn = r.last_acked_lsn.max(lsn);
            r.last_seen_at_unix_ms = now_ms;
        }
    }

    /// PLAN.md Phase 11.4 — replica reports applied + durable LSN
    /// after persisting a batch. Idempotent: only advances LSNs
    /// monotonically. `last_seen_at_unix_ms` always refreshes.
    /// Also signals `commit_waiter` so any thread blocked on
    /// `ack_n` / `quorum` can wake and re-check its threshold.
    pub fn ack_replica_lsn(&self, id: &str, applied_lsn: u64, durable_lsn: u64) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let mut replicas = self.replicas.write().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = replicas.iter_mut().find(|r| r.id == id) {
            r.last_acked_lsn = r.last_acked_lsn.max(applied_lsn);
            r.last_durable_lsn = r.last_durable_lsn.max(durable_lsn);
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
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
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

    pub fn replica_count(&self) -> usize {
        self.replicas
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
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
            lsn: 7,
            timestamp: 1,
            operation: ChangeOperation::Insert,
            collection: "users".to_string(),
            entity_id: 10,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1, 2, 3]),
            metadata: None,
        };
        let record2 = ChangeRecord {
            lsn: 8,
            timestamp: 2,
            operation: ChangeOperation::Update,
            collection: "users".to_string(),
            entity_id: 10,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![4, 5, 6]),
            metadata: None,
        };

        spool
            .append(record1.lsn, &record1.encode())
            .expect("append 1");
        spool
            .append(record2.lsn, &record2.encode())
            .expect("append 2");

        let entries = spool.read_since(0, usize::MAX).expect("read");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, 7);
        assert_eq!(entries[1].0, 8);

        spool.prune_through(7).expect("prune");
        let retained = spool.read_since(0, usize::MAX).expect("read retained");
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].0, 8);

        let _ = fs::remove_file(spool_path);
    }
}
