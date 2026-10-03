use super::*;
use crate::runtime::execution_context::{CurrentSnapshotGuard, SnapshotContext};
use crate::storage::transaction::snapshot::TxnContext;

impl KvAtomicOps<'_> {
    pub(super) fn pending_vault_entries(
        &self,
        collection: &str,
        key: Option<&str>,
    ) -> Vec<VaultEntry> {
        let connection_id = current_connection_id();
        if !self
            .runtime
            .inner
            .transaction_state
            .in_transaction(connection_id)
        {
            return Vec::new();
        }
        self.runtime
            .inner
            .pending_vault_writes
            .read()
            .get(&connection_id)
            .into_iter()
            .flatten()
            .filter(|write| {
                write.collection == collection
                    && write.tenant == self.tenant
                    && key.is_none_or(|key| write.key == key)
            })
            .filter_map(PendingVaultWrite::entry)
            .collect()
    }

    pub(super) fn stage_or_store_vault_version(
        &self,
        collection: &str,
        key: &str,
        mut entity: crate::storage::UnifiedEntity,
        metadata: Metadata,
        base_id: Option<crate::storage::EntityId>,
    ) -> RedDBResult<VaultEntry> {
        let manager = self.runtime.snapshot_manager();
        let transaction_xid = self.runtime.current_xid();
        let writer_xid = transaction_xid.unwrap_or_else(|| manager.begin());
        entity.set_xmin(writer_xid);
        let mut write = PendingVaultWrite {
            collection: collection.into(),
            entity,
            metadata,
            tenant: self.tenant.clone(),
            key: key.into(),
            base_id,
            writer_xid,
            applied: false,
        };
        if transaction_xid.is_some() {
            let mut pending = self.runtime.inner.pending_vault_writes.write();
            let writes = pending.entry(current_connection_id()).or_default();
            if let Some(first) = writes.iter().find(|previous| {
                previous.collection == write.collection
                    && previous.tenant == write.tenant
                    && previous.key == write.key
            }) {
                write.base_id = first.base_id;
            }
            let entry = write.entry().expect("vault writes contain keyed rows");
            writes.push(write);
            return Ok(entry);
        }

        // Publication stays under vault_write_lock until the WAL batch and
        // MVCC commit both complete. A reader with an earlier snapshot sees
        // the previous version even while these records are being inserted.
        crate::storage::UnifiedStore::begin_deferred_store_wal_capture();
        let stored = self.runtime.store_vault_write(&mut write);
        let actions = crate::storage::UnifiedStore::take_deferred_store_wal_capture();
        let result = stored.and_then(|()| {
            self.runtime
                .inner
                .db
                .store()
                .append_deferred_store_wal_actions(actions)
                .map_err(|error| RedDBError::Internal(error.to_string()))
        });
        if let Err(error) = result {
            manager.rollback(writer_xid);
            self.runtime.remove_applied_vault_writes(&[write])?;
            return Err(error);
        }
        manager.commit(writer_xid);
        self.runtime.invalidate_result_cache_for_table(collection);
        Ok(write
            .entry()
            .expect("stored vault writes contain keyed rows"))
    }
}

impl PendingVaultWrite {
    fn entry(&self) -> Option<VaultEntry> {
        let crate::storage::EntityData::Row(row) = &self.entity.data else {
            return None;
        };
        let version = super::super::keyed_spine::row_version(
            self.entity.id,
            row,
            self.entity.sequence_id as i64,
        )?;
        Some(VaultEntry::from_keyed_row(
            version,
            self.metadata.clone(),
            self.entity.created_at,
            self.entity.updated_at,
            self.entity.sequence_id,
            self.tenant.clone(),
        ))
    }
}

impl RedDBRuntime {
    pub(super) fn vault_read_snapshot(&self) -> SnapshotContext {
        super::super::execution_context::capture_current_snapshot()
            .unwrap_or_else(|| self.vault_write_snapshot())
    }

    pub(super) fn vault_write_snapshot(&self) -> SnapshotContext {
        let context = self
            .inner
            .transaction_state
            .context(current_connection_id());
        let serializable_reader = context.as_ref().and_then(|context| {
            (context.isolation == crate::storage::transaction::IsolationLevel::Serializable)
                .then_some(context.xid)
        });
        SnapshotContext {
            snapshot: self.current_snapshot(),
            manager: self.snapshot_manager(),
            own_xids: self.own_transaction_xids(),
            requires_index_fallback: true,
            serializable_reader,
        }
    }

