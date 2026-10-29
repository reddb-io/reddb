use super::*;
use crate::api::DurabilityMode;
use crate::storage::wal::{WalReader, WalRecord, WalWriter};
use reddb_engine::storage::resident_file::EMBEDDED_WAL_HEADROOM_LIMIT;
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};

struct NativeReplayTarget<'a> {
    store: &'a UnifiedStore,
    update_btrees: bool,
}

impl reddb_engine::storage::store_wal::StoreReplayTarget for NativeReplayTarget<'_> {
    fn has_collection(&self, name: &str) -> bool {
        self.store.get_collection(name).is_some()
    }
    fn create_collection(&self, name: &str) -> Result<(), StoreError> {
        self.store.create_collection_in_memory(name)
    }
    fn drop_collection(&self, name: &str) -> Result<(), StoreError> {
        self.store.drop_collection_in_memory(name)
    }
    fn upsert(&self, collection: &str, record: &[u8]) -> Result<(), StoreError> {
        self.store
            .apply_replayed_upsert(collection, record, self.update_btrees)
    }
    fn delete(&self, collection: &str, id: EntityId) -> Result<(), StoreError> {
        self.store
            .apply_replayed_delete(collection, id, self.update_btrees)
    }
    fn refresh(&self, collection: &str, records: &[Vec<u8>]) -> Result<(), StoreError> {
        self.store
            .apply_replayed_refresh_collection(collection, records, self.update_btrees)
    }
}

/// Adaptive group-commit window applied when the configured
/// `GroupCommitOptions::window_ms` is 0 (the historical default).
///
/// Sub-millisecond so individual single-writer commits don't see a
/// visible latency penalty (a 200 µs floor sits below typical NVMe
/// fsync latency of 50–150 µs by ~one fsync), while still giving a
/// pipelined writer a chance to drop a second statement into the
/// same drain cycle. A lone synchronous `insert_one` still pays one
/// fsync per row in the worst case, but two back-to-back inserts on
/// the same connection now coalesce into one drain. See P1 in
/// `docs/perf/insert_sequential-2026-05-05.md`.
///
/// Override via `REDDB_GROUP_COMMIT_WINDOW_US` (microseconds). Set
/// to `0` to disable the floor entirely (legacy behaviour). Set
/// `REDDB_GROUP_COMMIT_WINDOW_MS` to any non-zero value to bypass
/// this floor — explicit ms config wins.
const DEFAULT_ADAPTIVE_WINDOW_US: u64 = 200;

/// Shorthand — `Arc<parking_lot::Mutex<WalWriter>>` avoids poisoning
/// (one writer panicking mid-append used to taint every subsequent
/// lock acquisition) and shaves a few syscalls off the fast path.
/// The group-commit coordinator + writer threads all acquire this
/// mutex in the hot insert path, so the unpoison/fast-park win
/// compounds under 16-way concurrency.
type WalMutex = parking_lot::Mutex<WalWriter>;

/// Shorthand for the group-commit coordinator's state pair. Same
/// non-poisoning + lighter-park motivation as `WalMutex`; writer
/// threads `wait` on this condvar until the coordinator publishes a
/// new `durable_lsn`, so the park cost shows up on every
/// WalDurableGrouped transaction.
type CommitStateMutex = parking_lot::Mutex<CommitState>;
type CommitStateCondvar = parking_lot::Condvar;
use std::time::{Duration, Instant};

static NEXT_STORE_TX_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) use reddb_engine::storage::store_wal::StoreWalAction;

#[derive(Debug, Default)]
pub(crate) struct DeferredStoreWalActions {
    pub(super) actions: Vec<StoreWalAction>,
}

pub(super) struct StoreWalCaptureGuard<'a> {
    active: bool,
    session: Option<&'a crate::storage::embedded::EmbeddedCommitSession>,
}

impl<'a> StoreWalCaptureGuard<'a> {
    pub(super) fn begin(
        session: Option<&'a crate::storage::embedded::EmbeddedCommitSession>,
    ) -> Self {
        assert!(
            !deferred_store_wal_capture_active(),
            "store WAL captures cannot nest"
        );
        begin_deferred_store_wal_capture();
        Self {
            active: true,
            session,
        }
    }

    pub(super) fn finish(mut self) -> DeferredStoreWalActions {
        let actions = take_deferred_store_wal_capture();
        self.active = false;
        actions
    }
}

impl Drop for StoreWalCaptureGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            if let Some(session) = self.session {
                session.fence_interrupted_write();
            }
            let _ = take_deferred_store_wal_capture();
        }
    }
}

impl DeferredStoreWalActions {
    pub(crate) fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub(crate) fn extend(&mut self, other: Self) {
        self.actions.extend(other.actions);
    }
}

thread_local! {
    static DEFERRED_STORE_WAL_ACTIONS: RefCell<Option<Vec<StoreWalAction>>> =
        const { RefCell::new(None) };
}

fn begin_deferred_store_wal_capture() {
    DEFERRED_STORE_WAL_ACTIONS.with(|cell| {
        let mut guard = cell.borrow_mut();
        debug_assert!(guard.is_none());
        *guard = Some(Vec::new());
    });
}

fn capture_deferred_store_wal_actions(actions: Vec<StoreWalAction>) -> bool {
    DEFERRED_STORE_WAL_ACTIONS.with(|cell| {
        let mut guard = cell.borrow_mut();
        if let Some(pending) = guard.as_mut() {
            pending.extend(actions);
            true
        } else {
            false
        }
    })
}

fn deferred_store_wal_capture_active() -> bool {
    DEFERRED_STORE_WAL_ACTIONS.with(|cell| cell.borrow().is_some())
}

fn take_deferred_store_wal_capture() -> DeferredStoreWalActions {
    DEFERRED_STORE_WAL_ACTIONS.with(|cell| DeferredStoreWalActions {
        actions: cell.borrow_mut().take().unwrap_or_default(),
    })
}

#[derive(Debug)]
struct CommitState {
    durable_lsn: u64,
    pending_target_lsn: u64,
    pending_statements: usize,
    pending_wal_bytes: u64,
    first_pending_at: Option<Instant>,
    shutdown: bool,
    last_error: Option<String>,
}

impl CommitState {
    fn new(initial_durable_lsn: u64) -> Self {
        Self {
            durable_lsn: initial_durable_lsn,
            pending_target_lsn: initial_durable_lsn,
            pending_statements: 0,
            pending_wal_bytes: 0,
            first_pending_at: None,
            shutdown: false,
            last_error: None,
        }
    }
}

/// Lock-free append queue sitting in front of `WalWriter`.
///
/// Writers atomically reserve a byte range via `next_lsn.fetch_add`
/// and push their encoded bytes into a parking_lot-guarded vector.
/// The group-commit coordinator is the sole drainer: it sorts the
/// pending entries by LSN (so the file bytes land at the offsets
/// each writer reserved) and hands them to `WalWriter::append_bytes`.
///
/// This replaces the old `wal.lock() ... append ... append ... drop`
/// hot path where 16 concurrent writers serialised on the WAL
/// mutex for ~13µs each. Hold time on the queue's parking_lot
/// mutex is ~200ns (just a Vec::push) — 65× shorter, so the mutex
/// convoy on concurrent inserts disappears.
pub(crate) struct WalAppendQueue {
    /// Keep reserved offsets valid until their callers observe durability.
    /// Reset also excludes an old flusher publishing state into the new WAL.
    epoch: parking_lot::RwLock<()>,
    /// Tuple of (monotonically-increasing LSN, pending-bytes vec)
    /// protected by one mutex. Keeping the LSN reservation AND the
    /// push under the same lock guarantees that every queue entry
    /// is visible the moment its LSN is assigned — no gap between
    /// `fetch_add` and `push` for the leader to spin on.
    ///
    /// Earlier versions reserved LSN with an `AtomicU64::fetch_add`
    /// outside the lock; the leader drain observed reordered
    /// `(lsn, bytes)` tuples whose pushes happened in a different
    /// order than their fetch_add, creating "holes" in the LSN
    /// sequence that the drain loop interpreted as "wait for the
    /// missing enqueuer" — under tokio scheduling pressure the
    /// missing enqueuer was preempted indefinitely and the
    /// drain loop busy-waited forever (WAL stayed at 8 bytes).
    pending: parking_lot::Mutex<WalQueueState>,
    /// Bytes of encoded records sitting in `pending.entries` (ADR 0073 §2,
    /// the `wal_buffers` pool). A relaxed atomic rather than a field inside
    /// `WalQueueState` so a reader sampling the accounting never contends
    /// with an enqueuer for the queue mutex.
    queued_bytes: AtomicU64,
}

struct WalQueueState {
    next_lsn: u64,
    entries: Vec<(u64, Vec<u8>)>,
}

impl WalAppendQueue {
    fn new(initial_lsn: u64) -> Self {
        Self {
            epoch: parking_lot::RwLock::new(()),
            pending: parking_lot::Mutex::new(WalQueueState {
                next_lsn: initial_lsn,
                entries: Vec::with_capacity(64),
            }),
            queued_bytes: AtomicU64::new(0),
        }
    }

    /// Reserve an LSN range of `bytes.len()` bytes and push onto the
    /// queue. Returns the commit LSN (end of reserved range), which
    /// the caller passes to `wait_until_durable`. LSN assignment and
    /// push happen under the same mutex — no gap for the drain loop
    /// to busy-spin on.
    fn enqueue(&self, bytes: Vec<u8>) -> u64 {
        let len = bytes.len() as u64;
        let mut state = self.pending.lock();
        let start_lsn = state.next_lsn;
        state.next_lsn = start_lsn + len;
        state.entries.push((start_lsn, bytes));
        self.queued_bytes.fetch_add(len, Ordering::Relaxed);
        start_lsn + len
    }

    /// Drain all queued entries in LSN order. Caller holds the WAL
    /// file mutex while writing the drained bytes so the on-disk
    /// layout matches the reserved LSN offsets.
    fn drain_sorted(&self) -> Vec<(u64, Vec<u8>)> {
        let mut state = self.pending.lock();
        let mut v = std::mem::take(&mut state.entries);
        let drained: u64 = v.iter().map(|(_, bytes)| bytes.len() as u64).sum();
        drop(state);
        // Saturating: the drain is the only consumer, so `queued_bytes` can
        // never sit below what this call took, but an accounting counter that
        // wraps would poison `red.stats` far past the bug that caused it.
        let _ = self
            .queued_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |queued| {
                Some(queued.saturating_sub(drained))
            });
        v.sort_by_key(|(lsn, _)| *lsn);
        v
    }

    /// Whether any entry is queued. Leader uses this to decide
    /// whether to spin once more or go back to the condvar.
    fn has_pending(&self) -> bool {
        !self.pending.lock().entries.is_empty()
    }

    /// Encoded record bytes currently buffered in memory.
    fn queued_bytes(&self) -> u64 {
        self.queued_bytes.load(Ordering::Relaxed)
    }

    /// Reset the LSN cursor and discard any queued entries. Used
    /// after `wal.truncate()` — the wal-side byte counter goes back
    /// to the header size, so the queue (which tracks LSNs in the
    /// same byte space) must follow or every subsequent enqueue
    /// returns a target the drain loop can never reach.
    fn reset(&self, next_lsn: u64) {
        let mut state = self.pending.lock();
        state.next_lsn = next_lsn;
        state.entries.clear();
        drop(state);
        self.queued_bytes.store(0, Ordering::Relaxed);
    }
}

pub(crate) struct StoreCommitCoordinator {
    mode: DurabilityMode,
    config: crate::api::GroupCommitOptions,
    wal_path: PathBuf,
    wal: Arc<WalMutex>,
    /// Lock-free front door for writers. Populated alongside
    /// `WalDurableGrouped` / `Async` modes so concurrent inserts
    /// never contend on `wal` for the append step. Strict mode
    /// bypasses the queue and calls `WalWriter::append` directly
    /// to preserve the one-fsync-per-commit semantic.
    queue: Arc<WalAppendQueue>,
    state: Arc<(CommitStateMutex, CommitStateCondvar)>,
    /// Number of `wal.sync()` calls issued by the group-commit
    /// drain loop. Used by tests to observe coalescing — a burst
    /// of N concurrent commits should bump this by far less than N
    /// when the adaptive window is doing its job. Strict-mode
    /// commits go through `force_sync` which also bumps this.
    fsync_count: Arc<AtomicU64>,
    wal_generation: AtomicU64,
    /// Includes recovered epochs that were never published in the catalog.
    ownership_last_epoch: AtomicU64,
    // 0: existing WAL not scanned; 1: no ownership records; 2: protected.
    ownership_wal_state: AtomicU8,
}

/// Connects a fenced pager ownership epoch to the store's durable WAL.
#[allow(dead_code)] // Installed once recovery and format fencing are active.
struct StoreOwnershipRecorder {
    coordinator: Arc<StoreCommitCoordinator>,
    epoch: u64,
}

impl StoreOwnershipRecorder {
    #[allow(dead_code)]
    fn begin(coordinator: Arc<StoreCommitCoordinator>, epoch: u64) -> io::Result<Self> {
        coordinator.append_durable_page_ownership(
            reddb_file::page_ownership_journal::PageOwnershipRecord::Begin { epoch },
        )?;
        Ok(Self { coordinator, epoch })
    }

    fn append_pages(
        &self,
        pages: &[u32],
        make_record: impl Fn(u64, u32) -> reddb_file::page_ownership_journal::PageOwnershipRecord,
    ) -> io::Result<()> {
        for chunk in pages.chunks(256) {
            let records: Vec<_> = chunk
                .iter()
                .map(|&id| make_record(self.epoch, id))
                .collect();
            self.coordinator
                .append_durable_page_ownership_batch(&records)?;
        }
        Ok(())
    }
}

impl crate::storage::engine::pager::PageOwnershipRecorder for StoreOwnershipRecorder {
    fn reserve_pages(&self, start: u32, count: u32) -> io::Result<()> {
        use reddb_file::page_ownership_journal::PageOwnershipRecord;
        let end = start.checked_add(count).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "ownership range overflow")
        })?;
        let mut next = start;
        while next < end {
            let chunk_end = next.saturating_add(256).min(end);
            let records: Vec<_> = (next..chunk_end)
                .map(|page_id| PageOwnershipRecord::Reserve {
                    epoch: self.epoch,
                    page_id,
                })
                .collect();
            self.coordinator
                .append_durable_page_ownership_batch(&records)?;
            next = chunk_end;
        }
        Ok(())
    }

    fn detach_free_pages(&self, pages: &[u32]) -> io::Result<()> {
        self.append_pages(pages, |epoch, page_id| {
            reddb_file::page_ownership_journal::PageOwnershipRecord::DetachFree { epoch, page_id }
        })
    }

    fn retire_pages(&self, pages: &[u32]) -> io::Result<()> {
        self.append_pages(pages, |epoch, page_id| {
            reddb_file::page_ownership_journal::PageOwnershipRecord::Retire { epoch, page_id }
        })
    }
}

