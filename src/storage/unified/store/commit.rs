use super::*;
use crate::api::DurabilityMode;
use crate::storage::wal::{WalReader, WalRecord, WalWriter};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

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
    CreateCollection { name: String },
    DropCollection { name: String },
    UpsertEntityRecord { collection: String, record: Vec<u8> },
    DeleteEntityRecord { collection: String, entity_id: u64 },
    /// Batched upsert — one WAL action carrying N serialized entity
    /// records for the same collection. Saves the per-row Begin/
    /// PageWrite/Commit framing overhead on the bulk insert hot path.
    /// Replay applies every contained record in order.
    BulkUpsertEntityRecords {
        collection: String,
        records: Vec<Vec<u8>>,
    },
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
                let count =
                    u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                        as usize;
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

        if matches!(
            mode,
            DurabilityMode::WalDurableGrouped | DurabilityMode::Async
        ) {
            let wal_bg = Arc::clone(&wal);
            let queue_bg = Arc::clone(&queue);
            let state_bg = Arc::clone(&state);
            // window_ms == 0 is a valid configuration meaning "no wait" —
            // flush on every wakeup. Under single-writer workloads the
            // batching window adds pure latency (no one to batch with)
            // while capping individual insert throughput at ~1000 ops/s
            // for window_ms=1. Concurrent writers still batch naturally
            // via the SegQueue drain in the coordinator below.
            let window = Duration::from_millis(config.window_ms);
            let max_statements = config.max_statements.max(1);
            let max_wal_bytes = config.max_wal_bytes.max(1);
            std::thread::spawn(move || {
                Self::run_group_commit_loop(
                    wal_bg,
                    queue_bg,
                    state_bg,
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
        })
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
                wal.append(&WalRecord::Begin { tx_id })?;
                for action in actions {
                    let payload = action.encode();
                    wal.append(&WalRecord::PageWrite {
                        tx_id,
                        page_id: 0,
                        data: payload,
                    })?;
                }
                wal.append(&WalRecord::Commit { tx_id })?;
                wal.current_lsn()
            };
            self.force_sync()?;
            let _ = commit_lsn;
            return Ok(());
        }

        // Grouped / Async path — lock-free enqueue. Encode every
        // WalRecord into one contiguous byte blob OUTSIDE any lock,
        // then hand it to the queue with a single fetch_add+push.
        let mut blob: Vec<u8> = Vec::with_capacity(64 + actions.len() * 128);
        blob.extend_from_slice(&WalRecord::Begin { tx_id }.encode());
        let mut wal_bytes = 0u64;
        for action in actions {
            let payload = action.encode();
            wal_bytes = wal_bytes.saturating_add(payload.len() as u64);
            blob.extend_from_slice(
                &WalRecord::PageWrite {
                    tx_id,
                    page_id: 0,
                    data: payload,
                }
                .encode(),
            );
        }
        blob.extend_from_slice(&WalRecord::Commit { tx_id }.encode());

        let commit_lsn = self.queue.enqueue(blob);
        self.wait_until_durable(commit_lsn, wal_bytes)?;
        Ok(())
    }

    pub(crate) fn force_sync(&self) -> io::Result<()> {
        {
            let mut wal = self.wal.lock();
            wal.sync()?;
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
        drop(wal);

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
            let (_lsn, record) = record?;
            match record {
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
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("failed to replay store wal action: {err}"),
                )
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
                    None => wal.sync().map(|_| wal.durable_lsn()),
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
    pub(crate) fn wal_path_for_db(path: &Path) -> PathBuf {
        path.with_extension("rdb-uwal")
    }

    pub(crate) fn finish_paged_write(
        &self,
        actions: impl IntoIterator<Item = StoreWalAction>,
    ) -> Result<(), StoreError> {
        let actions: Vec<StoreWalAction> = actions.into_iter().collect();
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
        self.entity_cache
            .write()
            .retain(|entity_id, (collection, _)| {
                collection != name && !entity_ids.iter().any(|id| id.raw() == *entity_id)
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
        self.entity_cache.write().remove(&id.raw());
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
