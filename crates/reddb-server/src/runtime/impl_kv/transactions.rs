use super::*;
use crate::runtime::execution_context::{CurrentSnapshotGuard, SnapshotContext};
use crate::storage::transaction::snapshot::TxnContext;

impl KvAtomicOps<'_> {
    pub(super) fn record_vault_read(&self, collection: &str, key: Option<&str>) {
        let snapshot = self.runtime.vault_read_snapshot();
        if let Some(reader) = snapshot.serializable_reader {
            snapshot.manager.record_serializable_vault_read(
                reader,
                collection,
                self.tenant.as_deref(),
                key,
            );
        }
    }

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

        // Publication stays under vault_publication_lock until the WAL batch and
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
        self.runtime
            .inner
            .db
            .store()
            .pending_vault_versions
            .write()
            .remove(&write.entity.id);
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
        let mut vault_write_set = std::collections::HashSet::new();
        for write in &writes {
            vault_write_set.insert((
                write.collection.clone(),
                write.tenant.clone(),
                write.key.clone(),
            ));
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
            && (self
                .snapshot_manager()
                .serializable_commit_would_be_dangerous(context.xid, &write_set)
                || self
                    .snapshot_manager()
                    .serializable_vault_commit_would_be_dangerous(context.xid, &vault_write_set))
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
            .insert_vault_version(
                &write.collection,
                write.entity.clone(),
                write.metadata.clone(),
            )
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
        write.entity = store
            .get(&write.collection, write.entity.id)
            .ok_or_else(|| RedDBError::Internal("stored vault version disappeared".into()))?;
        Ok(())
    }

    fn remove_applied_vault_writes(&self, writes: &[PendingVaultWrite]) -> RedDBResult<()> {
        // These live-only rows never entered pager trees or a committed WAL.
        let store = self.inner.db.store();
        for write in writes.iter().filter(|write| write.applied) {
            if let Some(manager) = store.get_collection(&write.collection) {
                manager
                    .delete(write.entity.id)
                    .map_err(|error| RedDBError::Internal(error.to_string()))?;
            }
            store
                .pending_vault_versions
                .write()
                .remove(&write.entity.id);
        }
        Ok(())
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
                self.inner
                    .db
                    .store()
                    .pending_vault_versions
                    .write()
                    .remove(&write.entity.id);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthConfig, AuthStore};
    use crate::storage::{DeployProfile, StoragePackaging, StorageProfileSelection};
    use crate::{RedDBOptions, StorageDeployPreset};
    use std::path::Path;
    use std::sync::Arc;

    const CHILD_PATH: &str = "REDDB_VAULT_PREPARE_CRASH_PATH";
    const CHILD_PROFILE: &str = "REDDB_VAULT_PREPARE_CRASH_PROFILE";
    const TEST: &str = "runtime::impl_kv::transactions::tests::checkpoint_during_vault_prepare_cannot_recover_an_uncommitted_batch";

    fn open(path: &Path, operational: bool) -> RedDBRuntime {
        let profile = if operational {
            StorageProfileSelection {
                deploy_profile: DeployProfile::Embedded,
                packaging: StoragePackaging::OperationalDirectory,
                replica_count: 0,
                managed_backup: false,
                wal_retention: false,
            }
        } else {
            StorageDeployPreset::Serverless.selection()
        };
        let options = RedDBOptions::persistent(path)
            .with_storage_profile(profile)
            .unwrap();
        let runtime = RedDBRuntime::with_options(options).unwrap();
        let auth = Arc::new(
            AuthStore::with_vault_certificate(
                AuthConfig::default(),
                runtime.db().store().pager().unwrap().clone(),
                "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            )
            .unwrap(),
        );
        auth.ensure_vault_secret_key();
        runtime.set_auth_store(auth);
        runtime
    }

    #[test]
    fn checkpoint_during_vault_prepare_cannot_recover_an_uncommitted_batch() {
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let operational = std::env::var(CHILD_PROFILE).unwrap() == "true";
            let runtime = open(Path::new(&path), operational);
            runtime
                .execute_query("SET SECRET token = 'original'")
                .unwrap();
            runtime.checkpoint().unwrap();
            super::super::super::execution_context::set_current_connection_id(121);
            runtime.execute_query("BEGIN").unwrap();
            runtime
                .execute_query("SET SECRET token = 'pending'")
                .unwrap();
            runtime
                .execute_query("SET SECRET absent = 'pending-new-key'")
                .unwrap();
            let vault_store = runtime.inner.db.store();
            let _publication = vault_store.vault_publication_lock.lock();
            let _: RedDBResult<_> = runtime.inner.transaction_state.commit(121, |context| {
                runtime.prepare_pending_vault_writes(121, context)?;
                // Exercise both an incidental pager flush and a full checkpoint
                // exactly after materialization, before COMMIT's WAL append.
                runtime.inner.db.store().pager().unwrap().flush().unwrap();
                runtime.checkpoint()?;
                if !operational {
                    runtime.publish_serverless_generation()?;
                }
                std::process::exit(0);
            });
            panic!("crash injection did not exit");
        }
        let directory = tempfile::tempdir().unwrap();
        for operational in [false, true] {
            let path = directory.path().join(format!("prepare-{operational}.rdb"));
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(CHILD_PATH, &path)
                .env(CHILD_PROFILE, operational.to_string())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let runtime = open(&path, operational);
            if !operational {
                let ranges = runtime
                    .hydrate_current_serverless_collection("red.vault")
                    .unwrap()
                    .unwrap();
                assert_eq!(ranges.len(), 1);
                let snapshot = crate::storage::UnifiedStore::load_from_bytes_with_config(
                    &ranges[0].payload,
                    crate::storage::UnifiedStoreConfig::default(),
                )
                .unwrap();
                assert_eq!(snapshot.get_collection("red.vault").unwrap().count(), 1);
            }
            let revealed = runtime
                .execute_query("VAULT REVEAL red.vault.token")
                .unwrap();
            assert_eq!(
                revealed.result.records[0].get("value"),
                Some(&Value::text("original"))
            );
            assert!(runtime
                .execute_query("VAULT REVEAL red.vault.absent")
                .is_err());
            assert_eq!(
                runtime
                    .execute_query("VAULT HISTORY red.vault.token")
                    .unwrap()
                    .result
                    .len(),
                1
            );
        }
    }
}