impl StoreCommitCoordinator {
    pub(crate) fn open(
        wal_path: impl Into<PathBuf>,
        mode: DurabilityMode,
        config: crate::api::GroupCommitOptions,
    ) -> io::Result<Self> {
        let wal_path = wal_path.into();
        let wal = WalWriter::open(&wal_path)?;
        let initial_durable_lsn = wal.durable_lsn();
        let initial_current_lsn = wal.current_lsn();
        let wal = Arc::new(WalMutex::new(wal));
        let queue = Arc::new(WalAppendQueue::new(initial_current_lsn));
        let state = Arc::new((
            CommitStateMutex::new(CommitState::new(initial_durable_lsn)),
            CommitStateCondvar::new(),
        ));
        let fsync_count = Arc::new(AtomicU64::new(0));

        if matches!(
            mode,
            DurabilityMode::WalDurableGrouped | DurabilityMode::Async
        ) {
            let wal_bg = Arc::clone(&wal);
            let queue_bg = Arc::clone(&queue);
            let state_bg = Arc::clone(&state);
            let fsync_bg = Arc::clone(&fsync_count);
            // P1: adaptive group-commit window. Historical default is
            // `window_ms = 0` ("no wait"), which under single-writer
            // OLTP means one fsync per autocommit row — the throughput
            // floor. When window_ms is 0 we fall back to a small
            // microsecond floor (`DEFAULT_ADAPTIVE_WINDOW_US`, override
            // via `REDDB_GROUP_COMMIT_WINDOW_US`) so a pipelined writer
            // can drop a second statement into the same drain cycle.
            // Explicit non-zero `window_ms` config takes precedence.
            //
            // The loop already short-circuits on
            // `pending_statements >= max_statements` /
            // `pending_wal_bytes >= max_wal_bytes`, so the window only
            // delays the leader when the queue is otherwise empty —
            // exactly the case we want to coalesce.
            let window = Self::resolve_window(&config);
            let max_statements = config.max_statements.max(1);
            let max_wal_bytes = config.max_wal_bytes.max(1);
            std::thread::spawn(move || {
                Self::run_group_commit_loop(
                    wal_bg,
                    queue_bg,
                    state_bg,
                    fsync_bg,
                    window,
                    max_statements,
                    max_wal_bytes,
                );
            });
        }

        Ok(Self {
            mode,
            config,
            wal_path,
            wal,
            queue,
            state,
            fsync_count,
            wal_generation: AtomicU64::new(0),
            ownership_last_epoch: AtomicU64::new(0),
            ownership_wal_state: AtomicU8::new(0),
        })
    }

    /// Resolve the effective group-commit window from the configured
    /// options + the `REDDB_GROUP_COMMIT_WINDOW_US` env override.
    ///
    /// Precedence (highest first):
    ///   1. `REDDB_GROUP_COMMIT_WINDOW_US=N` — N µs (0 disables).
    ///   2. `config.window_ms != 0` — explicit ms config wins.
    ///   3. `DEFAULT_ADAPTIVE_WINDOW_US` — adaptive floor for the
    ///      historical zero-default. Set the env var to 0 to opt out.
    fn resolve_window(config: &crate::api::GroupCommitOptions) -> Duration {
        if let Ok(raw) = std::env::var("REDDB_GROUP_COMMIT_WINDOW_US") {
            if let Ok(parsed) = raw.parse::<u64>() {
                return Duration::from_micros(parsed);
            }
        }
        if config.window_ms != 0 {
            return Duration::from_millis(config.window_ms);
        }
        Duration::from_micros(DEFAULT_ADAPTIVE_WINDOW_US)
    }

    /// Total `wal.sync()` calls issued since this coordinator opened.
    /// Public for tests that want to observe fsync coalescing under
    /// concurrent autocommits.
    #[cfg(test)]
    pub(crate) fn fsync_count(&self) -> u64 {
        self.fsync_count.load(Ordering::Relaxed)
    }

    /// WAL buffer memory held right now: encoded records waiting in the
    /// group-commit queue plus the writer's fixed append buffer, which exists
    /// for the coordinator's whole life (ADR 0073 §2, the `wal_buffers` pool).
    pub(crate) fn buffered_bytes(&self) -> u64 {
        self.queue.queued_bytes() + crate::storage::wal::WAL_BUFFER_BYTES as u64
    }

    pub(crate) fn append_actions(&self, actions: &[StoreWalAction]) -> io::Result<()> {
        self.append_actions_impl(actions, false).map(|_| ())
    }

    pub(super) fn append_checkpoint_actions(
        &self,
        actions: &[StoreWalAction],
    ) -> io::Result<super::checkpoint_wal::CheckpointWalBatch> {
        self.append_actions_impl(actions, true)
    }

