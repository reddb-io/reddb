//! Primary-side replication: WAL record production and snapshot serving.
//!
//! The logical WAL spool byte format is a `reddb-file` contract. This module
//! owns runtime policy: appending after writes, syncing acknowledged records,
//! serving replica pulls, and pruning once slots make records removable.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reddb_file::{ReplicationSlot, ReplicationSlotInvalidationCause};
use tracing::warn;

mod slots;
use slots::{
    load_replication_slot_catalog, load_replication_slots, persist_replication_slot_catalog,
    persist_replication_slots,
};

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

fn logical_wal_entry_term(entry: &reddb_file::LogicalWalEntry) -> u64 {
    if entry.term == 0 {
        term_from_payload(&entry.data)
    } else {
        entry.term
    }
}

fn logical_wal_data_with_framing_term(entry: &reddb_file::LogicalWalEntry) -> Vec<u8> {
    let term = logical_wal_entry_term(entry);
    match crate::replication::cdc::ChangeRecord::decode(&entry.data) {
        Ok(mut record) if record.term != term => {
            record.term = term;
            record.encode()
        }
        _ => entry.data.clone(),
    }
}

/// One in every `SEEK_INDEX_INTERVAL` records is checkpointed into the
/// spool's in-memory seek index. A briefly-disconnected replica
/// resuming from its slot LSN binary-searches this sparse index and
/// seeks straight to the nearest preceding checkpoint, then scans
/// forward at most `SEEK_INDEX_INTERVAL` records — turning resume from
/// an O(n) full-file scan into a sub-linear seek (issue #832). The
/// index is rebuilt on `open` and extended on every `append`.
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
        if ordinal.is_multiple_of(reddb_file::LOGICAL_WAL_SEEK_INDEX_INTERVAL) {
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
        reddb_file::layout::logical_wal_path(data_path)
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
        let entries = reddb_file::read_and_repair_logical_wal_entries(&path)?;
        let current_lsn = entries.last().map(|entry| entry.lsn).unwrap_or(0);
        // Rebuild the sparse seek index from the (now repaired) file so
        // a post-restart resume is sub-linear from the first pull.
        let (seek_index, write_offset, record_count) =
            reddb_file::build_logical_wal_seek_index(&path)?;
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
        let frame = reddb_file::encode_logical_wal_v3(term, lsn, timestamp_ms, data)?;

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
        let entries = reddb_file::read_logical_wal_entries_from(&self.path, start_offset)?;
        Ok(entries
            .into_iter()
            .filter(|entry| entry.lsn > since_lsn)
            .take(max_count)
            .map(|entry| (entry.lsn, logical_wal_data_with_framing_term(&entry)))
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
        Ok(reddb_file::read_and_repair_logical_wal_entries(&self.path)?
            .into_iter()
            .next()
            .map(|entry| entry.lsn))
    }

    pub fn prune_through(&self, upto_lsn: u64) -> io::Result<()> {
        let previous_lsn = self.current_lsn();
        let mut retained: Vec<_> = reddb_file::read_and_repair_logical_wal_entries(&self.path)?
            .into_iter()
            .filter(|entry| entry.lsn > upto_lsn)
            .collect();
        for entry in &mut retained {
            entry.term = logical_wal_entry_term(entry);
        }
        let temp_path = reddb_file::layout::logical_wal_temp_path(&self.path);
        for entry in &mut retained {
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
            entry.timestamp_ms = timestamp_ms;
        }
        let current_lsn =
            reddb_file::rewrite_logical_wal_entries(&self.path, &temp_path, &retained)?;

        // The rewrite shifted every record's byte offset, so the old
        // seek index is stale — rebuild it from the compacted file.
        let (seek_index, write_offset, record_count) =
            reddb_file::build_logical_wal_seek_index(&self.path)?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.current_lsn = previous_lsn.max(current_lsn).max(upto_lsn);
        state.seek_index = seek_index;
        state.write_offset = write_offset;
        state.record_count = record_count;
        Ok(())
    }
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
    slot_catalog_path: Option<PathBuf>,
    primary_replica_file_plan: Option<reddb_file::PrimaryReplicaFilePlan>,
    primary_replica_wal_lock: Mutex<()>,
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
    FullRebootstrap {
        cause: ReplicationSlotInvalidationCause,
    },
}

impl PrimaryReplication {
    pub fn slot_path_for(data_path: &Path) -> PathBuf {
        reddb_file::layout::legacy_logical_slots_path(data_path)
    }

    pub fn primary_replica_root_for(data_path: &Path) -> PathBuf {
        reddb_file::layout::primary_replica_root(data_path)
    }

    pub fn slot_catalog_path_for(data_path: &Path) -> PathBuf {
        Self::primary_replica_file_plan_for(data_path).slots_path()
    }

    fn primary_replica_file_plan_for(data_path: &Path) -> reddb_file::PrimaryReplicaFilePlan {
        let root = Self::primary_replica_root_for(data_path);
        let timeline =
            Self::primary_replica_current_timeline_for_root(&root).unwrap_or_else(|err| {
                warn!(
                    target: "reddb::replication::primary",
                    error = %err,
                    "failed to read primary-replica timeline history; using initial timeline"
                );
                reddb_file::TimelineId::initial()
            });
        reddb_file::PrimaryReplicaFilePlan::new(root, timeline)
    }

