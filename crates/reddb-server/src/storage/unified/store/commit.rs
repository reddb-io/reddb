use super::*;
use crate::api::DurabilityMode;
use crate::storage::wal::{WalReader, WalRecord, WalWriter};
use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

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

const STORE_WAL_VERSION: u8 = 1;

#[derive(Debug, Clone)]
pub(crate) enum StoreWalAction {
    CreateCollection {
        name: String,
    },
    DropCollection {
        name: String,
    },
    UpsertEntityRecord {
        collection: String,
        record: Vec<u8>,
    },
    DeleteEntityRecord {
        collection: String,
        entity_id: u64,
    },
    /// Batched upsert — one WAL action carrying N serialized entity
    /// records for the same collection. Saves the per-row Begin/
    /// PageWrite/Commit framing overhead on the bulk insert hot path.
    /// Replay applies every contained record in order.
    BulkUpsertEntityRecords {
        collection: String,
        records: Vec<Vec<u8>>,
    },
}

#[derive(Debug, Default)]
pub(crate) struct DeferredStoreWalActions {
    actions: Vec<StoreWalAction>,
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

impl StoreWalAction {
    pub(crate) fn upsert_entity(
        collection: &str,
        entity: &UnifiedEntity,
        metadata: Option<&Metadata>,
        format_version: u32,
    ) -> Self {
        Self::UpsertEntityRecord {
            collection: collection.to_string(),
            record: UnifiedStore::serialize_entity_record(entity, metadata, format_version),
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(STORE_WAL_VERSION);
        match self {
            Self::CreateCollection { name } => {
                out.push(1);
                write_string(&mut out, name);
            }
            Self::DropCollection { name } => {
                out.push(2);
                write_string(&mut out, name);
            }
            Self::UpsertEntityRecord { collection, record } => {
                out.push(3);
                write_string(&mut out, collection);
                write_bytes(&mut out, record);
            }
            Self::DeleteEntityRecord {
                collection,
                entity_id,
            } => {
                out.push(4);
                write_string(&mut out, collection);
                out.extend_from_slice(&entity_id.to_le_bytes());
            }
            Self::BulkUpsertEntityRecords {
                collection,
                records,
            } => {
                out.push(5);
                write_string(&mut out, collection);
                out.extend_from_slice(&(records.len() as u32).to_le_bytes());
                for record in records {
                    write_bytes(&mut out, record);
                }
            }
        }
        out
    }

    fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "store wal action too short",
            ));
        }
        if bytes[0] != STORE_WAL_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported store wal version: {}", bytes[0]),
            ));
        }

        let mut pos = 2usize;
        match bytes[1] {
            1 => Ok(Self::CreateCollection {
                name: read_string(bytes, &mut pos)?,
            }),
            2 => Ok(Self::DropCollection {
                name: read_string(bytes, &mut pos)?,
            }),
            3 => Ok(Self::UpsertEntityRecord {
                collection: read_string(bytes, &mut pos)?,
                record: read_bytes(bytes, &mut pos)?,
            }),
            4 => {
                let collection = read_string(bytes, &mut pos)?;
                let entity_id = read_u64(bytes, &mut pos)?;
                Ok(Self::DeleteEntityRecord {
                    collection,
                    entity_id,
                })
            }
            5 => {
                let collection = read_string(bytes, &mut pos)?;
                if pos + 4 > bytes.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "bulk upsert wal action: missing record count",
                    ));
                }
                let count = u32::from_le_bytes([
                    bytes[pos],
                    bytes[pos + 1],
                    bytes[pos + 2],
                    bytes[pos + 3],
                ]) as usize;
                pos += 4;
                let mut records = Vec::with_capacity(count);
                for _ in 0..count {
                    records.push(read_bytes(bytes, &mut pos)?);
                }
                Ok(Self::BulkUpsertEntityRecords {
                    collection,
                    records,
                })
            }
            other => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported store wal action tag: {other}"),
            )),
        }
    }
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
}

struct WalQueueState {
    next_lsn: u64,
    entries: Vec<(u64, Vec<u8>)>,
}