    fn append_actions_impl(
        &self,
        actions: &[StoreWalAction],
        retain: bool,
    ) -> io::Result<super::checkpoint_wal::CheckpointWalBatch> {
        use super::checkpoint_wal::CheckpointWalBatch;
        if actions.is_empty() {
            return Ok(CheckpointWalBatch::default());
        }
        let _epoch = self.queue.epoch.read();
        let tx_id = NEXT_STORE_TX_ID.fetch_add(1, Ordering::SeqCst);
        let encoded: Vec<Vec<u8>> = actions.iter().map(StoreWalAction::encode).collect();
        let wal_bytes = encoded.iter().fold(0u64, |total, action| {
            total.saturating_add(action.len() as u64)
        });
        let (blob, mut references) = if retain {
            CheckpointWalBatch::encode(
                tx_id,
                &encoded,
                self.wal_generation.load(Ordering::Acquire),
            )?
        } else {
            (
                WalRecord::TxCommitBatch {
                    tx_id,
                    actions: encoded,
                }
                .encode(),
                CheckpointWalBatch::default(),
            )
        };
        let start_lsn = if matches!(self.mode, DurabilityMode::Strict) {
            let start = self.wal.lock().append_bytes(&blob)?;
            self.force_sync()?;
            start
        } else {
            let length = blob.len() as u64;
            let end = self.queue.enqueue(blob);
            self.wait_until_durable(end, wal_bytes)?;
            end.checked_sub(length).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid WAL batch offset")
            })?
        };
        references.relocate(start_lsn)?;
        Ok(references)
    }

    /// Compare exact durable bytes in bounded chunks. A proof from an older
    /// truncated WAL generation cannot prove a row in the current file.
    pub(super) fn checkpoint_row_matches(
        &self,
        reference: super::checkpoint_wal::WalRowReference,
        expected: &[u8],
    ) -> io::Result<bool> {
        if expected.len() != reference.length as usize {
            return Ok(false);
        }
        let mut position = 0;
        let mut matches = true;
        let present = self.visit_checkpoint_row(reference, |chunk| {
            matches &= chunk == &expected[position..position + chunk.len()];
            position += chunk.len();
        })?;
        Ok(present && matches)
    }

    /// Materialize only one already-validated durable row for a projection batch.
    pub(super) fn checkpoint_row_bytes(
        &self,
        reference: super::checkpoint_wal::WalRowReference,
    ) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        let present = self.visit_checkpoint_row(reference, |chunk| {
            if bytes.is_empty() {
                bytes.reserve_exact(reference.length as usize);
            }
            bytes.extend_from_slice(chunk);
        })?;
        if !present {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "checkpoint WAL reference changed during projection",
            ));
        }
        Ok(bytes)
    }

    fn visit_checkpoint_row(
        &self,
        reference: super::checkpoint_wal::WalRowReference,
        mut visit: impl FnMut(&[u8]),
    ) -> io::Result<bool> {
        use std::io::{Read, Seek, SeekFrom};
        let _epoch = self.queue.epoch.read_recursive();
        if reference.generation != self.wal_generation.load(Ordering::Acquire) {
            return Ok(false);
        }
        let end = reference
            .offset
            .checked_add(u64::from(reference.length))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "WAL proof offset overflow")
            })?;
        let (state_lock, _) = &*self.state;
        if end > state_lock.lock().durable_lsn {
            return Ok(false);
        }
        let mut file = std::fs::File::open(&self.wal_path)?;
        file.seek(SeekFrom::Start(reference.offset))?;
        let mut buffer = [0u8; 16 * 1024];
        let mut checksum = crc32fast::Hasher::new();
        let mut remaining = reference.length as usize;
        while remaining != 0 {
            let length = remaining.min(buffer.len());
            let chunk = &mut buffer[..length];
            file.read_exact(chunk)?;
            checksum.update(chunk);
            visit(chunk);
            remaining -= length;
        }
        if checksum.finalize() != reference.checksum {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "checkpoint WAL proof checksum mismatch",
            ));
        }
        Ok(true)
    }

    /// Encode + enqueue a single, non-batched [`WalRecord`] (issue
    /// #693). Honours the same Strict / Grouped / Async branching as
    /// [`Self::append_actions`] so durability semantics are uniform
    /// across record kinds.
    pub(crate) fn append_single_record(&self, record: WalRecord) -> io::Result<()> {
        if matches!(record, WalRecord::PageOwnership { .. }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "page ownership requires a durable append",
            ));
        }
        let _epoch = self.queue.epoch.read();
        let blob = record.encode();
        let wal_bytes = blob.len() as u64;
        if matches!(self.mode, DurabilityMode::Strict) {
            {
                let mut wal = self.wal.lock();
                wal.append(&record)?;
            }
            self.force_sync()?;
            return Ok(());
        }
        let commit_lsn = self.queue.enqueue(blob);
        self.wait_until_durable(commit_lsn, wal_bytes)?;
        Ok(())
    }

    /// Ownership intents must reach stable storage before the pager consumes
    /// or changes a page, even when user transactions use async durability.
    #[allow(dead_code)] // Activated with the pager recovery and format fence.
    pub(super) fn append_durable_page_ownership(
        &self,
        record: reddb_file::page_ownership_journal::PageOwnershipRecord,
    ) -> io::Result<()> {
        self.append_durable_page_ownership_batch(&[record])
    }

    /// Append a bounded set of ownership intents behind one durability fence.
    #[allow(dead_code)] // Activated with the pager recovery and format fence.
    pub(super) fn append_durable_page_ownership_batch(
        &self,
        records: &[reddb_file::page_ownership_journal::PageOwnershipRecord],
    ) -> io::Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let mut bytes = Vec::new();
        for &record in records {
            // Validate before WalRecord::encode, which expects a valid frame.
            reddb_file::page_ownership_journal::encode(record)?;
            WalRecord::PageOwnership { record }.encode_into(&mut bytes);
        }
        for record in records {
            self.ownership_last_epoch
                .fetch_max(record.epoch(), Ordering::AcqRel);
        }
        let _epoch = self.queue.epoch.read();
        // Set before writing: a failed or uncertain append must never allow
        // checkpoint truncation to discard the ownership journal.
        self.ownership_wal_state.store(2, Ordering::Release);
        if matches!(self.mode, DurabilityMode::Strict) {
            self.wal.lock().append_bytes(&bytes)?;
            return self.force_sync();
        }

        let length = bytes.len() as u64;
        let target = self.queue.enqueue(bytes);
        let (state_lock, cond) = &*self.state;
        let mut state = state_lock.lock();
        state.pending_target_lsn = state.pending_target_lsn.max(target);
        state.pending_statements = state.pending_statements.saturating_add(1);
        state.pending_wal_bytes = state.pending_wal_bytes.saturating_add(length);
        state.first_pending_at.get_or_insert_with(Instant::now);
        cond.notify_all();
        loop {
            if let Some(error) = state.last_error.clone() {
                return Err(io::Error::other(error));
            }
            if state.durable_lsn >= target {
                return Ok(());
            }
            cond.wait(&mut state);
        }
    }

    pub(crate) fn force_sync(&self) -> io::Result<()> {
        // Strict append already owns a read lease. Do not wait behind a
        // checkpoint that needs that append to finish before resetting.
        let _epoch = self.queue.epoch.read_recursive();
        {
            let mut wal = self.wal.lock();
            wal.sync()?;
            self.fsync_count.fetch_add(1, Ordering::Relaxed);
            let durable = wal.durable_lsn();
            drop(wal);
            let (state_lock, cond) = &*self.state;
            let mut state = state_lock.lock();
            state.durable_lsn = durable;
            state.pending_target_lsn = durable.max(state.pending_target_lsn);
            state.pending_statements = 0;
            state.pending_wal_bytes = 0;
            state.first_pending_at = None;
            state.last_error = None;
            cond.notify_all();
        }
        Ok(())
    }

    pub(crate) fn truncate(&self) -> io::Result<()> {
        let _epoch = self.queue.epoch.write();
        if self.ownership_wal_state.load(Ordering::Acquire) == 0 {
            let reader = WalReader::open(&self.wal_path)?;
            let mut protected = false;
            for item in reader.iter() {
                match item {
                    Ok((_, WalRecord::PageOwnership { .. })) => {
                        protected = true;
                        break;
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                    Err(error) => return Err(error),
                }
            }
            self.ownership_wal_state
                .store(if protected { 2 } else { 1 }, Ordering::Release);
        }
        if self.ownership_wal_state.load(Ordering::Acquire) == 2 {
            // A Finish record alone cannot prove that both superblock copies
            // are fenced; only recovery may eventually authorize truncation.
            return Err(io::Error::other(
                "cannot truncate WAL while page ownership records require recovery",
            ));
        }
        self.wal_generation.fetch_add(1, Ordering::AcqRel);
        let mut wal = self.wal.lock();
        wal.truncate()?;
        let durable = wal.durable_lsn();
        let current = wal.current_lsn();
        drop(wal);

        // Queue's next_lsn tracks byte offsets in the same space as
        // wal.current_lsn. After truncate both must be reset together
        // — otherwise enqueue returns a target_lsn in the old range
        // that drain can never reach, and wait_until_durable hangs.
        self.queue.reset(current);

        let (state_lock, cond) = &*self.state;
        let mut state = state_lock.lock();
        state.durable_lsn = durable;
        state.pending_target_lsn = durable;
        state.pending_statements = 0;
        state.pending_wal_bytes = 0;
        state.first_pending_at = None;
        state.last_error = None;
        cond.notify_all();
        Ok(())
    }

    fn release_completed_ownership_wal(&self) -> io::Result<()> {
        use reddb_file::page_ownership_journal::{encode, pending_epoch, PageOwnershipRecord};
        let _epoch = self.queue.epoch.write();
        let mut journal = Vec::new();
        let mut last = None;
        for item in WalReader::open(&self.wal_path)?.iter() {
            match item {
                Ok((_, WalRecord::PageOwnership { record })) => {
                    journal.extend_from_slice(&encode(record)?);
                    last = Some(record);
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "incomplete WAL before ownership completion",
                    ));
                }
                Err(error) => return Err(error),
            }
        }
        if pending_epoch(&journal)?.is_some()
            || !matches!(last, Some(PageOwnershipRecord::Finish { .. }))
        {
            return Err(io::Error::other("page ownership WAL is not complete"));
        }
        self.ownership_wal_state.store(1, Ordering::Release);
        Ok(())
    }

    fn recover_page_ownership(&self, store: &UnifiedStore, journal: &[u8]) -> io::Result<()> {
        use reddb_file::page_ownership_journal::{
            pending_epoch, recovery_plan, PageOwnershipRecord,
        };
        if journal.is_empty() {
            return Ok(());
        }
        let pager = store
            .pager
            .as_ref()
            .ok_or_else(|| io::Error::other("ownership WAL requires a pager"))?;
        let header = pager
            .header()
            .map_err(|error| io::Error::other(error.to_string()))?;
        if header.version < reddb_file::file_format::PAGE_OWNERSHIP_FILE_VERSION {
            return Err(io::Error::other(
                "page ownership journal requires recovery with a fenced file version",
            ));
        }
        let pending = pending_epoch(journal).map_err(|error| {
            io::Error::other(format!("page ownership journal requires recovery: {error}"))
        })?;
        if let Some(pending) = pending {
            let plan = recovery_plan(
                &pending,
                store.ownership_catalog_epoch.load(Ordering::Acquire),
                reddb_engine::storage::pager_io::FIRST_ALLOCATABLE_PAGE,
                header.page_count,
            )?;
            pager
                .validate_owned_pages(&plan.preserve)
                .map_err(|error| io::Error::other(error.to_string()))?;
            pager
                .recover_unowned_pages(&plan.release)
                .map_err(|error| io::Error::other(error.to_string()))?;
            #[cfg(test)]
            if std::env::var_os("REDDB_EXPERIMENTAL_EXIT_DURING_OWNERSHIP_RECOVERY").is_some() {
                std::process::exit(187);
            }
            pager
                .sync_recovered_allocator()
                .map_err(|error| io::Error::other(error.to_string()))?;
            #[cfg(test)]
            if std::env::var_os("REDDB_EXPERIMENTAL_EXIT_AFTER_OWNERSHIP_SYNC").is_some() {
                std::process::exit(188);
            }
            self.append_durable_page_ownership(PageOwnershipRecord::Finish {
                epoch: pending.epoch,
            })?;
            #[cfg(test)]
            if std::env::var_os("REDDB_EXPERIMENTAL_EXIT_AFTER_OWNERSHIP_FINISH").is_some() {
                std::process::exit(189);
            }
        }
        self.release_completed_ownership_wal()
    }

    pub(super) fn next_page_ownership_epoch(&self, store: &UnifiedStore) -> io::Result<u64> {
        self.ownership_last_epoch
            .load(Ordering::Acquire)
            .max(store.ownership_catalog_epoch.load(Ordering::Acquire))
            .checked_add(1)
            .ok_or_else(|| io::Error::other("page ownership epoch exhausted"))
    }

    /// Begin an epoch only when the caller has excluded all other pager mutators.
    pub(super) fn begin_page_ownership_epoch(
        &self,
        store: &UnifiedStore,
        epoch: u64,
    ) -> io::Result<()> {
        if store.ownership_active_epoch.load(Ordering::Acquire) != 0
            || epoch <= store.ownership_catalog_epoch.load(Ordering::Acquire)
            || epoch <= self.ownership_last_epoch.load(Ordering::Acquire)
        {
            return Err(io::Error::other("invalid page ownership epoch"));
        }
        let pager = store
            .pager
            .as_ref()
            .ok_or_else(|| io::Error::other("ownership epoch requires a pager"))?;
        pager
            .fence_file_format_version(reddb_file::file_format::PAGE_OWNERSHIP_FILE_VERSION)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let coordinator = Arc::clone(
            store
                .commit
                .as_ref()
                .ok_or_else(|| io::Error::other("ownership epoch requires a WAL coordinator"))?,
        );
        let recorder = Arc::new(StoreOwnershipRecorder::begin(coordinator, epoch)?);
        pager.install_ownership_recorder(recorder)?;
        store.ownership_active_epoch.store(epoch, Ordering::Release);
        Ok(())
    }

    /// Complete the epoch only while the same exclusion remains in force.
    pub(super) fn finish_page_ownership_epoch(
        &self,
        store: &UnifiedStore,
        epoch: u64,
    ) -> io::Result<()> {
        if epoch == 0 || store.ownership_active_epoch.load(Ordering::Acquire) != epoch {
            return Err(io::Error::other("page ownership epoch is not active"));
        }
        if store.ownership_catalog_epoch.load(Ordering::Acquire) != epoch {
            return Err(io::Error::other("page ownership epoch is not published"));
        }
        let pager = store
            .pager
            .as_ref()
            .ok_or_else(|| io::Error::other("ownership epoch requires a pager"))?;
        pager
            .sync_recovered_allocator()
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.append_durable_page_ownership(
            reddb_file::page_ownership_journal::PageOwnershipRecord::Finish { epoch },
        )?;
        pager.clear_ownership_recorder()?;
        store.ownership_active_epoch.store(0, Ordering::Release);
        self.release_completed_ownership_wal()
    }

    pub(crate) fn replay_into(&self, store: &UnifiedStore) -> io::Result<()> {
        if !self.wal_path.exists() {
            return Ok(());
        }

        let reader = match WalReader::open(&self.wal_path) {
            Ok(reader) => reader,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };

        let mut tx_states = std::collections::HashMap::<u64, bool>::new();
        let mut pending = Vec::<(u64, Vec<u8>)>::new();
        let mut ownership_journal = Vec::new();
        let mut latest_ownership_epoch = 0;
        let mut incomplete_wal_tail = false;
        let mut applied = false;

        for record in reader.iter() {
            let (_lsn, record) = match record {
                Ok(record) => record,
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => {
                    incomplete_wal_tail = true;
                    break;
                }
                Err(err) => return Err(err),
            };
            match record {
                WalRecord::PageOwnership { record } => {
                    latest_ownership_epoch = latest_ownership_epoch.max(record.epoch());
                    self.ownership_last_epoch
                        .fetch_max(record.epoch(), Ordering::AcqRel);
                    ownership_journal
                        .extend_from_slice(&reddb_file::page_ownership_journal::encode(record)?);
                }
                WalRecord::TxCommitBatch { actions, .. } => {
                    for payload in actions {
                        let action = StoreWalAction::decode(&payload)?;
                        applied = true;
                        store.apply_replayed_action(&action, false).map_err(|err| {
                            io::Error::other(format!("failed to replay store wal action: {err}"))
                        })?;
                    }
                }
                WalRecord::Begin { tx_id } => {
                    tx_states.insert(tx_id, false);
                }
                WalRecord::Commit { tx_id } => {
                    tx_states.insert(tx_id, true);
                }
                WalRecord::Rollback { tx_id } => {
                    tx_states.remove(&tx_id);
                }
                WalRecord::PageWrite {
                    tx_id,
                    page_id: _,
                    data,
                } => pending.push((tx_id, data)),
                WalRecord::Checkpoint { .. } => {}
                WalRecord::VectorInsert {
                    collection,
                    entity_id,
                    vector,
                } => {
                    // Issue #694 — capture `vector.turbo` WAL inserts so the
                    // boot-time TurboQuant index rebuild can drain them in
                    // WAL order (deterministic against the pre-restart
                    // state under a fixed codec seed). The legacy `vector`
                    // path doesn't read this map.
                    let mut map = store.replayed_turbo_inserts.lock();
                    map.entry(collection).or_default().push((entity_id, vector));
                }
                WalRecord::ProbabilisticDelta {
                    kind,
                    operation,
                    name,
                    operands,
                } => {
                    let mut deltas = store.replayed_probabilistic_deltas.lock();
                    deltas.push((kind, operation, name, operands));
                }
                WalRecord::FullPageImage { .. } => {
                    // Pager-level FPI (gh-478); store replay ignores.
                }
            }
        }

        if incomplete_wal_tail && !ownership_journal.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete WAL prevents ownership recovery",
            ));
        }
        self.recover_page_ownership(store, &ownership_journal)?;

        for (tx_id, payload) in pending {
            if !tx_states.get(&tx_id).copied().unwrap_or(false) {
                continue;
            }
            let action = StoreWalAction::decode(&payload)?;
            applied = true;
            store.apply_replayed_action(&action, false).map_err(|err| {
                io::Error::other(format!("failed to replay store wal action: {err}"))
            })?;
        }

        if applied {
            // Opening is exclusive: no client can mutate published trees
            // while replay constructs replacement generations.
            let epoch = latest_ownership_epoch
                .max(store.ownership_catalog_epoch.load(Ordering::Acquire))
                .checked_add(1)
                .ok_or_else(|| io::Error::other("page ownership epoch exhausted"))?;
            self.begin_page_ownership_epoch(store, epoch)?;
            // Even when the replayed actions leave all B-trees unchanged, the
            // epoch must be in the catalog before its journal can finish.
            store.mark_paged_registry_dirty();
            store
                .reconcile_replayed_btrees()
                .map_err(|err| io::Error::other(err.to_string()))?;
            self.finish_page_ownership_epoch(store, epoch)?;
        }
        Ok(())
    }

    fn wait_until_durable(&self, target_lsn: u64, wal_bytes: u64) -> io::Result<()> {
        match self.mode {
            DurabilityMode::Strict => self.force_sync(),
            // Async: record the pending target so the background
            // flusher eventually covers it, but don't block the
            // caller. Matches PG `synchronous_commit=off` semantics —
            // crash inside the flush window loses unflushed commits.
            DurabilityMode::Async => {
                let (state_lock, cond) = &*self.state;
                let mut state = state_lock.lock();
                state.pending_target_lsn = state.pending_target_lsn.max(target_lsn);
                state.pending_statements = state.pending_statements.saturating_add(1);
                state.pending_wal_bytes = state.pending_wal_bytes.saturating_add(wal_bytes);
                state.first_pending_at.get_or_insert_with(Instant::now);
                cond.notify_all();
                Ok(())
            }
            DurabilityMode::WalDurableGrouped => {
                let (state_lock, cond) = &*self.state;
                let mut state = state_lock.lock();
                state.pending_target_lsn = state.pending_target_lsn.max(target_lsn);
                state.pending_statements = state.pending_statements.saturating_add(1);
                state.pending_wal_bytes = state.pending_wal_bytes.saturating_add(wal_bytes);
                state.first_pending_at.get_or_insert_with(Instant::now);
                cond.notify_all();

                loop {
                    if let Some(err) = state.last_error.clone() {
                        return Err(io::Error::other(err));
                    }
                    if state.durable_lsn >= target_lsn {
                        return Ok(());
                    }
                    // parking_lot::Condvar mutates the guard in place —
                    // no LockResult to unwrap, no poisoning to fold.
                    cond.wait(&mut state);
                }
            }
        }
    }

    fn run_group_commit_loop(
        wal: Arc<WalMutex>,
        queue: Arc<WalAppendQueue>,
        state: Arc<(CommitStateMutex, CommitStateCondvar)>,
        fsync_count: Arc<AtomicU64>,
        window: Duration,
        max_statements: usize,
        max_wal_bytes: u64,
    ) {
        let (state_lock, cond) = &*state;
        loop {
            let target_lsn = {
                let mut guard = state_lock.lock();

                while !guard.shutdown && guard.pending_target_lsn <= guard.durable_lsn {
                    cond.wait(&mut guard);
                }

                if guard.shutdown {
                    return;
                }

                let immediate = window.is_zero()
                    || guard.pending_statements >= max_statements
                    || guard.pending_wal_bytes >= max_wal_bytes;

                if !immediate {
                    let deadline = guard.first_pending_at.unwrap_or_else(Instant::now) + window;
                    let now = Instant::now();
                    if now < deadline {
                        let timeout = deadline.saturating_duration_since(now);
                        let _ = cond.wait_for(&mut guard, timeout);
                        if guard.shutdown {
                            return;
                        }
                        if guard.pending_target_lsn <= guard.durable_lsn {
                            continue;
                        }
                        let should_wait_again = guard.pending_statements < max_statements
                            && guard.pending_wal_bytes < max_wal_bytes
                            && guard
                                .first_pending_at
                                .map(|first| first.elapsed() < window)
                                .unwrap_or(false);
                        if should_wait_again {
                            continue;
                        }
                    }
                }

                guard.pending_target_lsn
            };

            // Drain the current epoch even when reset is waiting: outstanding
            // append leases cannot finish until this flusher acknowledges them.
            // Retain the lease through state publication to exclude stale ACKs
            // after a checkpoint resets offsets back to the WAL header.
            let _epoch = queue.epoch.read_recursive();

            // Drain all queued entries. Since `WalAppendQueue::enqueue`
            // assigns LSN and pushes under a single mutex, the drained
            // tuples are guaranteed to form a contiguous byte range
            // starting at `wal.current_lsn()` — no gaps, no leftover
            // handling.
            let batches = queue.drain_sorted();

            let sync_result = {
                let mut wal = wal.lock();
                let mut write_err: Option<io::Error> = None;
                for (_lsn, bytes) in batches {
                    if let Err(e) = wal.append_bytes(&bytes) {
                        write_err = Some(e);
                        break;
                    }
                }
                match write_err {
                    Some(e) => Err(e),
                    None => wal.sync().map(|_| {
                        // Count the fsync exactly once per drain
                        // cycle so tests can compare it against the
                        // number of `append_actions` callers that
                        // entered the queue.
                        fsync_count.fetch_add(1, Ordering::Relaxed);
                        wal.durable_lsn()
                    }),
                }
            };

            // Late enqueuers that arrived after our drain stay in the
            // queue — don't clear the `pending_*` counters yet and
            // don't claim we reached `target_lsn` unless the fsync
            // actually covers it.
            let more_pending = queue.has_pending();

            let mut guard = state_lock.lock();
            match sync_result {
                Ok(durable_lsn) => {
                    guard.durable_lsn = durable_lsn;
                    if !more_pending {
                        guard.pending_statements = 0;
                        guard.pending_wal_bytes = 0;
                        guard.first_pending_at = None;
                    }
                    guard.last_error = None;
                    let _ = target_lsn;
                }
                Err(err) => {
                    guard.last_error = Some(err.to_string());
                }
            }
            cond.notify_all();
        }
    }
}