    fn primary_replica_current_timeline_for_root(
        root: &Path,
    ) -> Result<reddb_file::TimelineId, reddb_file::RdbFileError> {
        let path = reddb_file::PrimaryReplicaFilePlan::new(root, reddb_file::TimelineId::initial())
            .timeline_history_path();
        match reddb_file::TimelineHistory::read_from_path(&path) {
            Ok(history) => Ok(history
                .current()
                .unwrap_or_else(reddb_file::TimelineId::initial)),
            Err(reddb_file::RdbFileError::Io(err))
                if err.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(reddb_file::TimelineId::initial())
            }
            Err(err) => Err(err),
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
        let slot_catalog_path = data_path.map(Self::slot_catalog_path_for);
        let primary_replica_file_plan = data_path.map(Self::primary_replica_file_plan_for);
        let mut slots = load_replication_slot_catalog(slot_catalog_path.as_deref(), now_ms);
        slots.extend(load_replication_slots(slot_path.as_deref(), now_ms));
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
            slot_catalog_path,
            primary_replica_file_plan,
            primary_replica_wal_lock: Mutex::new(()),
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
        if let Some(plan) = &self.primary_replica_file_plan {
            let _guard = self
                .primary_replica_wal_lock
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            match Self::primary_replica_current_timeline_for_root(&plan.root) {
                Ok(timeline) => {
                    let plan = reddb_file::PrimaryReplicaFilePlan::new(plan.root.clone(), timeline);
                    if let Err(err) = plan.append_wal_record(lsn, &encoded) {
                        warn!(
                            target: "reddb::replication::primary",
                            lsn,
                            error = %err,
                            "failed to append primary-replica WAL segment"
                        );
                    }
                }
                Err(err) => {
                    warn!(
                        target: "reddb::replication::primary",
                        lsn,
                        error = %err,
                        "failed to read primary-replica timeline history; skipping WAL append"
                    );
                }
            }
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
        self.enforce_retention_limits(crate::utils::now_unix_millis() as u128);
        let prune_lsn = self
            .retention_floor_lsn()
            .map(|floor| floor.min(archived_lsn))
            .unwrap_or(archived_lsn);
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
        let mut slot = ReplicationSlot::new(
            id.to_string(),
            reddb_file::TimelineId::initial(),
            initial_lsn,
        );
        slot.last_seen_at_unix_ms = now_ms;
        slots.insert(id.to_string(), slot);
        let restart_lsn = initial_lsn;
        self.persist_slots_locked(&slots);
        restart_lsn
    }

    fn advance_slot(&self, id: &str, confirmed_lsn: u64, restart_lsn: u64, now_ms: u128) {
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        let slot = slots.entry(id.to_string()).or_insert_with(|| {
            let mut slot =
                ReplicationSlot::new(id.to_string(), reddb_file::TimelineId::initial(), 0);
            slot.last_seen_at_unix_ms = now_ms;
            slot
        });
        if slot.invalidation_reason.is_some() {
            return;
        }
        slot.confirmed_write_lsn = slot.confirmed_lsn().max(confirmed_lsn).max(restart_lsn);
        slot.restart_lsn = slot.restart_lsn.max(restart_lsn);
        slot.confirmed_flush_lsn = slot.confirmed_flush_lsn.max(slot.restart_lsn);
        slot.confirmed_apply_lsn = slot.confirmed_apply_lsn.max(slot.restart_lsn);
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

    pub fn enforce_retention_limits(
        &self,
        now_ms: u128,
    ) -> Vec<(String, ReplicationSlotInvalidationCause)> {
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
                Some(ReplicationSlotInvalidationCause::Horizon)
            } else if self.slot_idle_timeout_ms > 0
                && now_ms.saturating_sub(slot.last_seen_at_unix_ms)
                    > u128::from(self.slot_idle_timeout_ms)
            {
                Some(ReplicationSlotInvalidationCause::IdleTimeout)
            } else {
                None
            };
            if let Some(reason) = reason {
                slot.invalidation_reason = Some(reason);
                slot.invalidated_at_unix_ms = Some(now_ms);
                invalidated.push((slot.replica_id.clone(), reason));
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
    ) -> Option<ReplicationSlotInvalidationCause> {
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
            slot.invalidation_reason = Some(ReplicationSlotInvalidationCause::WalRemoved);
            slot.invalidated_at_unix_ms = Some(now_ms);
            self.persist_slots_locked(&slots);
            return Some(ReplicationSlotInvalidationCause::WalRemoved);
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
            .find(|slot| slot.replica_id == id)
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
        if let Err(err) = persist_replication_slot_catalog(self.slot_catalog_path.as_deref(), slots)
        {
            warn!(
                target: "reddb::replication::slots",
                error = %err,
                "failed to persist binary replication slot catalog"
            );
        }
    }
}

#[cfg(test)]
mod tests;
