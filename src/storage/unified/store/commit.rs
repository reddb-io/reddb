use super::*;
use crate::api::DurabilityMode;
use crate::storage::wal::{WalReader, WalRecord, WalWriter};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

static NEXT_STORE_TX_ID: AtomicU64 = AtomicU64::new(1);

const STORE_WAL_VERSION: u8 = 1;

#[derive(Debug, Clone)]
pub(crate) enum StoreWalAction {
    CreateCollection { name: String },
    DropCollection { name: String },
    UpsertEntityRecord { collection: String, record: Vec<u8> },
    DeleteEntityRecord { collection: String, entity_id: u64 },
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

pub(crate) struct StoreCommitCoordinator {
    mode: DurabilityMode,
    config: crate::api::GroupCommitOptions,
    wal_path: PathBuf,
    wal: Arc<Mutex<WalWriter>>,
    state: Arc<(Mutex<CommitState>, Condvar)>,
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
        let wal = Arc::new(Mutex::new(wal));
        let state = Arc::new((
            Mutex::new(CommitState::new(initial_durable_lsn)),
            Condvar::new(),
        ));

        if matches!(
            mode,
            DurabilityMode::WalDurableGrouped | DurabilityMode::Async
        ) {
            let wal_bg = Arc::clone(&wal);
            let state_bg = Arc::clone(&state);
            let window = Duration::from_millis(config.window_ms.max(1));
            let max_statements = config.max_statements.max(1);
            let max_wal_bytes = config.max_wal_bytes.max(1);
            std::thread::spawn(move || {
                Self::run_group_commit_loop(
                    wal_bg,
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
            state,
        })
    }

    pub(crate) fn append_actions(&self, actions: &[StoreWalAction]) -> io::Result<()> {
        if actions.is_empty() {
            return Ok(());
        }

        let tx_id = NEXT_STORE_TX_ID.fetch_add(1, Ordering::SeqCst);
        let commit_lsn = {
            let mut wal = self
                .wal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            wal.append(&WalRecord::Begin { tx_id })?;
            let mut wal_bytes = 0u64;
            for action in actions {
                let payload = action.encode();
                wal_bytes = wal_bytes.saturating_add(payload.len() as u64);
                wal.append(&WalRecord::PageWrite {
                    tx_id,
                    page_id: 0,
                    data: payload,
                })?;
            }
            wal.append(&WalRecord::Commit { tx_id })?;
            let commit_lsn = wal.current_lsn();
            drop(wal);
            self.wait_until_durable(commit_lsn, wal_bytes)?;
            commit_lsn
        };

        let _ = commit_lsn;
        Ok(())
    }

    pub(crate) fn force_sync(&self) -> io::Result<()> {
        {
            let mut wal = self
                .wal
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            wal.sync()?;
            let durable = wal.durable_lsn();
            drop(wal);
            let (state_lock, cond) = &*self.state;
            let mut state = state_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        let mut wal = self
            .wal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        wal.truncate()?;
        let durable = wal.durable_lsn();
        drop(wal);

        let (state_lock, cond) = &*self.state;
        let mut state = state_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
                let mut state = state_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state.pending_target_lsn = state.pending_target_lsn.max(target_lsn);
                state.pending_statements = state.pending_statements.saturating_add(1);
                state.pending_wal_bytes = state.pending_wal_bytes.saturating_add(wal_bytes);
                state.first_pending_at.get_or_insert_with(Instant::now);
                cond.notify_all();
                Ok(())
            }
            DurabilityMode::WalDurableGrouped => {
                let (state_lock, cond) = &*self.state;
                let mut state = state_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
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
                    state = cond
                        .wait(state)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }
            }
        }
    }

    fn run_group_commit_loop(
        wal: Arc<Mutex<WalWriter>>,
        state: Arc<(Mutex<CommitState>, Condvar)>,
        window: Duration,
        max_statements: usize,
        max_wal_bytes: u64,
    ) {
        let (state_lock, cond) = &*state;
        loop {
            let target_lsn = {
                let mut guard = state_lock
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());

                while !guard.shutdown && guard.pending_target_lsn <= guard.durable_lsn {
                    guard = cond
                        .wait(guard)
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                }

                if guard.shutdown {
                    return;
                }

                let immediate = guard.pending_statements >= max_statements
                    || guard.pending_wal_bytes >= max_wal_bytes;

                if !immediate {
                    let deadline = guard.first_pending_at.unwrap_or_else(Instant::now) + window;
                    let now = Instant::now();
                    if now < deadline {
                        let timeout = deadline.saturating_duration_since(now);
                        let (next_guard, _) = cond
                            .wait_timeout(guard, timeout)
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        guard = next_guard;
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

            let sync_result = {
                let mut wal = wal.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                wal.sync().map(|_| wal.durable_lsn())
            };

            let mut guard = state_lock
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match sync_result {
                Ok(durable_lsn) => {
                    guard.durable_lsn = durable_lsn.max(target_lsn);
                    guard.pending_statements = 0;
                    guard.pending_wal_bytes = 0;
                    guard.first_pending_at = None;
                    guard.last_error = None;
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
        if let Ok(mut state) = state_lock.lock() {
            state.shutdown = true;
            cond.notify_all();
        }
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
                .or_insert_with(|| BTree::new(Arc::clone(pager)));
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