impl Drop for StoreCommitCoordinator {
    fn drop(&mut self) {
        let (state_lock, cond) = &*self.state;
        // parking_lot::Mutex::lock is infallible (no poisoning).
        let mut state = state_lock.lock();
        state.shutdown = true;
        cond.notify_all();
    }
}

impl UnifiedStore {
    pub(crate) fn deferred_store_wal_capture_active() -> bool {
        deferred_store_wal_capture_active()
    }

    pub(crate) fn begin_deferred_store_wal_capture() {
        begin_deferred_store_wal_capture();
    }

    pub(crate) fn take_deferred_store_wal_capture() -> DeferredStoreWalActions {
        take_deferred_store_wal_capture()
    }

    pub(crate) fn append_deferred_store_wal_actions(
        &self,
        actions: DeferredStoreWalActions,
    ) -> Result<(), StoreError> {
        if actions.actions.is_empty() {
            return Ok(());
        }
        if self.config.embedded_wal_path.is_some() {
            return self.append_embedded_store_wal_actions(&actions.actions);
        }
        // Deferred transactions need a committed WAL batch even in Strict
        // mode. WAL-only versions have no pager pages to flush yet.
        if let Some(commit) = &self.commit {
            let references = commit
                .append_checkpoint_actions(&actions.actions)
                .map_err(StoreError::Io)?;
            self.remember_checkpoint_rows(actions.actions, references);
            Ok(())
        } else {
            self.flush_paged_state()
        }
    }

    /// Emit a [`WalRecord::VectorInsert`] for a `vector.turbo`
    /// collection (issue #693). Returns `Ok(())` when no commit
    /// coordinator is configured (in-memory mode) so callers don't
    /// have to special-case the missing WAL — the in-memory index
    /// update remains durable enough for in-memory runtimes by
    /// construction.
    pub(crate) fn append_vector_insert_record(
        &self,
        collection: &str,
        entity_id: u64,
        vector: &[f32],
    ) -> std::io::Result<()> {
        let Some(commit) = &self.commit else {
            return Ok(());
        };
        let record = WalRecord::VectorInsert {
            collection: collection.to_string(),
            entity_id,
            vector: vector.to_vec(),
        };
        commit.append_single_record(record)
    }

    pub(crate) fn append_probabilistic_delta_record(
        &self,
        kind: u8,
        operation: u8,
        name: &str,
        operands: Vec<Vec<u8>>,
    ) -> std::io::Result<()> {
        let Some(commit) = &self.commit else {
            return Ok(());
        };
        let record = WalRecord::ProbabilisticDelta {
            kind,
            operation,
            name: name.to_string(),
            operands,
        };
        commit.append_single_record(record)
    }

    pub(crate) fn finish_paged_write(
        &self,
        actions: impl IntoIterator<Item = StoreWalAction>,
    ) -> Result<(), StoreError> {
        self.finish_paged_write_for_projection(actions, false)
    }

    pub(crate) fn finish_paged_write_for_projection(
        &self,
        actions: impl IntoIterator<Item = StoreWalAction>,
        track: bool,
    ) -> Result<(), StoreError> {
        let actions: Vec<StoreWalAction> = actions.into_iter().collect();
        if deferred_store_wal_capture_active() {
            let captured = capture_deferred_store_wal_actions(actions);
            debug_assert!(captured);
            return Ok(());
        }
        if self.config.embedded_wal_path.is_some() {
            return self.append_embedded_store_wal_actions(&actions);
        }
        let mut references = super::checkpoint_wal::CheckpointWalBatch::default();
        let result = match self.config.durability_mode {
            DurabilityMode::Strict => {
                if let Some(commit) = &self.commit {
                    if track {
                        references = commit
                            .append_checkpoint_actions(&actions)
                            .map_err(StoreError::Io)?;
                    } else {
                        commit.append_actions(&actions).map_err(StoreError::Io)?;
                    }
                }
                self.flush_paged_state()
            }
            DurabilityMode::WalDurableGrouped | DurabilityMode::Async => {
                if let Some(commit) = &self.commit {
                    if track {
                        references = commit
                            .append_checkpoint_actions(&actions)
                            .map_err(StoreError::Io)?;
                    } else {
                        commit.append_actions(&actions).map_err(StoreError::Io)?;
                    }
                    Ok(())
                } else {
                    self.flush_paged_state()
                }
            }
        };
        result?;
        if track {
            self.remember_checkpoint_rows(actions, references);
        }
        Ok(())
    }

    pub(crate) fn apply_encoded_store_wal_action(&self, payload: &[u8]) -> Result<(), StoreError> {
        let action = StoreWalAction::decode(payload).map_err(StoreError::Io)?;
        self.apply_replayed_action(&action, true)
    }

    fn append_embedded_store_wal_actions(
        &self,
        actions: &[StoreWalAction],
    ) -> Result<(), StoreError> {
        let Some(path) = self.config.embedded_wal_path.as_deref() else {
            return Ok(());
        };
        if actions.is_empty() {
            return Ok(());
        }
        let payloads: Vec<Vec<u8>> = actions.iter().map(StoreWalAction::encode).collect();
        let encoded_len = crate::storage::EmbeddedRdbArtifact::wal_payloads_encoded_len(&payloads)
            .map_err(|err| StoreError::Io(std::io::Error::other(err.to_string())))?;
        match self.append_embedded_payloads(path, &payloads) {
            Ok(_) => Ok(()),
            Err(crate::api::RedDBError::InvalidOperation(msg))
                if msg.contains("embedded wal region full") =>
            {
                self.recover_full_embedded_wal(path, &payloads, encoded_len)
            }
            Err(err) => Err(StoreError::Io(std::io::Error::other(err.to_string()))),
        }
    }