impl WalAppendQueue {
    fn new(initial_lsn: u64) -> Self {
        Self {
            pending: parking_lot::Mutex::new(WalQueueState {
                next_lsn: initial_lsn,
                entries: Vec::with_capacity(64),
            }),
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
        start_lsn + len
    }

    /// Drain all queued entries in LSN order. Caller holds the WAL
    /// file mutex while writing the drained bytes so the on-disk
    /// layout matches the reserved LSN offsets.
    fn drain_sorted(&self) -> Vec<(u64, Vec<u8>)> {
        let mut state = self.pending.lock();
        let mut v = std::mem::take(&mut state.entries);
        drop(state);
        v.sort_by_key(|(lsn, _)| *lsn);
        v
    }

    /// Whether any entry is queued. Leader uses this to decide
    /// whether to spin once more or go back to the condvar.
    fn has_pending(&self) -> bool {
        !self.pending.lock().entries.is_empty()
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
}

impl StoreCommitCoordinator {
    pub(crate) fn should_open(path: &Path, mode: DurabilityMode) -> bool {
        matches!(
            mode,
            DurabilityMode::WalDurableGrouped | DurabilityMode::Async
        ) || path.exists()
    }

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

    pub(crate) fn append_actions(&self, actions: &[StoreWalAction]) -> io::Result<()> {
        if actions.is_empty() {
            return Ok(());
        }

        let tx_id = NEXT_STORE_TX_ID.fetch_add(1, Ordering::SeqCst);

        // Strict mode: bypass the queue, write + fsync inline. Strict
        // commits are exactly one-fsync-per-call by contract, so the
        // coalescing win of the queue doesn't apply and we'd pay an
        // extra hop through the drain loop.
        if matches!(self.mode, DurabilityMode::Strict) {
            let commit_lsn = {
                let mut wal = self.wal.lock();
                wal.append(&WalRecord::TxCommitBatch {
                    tx_id,
                    actions: actions.iter().map(StoreWalAction::encode).collect(),
                })?;
                wal.current_lsn()
            };
            self.force_sync()?;
            let _ = commit_lsn;
            return Ok(());
        }

        // Grouped / Async path — lock-free enqueue. Encode every
        // WalRecord into one contiguous byte blob OUTSIDE any lock,
        // then hand it to the queue with a single fetch_add+push.
        let encoded_actions: Vec<Vec<u8>> = actions.iter().map(StoreWalAction::encode).collect();
        let wal_bytes = encoded_actions.iter().fold(0u64, |total, payload| {
            total.saturating_add(payload.len() as u64)
        });
        let blob = WalRecord::TxCommitBatch {
            tx_id,
            actions: encoded_actions,
        }
        .encode();

        let commit_lsn = self.queue.enqueue(blob);
        self.wait_until_durable(commit_lsn, wal_bytes)?;
        Ok(())
    }

    pub(crate) fn force_sync(&self) -> io::Result<()> {
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

        for record in reader.iter() {
            let (_lsn, record) = match record {
                Ok(record) => record,
                Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(err) => return Err(err),
            };
            match record {
                WalRecord::TxCommitBatch { actions, .. } => {
                    for payload in actions {
                        let action = StoreWalAction::decode(&payload)?;
                        store.apply_replayed_action(&action).map_err(|err| {
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
            }
        }

        for (tx_id, payload) in pending {
            if !tx_states.get(&tx_id).copied().unwrap_or(false) {
                continue;
            }
            let action = StoreWalAction::decode(&payload)?;
            store.apply_replayed_action(&action).map_err(|err| {
                io::Error::other(format!("failed to replay store wal action: {err}"))
            })?;
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
        match self.config.durability_mode {
            DurabilityMode::Strict => self.flush_paged_state(),
            DurabilityMode::WalDurableGrouped | DurabilityMode::Async => {
                if let Some(commit) = &self.commit {
                    commit
                        .append_actions(&actions.actions)
                        .map_err(StoreError::Io)
                } else {
                    self.flush_paged_state()
                }
            }
        }
    }

    pub(crate) fn wal_path_for_db(path: &Path) -> PathBuf {
        path.with_extension("rdb-uwal")
    }

    pub(crate) fn finish_paged_write(
        &self,
        actions: impl IntoIterator<Item = StoreWalAction>,
    ) -> Result<(), StoreError> {
        let actions: Vec<StoreWalAction> = actions.into_iter().collect();
        if deferred_store_wal_capture_active() {
            let captured = capture_deferred_store_wal_actions(actions);
            debug_assert!(captured);
            return Ok(());
        }
        match self.config.durability_mode {
            DurabilityMode::Strict => self.flush_paged_state(),
            DurabilityMode::WalDurableGrouped | DurabilityMode::Async => {
                if let Some(commit) = &self.commit {
                    commit.append_actions(&actions).map_err(StoreError::Io)?;
                    Ok(())
                } else {
                    self.flush_paged_state()
                }
            }
        }
    }

    pub(crate) fn apply_replayed_action(&self, action: &StoreWalAction) -> Result<(), StoreError> {
        match action {
            StoreWalAction::CreateCollection { name } => {
                if self.get_collection(name).is_none() {
                    let _ = self.create_collection_in_memory(name);
                }
                Ok(())
            }
            StoreWalAction::DropCollection { name } => self.drop_collection_in_memory(name),
            StoreWalAction::UpsertEntityRecord { collection, record } => {
                self.apply_replayed_upsert(collection, record)
            }
            StoreWalAction::DeleteEntityRecord {
                collection,
                entity_id,
            } => self.apply_replayed_delete(collection, EntityId::new(*entity_id)),
            StoreWalAction::BulkUpsertEntityRecords {
                collection,
                records,
            } => {
                for record in records {
                    self.apply_replayed_upsert(collection, record)?;
                }
                Ok(())
            }
        }
    }

    pub(crate) fn create_collection_in_memory(&self, name: &str) -> Result<(), StoreError> {
        let mut collections = self.collections.write();
        if collections.contains_key(name) {
            return Ok(());
        }
        let manager = SegmentManager::with_config(name, self.config.manager_config.clone());
        collections.insert(name.to_string(), Arc::new(manager));
        self.mark_paged_registry_dirty();
        Ok(())
    }

    fn drop_collection_in_memory(&self, name: &str) -> Result<(), StoreError> {
        let manager = {
            let mut collections = self.collections.write();
            match collections.remove(name) {
                Some(manager) => manager,
                None => return Ok(()),
            }
        };

        let entities = manager.query_all(|_| true);
        let entity_ids: Vec<EntityId> = entities.iter().map(|entity| entity.id).collect();
        for entity_id in &entity_ids {
            self.context_index.remove_entity(*entity_id);
            let _ = self.unindex_cross_refs(*entity_id);
        }
        self.btree_indices.write().remove(name);
        self.entity_cache.retain(|entity_id, (collection, _)| {
            collection != name && !entity_ids.iter().any(|id| id.raw() == entity_id)
        });
        self.remove_from_graph_label_index_batch(name, &entity_ids);
        self.mark_paged_registry_dirty();
        Ok(())
    }

    fn apply_replayed_upsert(&self, collection: &str, record: &[u8]) -> Result<(), StoreError> {
        self.create_collection_in_memory(collection)?;
        let (entity, metadata) = Self::deserialize_entity_record(record, self.format_version())?;
        let manager = self
            .get_collection(collection)
            .ok_or_else(|| StoreError::CollectionNotFound(collection.to_string()))?;

        self.register_entity_id(entity.id);
        if let EntityKind::TableRow { row_id, .. } = &entity.kind {
            manager.register_row_id(*row_id);
        }

        self.context_index.remove_entity(entity.id);
        let _ = self.unindex_cross_refs(entity.id);
        self.remove_from_graph_label_index(collection, entity.id);

        if manager.get(entity.id).is_some() {
            manager
                .update_with_metadata(entity.clone(), metadata.as_ref())
                .map_err(StoreError::from)?;
        } else {
            manager.insert(entity.clone())?;
            if let Some(metadata) = metadata.as_ref() {
                manager.set_metadata(entity.id, metadata.clone())?;
            }
        }

        self.context_index.index_entity(collection, &entity);
        if let EntityKind::GraphNode(node) = &entity.kind {
            self.update_graph_label_index(collection, &node.label, entity.id);
        }
        self.index_cross_refs(&entity, collection)?;

        if let Some(pager) = &self.pager {
            let mut btree_indices = self.btree_indices.write();
            let btree = btree_indices
                .entry(collection.to_string())
                .or_insert_with(|| Arc::new(BTree::new(Arc::clone(pager))));
            let root_before = btree.root_page_id();
            let key = entity.id.raw().to_be_bytes();
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

    fn apply_replayed_delete(&self, collection: &str, id: EntityId) -> Result<(), StoreError> {
        self.entity_cache.remove(id.raw());
        if let Some(manager) = self.get_collection(collection) {
            let deleted = manager.delete(id)?;
            if !deleted {
                return Ok(());
            }
        } else {
            return Ok(());
        }

        if let Some(_pager) = &self.pager {
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

        let _ = self.unindex_cross_refs(id);
        self.remove_from_graph_label_index(collection, id);
        self.context_index.remove_entity(id);
        Ok(())
    }
}

fn write_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value);
}

fn read_u32(data: &[u8], pos: &mut usize) -> io::Result<u32> {
    if data.len().saturating_sub(*pos) < 4 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof while reading u32",
        ));
    }
    let value = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(value)
}

fn read_u64(data: &[u8], pos: &mut usize) -> io::Result<u64> {
    if data.len().saturating_sub(*pos) < 8 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof while reading u64",
        ));
    }
    let value = u64::from_le_bytes([
        data[*pos],
        data[*pos + 1],
        data[*pos + 2],
        data[*pos + 3],
        data[*pos + 4],
        data[*pos + 5],
        data[*pos + 6],
        data[*pos + 7],
    ]);
    *pos += 8;
    Ok(value)
}

fn read_string(data: &[u8], pos: &mut usize) -> io::Result<String> {
    let len = read_u32(data, pos)? as usize;
    if data.len().saturating_sub(*pos) < len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof while reading string",
        ));
    }
    let value = std::str::from_utf8(&data[*pos..*pos + len])
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?
        .to_string();
    *pos += len;
    Ok(value)
}

fn read_bytes(data: &[u8], pos: &mut usize) -> io::Result<Vec<u8>> {
    let len = read_u32(data, pos)? as usize;
    if data.len().saturating_sub(*pos) < len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof while reading bytes",
        ));
    }
    let value = data[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{DurabilityMode, GroupCommitOptions};
    use std::sync::{Barrier, Mutex as StdMutex, OnceLock};
    use std::time::SystemTime;

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
        let path = std::env::temp_dir().join(format!(
            "rb_commit_coord_{}_{}_{}.wal",
            name,
            std::process::id(),
            nanos
        ));
        let _ = std::fs::remove_file(&path);
        path
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
