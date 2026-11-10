use super::commit::StoreWalAction;
use super::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize};

thread_local! {
    static BOOT_SYSTEM_INFO_UPDATE: Cell<bool> = const { Cell::new(false) };
}

const CACHE_COLLECTION: &str = "red.cache.blob_l2";
const EPOCH_KEY: &str = "__result_data_epoch_v1";

#[derive(Default)]
pub(super) struct ResultCacheEpochState {
    epoch: AtomicU64,
    id: AtomicU64,
    pending: AtomicUsize,
    poisoned: AtomicBool,
}

pub(super) struct ResultCacheEpochGuard {
    state: Arc<ResultCacheEpochState>,
    committed: bool,
}

impl ResultCacheEpochGuard {
    pub(super) fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for ResultCacheEpochGuard {
    fn drop(&mut self) {
        if !self.committed {
            self.state.poisoned.store(true, Ordering::SeqCst);
        }
        self.state.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

impl UnifiedStore {
    // Boot-only process telemetry changes on every restart but cannot affect
    // persisted user-table results. System-collection results are not saved.
    pub(crate) fn with_boot_system_info_update<T>(&self, f: impl FnOnce() -> T) -> T {
        struct Reset(bool);
        impl Drop for Reset {
            fn drop(&mut self) {
                BOOT_SYSTEM_INFO_UPDATE.with(|flag| flag.set(self.0));
            }
        }
        let previous = BOOT_SYSTEM_INFO_UPDATE.with(|flag| flag.replace(true));
        let _reset = Reset(previous);
        f()
    }

    fn read_result_cache_epoch_marker(&self) -> Result<Option<(EntityId, u64)>, StoreError> {
        let Some(manager) = self.get_collection(CACHE_COLLECTION) else {
            return Ok(None);
        };
        let row = manager.try_project_key_entity(EPOCH_KEY, None, true, usize::MAX, |entity| {
            (
                entity.id,
                entity
                    .data
                    .as_row()
                    .and_then(|row| row.get_field("value"))
                    .cloned(),
            )
        })?;
        match row {
            None => Ok(None),
            Some((id, Some(Value::UnsignedInteger(epoch)))) if epoch > 0 => Ok(Some((id, epoch))),
            Some(_) => Err(StoreError::Io(std::io::Error::other(
                "invalid result-cache epoch marker",
            ))),
        }
    }

    /// Load only after WAL replay, before any caller can mutate the store.
    pub(crate) fn load_result_cache_epoch(&self) -> Result<(), StoreError> {
        let _publication = self.vault_publication_lock.lock();
        if let Some((id, epoch)) = self.read_result_cache_epoch_marker()? {
            self.result_cache_epoch.id.store(id.raw(), Ordering::SeqCst);
            self.result_cache_epoch.epoch.store(epoch, Ordering::SeqCst);
        } else {
            self.result_cache_epoch.epoch.store(0, Ordering::SeqCst);
            self.result_cache_epoch.id.store(0, Ordering::SeqCst);
        }
        Ok(())
    }

    /// None fences caching while a publication is pending or its outcome is uncertain.
    pub(crate) fn result_cache_epoch(&self) -> Option<u64> {
        let state = &self.result_cache_epoch;
        if state.poisoned.load(Ordering::SeqCst) || state.pending.load(Ordering::SeqCst) != 0 {
            return None;
        }
        let epoch = state.epoch.load(Ordering::SeqCst);
        if state.poisoned.load(Ordering::SeqCst)
            || state.pending.load(Ordering::SeqCst) != 0
            || state.epoch.load(Ordering::SeqCst) != epoch
        {
            return None;
        }
        (epoch != 0).then_some(epoch)
    }

    pub(crate) fn result_cache_epoch_uninitialized(&self) -> bool {
        self.result_cache_epoch.epoch.load(Ordering::SeqCst) == 0
            && !self.result_cache_epoch.poisoned.load(Ordering::SeqCst)
            && self.result_cache_epoch.pending.load(Ordering::SeqCst) == 0
    }

    /// Activate epoch tracking before a result can be persisted. Cache rows
    /// written by older versions have another payload format and are ignored.
    pub(crate) fn ensure_result_cache_epoch(&self) -> Result<u64, StoreError> {
        if let Some(epoch) = self.result_cache_epoch() {
            return Ok(epoch);
        }
        if self.result_cache_epoch.epoch.load(Ordering::SeqCst) != 0
            || self.result_cache_epoch.poisoned.load(Ordering::SeqCst)
            || self.result_cache_epoch.pending.load(Ordering::SeqCst) != 0
        {
            return Err(StoreError::Io(std::io::Error::other(
                "result-cache epoch unavailable",
            )));
        }
        let _publication = self.vault_publication_lock.lock();
        if let Some(epoch) = self.result_cache_epoch() {
            return Ok(epoch);
        }
        if self.result_cache_epoch.epoch.load(Ordering::SeqCst) != 0
            || self.result_cache_epoch.poisoned.load(Ordering::SeqCst)
            || self.result_cache_epoch.pending.load(Ordering::SeqCst) != 0
        {
            return Err(StoreError::Io(std::io::Error::other(
                "result-cache epoch unavailable",
            )));
        }
        if self.is_read_only() {
            return Err(StoreError::Io(std::io::Error::other(
                "read-only result-cache epoch",
            )));
        }
        if let Some((id, epoch)) = self.read_result_cache_epoch_marker()? {
            self.result_cache_epoch.id.store(id.raw(), Ordering::SeqCst);
            self.result_cache_epoch.epoch.store(epoch, Ordering::SeqCst);
            return Ok(epoch);
        }
        let id = self.next_entity_id();
        let entity = Self::result_cache_epoch_entity(id, 1);
        self.publish_rows(super::row_publication::RowPublication {
            inserts: vec![(CACHE_COLLECTION.to_string(), entity, None)],
            deletes: Vec::new(),
        })?;
        self.sync_control_metadata()?;
        self.result_cache_epoch.id.store(id.raw(), Ordering::SeqCst);
        self.result_cache_epoch.epoch.store(1, Ordering::SeqCst);
        Ok(1)
    }

    fn result_cache_epoch_entity(id: EntityId, epoch: u64) -> UnifiedEntity {
        UnifiedEntity::new(
            id,
            EntityKind::TableRow {
                table: Arc::from(CACHE_COLLECTION),
                row_id: 0,
            },
            EntityData::Row(RowData::with_names(
                vec![Value::text(EPOCH_KEY), Value::UnsignedInteger(epoch)],
                vec!["key".into(), "value".into()],
            )),
        )
    }

    pub(super) fn note_in_memory_result_mutation(
        &self,
        collection: &str,
    ) -> Result<(), StoreError> {
        if self.result_cache_epoch.epoch.load(Ordering::SeqCst) == 0 {
            return Ok(());
        }
        let _publication = self.vault_publication_lock.lock();
        let mut actions = vec![StoreWalAction::CreateCollection {
            name: collection.to_string(),
        }];
        if let Some(guard) = self.append_result_cache_epoch_action(&mut actions)? {
            guard.commit();
        }
        Ok(())
    }

    /// Called with the publication fence held. Its guard must be committed
    /// only after the data and marker WAL batch is known durable.
    pub(super) fn append_result_cache_epoch_action(
        &self,
        actions: &mut Vec<StoreWalAction>,
    ) -> Result<Option<ResultCacheEpochGuard>, StoreError> {
        let state = &self.result_cache_epoch;
        let epoch = state.epoch.load(Ordering::SeqCst);
        if BOOT_SYSTEM_INFO_UPDATE.with(Cell::get)
            || epoch == 0
            || !actions.iter().any(|action| match action {
                StoreWalAction::CreateCollection { name }
                | StoreWalAction::DropCollection { name } => name != CACHE_COLLECTION,
                StoreWalAction::UpsertEntityRecord { collection, .. }
                | StoreWalAction::DeleteEntityRecord { collection, .. }
                | StoreWalAction::BulkUpsertEntityRecords { collection, .. }
                | StoreWalAction::RefreshCollection { collection, .. } => {
                    collection != CACHE_COLLECTION
                }
                StoreWalAction::SetAuxMetadata { .. } => true,
            })
        {
            return Ok(None);
        }
        state.pending.fetch_add(1, Ordering::SeqCst);
        let guard = ResultCacheEpochGuard {
            state: Arc::clone(state),
            committed: false,
        };
        let next = epoch
            .checked_add(1)
            .ok_or_else(|| StoreError::Io(std::io::Error::other("result-cache epoch exhausted")))?;
        let id = EntityId::new(state.id.load(Ordering::SeqCst));
        let Some(mut entity) = self.try_get(CACHE_COLLECTION, id, usize::MAX)? else {
            return Err(StoreError::Io(std::io::Error::other(
                "result-cache epoch marker disappeared",
            )));
        };
        entity.data = EntityData::Row(RowData::with_names(
            vec![Value::text(EPOCH_KEY), Value::UnsignedInteger(next)],
            vec!["key".into(), "value".into()],
        ));
        let action =
            StoreWalAction::upsert_entity(CACHE_COLLECTION, &entity, None, self.format_version());
        self.apply_replayed_action(&action, true)?;
        state.epoch.store(next, Ordering::SeqCst);
        actions.push(action);
        Ok(Some(guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_or_failed_epoch_publication_disables_result_cache() {
        let store = UnifiedStore::new();
        assert_eq!(store.ensure_result_cache_epoch().expect("initial epoch"), 1);
        let _publication = store.vault_publication_lock.lock();
        let mut actions = vec![StoreWalAction::CreateCollection {
            name: "items".into(),
        }];
        let guard = store
            .append_result_cache_epoch_action(&mut actions)
            .expect("prepare")
            .expect("guard");
        assert_eq!(
            store.result_cache_epoch(),
            None,
            "pending data must not use the cache"
        );
        guard.commit();
        assert_eq!(store.result_cache_epoch(), Some(2));
        let mut actions = vec![StoreWalAction::CreateCollection {
            name: "items".into(),
        }];
        let guard = store
            .append_result_cache_epoch_action(&mut actions)
            .expect("prepare")
            .expect("guard");
        drop(guard);
        assert_eq!(
            store.result_cache_epoch(),
            None,
            "an uncertain commit must fence the cache"
        );
        assert!(store.ensure_result_cache_epoch().is_err());
    }

    #[test]
    fn boot_system_telemetry_does_not_advance_data_epoch() {
        let store = UnifiedStore::new();
        assert_eq!(store.ensure_result_cache_epoch().expect("initial epoch"), 1);
        store.with_boot_system_info_update(|| {
            store
                .try_set_config_tree("red.system", &crate::json!({"pid": 7}))
                .expect("boot telemetry");
        });
        assert_eq!(store.result_cache_epoch(), Some(1));
        store
            .try_set_config_tree("red.config", &crate::json!({"key": "new"}))
            .expect("user config");
        assert!(store.result_cache_epoch().is_some_and(|epoch| epoch > 1));
    }

    #[test]
    fn deferred_transaction_persists_epoch_in_the_same_wal_batch() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("epoch-transaction.rdb");
        {
            let store = UnifiedStore::open(&path).expect("store");
            assert_eq!(store.ensure_result_cache_epoch().expect("initial epoch"), 1);
            UnifiedStore::begin_deferred_store_wal_capture();
            store
                .insert_auto(
                    "items",
                    UnifiedEntity::table_row(EntityId::new(0), "items", 0, vec![Value::Integer(7)]),
                )
                .expect("stage row");
            let actions = UnifiedStore::take_deferred_store_wal_capture();
            assert_eq!(
                store.result_cache_epoch(),
                Some(1),
                "uncommitted actions must not advance durable epoch"
            );
            store
                .append_deferred_store_wal_actions(actions)
                .expect("commit batch");
            assert_eq!(store.result_cache_epoch(), Some(2));
        }
        let store = UnifiedStore::open(&path).expect("reopen");
        assert_eq!(store.result_cache_epoch(), Some(2));
        assert_eq!(store.get_collection("items").expect("items").count(), 1);
    }

    #[test]
    fn internal_blob_publication_does_not_advance_data_epoch() {
        let store = UnifiedStore::new();
        assert_eq!(store.ensure_result_cache_epoch().expect("initial epoch"), 1);
        let _publication = store.vault_publication_lock.lock();
        let mut actions = vec![StoreWalAction::CreateCollection {
            name: CACHE_COLLECTION.into(),
        }];
        assert!(store
            .append_result_cache_epoch_action(&mut actions)
            .expect("prepare")
            .is_none());
        assert_eq!(store.result_cache_epoch(), Some(1));
    }
}