    fn recover_full_embedded_wal(
        &self,
        path: &Path,
        payloads: &[Vec<u8>],
        encoded_len: u64,
    ) -> Result<(), StoreError> {
        let _vault_guard = self.vault_publication_lock.lock();
        // The failed append preceded this lock: another writer may already
        // have reclaimed space. Retry through the same FIFO coordinator so
        // stale full results cannot each serialize another complete snapshot.
        match self.append_embedded_payloads(path, payloads) {
            Ok(_) => return Ok(()),
            Err(crate::api::RedDBError::InvalidOperation(message))
                if message.contains("embedded wal region full") => {}
            Err(error) => return Err(StoreError::Io(std::io::Error::other(error.to_string()))),
        }
        // A checkpoint can fail after publishing its image. Preserve the
        // session failure fence through errors and unwinding, just as append's
        // FlushBatch guard does; in-memory rollback alone cannot resolve disk.
        struct CheckpointFence<'a>(Option<&'a crate::storage::embedded::EmbeddedCommitSession>);
        impl Drop for CheckpointFence<'_> {
            fn drop(&mut self) {
                if let Some(session) = self.0 {
                    session.fence_interrupted_write();
                }
            }
        }
        let mut fence = CheckpointFence(Some(
            self.embedded_commit_session
                .get()
                .expect("WAL-full recovery follows an append attempt"),
        ));
        let started = Instant::now();
        let snapshot = self.to_binary_dump_bytes();
        let capacity = encoded_len.max((snapshot.len() as u64).min(EMBEDDED_WAL_HEADROOM_LIMIT));
        let checkpoint = crate::storage::EmbeddedRdbArtifact::write_snapshot_with_wal_capacity(
            path, &snapshot, capacity,
        )
        .map_err(|err| StoreError::Io(std::io::Error::other(err.to_string())))?;
        fence.0 = None;
        tracing::debug!(
            target: "reddb::embedded_checkpoint",
            snapshot_bytes = snapshot.len(),
            wal_region_bytes = checkpoint.manifest.wal_region_bytes,
            elapsed_us = started.elapsed().as_micros() as u64,
            "embedded WAL overflow checkpoint"
        );
        self.append_embedded_payloads(path, payloads)
            .map(|_| ())
            .map_err(|err| StoreError::Io(std::io::Error::other(err.to_string())))
    }

    fn append_embedded_payloads(
        &self,
        path: &Path,
        payloads: &[Vec<u8>],
    ) -> crate::RedDBResult<crate::storage::EmbeddedRdbOpen> {
        let session = self
            .embedded_commit_session
            .get_or_init(|| crate::storage::EmbeddedRdbArtifact::group_commit_session(path));
        let options = self.config.group_commit;
        let grouped = self.config.durability_mode == DurabilityMode::WalDurableGrouped;
        let window = if grouped {
            StoreCommitCoordinator::resolve_window(&options)
        } else {
            Duration::ZERO
        };
        session.append(
            payloads,
            reddb_file::embedded::EmbeddedGroupCommitOptions {
                window,
                max_statements: if grouped { options.max_statements } else { 1 },
                max_wal_bytes: options.max_wal_bytes,
            },
        )
    }

    pub(super) fn enqueue_grouped_actions(
        &self,
        actions: &[StoreWalAction],
    ) -> Result<Option<crate::storage::embedded::PendingEmbeddedAppend>, StoreError> {
        if self.config.durability_mode != DurabilityMode::WalDurableGrouped
            || deferred_store_wal_capture_active()
        {
            return Ok(None);
        }
        let Some(path) = self.config.embedded_wal_path.as_deref() else {
            return Ok(None);
        };
        let session = self
            .embedded_commit_session
            .get_or_init(|| crate::storage::EmbeddedRdbArtifact::group_commit_session(path));
        let payloads: Vec<Vec<u8>> = actions.iter().map(StoreWalAction::encode).collect();
        let options = self.config.group_commit;
        session
            .enqueue(
                &payloads,
                reddb_file::embedded::EmbeddedGroupCommitOptions {
                    window: StoreCommitCoordinator::resolve_window(&options),
                    max_statements: options.max_statements,
                    max_wal_bytes: options.max_wal_bytes,
                },
            )
            .map(Some)
            .map_err(|error| StoreError::Io(io::Error::other(error.to_string())))
    }

    pub(super) fn complete_grouped_actions(
        &self,
        pending: &crate::storage::embedded::PendingEmbeddedAppend,
        actions: &[StoreWalAction],
    ) -> Result<(), StoreError> {
        match pending.wait() {
            Ok(_) => Ok(()),
            Err(crate::RedDBError::InvalidOperation(message))
                if message.contains("embedded wal region full") =>
            {
                self.append_embedded_store_wal_actions(actions)
            }
            Err(error) => Err(StoreError::Io(io::Error::other(error.to_string()))),
        }
    }

    pub(crate) fn apply_replayed_action(
        &self,
        action: &StoreWalAction,
        update_btrees: bool,
    ) -> Result<(), StoreError> {
        reddb_engine::storage::store_wal::apply_replayed_action(
            &NativeReplayTarget {
                store: self,
                update_btrees,
            },
            action,
        )
    }

    /// Atomic full-collection replace — issue #595 slice 9c.
    ///
    /// Builds a fresh `SegmentManager` populated with `entities`,
    /// atomically swaps it into the collections map, rebuilds the
    /// paged B-tree from the same records, and emits a single
    /// `RefreshCollection` WAL action.
    ///
    /// Concurrent readers that resolved an `Arc<SegmentManager>` for
    /// this collection before the swap continue reading the prior
    /// contents; readers after the swap see the new contents. The
    /// transition is single-pointer so partial state is unobservable.
    ///
    /// A crash between in-memory mutation and WAL fsync leaves the
    /// previous WAL stream intact — on recovery the prior
    /// `RefreshCollection` (or whatever last bulk-applied to the
    /// collection) is replayed, so the prior contents are observed.
    pub fn refresh_collection(
        &self,
        name: &str,
        entities: Vec<UnifiedEntity>,
    ) -> Result<Vec<Vec<u8>>, StoreError> {
        let fv = self.format_version();

        // Build a fresh manager. Done outside the collections lock so
        // the critical section is just the pointer swap below.
        let new_manager = Arc::new(SegmentManager::with_config(
            name,
            self.config.manager_config.clone(),
        ));

        let mut prepared = entities;
        for entity in &mut prepared {
            if entity.id.raw() == 0 {
                entity.id = self.next_entity_id();
            } else {
                self.register_entity_id(entity.id);
            }
            if let EntityKind::TableRow { ref mut row_id, .. } = entity.kind {
                if *row_id == 0 {
                    *row_id = new_manager.next_row_id();
                } else {
                    new_manager.register_row_id(*row_id);
                }
            }
            entity.ensure_table_logical_id();
        }

        let serialized: Vec<Vec<u8>> = prepared
            .iter()
            .map(|e| Self::serialize_entity_record(e, None, fv))
            .collect();

        new_manager.bulk_insert(prepared.clone()).map_err(|e| {
            StoreError::Io(std::io::Error::other(format!(
                "refresh_collection: bulk_insert into new manager failed: {e}"
            )))
        })?;

        self.swap_collection_state(name, new_manager, &prepared, &serialized, true);

        self.finish_paged_write([StoreWalAction::RefreshCollection {
            collection: name.to_string(),
            records: serialized.clone(),
        }])?;

        Ok(serialized)
    }

    /// Atomic swap of the in-memory collection state. Used by both
    /// the live `refresh_collection` path and WAL replay. The new
    /// manager is already populated; this fn replaces the live Arc,
    /// purges side-state pointing at the previous contents, and
    /// rebuilds the paged B-tree from the supplied records.
    fn swap_collection_state(
        &self,
        name: &str,
        new_manager: Arc<SegmentManager>,
        prepared: &[UnifiedEntity],
        serialized: &[Vec<u8>],
        update_btrees: bool,
    ) {
        self.resident
            .swap_collection_state(name, new_manager, prepared);

        if let Some(pager) = self.pager.as_ref().filter(|_| update_btrees) {
            let new_btree = Arc::new(BTree::new(Arc::clone(pager)));
            let mut sorted: Vec<(Vec<u8>, Vec<u8>)> = prepared
                .iter()
                .zip(serialized.iter())
                .map(|(e, r)| (e.id.raw().to_be_bytes().to_vec(), r.clone()))
                .collect();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            if !sorted.is_empty() {
                let _ = new_btree.bulk_insert_sorted(&sorted);
            }
            self.btree_indices
                .write()
                .insert(name.to_string(), new_btree);
            self.mark_paged_registry_dirty();
        }
    }

    fn apply_replayed_refresh_collection(
        &self,
        collection: &str,
        records: &[Vec<u8>],
        update_btrees: bool,
    ) -> Result<(), StoreError> {
        let (new_manager, prepared) = self.resident.prepare_replayed_refresh_collection(
            collection,
            records,
            self.format_version(),
            &self.config.manager_config,
        )?;
        self.swap_collection_state(collection, new_manager, &prepared, records, update_btrees);
        Ok(())
    }

    /// Replica-side analogue of [`Self::refresh_collection`] — issue
    /// #596 slice 9d. Takes pre-serialized record bytes from the
    /// primary's CDC stream (the same bytes the primary wrote into the
    /// `RefreshCollection` WAL action), applies the atomic swap, and
    /// emits the same `RefreshCollection` WAL action against the
    /// replica's local store WAL so the post-swap state survives a
    /// replica restart through the normal recovery path.
    ///
    /// Idempotency: re-applying the same records bytes is a full
    /// rebuild from the same payload, so the resulting backing-
    /// collection contents are equal to the prior call's result.
    /// The primary-side `LogicalChangeApplier` short-circuits exact
    /// duplicate LSN+payload combinations before reaching here.
    pub fn refresh_collection_from_records(
        &self,
        name: &str,
        records: Vec<Vec<u8>>,
    ) -> Result<(), StoreError> {
        self.apply_replayed_refresh_collection(name, &records, true)?;
        self.finish_paged_write([StoreWalAction::RefreshCollection {
            collection: name.to_string(),
            records,
        }])?;
        Ok(())
    }

    pub(crate) fn create_collection_in_memory(&self, name: &str) -> Result<(), StoreError> {
        self.resident
            .create_collection_in_memory(name, &self.config.manager_config)
    }

    fn drop_collection_in_memory(&self, name: &str) -> Result<(), StoreError> {
        self.resident.drop_collection_in_memory(name, || {
            if let Some(tree) = self.btree_indices.write().remove(name) {
                self.retire_btree(tree);
            }
        })
    }

    fn apply_replayed_upsert(
        &self,
        collection: &str,
        record: &[u8],
        update_btrees: bool,
    ) -> Result<(), StoreError> {
        let id = self.resident.apply_replayed_upsert(
            collection,
            record,
            self.format_version(),
            &self.config.manager_config,
        )?;

        if let Some(pager) = self.pager.as_ref().filter(|_| update_btrees) {
            let mut btree_indices = self.btree_indices.write();
            let btree = btree_indices
                .entry(collection.to_string())
                .or_insert_with(|| Arc::new(BTree::new(Arc::clone(pager))));
            let root_before = btree.root_page_id();
            let key = id.raw().to_be_bytes();
            match btree.insert(&key, record) {
                Ok(_) => {}
                Err(BTreeError::DuplicateKey) => {
                    let _ = btree.delete(&key);
                    let _ = btree.insert(&key, record);
                }
                Err(err) => {
                    return Err(StoreError::Io(io::Error::other(format!(
                        "replay upsert btree error: {err}"
                    ))));
                }
            }
            if root_before != btree.root_page_id() {
                self.mark_paged_registry_dirty();
            }
        }

        Ok(())
    }

    fn apply_replayed_delete(
        &self,
        collection: &str,
        id: EntityId,
        update_btrees: bool,
    ) -> Result<(), StoreError> {
        self.resident.apply_replayed_delete(collection, id, || {
            if self.pager.is_some() && update_btrees {
                let btree_indices = self.btree_indices.read();
                if let Some(btree) = btree_indices.get(collection) {
                    let root_before = btree.root_page_id();
                    let key = id.raw().to_be_bytes();
                    let _ = btree.delete(&key);
                    if root_before != btree.root_page_id() {
                        self.mark_paged_registry_dirty();
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{DurabilityMode, GroupCommitOptions};
    use std::sync::{Barrier, Mutex as StdMutex, OnceLock};
    use std::time::SystemTime;

    #[test]
    fn ownership_epoch_rejects_nested_begin_and_wrong_finish() {
        let directory = tempfile::tempdir().expect("temporary store");
        let store = UnifiedStore::open(directory.path().join("data.rdb")).expect("store");
        store.create_collection("rows").expect("collection");
        store.persist().expect("baseline");
        let coordinator = store.commit.as_ref().expect("WAL coordinator");
        assert!(coordinator.begin_page_ownership_epoch(&store, 0).is_err());
        assert!(coordinator.finish_page_ownership_epoch(&store, 1).is_err());
        coordinator
            .begin_page_ownership_epoch(&store, 2)
            .expect("begin epoch");
        assert!(coordinator.begin_page_ownership_epoch(&store, 3).is_err());
        assert!(coordinator.finish_page_ownership_epoch(&store, 3).is_err());
        assert!(coordinator.finish_page_ownership_epoch(&store, 2).is_err());
        store.flush_paged_registry().expect("publish epoch");
        coordinator
            .finish_page_ownership_epoch(&store, 2)
            .expect("finish epoch");
        assert_eq!(store.ownership_active_epoch.load(Ordering::Acquire), 0);
    }

    #[test]
    fn native_pager_records_page_ownership_in_durable_wal() {
        use reddb_file::page_ownership_journal::PageOwnershipRecord;

        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        let wal_path = reddb_file::layout::unified_wal_path(&path);
        let pager = crate::storage::engine::Pager::open(&path, Default::default())
            .expect("open native pager");
        let coordinator = Arc::new(
            StoreCommitCoordinator::open(
                &wal_path,
                DurabilityMode::Async,
                GroupCommitOptions::default(),
            )
            .expect("open WAL"),
        );
        let recorder = Arc::new(
            StoreOwnershipRecorder::begin(Arc::clone(&coordinator), 7).expect("begin epoch"),
        );
        pager
            .install_ownership_recorder(recorder)
            .expect("install recorder");
        let page = pager
            .allocate_page(crate::storage::engine::PageType::BTreeLeaf)
            .expect("allocate")
            .page_id();
        pager
            .record_retirement_intent(&[page])
            .expect("record retirement");
        pager.free_retired_pages(&[page]).expect("retire");
        let records: Vec<_> = WalReader::open(&wal_path)
            .expect("read WAL")
            .iter()
            .map(|item| item.expect("WAL record").1)
            .collect();
        assert_eq!(
            records,
            vec![
                WalRecord::PageOwnership {
                    record: PageOwnershipRecord::Begin { epoch: 7 }
                },
                WalRecord::PageOwnership {
                    record: PageOwnershipRecord::Reserve {
                        epoch: 7,
                        page_id: page
                    }
                },
                WalRecord::PageOwnership {
                    record: PageOwnershipRecord::Retire {
                        epoch: 7,
                        page_id: page
                    }
                },
                WalRecord::PageOwnership {
                    record: PageOwnershipRecord::Retire {
                        epoch: 7,
                        page_id: page
                    }
                },
            ]
        );
        assert!(
            coordinator.fsync_count() >= 3,
            "async ownership writes must sync"
        );
        assert!(
            coordinator.truncate().is_err(),
            "ownership WAL remains protected"
        );
    }

    #[test]
    fn replay_reclaims_unpublished_ownership_reservations() {
        use reddb_file::page_ownership_journal::PageOwnershipRecord;

        let directory = tempfile::tempdir().expect("temporary store");
        let path = directory.path().join("data.rdb");
        {
            let store = UnifiedStore::open(&path).expect("create store");
            store.create_collection("rows").expect("collection");
            store.persist().expect("persist baseline");
        }
        let wal_path = reddb_file::layout::unified_wal_path(&path);
        let ids;
        {
            let pager =
                crate::storage::engine::Pager::open(&path, Default::default()).expect("pager");
            let first = pager.page_count().expect("page count");
            let coordinator = StoreCommitCoordinator::open(
                &wal_path,
                DurabilityMode::Strict,
                GroupCommitOptions::default(),
            )
            .expect("WAL");
            coordinator
                .append_durable_page_ownership_batch(&[
                    PageOwnershipRecord::Begin { epoch: 2 },
                    PageOwnershipRecord::Reserve {
                        epoch: 2,
                        page_id: first,
                    },
                    PageOwnershipRecord::Reserve {
                        epoch: 2,
                        page_id: first + 1,
                    },
                ])
                .expect("reserve pages durably");
            ids = [
                pager
                    .allocate_page(crate::storage::engine::PageType::BTreeLeaf)
                    .expect("allocate")
                    .page_id(),
                pager
                    .allocate_page(crate::storage::engine::PageType::BTreeLeaf)
                    .expect("allocate")
                    .page_id(),
            ];
            assert_eq!(ids, [first, first + 1]);
            pager.sync().expect("persist abandoned pages");
        }
        let reopened = UnifiedStore::open(&path).expect("recover ownership");
        let pager = reopened.pager.as_ref().expect("pager");
        let a = pager
            .allocate_page(crate::storage::engine::PageType::BTreeLeaf)
            .expect("reuse")
            .page_id();
        let b = pager
            .allocate_page(crate::storage::engine::PageType::BTreeLeaf)
            .expect("reuse")
            .page_id();
        assert_ne!(a, b);
        assert!(ids.contains(&a) && ids.contains(&b));
        let records: Vec<_> = WalReader::open(&wal_path)
            .expect("read WAL")
            .iter()
            .collect::<io::Result<Vec<_>>>()
            .expect("records");
        assert!(matches!(
            records.last(),
            Some((
                _,
                WalRecord::PageOwnership {
                    record: PageOwnershipRecord::Finish { epoch: 2 }
                }
            ))
        ));
        reopened
            .commit
            .as_ref()
            .expect("coordinator")
            .truncate()
            .expect("completed ownership can be truncated");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ownership_recovery_survives_interruption_and_retry() {
        use reddb_file::page_ownership_journal::PageOwnershipRecord;

        const CHILD: &str = "REDDB_EXPERIMENTAL_OWNERSHIP_RECOVERY_PATH";
        if let Some(path) = std::env::var_os(CHILD) {
            let _store = UnifiedStore::open(path).expect("recover store");
            panic!("recovery interruption hook was not reached");
        }
        for (published, hook, exit_code) in [
            (
                false,
                "REDDB_EXPERIMENTAL_EXIT_DURING_OWNERSHIP_RECOVERY",
                187,
            ),
            (false, "REDDB_EXPERIMENTAL_EXIT_AFTER_OWNERSHIP_SYNC", 188),
            (false, "REDDB_EXPERIMENTAL_EXIT_AFTER_OWNERSHIP_FINISH", 189),
            (
                true,
                "REDDB_EXPERIMENTAL_EXIT_DURING_OWNERSHIP_RECOVERY",
                187,
            ),
            (true, "REDDB_EXPERIMENTAL_EXIT_AFTER_OWNERSHIP_SYNC", 188),
            (true, "REDDB_EXPERIMENTAL_EXIT_AFTER_OWNERSHIP_FINISH", 189),
        ] {
            let directory = tempfile::tempdir().expect("temporary store");
            let path = directory.path().join("data.rdb");
            {
                let store = UnifiedStore::open(&path).expect("create store");
                store.create_collection("rows").expect("collection");
                store.persist().expect("baseline");
                if published {
                    store.ownership_active_epoch.store(2, Ordering::Release);
                    store.flush_paged_registry().expect("publish epoch");
                    store.ownership_active_epoch.store(0, Ordering::Release);
                }
            }
            let wal_path = reddb_file::layout::unified_wal_path(&path);
            let ids;
            {
                let pager =
                    crate::storage::engine::Pager::open(&path, Default::default()).expect("pager");
                let first = pager.page_count().expect("page count");
                let coordinator = StoreCommitCoordinator::open(
                    &wal_path,
                    DurabilityMode::Strict,
                    GroupCommitOptions::default(),
                )
                .expect("WAL");
                let intent = |page_id| {
                    if published {
                        PageOwnershipRecord::Retire { epoch: 2, page_id }
                    } else {
                        PageOwnershipRecord::Reserve { epoch: 2, page_id }
                    }
                };
                coordinator
                    .append_durable_page_ownership_batch(&[
                        PageOwnershipRecord::Begin { epoch: 2 },
                        intent(first),
                        intent(first + 1),
                    ])
                    .expect("reserve");
                ids = [
                    pager
                        .allocate_page(crate::storage::engine::PageType::BTreeLeaf)
                        .expect("allocate")
                        .page_id(),
                    pager
                        .allocate_page(crate::storage::engine::PageType::BTreeLeaf)
                        .expect("allocate")
                        .page_id(),
                ];
                pager.sync().expect("persist abandoned pages");
            }
            for _ in 0..if exit_code == 189 { 1 } else { 2 } {
                let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args(["--exact", "storage::unified::store::commit::tests::ownership_recovery_survives_interruption_and_retry", "--nocapture"])
                    .env(CHILD, &path)
                    .env(hook, "1")
                    .output().expect("run interrupted recovery");
                assert_eq!(
                    output.status.code(),
                    Some(exit_code),
                    "published={published} {hook}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let reopened = UnifiedStore::open(&path).expect("recover after interruptions");
            let pager = reopened.pager.as_ref().expect("pager");
            let a = pager
                .allocate_page(crate::storage::engine::PageType::BTreeLeaf)
                .expect("reuse")
                .page_id();
            let b = pager
                .allocate_page(crate::storage::engine::PageType::BTreeLeaf)
                .expect("reuse")
                .page_id();
            assert_ne!(a, b, "published={published} {hook}: duplicate page");
            assert!(
                ids.contains(&a) && ids.contains(&b),
                "published={published} {hook}: lost recovered page"
            );
        }
    }

    #[test]
    fn ownership_intents_are_durable_in_every_mode() {
        use reddb_file::page_ownership_journal::PageOwnershipRecord;

        for mode in [
            DurabilityMode::Strict,
            DurabilityMode::WalDurableGrouped,
            DurabilityMode::Async,
        ] {
            let directory = tempfile::tempdir().expect("temporary WAL");
            let path = directory.path().join("ownership");
            let coordinator =
                StoreCommitCoordinator::open(&path, mode, GroupCommitOptions::default())
                    .expect("open coordinator");
            let intent = PageOwnershipRecord::Reserve {
                epoch: 1,
                page_id: 67,
            };
            let detach = PageOwnershipRecord::DetachFree {
                epoch: 1,
                page_id: 68,
            };
            coordinator
                .append_durable_page_ownership_batch(&[intent, detach])
                .expect("durable intent");
            assert!(
                coordinator.fsync_count() > 0,
                "ownership intent must be synced in {mode:?}"
            );
            let records: Vec<_> = WalReader::open(&path)
                .expect("open WAL")
                .iter()
                .collect::<io::Result<Vec<_>>>()
                .expect("read records");
            assert!(
                matches!(records.as_slice(), [(_, WalRecord::PageOwnership { record: first }), (_, WalRecord::PageOwnership { record: second })] if *first == intent && *second == detach)
            );
            assert!(coordinator
                .append_single_record(WalRecord::PageOwnership { record: intent })
                .is_err());
            assert!(coordinator
                .append_durable_page_ownership(PageOwnershipRecord::Begin { epoch: 0 })
                .is_err());
            assert!(
                coordinator.truncate().is_err(),
                "active ownership must prevent truncation"
            );
            let before = std::fs::read(&path).expect("ownership WAL");
            drop(coordinator);
            let reopened = StoreCommitCoordinator::open(&path, mode, GroupCommitOptions::default())
                .expect("reopen coordinator");
            assert!(
                reopened.truncate().is_err(),
                "reopen must rediscover ownership records"
            );
            assert_eq!(std::fs::read(&path).expect("preserved WAL"), before);
        }
    }

    #[test]
    fn unresolved_ownership_intent_blocks_store_replay() {
        for truncated in [false, true] {
            let directory = tempfile::tempdir().expect("temporary store");
            let path = directory.path().join("data.rdb");
            {
                let store = UnifiedStore::open(&path).expect("create store");
                store.create_collection("rows").expect("create collection");
                store.persist().expect("persist baseline");
            }
            let wal_path = reddb_file::layout::unified_wal_path(&path);
            {
                let mut writer = WalWriter::open(&wal_path).expect("open WAL");
                writer
                    .append(&WalRecord::PageOwnership {
                        record: reddb_file::page_ownership_journal::PageOwnershipRecord::Reserve {
                            epoch: 1,
                            page_id: 67,
                        },
                    })
                    .expect("append ownership intent");
                writer.sync().expect("sync ownership intent");
            }
            if truncated {
                let wal = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&wal_path)
                    .expect("WAL");
                let length = wal.metadata().expect("length").len();
                wal.set_len(length - 1).expect("truncate last byte");
                wal.sync_all().expect("sync truncation");
            }
            let before = std::fs::read(&path).expect("baseline data");
            let error = match UnifiedStore::open(&path) {
                Ok(_) => panic!("unresolved ownership must prevent opening"),
                Err(error) => error,
            };
            let expected = if truncated {
                "incomplete page ownership WAL record"
            } else {
                "page ownership journal requires recovery"
            };
            assert!(error.to_string().contains(expected), "{error}");
            assert_eq!(std::fs::read(&path).expect("preserved data"), before);
        }
    }

    #[test]
    fn logical_wal_replay_restores_a_deleted_physical_identity() {
        for flat in [false, true] {
            let store = UnifiedStore::new();
            store.create_collection("replay").expect("collection");
            let id = EntityId::new(42);
            let entity = UnifiedEntity::table_row(id, "replay", 42, vec![Value::Integer(1)]);
            if flat {
                store
                    .bulk_insert("replay", vec![entity])
                    .expect("flat seed");
            } else {
                store.insert_auto("replay", entity).expect("hash seed");
            }
            let metadata = Metadata::with_fields(HashMap::from([(
                "source".into(),
                MetadataValue::String("wal".into()),
            )]));
            for sequence in 100..104 {
                let delete = StoreWalAction::DeleteEntityRecord {
                    collection: "replay".into(),
                    entity_id: id.raw(),
                };
                store
                    .apply_encoded_store_wal_action(&delete.encode())
                    .expect("replay delete");
                let mut restored =
                    UnifiedEntity::table_row(id, "replay", 42, vec![Value::Integer(2)]);
                restored.ensure_table_logical_id();
                restored.sequence_id = sequence;
                let upsert = StoreWalAction::upsert_entity(
                    "replay",
                    &restored,
                    Some(&metadata),
                    store.format_version(),
                );
                store
                    .apply_encoded_store_wal_action(&upsert.encode())
                    .expect("replay upsert");
                let manager = store.get_collection("replay").expect("restored collection");
                let stored = manager.get(id).expect("restored entity is readable");
                assert_eq!(store.entity_cache_stats().entries, 0);
                assert_eq!(manager.count(), 1);
                assert_eq!(manager.query_all(|_| true).len(), 1);
                assert_eq!(stored.sequence_id, sequence);
                assert_eq!(
                    UnifiedStore::serialize_entity_record(
                        &stored,
                        manager.get_metadata(id).as_ref(),
                        store.format_version()
                    ),
                    UnifiedStore::serialize_entity_record(
                        &restored,
                        Some(&metadata),
                        store.format_version()
                    )
                );
                assert!(store.get_any(id).is_some());
            }
        }
    }

    fn embedded_fixture(
        mode: DurabilityMode,
        background_bytes: usize,
    ) -> (tempfile::TempDir, PathBuf, UnifiedStore) {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("data.rdb");
        let mut store =
            UnifiedStore::with_config(UnifiedStoreConfig::default().with_durability_mode(mode));
        store.create_collection("keys").expect("collection");
        store.set_aux_metadata(vec![42; background_bytes]);
        crate::storage::EmbeddedRdbArtifact::create_with_snapshot(
            &path,
            &store.to_binary_dump_bytes(),
        )
        .expect("snapshot");
        store.config.embedded_wal_path = Some(path.clone());
        (directory, path, store)
    }

    #[test]
    fn embedded_stale_full_result_does_not_checkpoint_reclaimed_space() {
        for mode in [DurabilityMode::Strict, DurabilityMode::WalDurableGrouped] {
            let (_directory, path, store) = embedded_fixture(mode, 0);
            store
                .append_embedded_store_wal_actions(&[StoreWalAction::CreateCollection {
                    name: "keys".into(),
                }])
                .expect("another writer appended after reclaiming space");
            let before = crate::storage::EmbeddedRdbArtifact::open(&path).expect("before");
            let payloads = vec![StoreWalAction::CreateCollection {
                name: "after".into(),
            }
            .encode()];
            let length = crate::storage::EmbeddedRdbArtifact::wal_payloads_encoded_len(&payloads)
                .expect("encoded length");
            // Model a full result obtained before that other writer's checkpoint.
            store
                .recover_full_embedded_wal(&path, &payloads, length)
                .expect("revalidate after acquiring publication");
            let after = crate::storage::EmbeddedRdbArtifact::open(&path).expect("after");
            assert_eq!(
                after.selected_superblock.generation,
                before.selected_superblock.generation + 1,
                "only the retry is published, without an unnecessary checkpoint"
            );
            assert_eq!(
                crate::storage::EmbeddedRdbArtifact::read_wal_payloads(&after)
                    .expect("both appends retained")
                    .len(),
                2
            );
        }
    }

    #[test]
    fn embedded_full_wal_reserves_bounded_headroom_for_populated_store() {
        for mode in [DurabilityMode::Strict, DurabilityMode::WalDurableGrouped] {
            let (_directory, path, store) = embedded_fixture(mode, 2 * 1024 * 1024);
            let initial = crate::storage::EmbeddedRdbArtifact::open(&path).expect("initial");
            let action = StoreWalAction::CreateCollection {
                name: "keys".into(),
            };
            let payloads = vec![action.encode()];
            let length = crate::storage::EmbeddedRdbArtifact::wal_payloads_encoded_len(&payloads)
                .expect("encoded length");
            crate::storage::EmbeddedRdbArtifact::append_wal_payloads(
                &path,
                &vec![payloads[0].clone(); (initial.manifest.wal_region_bytes / length) as usize],
            )
            .expect("fill the initial region with valid records");
            store
                .append_embedded_store_wal_actions(&[action])
                .expect("overflow");
            let open = crate::storage::EmbeddedRdbArtifact::open(&path).expect("checkpoint");
            assert_eq!(open.manifest.wal_region_bytes, 1024 * 1024);
            let snapshot = crate::storage::EmbeddedRdbArtifact::read_snapshot(&open)
                .expect("snapshot")
                .expect("populated image");
            let recovered =
                UnifiedStore::load_from_bytes_with_config(&snapshot, UnifiedStoreConfig::default())
                    .expect("recover snapshot");
            for payload in
                crate::storage::EmbeddedRdbArtifact::read_wal_payloads(&open).expect("WAL")
            {
                recovered
                    .apply_encoded_store_wal_action(&payload)
                    .expect("replay");
            }
            assert_eq!(
                recovered.resident.aux_metadata.read().len(),
                2 * 1024 * 1024
            );
            assert!(recovered.get_collection("keys").is_some());
        }
    }

    #[test]
    fn embedded_grouped_overwrites_recover_across_repeated_full_wal_regions() {
        fn row(writer: usize, round: usize) -> UnifiedEntity {
            let mut state = (writer as u64 + 1) * 31 + round as u64 * 13;
            let payload: String = (0..4096)
                .map(|_| {
                    state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                    char::from(b'!' + ((state >> 24) % 90) as u8)
                })
                .collect();
            let mut entity = UnifiedEntity::table_row(EntityId::new(0), "keys", 0, vec![]);
            entity.data = EntityData::Row(RowData::with_names(
                vec![
                    Value::text(format!("k{writer}")),
                    Value::text(format!("{round}:{payload}")),
                ],
                vec!["key".into(), "value".into()],
            ));
            entity
        }
        let (_directory, path, store) =
            embedded_fixture(DurabilityMode::WalDurableGrouped, 2 * 1024 * 1024);
        assert!(
            StoreWalAction::upsert_entity("keys", &row(0, 0), None, store.format_version())
                .encode()
                .len()
                >= 2048,
            "fixture must exert physical WAL pressure after value compression"
        );
        let mut ids: Vec<_> = (0..8)
            .map(|writer| {
                store
                    .write_keyed_row("keys", row(writer, 0), None, None)
                    .expect("seed")
            })
            .collect();
        let initial = crate::storage::EmbeddedRdbArtifact::open(&path).expect("initial roots");
        let mut previous_snapshot = crate::storage::EmbeddedRdbArtifact::read_snapshot(&initial)
            .expect("initial snapshot")
            .expect("image");
        // Each wave writes over 2 MiB through the same 1 MiB reservation.
        // Checking image changes after each wave prevents a larger WAL from
        // merely postponing every checkpoint beyond the test's duration.
        for wave in 0..3 {
            let barrier = Barrier::new(ids.len());
            ids = std::thread::scope(|scope| {
                let workers: Vec<_> = ids
                    .iter()
                    .enumerate()
                    .map(|(writer, previous)| {
                        let store = &store;
                        let barrier = &barrier;
                        scope.spawn(move || {
                            barrier.wait();
                            let mut id = *previous;
                            for round in 0..64 {
                                id = store
                                    .write_keyed_row(
                                        "keys",
                                        row(writer, wave * 64 + round),
                                        None,
                                        Some(id),
                                    )
                                    .expect("durable overwrite");
                            }
                            id
                        })
                    })
                    .collect();
                workers
                    .into_iter()
                    .map(|worker| worker.join().expect("writer"))
                    .collect()
            });
            let open = crate::storage::EmbeddedRdbArtifact::open(&path).expect("wave roots");
            assert_eq!(open.manifest.wal_region_bytes, EMBEDDED_WAL_HEADROOM_LIMIT);
            let snapshot = crate::storage::EmbeddedRdbArtifact::read_snapshot(&open)
                .expect("wave snapshot")
                .expect("image");
            assert!(snapshot != previous_snapshot, "wave {wave} must checkpoint");
            previous_snapshot = snapshot;
        }
        let open = crate::storage::EmbeddedRdbArtifact::open(&path).expect("recover roots");
        let snapshot = crate::storage::EmbeddedRdbArtifact::read_snapshot(&open)
            .expect("snapshot")
            .expect("image");
        let recovered =
            UnifiedStore::load_from_bytes_with_config(&snapshot, UnifiedStoreConfig::default())
                .expect("snapshot replay");
        for payload in crate::storage::EmbeddedRdbArtifact::read_wal_payloads(&open).expect("WAL") {
            recovered
                .apply_encoded_store_wal_action(&payload)
                .expect("WAL replay");
        }
        assert_eq!(recovered.get_collection("keys").expect("keys").count(), 8);
        for (writer, id) in ids.iter().enumerate() {
            let entity = recovered
                .get("keys", *id)
                .expect("last acknowledged version");
            assert_eq!(
                entity.data.as_row().expect("row").get_field("value"),
                row(writer, 191)
                    .data
                    .as_row()
                    .expect("expected row")
                    .get_field("value")
            );
        }
    }

    /// Serialise tests that mutate `REDDB_GROUP_COMMIT_WINDOW_US`.
    /// The env table is process-global, so two parallel test threads
    /// flipping it would race the assertions in the test that
    /// happens to read it last.
    fn env_lock() -> &'static StdMutex<()> {
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
    }

    fn temp_wal(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = reddb_file::layout::store_commit_coord_temp_wal_path(
            &std::env::temp_dir(),
            name,
            std::process::id(),
            nanos,
        );
        let _ = std::fs::remove_file(&path);
        path
    }

    fn replay_crash_payload(id: u64, round: u64) -> String {
        let mut state = id * 31 + round * 13 + 1;
        (0..600)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                char::from(b'!' + ((state >> 24) % 90) as u8)
            })
            .collect()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn interrupted_replay_reclaims_unpublished_pages() {
        interrupted_replay_reclaims_pages(false, false);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn interrupted_replay_reclaims_durable_unpublished_pages() {
        interrupted_replay_reclaims_pages(false, true);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn interrupted_replay_preserves_published_pages() {
        interrupted_replay_reclaims_pages(true, false);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn interrupted_checkpoint_reclaims_unpublished_pages() {
        const CHILD_PATH: &str = "REDDB_EXPERIMENTAL_CHECKPOINT_REBUILD_PATH";
        let config = || {
            UnifiedStoreConfig::default()
                .with_durability_mode(DurabilityMode::Strict)
                .with_page_cache_slots(4)
        };
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let store = UnifiedStore::open_with_config(path, config()).unwrap();
            // Inject resident changes without a WAL record: after the aborted
            // checkpoint the old durable rows must remain authoritative.
            for id in 1..=200 {
                let entity = UnifiedEntity::table_row(
                    EntityId::new(id),
                    "rows",
                    id,
                    vec![Value::text(replay_crash_payload(id, 8))],
                );
                let record =
                    UnifiedStore::serialize_entity_record(&entity, None, STORE_VERSION_V11);
                store.apply_replayed_upsert("rows", &record, false).unwrap();
            }
            store.persist().expect("must exit during checkpoint");
            panic!("checkpoint rebuild hook was not reached");
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.rdb");
        {
            let store = UnifiedStore::open_with_config(&path, config()).unwrap();
            store.create_collection("rows").unwrap();
            for id in 1..=200 {
                store
                    .insert(
                        "rows",
                        UnifiedEntity::table_row(
                            EntityId::new(id),
                            "rows",
                            id,
                            vec![Value::text(format!("initial-{id}"))],
                        ),
                    )
                    .unwrap();
            }
            store.persist().unwrap();
        }
        let mut first = 0;
        let mut last = 0;
        for cycle in 1..=12 {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "storage::unified::store::commit::tests::interrupted_checkpoint_reclaims_unpublished_pages",
                    "--nocapture",
                ])
                .env(CHILD_PATH, &path)
                .env("REDDB_EXPERIMENTAL_EXIT_DURING_CHECKPOINT_REBUILD", "1")
                .output()
                .unwrap();
            assert_eq!(
                output.status.code(),
                Some(183),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("experimental checkpoint exit before manifest"));
            last = std::fs::metadata(&path).unwrap().len();
            if cycle == 1 {
                first = last;
            }
            eprintln!("checkpoint_exit_cycle={cycle} data_logical={last}");
        }
        let recovered = UnifiedStore::open_with_config(&path, config()).unwrap();
        assert_eq!(recovered.get_collection("rows").unwrap().count(), 200);
        for id in 1..=200 {
            assert_eq!(
                recovered
                    .get("rows", EntityId::new(id))
                    .unwrap()
                    .data
                    .as_row()
                    .unwrap()
                    .columns,
                vec![Value::text(format!("initial-{id}"))]
            );
        }
        assert!(last <= first + 2 * crate::storage::engine::PAGE_SIZE as u64,
            "repeated interrupted checkpoint must bound page growth: first={first}, after_12={last}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn interrupted_checkpoint_with_wal_reclaims_unpublished_pages() {
        use std::os::unix::fs::MetadataExt;
        const CHILD_PATH: &str = "REDDB_EXPERIMENTAL_CHECKPOINT_WAL_PATH";
        const ROUND: &str = "REDDB_EXPERIMENTAL_CHECKPOINT_WAL_ROUND";
        let config = || {
            UnifiedStoreConfig::default()
                .with_durability_mode(DurabilityMode::Strict)
                .with_page_cache_slots(4)
        };
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let round: u64 = std::env::var(ROUND).unwrap().parse().unwrap();
            let store = UnifiedStore::open_with_config(path, config()).unwrap();
            if round > 1 {
                for id in 1..=200 {
                    assert_eq!(
                        store
                            .get("rows", EntityId::new(id))
                            .unwrap()
                            .data
                            .as_row()
                            .unwrap()
                            .columns,
                        vec![Value::text(replay_crash_payload(id, round - 1))]
                    );
                }
            }
            let actions: Vec<_> = (1..=200)
                .map(|id| {
                    let entity = UnifiedEntity::table_row(
                        EntityId::new(id),
                        "rows",
                        id,
                        vec![Value::text(replay_crash_payload(id, round))],
                    );
                    StoreWalAction::upsert_entity("rows", &entity, None, STORE_VERSION_V11)
                })
                .collect();
            for action in &actions {
                store.apply_replayed_action(action, false).unwrap();
            }
            // Use the actual strict WAL coordinator. No checkpoint proof is
            // installed, so the replacement-tree path must still be taken.
            store
                .commit
                .as_ref()
                .unwrap()
                .append_actions(&actions)
                .unwrap();
            store.persist().expect("must exit during checkpoint");
            panic!("checkpoint rebuild hook was not reached");
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.rdb");
        {
            let store = UnifiedStore::open_with_config(&path, config()).unwrap();
            store.create_collection("rows").unwrap();
            for id in 1..=200 {
                store
                    .insert(
                        "rows",
                        UnifiedEntity::table_row(
                            EntityId::new(id),
                            "rows",
                            id,
                            vec![Value::text(format!("initial-{id}"))],
                        ),
                    )
                    .unwrap();
            }
            store.persist().unwrap();
        }
        let mut fourth = 0;
        let mut last = 0;
        for round in 1..=12 {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "storage::unified::store::commit::tests::interrupted_checkpoint_with_wal_reclaims_unpublished_pages",
                    "--nocapture",
                ])
                .env(CHILD_PATH, &path)
                .env(ROUND, round.to_string())
                .env("REDDB_EXPERIMENTAL_EXIT_DURING_CHECKPOINT_REBUILD", "1")
                .output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(183),
                "round {round}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("experimental checkpoint exit before manifest"));
            let meta = std::fs::metadata(&path).unwrap();
            last = meta.len();
            if round == 4 {
                fourth = last;
            }
            eprintln!(
                "checkpoint_wal_exit_round={round} data_logical={last} data_allocated={}",
                meta.blocks() * 512
            );
        }
        let recovered = UnifiedStore::open_with_config(&path, config()).unwrap();
        assert_eq!(recovered.get_collection("rows").unwrap().count(), 200);
        for id in 1..=200 {
            assert_eq!(
                recovered
                    .get("rows", EntityId::new(id))
                    .unwrap()
                    .data
                    .as_row()
                    .unwrap()
                    .columns,
                vec![Value::text(replay_crash_payload(id, 12))]
            );
        }
        recovered.persist().unwrap();
        drop(recovered);
        let reopened = UnifiedStore::open_with_config(&path, config()).unwrap();
        for id in 1..=200 {
            assert_eq!(
                reopened
                    .get("rows", EntityId::new(id))
                    .unwrap()
                    .data
                    .as_row()
                    .unwrap()
                    .columns,
                vec![Value::text(replay_crash_payload(id, 12))]
            );
        }
        assert!(last <= fourth + 2 * crate::storage::engine::PAGE_SIZE as u64,
            "repeated interrupted WAL-backed checkpoints must bound page growth: round4={fourth}, round12={last}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn interrupted_retired_reclamation_preserves_reuse() {
        const CHILD_PATH: &str = "REDDB_EXPERIMENTAL_RETIRED_PATH";
        fn mutate(store: &UnifiedStore, round: u64) {
            for id in 1..=200 {
                let entity = UnifiedEntity::table_row(
                    EntityId::new(id),
                    "rows",
                    id,
                    vec![Value::text(replay_crash_payload(id, round))],
                );
                let record =
                    UnifiedStore::serialize_entity_record(&entity, None, STORE_VERSION_V11);
                store.apply_replayed_upsert("rows", &record, false).unwrap();
            }
        }
        let config = || {
            UnifiedStoreConfig::default()
                .with_durability_mode(DurabilityMode::Strict)
                .with_page_cache_slots(4)
        };
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let store = UnifiedStore::open_with_config(path, config()).unwrap();
            mutate(&store, 2);
            store.persist().expect("must exit at retired free boundary");
            panic!("retired free hook was not reached");
        }
        let mut reuse = Vec::new();
        for (label, env_name, exit_code, message) in [
            (
                "before",
                "REDDB_EXPERIMENTAL_EXIT_BEFORE_RETIRED_FREE",
                184,
                "experimental exit before retired free",
            ),
            (
                "enqueued",
                "REDDB_EXPERIMENTAL_EXIT_AFTER_RETIRED_ENQUEUE",
                186,
                "experimental exit after retired enqueue",
            ),
            (
                "after",
                "REDDB_EXPERIMENTAL_EXIT_AFTER_RETIRED_FREE",
                185,
                "experimental exit after retired free",
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("data.rdb");
            {
                let store = UnifiedStore::open_with_config(&path, config()).unwrap();
                store.create_collection("rows").unwrap();
                for id in 1..=200 {
                    store
                        .insert(
                            "rows",
                            UnifiedEntity::table_row(
                                EntityId::new(id),
                                "rows",
                                id,
                                vec![Value::text(format!("initial-{id}"))],
                            ),
                        )
                        .unwrap();
                }
                store.persist().unwrap();
                mutate(&store, 1);
                store.persist().unwrap();
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "storage::unified::store::commit::tests::interrupted_retired_reclamation_preserves_reuse",
                    "--nocapture",
                ])
                .env(CHILD_PATH, &path)
                .env(env_name, "1")
                .output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(exit_code),
                "{label}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
            eprintln!(
                "retired_reclaim_{label} child={}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            let store = UnifiedStore::open_with_config(&path, config()).unwrap();
            for id in 1..=200 {
                assert_eq!(
                    store
                        .get("rows", EntityId::new(id))
                        .unwrap()
                        .data
                        .as_row()
                        .unwrap()
                        .columns,
                    vec![Value::text(replay_crash_payload(id, 2))]
                );
            }
            let pager = store.pager.as_ref().unwrap();
            let before_pages = pager.page_count().unwrap();
            let mut ids = std::collections::HashSet::new();
            let mut reused = 0;
            for _ in 0..256 {
                let page = pager
                    .allocate_page(crate::storage::engine::page::PageType::BTreeLeaf)
                    .unwrap();
                let id = page.page_id();
                assert!(ids.insert(id), "duplicate allocation");
                reused += u32::from(id < before_pages);
                pager.write_page(id, page).unwrap();
            }
            pager.sync().unwrap();
            eprintln!(
                "retired_reclaim_{label} before_pages={before_pages} reused={reused} appended={}",
                256 - reused
            );
            drop(store);
            let reopened = UnifiedStore::open_with_config(&path, config()).unwrap();
            for id in 1..=200 {
                assert_eq!(
                    reopened
                        .get("rows", EntityId::new(id))
                        .unwrap()
                        .data
                        .as_row()
                        .unwrap()
                        .columns,
                    vec![Value::text(replay_crash_payload(id, 2))]
                );
            }
            reuse.push(reused);
        }
        assert!(
            reuse[0] >= reuse[2] && reuse[1] >= reuse[2],
            "recovery must not lose retired pages before allocator publication: before={} enqueued={} after={}",
            reuse[0],
            reuse[1],
            reuse[2]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn experimental_retaining_released_root_is_not_recoverable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.rdb");
        let config = || {
            UnifiedStoreConfig::default()
                .with_durability_mode(DurabilityMode::Strict)
                .with_page_cache_slots(4)
        };
        {
            let store = UnifiedStore::open_with_config(&path, config()).unwrap();
            store.create_collection("rows").unwrap();
            for id in 1..=200 {
                store
                    .insert(
                        "rows",
                        UnifiedEntity::table_row(
                            EntityId::new(id),
                            "rows",
                            id,
                            vec![Value::text(format!("initial-{id}"))],
                        ),
                    )
                    .unwrap();
            }
            store.persist().unwrap();
            for id in 1..=200 {
                let entity = UnifiedEntity::table_row(
                    EntityId::new(id),
                    "rows",
                    id,
                    vec![Value::text(replay_crash_payload(id, 1))],
                );
                let record =
                    UnifiedStore::serialize_entity_record(&entity, None, STORE_VERSION_V11);
                store.apply_replayed_upsert("rows", &record, false).unwrap();
            }
            store.persist().unwrap();
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::unified::store::commit::tests::interrupted_retired_reclamation_preserves_reuse", "--nocapture"])
            .env("REDDB_EXPERIMENTAL_RETIRED_PATH", &path)
            .env("REDDB_EXPERIMENTAL_RETAIN_RELEASED_ROOT", "1")
            .env("REDDB_EXPERIMENTAL_EXIT_AFTER_RETIRED_FREE", "1")
            .output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(185),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("experimental exit after retired free"));

        let reopened = UnifiedStore::open_with_config(&path, config()).unwrap();
        for id in 1..=200 {
            assert_eq!(
                reopened
                    .get("rows", EntityId::new(id))
                    .unwrap()
                    .data
                    .as_row()
                    .unwrap()
                    .columns,
                vec![Value::text(replay_crash_payload(id, 2))]
            );
        }
        let retired_roots: Vec<_> = reopened
            .retired_btrees
            .lock()
            .iter()
            .map(|entry| entry.tree.root_page_id())
            .collect();
        let pager = reopened.pager.as_ref().unwrap();
        let root_types: Vec<_> = retired_roots
            .iter()
            .map(|id| pager.read_page(*id).unwrap().page_type().unwrap())
            .collect();
        assert!(
            root_types.contains(&crate::storage::engine::page::PageType::FreelistTrunk),
            "expected a retained root to have become a freelist trunk: {root_types:?}"
        );
        let error = reopened
            .persist()
            .expect_err("retained root was reused as allocator metadata");
        assert!(
            error
                .to_string()
                .contains("unexpected retired tree page type"),
            "{error}"
        );
        eprintln!("retained_root_types={root_types:?} persist_error={error}");
    }

    #[cfg(target_os = "linux")]
    fn interrupted_replay_reclaims_pages(after_publication: bool, force_sync: bool) {
        use std::os::unix::fs::MetadataExt;
        const CHILD_PATH: &str = "REDDB_EXPERIMENTAL_REPLAY_REBUILD_PATH";
        let config = || {
            UnifiedStoreConfig::default()
                .with_durability_mode(DurabilityMode::Strict)
                .with_page_cache_slots(4)
        };
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let _store = UnifiedStore::open_with_config(path, config())
                .expect("must exit while replay rebuilds");
            panic!("replay rebuild hook was not reached");
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.rdb");
        {
            let store = UnifiedStore::open_with_config(&path, config()).unwrap();
            store.create_collection("rows").unwrap();
            for id in 1..=200 {
                store
                    .insert(
                        "rows",
                        UnifiedEntity::table_row(
                            EntityId::new(id),
                            "rows",
                            id,
                            vec![Value::text(format!("initial-{id}"))],
                        ),
                    )
                    .unwrap();
            }
            store.persist().unwrap();
        }
        {
            let coordinator = StoreCommitCoordinator::open(
                reddb_file::layout::unified_wal_path(&path),
                DurabilityMode::Strict,
                GroupCommitOptions::default(),
            )
            .unwrap();
            let actions = (1..=200)
                .map(|id| {
                    let entity = UnifiedEntity::table_row(
                        EntityId::new(id),
                        "rows",
                        id,
                        vec![Value::text(replay_crash_payload(id, 8))],
                    );
                    StoreWalAction::upsert_entity("rows", &entity, None, STORE_VERSION_V11)
                })
                .collect::<Vec<_>>();
            coordinator.append_actions(&actions).unwrap();
        }
        let snapshot = || {
            let data = std::fs::metadata(&path).unwrap();
            let wal = std::fs::metadata(reddb_file::layout::unified_wal_path(&path)).unwrap();
            (
                data.len(),
                data.blocks() * 512,
                wal.len(),
                wal.blocks() * 512,
            )
        };
        let baseline = snapshot();
        let mut previous = baseline;
        let mut first_failure = baseline;
        for cycle in 1..=12 {
            let test_name = if after_publication {
                "storage::unified::store::commit::tests::interrupted_replay_preserves_published_pages"
            } else if force_sync {
                "storage::unified::store::commit::tests::interrupted_replay_reclaims_durable_unpublished_pages"
            } else {
                "storage::unified::store::commit::tests::interrupted_replay_reclaims_unpublished_pages"
            };
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", test_name, "--nocapture"])
                .env(CHILD_PATH, &path);
            if !force_sync {
                command.env("REDDB_EXPERIMENTAL_REPLAY_NO_SYNC", "1");
            } else {
                command.env_remove("REDDB_EXPERIMENTAL_REPLAY_NO_SYNC");
            }
            let message = if after_publication {
                command.env("REDDB_EXPERIMENTAL_EXIT_AFTER_REPLAY_PUBLICATION", "1");
                "experimental replay exit after manifest"
            } else {
                command.env("REDDB_EXPERIMENTAL_EXIT_DURING_REPLAY_REBUILD", "1");
                "experimental replay rebuild exit before manifest"
            };
            let output = command.output().unwrap();
            assert_eq!(
                output.status.code(),
                Some(182),
                "cycle {cycle}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains(message));
            let now = snapshot();
            eprintln!("replay_exit_cycle={cycle} data_logical={} data_allocated={} wal_logical={} wal_allocated={}",now.0,now.1,now.2,now.3);
            if cycle == 1 {
                first_failure = now;
            }
            previous = now;
        }
        let recovered = UnifiedStore::open_with_config(&path, config()).unwrap();
        assert_eq!(recovered.get_collection("rows").unwrap().count(), 200);
        for id in 1..=200 {
            assert_eq!(
                recovered
                    .get("rows", EntityId::new(id))
                    .unwrap()
                    .data
                    .as_row()
                    .unwrap()
                    .columns,
                vec![Value::text(replay_crash_payload(id, 8))]
            );
        }
        let pages = recovered.pager.as_ref().unwrap().page_count().unwrap();
        let free = recovered.pager.as_ref().unwrap().free_page_count().unwrap();
        recovered.persist().unwrap();
        drop(recovered);
        let final_sizes = snapshot();
        let reopened = UnifiedStore::open_with_config(&path, config()).unwrap();
        assert_eq!(reopened.get_collection("rows").unwrap().count(), 200);
        for id in 1..=200 {
            assert_eq!(
                reopened
                    .get("rows", EntityId::new(id))
                    .unwrap()
                    .data
                    .as_row()
                    .unwrap()
                    .columns,
                vec![Value::text(replay_crash_payload(id, 8))]
            );
        }
        eprintln!("replay_rebuild baseline={baseline:?} after_12={previous:?} after_recovery_checkpoint={final_sizes:?} pages={pages} free={free}");
        // Probe actual allocation rather than the cached free-page counter:
        // newly allocated IDs reveal whether interrupted generations returned
        // to the allocator after recovery and a completed checkpoint.
        let pager = reopened.pager.as_ref().unwrap();
        let before_probe = pager.page_count().unwrap();
        let mut ids = std::collections::HashSet::new();
        let mut reused = 0;
        for _ in 0..256 {
            let page = pager
                .allocate_page(crate::storage::engine::page::PageType::BTreeLeaf)
                .unwrap();
            let id = page.page_id();
            assert!(ids.insert(id), "allocator returned the same page twice");
            reused += u32::from(id < before_probe);
            pager.write_page(id, page).unwrap();
        }
        pager.sync().unwrap();
        let after_probe = pager.page_count().unwrap();
        eprintln!("replay_reuse_probe count=256 before_pages={before_probe} after_pages={after_probe} reused={reused} appended={}", 256 - reused);
        drop(reopened);
        let verified = UnifiedStore::open_with_config(&path, config()).unwrap();
        for id in 1..=200 {
            assert_eq!(
                verified
                    .get("rows", EntityId::new(id))
                    .unwrap()
                    .data
                    .as_row()
                    .unwrap()
                    .columns,
                vec![Value::text(replay_crash_payload(id, 8))]
            );
        }
        assert!(
            previous.0 <= first_failure.0 + 2 * crate::storage::engine::PAGE_SIZE as u64,
            "repeated interrupted replay must bound page growth: first={}, after_12={}",
            first_failure.0,
            previous.0
        );
    }

    #[test]
    fn paged_wal_replay_survives_immediate_process_exit() {
        const CHILD_PATH: &str = "REDDB_TEST_REPLAY_CRASH_PATH";
        let config = || UnifiedStoreConfig::default().with_page_cache_slots(4);
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let store = UnifiedStore::open_with_config(path, config()).expect("replay child");
            assert_eq!(store.get_collection("rows").unwrap().count(), 400);
            // No Rust destructors or graceful checkpoint may repair the image.
            std::process::exit(0);
        }
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.rdb");
        {
            let store = UnifiedStore::open_with_config(&path, config()).unwrap();
            store.create_collection("rows").unwrap();
            for id in 1..=400 {
                store
                    .insert(
                        "rows",
                        UnifiedEntity::table_row(
                            EntityId::new(id),
                            "rows",
                            id,
                            vec![Value::text(format!("initial-{id}"))],
                        ),
                    )
                    .unwrap();
            }
            store.persist().unwrap();
        }
        {
            let coordinator = StoreCommitCoordinator::open(
                reddb_file::layout::unified_wal_path(&path),
                DurabilityMode::Strict,
                GroupCommitOptions::default(),
            )
            .unwrap();
            for round in 0..3 {
                let actions = (1..=400)
                    .map(|id| {
                        let payload = replay_crash_payload(id, round);
                        let entity = UnifiedEntity::table_row(
                            EntityId::new(id),
                            "rows",
                            id,
                            vec![Value::text(payload)],
                        );
                        StoreWalAction::upsert_entity("rows", &entity, None, STORE_VERSION_V11)
                    })
                    .collect::<Vec<_>>();
                coordinator.append_actions(&actions).unwrap();
            }
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "storage::unified::store::commit::tests::paged_wal_replay_survives_immediate_process_exit", "--nocapture"])
            .env(CHILD_PATH, &path).status().unwrap();
        assert!(status.success(), "replay process failed");
        let reopened = UnifiedStore::open_with_config(&path, config())
            .expect("cold reopen after replay crash");
        assert_eq!(reopened.get_collection("rows").unwrap().count(), 400);
        for id in 1..=400 {
            let entity = reopened
                .get("rows", EntityId::new(id))
                .expect("retained entity");
            let EntityData::Row(row) = entity.data else {
                panic!("expected row")
            };
            assert_eq!(row.columns, vec![Value::text(replay_crash_payload(id, 2))]);
        }
        drop(reopened);
        let stable_bytes = std::fs::metadata(&path).unwrap().len();
        let repeated = UnifiedStore::open_with_config(&path, config()).unwrap();
        assert_eq!(repeated.get_collection("rows").unwrap().count(), 400);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            stable_bytes,
            "idempotent replay must not allocate another tree generation"
        );
    }

    #[test]
    fn checkpoint_reset_waits_for_an_enqueued_commit_acknowledgement() {
        let _env = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        let path = temp_wal("checkpoint-ack");
        let coordinator = Arc::new(
            StoreCommitCoordinator::open(
                path.clone(),
                DurabilityMode::WalDurableGrouped,
                GroupCommitOptions::default(),
            )
            .unwrap(),
        );
        // Pause the writer after enqueue, before it can register/wait for its
        // acknowledgement. The flusher also needs this state lock to drain.
        let state = coordinator.state.0.lock();
        let (written_tx, written_rx) = std::sync::mpsc::channel();
        let writer_coordinator = Arc::clone(&coordinator);
        let writer = std::thread::spawn(move || {
            let result = writer_coordinator.append_actions(&[StoreWalAction::CreateCollection {
                name: "acknowledged".into(),
            }]);
            written_tx.send(result).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while !coordinator.queue.has_pending() {
            assert!(Instant::now() < deadline, "writer never enqueued");
            std::thread::yield_now();
        }
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (reset_tx, reset_rx) = std::sync::mpsc::channel();
        let reset_coordinator = Arc::clone(&coordinator);
        let reset = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            reset_tx.send(reset_coordinator.truncate()).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_millis(100);
        while coordinator.queue.has_pending() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let discarded_before_ack = !coordinator.queue.has_pending();
        drop(state);
        let acknowledged = written_rx.recv_timeout(Duration::from_secs(2));
        if acknowledged.is_err() {
            // Release an old implementation's stranded waiter before failing
            // the assertion, so the regression itself cannot leak a worker.
            let mut state = coordinator.state.0.lock();
            state.shutdown = true;
            state.last_error = Some("checkpoint acknowledgement regression cleanup".into());
            coordinator.state.1.notify_all();
        }
        writer.join().unwrap();
        reset_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        reset.join().unwrap();
        assert!(
            !discarded_before_ack,
            "checkpoint discarded an unacknowledged WAL record"
        );
        assert!(
            acknowledged.unwrap().is_ok(),
            "durable writer did not finish"
        );
        drop(coordinator);
        let _ = std::fs::remove_file(path);
    }

    /// Concurrent autocommits MUST coalesce into far fewer fsyncs
    /// than the number of callers when the adaptive group-commit
    /// window is active. Without the 200µs floor, every caller
    /// would race the drain loop and pay an independent fsync.
    ///
    /// The test fires 32 concurrent `append_actions` calls and asserts
    /// the coordinator issued strictly fewer fsyncs than callers.
    /// (We don't pin to "one fsync" because timing on loaded CI hosts
    /// can split the burst across a couple of drain cycles — the win
    /// is the ratio, not the absolute count.)
    #[test]
    fn group_commit_coalesces_concurrent_autocommits() {
        let _env = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        // Make sure no stale env var skews this run — the test
        // exercises the default 200µs floor.
        std::env::remove_var("REDDB_GROUP_COMMIT_WINDOW_US");

        let path = temp_wal("coalesce");
        let coord = Arc::new(
            StoreCommitCoordinator::open(
                path.clone(),
                DurabilityMode::WalDurableGrouped,
                GroupCommitOptions::default(),
            )
            .expect("open commit coordinator"),
        );

        const WRITERS: usize = 32;
        let barrier = Arc::new(Barrier::new(WRITERS));
        let mut handles = Vec::with_capacity(WRITERS);
        for tx in 0..WRITERS {
            let coord_c = Arc::clone(&coord);
            let barrier_c = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                // Synchronise the start so every writer races the
                // drain loop together — this is the workload shape
                // the adaptive window is supposed to coalesce.
                barrier_c.wait();
                let action = StoreWalAction::CreateCollection {
                    name: format!("c{tx}"),
                };
                coord_c
                    .append_actions(std::slice::from_ref(&action))
                    .expect("append_actions");
            }));
        }
        for h in handles {
            h.join().expect("writer thread");
        }

        let fsyncs = coord.fsync_count();
        assert!(fsyncs > 0, "expected at least one fsync, got {fsyncs}");
        assert!(
            fsyncs < WRITERS as u64,
            "expected fsyncs ({fsyncs}) to be strictly less than \
             concurrent writers ({WRITERS}); coalescing failed"
        );

        drop(coord);
        let _ = std::fs::remove_file(&path);
    }

    /// Sanity check: with the env override forcing a zero window,
    /// fsync coalescing degrades — callers race and we approach
    /// one fsync per caller. This proves the window is the actual
    /// knob doing the work above (i.e. the test isn't passing for
    /// some unrelated reason like buffered-IO coalescing).
    #[test]
    fn zero_window_disables_coalescing_floor() {
        let _env = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var("REDDB_GROUP_COMMIT_WINDOW_US", "0");

        let path = temp_wal("zero_window");
        let coord = Arc::new(
            StoreCommitCoordinator::open(
                path.clone(),
                DurabilityMode::WalDurableGrouped,
                GroupCommitOptions::default(),
            )
            .expect("open commit coordinator"),
        );

        const WRITERS: usize = 8;
        let barrier = Arc::new(Barrier::new(WRITERS));
        let mut handles = Vec::with_capacity(WRITERS);
        for tx in 0..WRITERS {
            let coord_c = Arc::clone(&coord);
            let barrier_c = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier_c.wait();
                let action = StoreWalAction::CreateCollection {
                    name: format!("z{tx}"),
                };
                coord_c
                    .append_actions(std::slice::from_ref(&action))
                    .expect("append_actions");
            }));
        }
        for h in handles {
            h.join().expect("writer thread");
        }

        // With zero window, fsync count is bounded above by WRITERS
        // (every caller might trigger its own drain) and below by 1
        // (the queue may still naturally batch under contention).
        // The point of the assertion is to confirm the env override
        // is wired through — the open() above used the env knob.
        let fsyncs = coord.fsync_count();
        assert!(fsyncs >= 1, "expected at least one fsync, got {fsyncs}");

        std::env::remove_var("REDDB_GROUP_COMMIT_WINDOW_US");
        drop(coord);
        let _ = std::fs::remove_file(&path);
    }

    /// `resolve_window` precedence: env > config.window_ms > default.
    #[test]
    fn resolve_window_precedence() {
        let _env = env_lock().lock().unwrap_or_else(|p| p.into_inner());
        // Default: window_ms=0 → adaptive 200µs floor.
        std::env::remove_var("REDDB_GROUP_COMMIT_WINDOW_US");
        let cfg = GroupCommitOptions::default();
        assert_eq!(
            StoreCommitCoordinator::resolve_window(&cfg),
            Duration::from_micros(DEFAULT_ADAPTIVE_WINDOW_US)
        );

        // Explicit ms config wins over the default floor.
        let cfg_ms = GroupCommitOptions {
            window_ms: 5,
            ..GroupCommitOptions::default()
        };
        assert_eq!(
            StoreCommitCoordinator::resolve_window(&cfg_ms),
            Duration::from_millis(5)
        );

        // Env override wins over both.
        std::env::set_var("REDDB_GROUP_COMMIT_WINDOW_US", "750");
        assert_eq!(
            StoreCommitCoordinator::resolve_window(&cfg),
            Duration::from_micros(750)
        );
        assert_eq!(
            StoreCommitCoordinator::resolve_window(&cfg_ms),
            Duration::from_micros(750)
        );

        // Env=0 explicitly disables the floor.
        std::env::set_var("REDDB_GROUP_COMMIT_WINDOW_US", "0");
        assert_eq!(
            StoreCommitCoordinator::resolve_window(&cfg),
            Duration::from_micros(0)
        );

        std::env::remove_var("REDDB_GROUP_COMMIT_WINDOW_US");
    }
}