    pub(crate) fn prepare_pending_vault_writes(
        &self,
        connection_id: u64,
        context: &TxnContext,
    ) -> RedDBResult<()> {
        let writes = self
            .inner
            .pending_vault_writes
            .read()
            .get(&connection_id)
            .cloned()
            .unwrap_or_default();
        if writes.is_empty() {
            return Ok(());
        }
        // TransactionState removes the context before prepare, so this is a
        // fresh committed view without the connection's staged overlay.
        let _snapshot_guard = CurrentSnapshotGuard::install(self.vault_write_snapshot());
        let mut checked = std::collections::HashSet::new();
        let mut write_set = std::collections::HashSet::new();
        for write in &writes {
            write_set.insert((write.collection.clone(), write.entity.id));
            if let Some(base_id) = write.base_id {
                write_set.insert((write.collection.clone(), base_id));
            }
            if !checked.insert((&write.collection, &write.tenant, &write.key)) {
                continue;
            }
            let operations = KvAtomicOps::for_tenant(self, write.tenant.clone());
            let current = operations.get_vault_entry(&write.collection, &write.key)?;
            if current.as_ref().map(|entry| entry.id) != write.base_id {
                return Err(RedDBError::Query(format!(
                    "serialization conflict: vault key {} was modified concurrently",
                    write.collection,
                )));
            }
        }
        if context.isolation == crate::storage::transaction::IsolationLevel::Serializable
            && self
                .snapshot_manager()
                .serializable_commit_would_be_dangerous(context.xid, &write_set)
        {
            return Err(RedDBError::Query(
                "serialization conflict: serializable vault transaction".into(),
            ));
        }

        crate::storage::UnifiedStore::begin_deferred_store_wal_capture();
        let result = (|| {
            for (index, mut write) in writes.into_iter().enumerate() {
                write.entity.set_xmin(context.xid);
                // Track partial application too: an insert can fail after its
                // in-memory update and still needs removing on commit failure.
                self.inner
                    .pending_vault_writes
                    .write()
                    .get_mut(&connection_id)
                    .expect("pending vault write set remains until commit")[index]
                    .applied = true;
                self.store_vault_write(&mut write)?;
            }
            Ok(())
        })();
        let captured = crate::storage::UnifiedStore::take_deferred_store_wal_capture();
        if result.is_ok() {
            self.record_pending_store_wal_actions(connection_id, captured);
        }
        result
    }

    fn store_vault_write(&self, write: &mut PendingVaultWrite) -> RedDBResult<()> {
        let store = self.inner.db.store();
        write.applied = true;
        store
            .insert(&write.collection, write.entity.clone())
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
        if !write.metadata.fields.is_empty() {
            store
                .set_metadata(&write.collection, write.entity.id, write.metadata.clone())
                .map_err(|error| RedDBError::Internal(error.to_string()))?;
        }
        write.entity = store
            .get(&write.collection, write.entity.id)
            .ok_or_else(|| RedDBError::Internal("stored vault version disappeared".into()))?;
        Ok(())
    }

    fn remove_applied_vault_writes(&self, writes: &[PendingVaultWrite]) -> RedDBResult<()> {
        // Rollback cleanup must not publish a delete WAL batch for rows whose
        // insert batch was never committed. It still repairs the pager trees.
        crate::storage::UnifiedStore::begin_deferred_store_wal_capture();
        let result = (|| {
            let store = self.inner.db.store();
            for write in writes.iter().filter(|write| write.applied) {
                if store.get_collection(&write.collection).is_some() {
                    store
                        .delete(&write.collection, write.entity.id)
                        .map_err(|error| RedDBError::Internal(error.to_string()))?;
                }
            }
            Ok(())
        })();
        let _discarded = crate::storage::UnifiedStore::take_deferred_store_wal_capture();
        result
    }

    pub(crate) fn discard_pending_vault_writes(&self, connection_id: u64) -> RedDBResult<()> {
        let writes = self
            .inner
            .pending_vault_writes
            .write()
            .remove(&connection_id)
            .unwrap_or_default();
        self.remove_applied_vault_writes(&writes)
    }

    pub(crate) fn finalize_pending_vault_writes(&self, connection_id: u64) {
        let writes = self
            .inner
            .pending_vault_writes
            .write()
            .remove(&connection_id);
        if let Some(writes) = writes {
            for write in writes {
                self.invalidate_result_cache_for_table(&write.collection);
            }
        }
    }

    pub(crate) fn rollback_vault_savepoint(&self, connection_id: u64, writer_xid: u64) {
        if let Some(writes) = self
            .inner
            .pending_vault_writes
            .write()
            .get_mut(&connection_id)
        {
            writes.retain(|write| write.writer_xid < writer_xid);
        }
        if let Some(events) = self
            .inner
            .pending_kv_watch_events
            .write()
            .get_mut(&connection_id)
        {
            events.retain(|(xid, _)| *xid < writer_xid);
        }
    }
}
