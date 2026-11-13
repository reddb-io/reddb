//! MVCC pending-action lifecycle & commit-time conflict detection.
//!
//! Extracted from `impl_core.rs` (impl_core slice 3/10, issue #1624). Houses
//! the five MVCC families that PRD #1620 (TM v2) will extend:
//!
//! - **Pending write-set records** — deferred tombstones, versioned updates,
//!   store-WAL actions, and event-emission gating for DML.
//! - **First-committer-wins conflict checks** — snapshot conflict detection
//!   and logical/table-row write-conflict guards.
//! - **Commit/rollback finalization** — stamp restore, tombstone/versioned
//!   update finalize/revive, and savepoint-scoped revival.
//! - **Transactional side-effect queues** — queue wakes, claim-lock release,
//!   and KV watch events flushed at commit / dropped at rollback.
//! - **Snapshot / xid accessors** — the per-connection snapshot, xid, and
//!   vacuum-cutoff surface used by transports and tests.
//!
//! Behaviour-preserving move: every item keeps its name, signature and
//! visibility so `execute_query_inner` and sibling-file callers need no
//! call-site edits.

use super::execution_context::current_connection_id;
use super::*;

thread_local! {
    static DEFERRED_UPDATES: std::cell::RefCell<Option<Vec<DeferredUpdate>>> = const { std::cell::RefCell::new(None) };
    static GRAPH_INSERT_WRITER: std::cell::Cell<Option<(usize, u64, u64)>> = const { std::cell::Cell::new(None) };
    static DML_CAPTURE_OWNER: std::cell::Cell<Option<(usize, u64, bool)>> = const { std::cell::Cell::new(None) };
    static DML_CAPTURE_GRAPH_WRITER: std::cell::RefCell<Option<GraphInsertWriter>> = const { std::cell::RefCell::new(None) };
}

struct GraphInsertWriter {
    manager: std::sync::Arc<crate::storage::transaction::snapshot::SnapshotManager>,
    xid: u64,
    finished: bool,
}

impl GraphInsertWriter {
    fn begin(runtime: &RedDBRuntime) -> Self {
        let manager = runtime.snapshot_manager();
        let xid = manager.begin();
        let identity = std::sync::Arc::as_ptr(&runtime.inner) as usize;
        GRAPH_INSERT_WRITER.with(|slot| {
            assert!(slot
                .replace(Some((identity, current_connection_id(), xid)))
                .is_none());
        });
        Self {
            manager,
            xid,
            finished: false,
        }
    }

    fn commit(&mut self) {
        self.manager.commit(self.xid);
        self.finished = true;
    }
}

impl Drop for GraphInsertWriter {
    fn drop(&mut self) {
        GRAPH_INSERT_WRITER.with(|slot| slot.set(None));
        if !self.finished {
            self.manager.rollback(self.xid);
        }
    }
}

struct DeferredUpdate {
    runtime: RedDBRuntime,
    collection: String,
    old_id: EntityId,
    new_id: EntityId,
    previous_xmax: u64,
    update: Arc<crate::application::entity::AutocommitUpdate>,
    committed: bool,
}

impl Drop for DeferredUpdate {
    fn drop(&mut self) {
        if !self.committed {
            let topology = self
                .runtime
                .index_store_ref()
                .collection_topology_lock(&self.collection);
            if let Err(error) = self.runtime.revive_versioned_update(
                &self.collection,
                self.old_id,
                self.new_id,
                self.update.xid,
                self.previous_xmax,
                topology.read_recursive(),
            ) {
                tracing::error!(%error, "autocommit update rollback failed");
            }
        }
    }
}

struct DeferredUpdates;

impl DeferredUpdates {
    fn begin() -> Self {
        DEFERRED_UPDATES.with(|slot| {
            assert!(slot.borrow().is_none(), "update WAL scopes cannot nest");
            *slot.borrow_mut() = Some(Vec::new());
        });
        Self
    }

    fn commit(self) {
        DEFERRED_UPDATES.with(|slot| {
            for mut pending in slot.borrow_mut().take().expect("update WAL scope") {
                let update = &pending.update;
                update.commit();
                pending.committed = true;
            }
        });
    }

    fn prepare(&self) -> RedDBResult<()> {
        DEFERRED_UPDATES.with(|slot| {
            for pending in slot.borrow().as_ref().expect("update WAL scope") {
                let store = pending.runtime.db().store();
                let Some(manager) = store.get_collection(&pending.collection) else {
                    return Err(RedDBError::Internal(
                        "updated collection disappeared".into(),
                    ));
                };
                let epoch = manager
                    .try_get(pending.new_id, usize::MAX)?
                    .and_then(|entity| entity.branch_version().map(|version| version.origin));
                let old_owned = manager
                    .try_get(pending.old_id, usize::MAX)
                    .map_err(|error| RedDBError::Internal(error.to_string()))?
                    .is_some_and(|old| {
                        epoch.map_or(old.xmax, |epoch| old.branch_xmax(epoch)) == pending.update.xid
                    });
                let new_owned = manager
                    .try_get(pending.new_id, usize::MAX)
                    .map_err(|error| RedDBError::Internal(error.to_string()))?
                    .is_some_and(|new| new.xmin == pending.update.xid);
                if !old_owned || !new_owned {
                    return Err(RedDBRuntime::conflict_error(
                        &pending.collection,
                        pending.old_id,
                        pending.update.xid,
                    ));
                }
            }
            Ok(())
        })
    }
}

impl Drop for DeferredUpdates {
    fn drop(&mut self) {
        let pending = DEFERRED_UPDATES.with(|slot| slot.borrow_mut().take());
        if let Some(pending) = pending {
            for update in pending.into_iter().rev() {
                drop(update);
            }
        }
    }
}

pub(crate) fn autocommit_update_capture_available() -> bool {
    DEFERRED_UPDATES.with(|slot| slot.borrow().is_some())
}

pub(crate) fn publish_autocommit_update(
    runtime: &RedDBRuntime,
    item: &crate::application::entity::AppliedEntityMutation,
    update: &Arc<crate::application::entity::AutocommitUpdate>,
) {
    if UnifiedStore::deferred_store_wal_capture_active() {
        DEFERRED_UPDATES.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .expect("update WAL scope")
                .push(DeferredUpdate {
                    runtime: runtime.clone(),
                    collection: item.collection.clone(),
                    old_id: item.replaced_entity.as_ref().expect("versioned update").id,
                    new_id: item.entity.id,
                    previous_xmax: item.replaced_entity_previous_xmax,
                    update: Arc::clone(update),
                    committed: false,
                });
        });
    } else {
        update.commit();
    }
}

/// An unwinding DML callback must not leave a thread-local capture installed.
struct DmlWalCapture(bool);

struct CapturedDmlWal {
    actions: crate::storage::unified::DeferredStoreWalActions,
    graph_writer: Option<GraphInsertWriter>,
}

impl DmlWalCapture {
    fn begin(runtime: &RedDBRuntime, transactional: bool) -> Self {
        assert!(
            !UnifiedStore::deferred_store_wal_capture_active(),
            "DML WAL captures cannot nest"
        );
        let identity = std::sync::Arc::as_ptr(&runtime.inner) as usize;
        DML_CAPTURE_OWNER.with(|slot| {
            assert!(slot
                .replace(Some((identity, current_connection_id(), transactional)))
                .is_none());
        });
        UnifiedStore::begin_deferred_store_wal_capture();
        Self(true)
    }

    fn finish(mut self) -> CapturedDmlWal {
        self.0 = false;
        DML_CAPTURE_OWNER.with(|slot| slot.set(None));
        CapturedDmlWal {
            actions: UnifiedStore::take_deferred_store_wal_capture(),
            graph_writer: DML_CAPTURE_GRAPH_WRITER.with(|slot| slot.borrow_mut().take()),
        }
    }
}

impl Drop for DmlWalCapture {
    fn drop(&mut self) {
        if self.0 {
            DML_CAPTURE_OWNER.with(|slot| slot.set(None));
            let _ = UnifiedStore::take_deferred_store_wal_capture();
            DML_CAPTURE_GRAPH_WRITER.with(|slot| drop(slot.borrow_mut().take()));
        }
    }
}

impl RedDBRuntime {
    /// Refresh an autocommit state mutation after acquiring its publication
    /// fence. Explicit transactions retain their original read snapshot.
    pub(super) fn vcs_state_write_snapshot(
        &self,
        collection: &str,
    ) -> RedDBResult<(
        super::execution_context::SnapshotContext,
        crate::storage::transaction::snapshot::SnapshotRetentionGuard,
    )> {
        self.vcs_writer_epoch(collection)?;
        let (mut context, retention) = self.native_read_snapshot()?;
        if self.current_xid().is_none() {
            context.snapshot = self.snapshot_manager().fresh_read_snapshot();
            context.branch = self.vcs_branch_read_view(&context.snapshot, None)?;
        }
        Ok((context, retention))
    }

    /// Append one native state version. The caller serializes its logical key
    /// before taking the publication fence and owns the enclosing WAL capture.
    pub(crate) fn insert_vcs_state_version(
        &self,
        collection: &str,
        mut entity: UnifiedEntity,
        previous: Option<EntityId>,
    ) -> RedDBResult<EntityId> {
        let store = self.db().store();
        let epoch = self.vcs_writer_epoch(collection)?.ok_or_else(|| {
            RedDBError::InvalidOperation("state version requires a versioned collection".into())
        })?;
        let xid = self
            .current_xid()
            .or_else(|| self.graph_insert_xid())
            .ok_or_else(|| {
                RedDBError::InvalidOperation("state version requires a WAL writer".into())
            })?;
        entity.id = store.next_entity_id();
        entity.set_xmin(xid);
        entity.set_xmax(0);
        entity.clear_branch_version();
        entity.set_branch_origin(epoch);
        let id = entity.id;
        if let Some(previous) = previous {
            let mut old = store
                .try_get(collection, previous, usize::MAX)?
                .ok_or_else(|| RedDBError::NotFound(previous.to_string()))?;
            entity.set_logical_id(old.logical_id());
            if let (
                crate::storage::EntityKind::TableRow {
                    row_id: destination,
                    ..
                },
                crate::storage::EntityKind::TableRow { row_id: source, .. },
            ) = (&mut entity.kind, &old.kind)
            {
                *destination = *source;
            }
            let previous_xmax = old.branch_xmax(epoch);
            old.set_branch_xmax(epoch, xid);
            store.install_versioned_table_row_update(collection, old, entity, None)?;
            if self.current_xid().is_some() {
                self.record_pending_versioned_update(
                    current_connection_id(),
                    collection,
                    previous,
                    id,
                    xid,
                    previous_xmax,
                );
            }
        } else {
            store.insert_auto(collection, entity)?;
        }
        Ok(id)
    }

    pub(crate) fn graph_insert_xid(&self) -> Option<u64> {
        let identity = std::sync::Arc::as_ptr(&self.inner) as usize;
        GRAPH_INSERT_WRITER.with(|slot| {
            slot.get().and_then(|(owner, conn, xid)| {
                (owner == identity && conn == current_connection_id()).then_some(xid)
            })
        })
    }

    /// Publish a versioned graph or vector insert after its WAL batch.
    pub(crate) fn with_versioned_graph_insert<T>(
        &self,
        f: impl FnOnce() -> RedDBResult<T>,
    ) -> RedDBResult<T> {
        let identity = std::sync::Arc::as_ptr(&self.inner) as usize;
        let connection_id = current_connection_id();
        if self.current_xid().is_some() {
            if UnifiedStore::deferred_store_wal_capture_active() {
                if DML_CAPTURE_OWNER.with(|slot| slot.get())
                    == Some((identity, connection_id, true))
                {
                    return f();
                }
                return Err(RedDBError::InvalidOperation(
                    "versioned entity insertion cannot join an unrelated WAL capture".to_string(),
                ));
            }
            return self.with_deferred_store_wal_if_transaction(f);
        }
        if self.graph_insert_xid().is_some() {
            return f();
        }
        if UnifiedStore::deferred_store_wal_capture_active() {
            if DML_CAPTURE_OWNER.with(|slot| slot.get()) != Some((identity, connection_id, false))
                || GRAPH_INSERT_WRITER.with(|slot| slot.get().is_some())
            {
                return Err(RedDBError::InvalidOperation(
                    "versioned entity insertion cannot join an unrelated WAL capture".to_string(),
                ));
            }
            let writer = GraphInsertWriter::begin(self);
            let result = f();
            DML_CAPTURE_GRAPH_WRITER.with(|slot| {
                assert!(slot.borrow_mut().replace(writer).is_none());
            });
            return result;
        }
        self.drain_committed_writes_under_pressure(0)?;
        let deletions = super::delete_publication::DeletePublicationScope::begin();
        let updates = DeferredUpdates::begin();
        let mut writer = GraphInsertWriter::begin(self);
        let capture = DmlWalCapture::begin(self, false);
        let result = f();
        let CapturedDmlWal {
            actions,
            mut graph_writer,
        } = capture.finish();
        if result.is_err() {
            self.flush_deferred_cache_invalidation();
            return result;
        }
        let published = deletions.publish(self, || {
            updates.prepare()?;
            self.inner
                .db
                .store()
                .append_deferred_store_wal_actions(actions)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            if let Some(writer) = &mut graph_writer {
                writer.commit();
            }
            writer.commit();
            updates.commit();
            Ok(())
        });
        self.flush_deferred_cache_invalidation();
        published?;
        result
    }

    pub(crate) fn with_graph_insert_for_dml<T>(
        &self,
        query: &InsertQuery,
        f: impl FnOnce() -> RedDBResult<T>,
    ) -> RedDBResult<T> {
        if matches!(
            query.entity_type,
            InsertEntityType::Node
                | InsertEntityType::Edge
                | InsertEntityType::Vector
                | InsertEntityType::Document
        ) && (self.vcs_is_versioned(&query.table)? || self.has_graph_contract(&query.table))
        {
            self.with_versioned_graph_insert(f)
        } else {
            self.with_deferred_store_wal_for_dml(self.insert_may_emit_events(query), f)
        }
    }

    pub(crate) fn rollback_connection(
        &self,
        connection_id: u64,
    ) -> RedDBResult<Option<crate::storage::transaction::snapshot::TxnContext>> {
        // Keep writer XIDs active until undo has consumed their row fields.
        // Otherwise GC can reclaim an aborted version while undo waits for
        // index backfill, leaving the version's secondary-index entries behind.
        let undo = self.revive_pending_versioned_updates(connection_id);
        self.revive_pending_tombstones(connection_id);
        let context = self.inner.transaction_state.rollback(connection_id);
        self.discard_pending_queue_dedup(connection_id);
        self.discard_pending_kv_watch_events(connection_id);
        self.discard_pending_queue_wakes(connection_id);
        self.discard_pending_store_wal_actions(connection_id);
        self.release_pending_claim_locks(connection_id);
        self.discard_pending_vault_writes(connection_id)?;
        undo?;
        Ok(context)
    }

    /// Record that the running transaction has marked `id` in `collection`
    /// for deletion (Phase 2.3.2b MVCC tombstones). `stamper_xid` is the
    /// xid that was written into `xmax` — either the parent txn xid or
    /// the innermost savepoint sub-xid. Savepoint rollback filters by
    /// this xid to revive only its own tombstones.
    pub(crate) fn record_pending_tombstone(
        &self,
        conn_id: u64,
        collection: &str,
        id: crate::storage::unified::entity::EntityId,
        stamper_xid: crate::storage::transaction::snapshot::Xid,
        previous_xmax: crate::storage::transaction::snapshot::Xid,
        branch_epoch: Option<u64>,
    ) {
        self.inner
            .pending_tombstones
            .write()
            .entry(conn_id)
            .or_default()
            .push((
                collection.to_string(),
                id,
                stamper_xid,
                previous_xmax,
                branch_epoch,
            ));
    }

    pub(crate) fn record_pending_versioned_update(
        &self,
        conn_id: u64,
        collection: &str,
        old_id: crate::storage::unified::entity::EntityId,
        new_id: crate::storage::unified::entity::EntityId,
        stamper_xid: crate::storage::transaction::snapshot::Xid,
        previous_xmax: crate::storage::transaction::snapshot::Xid,
    ) {
        self.inner
            .pending_versioned_updates
            .write()
            .entry(conn_id)
            .or_default()
            .push((
                collection.to_string(),
                old_id,
                new_id,
                stamper_xid,
                previous_xmax,
            ));
    }

    pub(crate) fn record_pending_queue_dedup(
        &self,
        conn_id: u64,
        queue: &str,
        dedup_key: &str,
        metadata_id: crate::storage::unified::entity::EntityId,
    ) {
        self.inner
            .pending_queue_dedup
            .write()
            .entry(conn_id)
            .or_default()
            .push((queue.to_string(), dedup_key.to_string(), metadata_id));
    }

    pub(crate) fn with_deferred_store_wal_if_transaction<T>(
        &self,
        f: impl FnOnce() -> RedDBResult<T>,
    ) -> RedDBResult<T> {
        let conn_id = current_connection_id();
        if !self.inner.transaction_state.in_transaction(conn_id) {
            return f();
        }

        let logical_start = self
            .inner
            .pending_logical_records
            .lock()
            .get(&conn_id)
            .map_or(0, |pending| pending.len());
        let capture = DmlWalCapture::begin(self, true);
        let result = f();
        let CapturedDmlWal {
            actions,
            graph_writer,
        } = capture.finish();
        if graph_writer.is_some() {
            return Err(RedDBError::InvalidOperation(
                "autocommit graph writer cannot join a transaction WAL capture".to_string(),
            ));
        }
        self.flush_deferred_cache_invalidation();
        match result {
            Ok(value) => {
                self.record_pending_store_wal_actions(conn_id, actions);
                Ok(value)
            }
            Err(err) => {
                if let Some(records) = self.inner.pending_logical_records.lock().get_mut(&conn_id) {
                    records.truncate(logical_start);
                }
                Err(err)
            }
        }
    }

    pub(crate) fn with_deferred_store_wal_for_dml<T>(
        &self,
        capture_autocommit_events: bool,
        f: impl FnOnce() -> RedDBResult<T>,
    ) -> RedDBResult<T> {
        let conn_id = current_connection_id();
        if self.inner.transaction_state.in_transaction(conn_id) {
            return self.with_deferred_store_wal_if_transaction(f);
        }
        self.drain_committed_writes_under_pressure(0)?;
        if !capture_autocommit_events {
            return f();
        }

        let deletions = super::delete_publication::DeletePublicationScope::begin();
        let updates = DeferredUpdates::begin();
        let capture = DmlWalCapture::begin(self, false);
        let result = f();
        let CapturedDmlWal {
            actions,
            mut graph_writer,
        } = capture.finish();
        if result.is_err() && (deletions.has_pending() || graph_writer.is_some()) {
            self.flush_deferred_cache_invalidation();
            return result;
        }
        let published = deletions.publish(self, || {
            updates.prepare()?;
            self.inner
                .db
                .store()
                .append_deferred_store_wal_actions(actions)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            if let Some(writer) = &mut graph_writer {
                writer.commit();
            }
            updates.commit();
            Ok(())
        });
        self.flush_deferred_cache_invalidation();
        published?;
        result
    }

    pub(crate) fn insert_may_emit_events(&self, query: &InsertQuery) -> bool {
        !query.suppress_events
            && self.collection_has_event_subscriptions_for_operation(
                &query.table,
                crate::catalog::SubscriptionOperation::Insert,
            )
    }

    pub(crate) fn update_may_emit_events(&self, query: &UpdateQuery) -> bool {
        !query.suppress_events
            && self.collection_has_event_subscriptions_for_operation(
                &query.table,
                crate::catalog::SubscriptionOperation::Update,
            )
    }

    pub(crate) fn delete_may_emit_events(&self, query: &DeleteQuery) -> bool {
        !query.suppress_events
            && self.collection_has_event_subscriptions_for_operation(
                &query.table,
                crate::catalog::SubscriptionOperation::Delete,
            )
    }

    fn collection_has_event_subscriptions_for_operation(
        &self,
        collection: &str,
        operation: crate::catalog::SubscriptionOperation,
    ) -> bool {
        let Some(contract) = self.db().collection_contract_arc(collection) else {
            return false;
        };
        contract.subscriptions.iter().any(|subscription| {
            subscription.enabled
                && (subscription.ops_filter.is_empty()
                    || subscription.ops_filter.contains(&operation))
        })
    }

    pub(crate) fn record_pending_store_wal_actions(
        &self,
        conn_id: u64,
        actions: crate::storage::unified::DeferredStoreWalActions,
    ) {
        if actions.is_empty() {
            return;
        }
        let mut guard = self.inner.pending_store_wal_actions.write();
        guard
            .entry(conn_id)
            .or_default()
            .push((self.current_xid().unwrap_or(0), actions));
    }

    pub(crate) fn flush_pending_store_wal_actions(&self, conn_id: u64) -> RedDBResult<()> {
        let groups = self
            .inner
            .pending_store_wal_actions
            .write()
            .remove(&conn_id)
            .unwrap_or_default();
        let mut actions = crate::storage::unified::DeferredStoreWalActions::default();
        for (xid, group) in groups {
            if !self.snapshot_manager().is_aborted(xid) {
                actions.extend(group);
            }
        }
        self.commit_transactional_replication(conn_id, actions)
    }

    pub(crate) fn discard_pending_store_wal_actions(&self, conn_id: u64) {
        self.inner.pending_commit_policies.lock().remove(&conn_id);
        self.inner.pending_logical_records.lock().remove(&conn_id);
        self.inner
            .pending_store_wal_actions
            .write()
            .remove(&conn_id);
    }

    fn xid_conflicts_with_snapshot(
        &self,
        xid: crate::storage::transaction::snapshot::Xid,
        snapshot: &crate::storage::transaction::snapshot::Snapshot,
        own_xids: &std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
    ) -> bool {
        xid != 0
            && !own_xids.contains(&xid)
            && !self
                .inner
                .transaction_state
                .snapshot_manager()
                .is_aborted(xid)
            && !self
                .inner
                .transaction_state
                .snapshot_manager()
                .is_active(xid)
            && (xid > snapshot.xid || snapshot.in_progress.contains(&xid))
    }

    pub(super) fn conflict_error(
        collection: &str,
        logical_id: crate::storage::unified::entity::EntityId,
        xid: crate::storage::transaction::snapshot::Xid,
    ) -> RedDBError {
        RedDBError::Query(format!(
            "serialization conflict: table row {collection}/{} was modified by concurrent transaction {xid}",
            logical_id.raw()
        ))
    }

    fn check_logical_row_conflict(
        &self,
        collection: &str,
        logical_id: crate::storage::unified::entity::EntityId,
        excluded_ids: &[crate::storage::unified::entity::EntityId],
        snapshot: &crate::storage::transaction::snapshot::Snapshot,
        own_xids: &std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
        branch_epoch: Option<u64>,
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(());
        };

        for candidate in manager.query_all(|_| true) {
            if let Some(epoch) = branch_epoch {
                if candidate
                    .branch_version()
                    .is_some_and(|version| version.origin != epoch)
                {
                    continue;
                }
            }
            if excluded_ids.contains(&candidate.id) || candidate.logical_id() != logical_id {
                continue;
            }
            if self.xid_conflicts_with_snapshot(candidate.xmin, snapshot, own_xids) {
                return Err(Self::conflict_error(collection, logical_id, candidate.xmin));
            }
            if self.xid_conflicts_with_snapshot(candidate.xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(collection, logical_id, candidate.xmax));
            }
        }
        Ok(())
    }

    pub(crate) fn check_table_row_write_conflicts(
        &self,
        conn_id: u64,
        snapshot: &crate::storage::transaction::snapshot::Snapshot,
        own_xids: &std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
        isolation: crate::storage::transaction::IsolationLevel,
    ) -> RedDBResult<()> {
        let versioned_updates = self
            .inner
            .pending_versioned_updates
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();
        let tombstones = self
            .inner
            .pending_tombstones
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();

        let mut serializable_write_set = std::collections::HashSet::new();
        let store = self.inner.db.store();
        for (collection, old_id, new_id, xid, previous_xmax) in versioned_updates {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            let Some(old) = manager.get(old_id) else {
                continue;
            };
            let logical_id = old.logical_id();
            let epoch = manager
                .get(new_id)
                .and_then(|entity| entity.branch_version().map(|version| version.origin));
            let old_xmax = epoch.map_or(old.xmax, |epoch| old.branch_xmax(epoch));
            serializable_write_set.insert((collection.clone(), logical_id));
            if self.xid_conflicts_with_snapshot(previous_xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, previous_xmax));
            }
            if old_xmax != xid && self.xid_conflicts_with_snapshot(old_xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, old_xmax));
            }
            self.check_logical_row_conflict(
                &collection,
                logical_id,
                &[old_id, new_id],
                snapshot,
                own_xids,
                epoch,
            )?;
        }

        for (collection, id, xid, previous_xmax, epoch) in tombstones {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            let Some(entity) = manager.get(id) else {
                continue;
            };
            let logical_id = entity.logical_id();
            let xmax = epoch.map_or(entity.xmax, |epoch| entity.branch_xmax(epoch));
            serializable_write_set.insert((collection.clone(), logical_id));
            if self.xid_conflicts_with_snapshot(previous_xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, previous_xmax));
            }
            if xmax != xid && self.xid_conflicts_with_snapshot(xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, xmax));
            }
            self.check_logical_row_conflict(
                &collection,
                logical_id,
                &[id],
                snapshot,
                own_xids,
                epoch,
            )?;
        }

        if isolation == crate::storage::transaction::IsolationLevel::Serializable
            && self
                .inner
                .transaction_state
                .snapshot_manager()
                .serializable_commit_would_be_dangerous(
                    *own_xids.iter().min().unwrap_or(&0),
                    &serializable_write_set,
                )
        {
            return Err(RedDBError::Query(
                "serialization conflict: serializable transaction would complete rw-antidependency dangerous structure".to_string(),
            ));
        }

        Ok(())
    }

    pub(crate) fn check_queue_dedup_write_conflicts(
        &self,
        conn_id: u64,
        snapshot: &crate::storage::transaction::snapshot::Snapshot,
        own_xids: &std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
    ) -> RedDBResult<()> {
        let pending = self
            .inner
            .pending_queue_dedup
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();
        if pending.is_empty() {
            return Ok(());
        }

        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(crate::runtime::impl_queue::QUEUE_META_COLLECTION)
        else {
            return Ok(());
        };

        for (queue, dedup_key, metadata_id) in pending {
            for candidate in manager.query_all(|entity| {
                entity.data.as_row().is_some_and(|row| {
                    crate::runtime::impl_queue::row_text(row, "kind").as_deref()
                        == Some(crate::runtime::impl_queue::KIND_QUEUE_PUSH_DEDUP)
                        && crate::runtime::impl_queue::row_text(row, "queue").as_deref()
                            == Some(&queue)
                        && crate::runtime::impl_queue::row_text(row, "dedup_key").as_deref()
                            == Some(&dedup_key)
                })
            }) {
                if candidate.id == metadata_id
                    || self
                        .inner
                        .transaction_state
                        .snapshot_manager()
                        .is_aborted(candidate.xmin)
                {
                    continue;
                }
                if self.xid_conflicts_with_snapshot(candidate.xmin, snapshot, own_xids) {
                    return Err(RedDBError::Query(format!(
                        "serialization conflict: queue dedup key {queue}/{dedup_key} was modified by concurrent transaction {}",
                        candidate.xmin
                    )));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn restore_pending_write_stamps(&self, conn_id: u64) {
        let versioned_updates = self
            .inner
            .pending_versioned_updates
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();
        let tombstones = self
            .inner
            .pending_tombstones
            .read()
            .get(&conn_id)
            .cloned()
            .unwrap_or_default();

        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        for (collection, old_id, new_id, xid, _previous_xmax) in versioned_updates {
            if let Some(manager) = store.get_collection(&collection) {
                let epoch = manager
                    .get(new_id)
                    .and_then(|entity| entity.branch_version().map(|version| version.origin));
                if let Some(mut entity) = manager.get(old_id) {
                    if let Some(epoch) = epoch {
                        entity.set_branch_xmax(epoch, xid);
                    } else {
                        entity.set_xmax(xid);
                    }
                    let _ = manager.update(entity);
                }
            }
        }
        for (collection, id, xid, _previous_xmax, epoch) in tombstones {
            if let Some(manager) = store.get_collection(&collection) {
                if let Some(mut entity) = manager.get(id) {
                    if let Some(epoch) = epoch {
                        entity.set_branch_xmax(epoch, xid);
                    } else {
                        entity.set_xmax(xid);
                    }
                    let _ = manager.update(entity);
                }
            }
        }
    }

    pub(crate) fn finalize_pending_versioned_updates(&self, conn_id: u64) {
        self.inner
            .pending_versioned_updates
            .write()
            .remove(&conn_id);
    }

    pub(crate) fn finalize_pending_queue_dedup(&self, conn_id: u64) {
        self.inner.pending_queue_dedup.write().remove(&conn_id);
    }

    pub(crate) fn discard_pending_queue_dedup(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_queue_dedup.write().remove(&conn_id) else {
            return;
        };
        let store = self.inner.db.store();
        for (_queue, _dedup_key, metadata_id) in pending {
            let _ = store.delete(
                crate::runtime::impl_queue::QUEUE_META_COLLECTION,
                metadata_id,
            );
        }
    }

    pub(crate) fn revive_pending_versioned_updates(&self, conn_id: u64) -> RedDBResult<()> {
        let Some(pending) = self
            .inner
            .pending_versioned_updates
            .write()
            .remove(&conn_id)
        else {
            return Ok(());
        };
        // Undo newest versions first so repeated writes restore the original chain.
        for (collection, old_id, new_id, xid, previous_xmax) in pending.into_iter().rev() {
            let topology_lock = self.index_store_ref().collection_topology_lock(&collection);
            #[cfg(test)]
            self.index_store_ref().mutation_test_hook(
                &collection,
                super::index_store::MutationTestPhase::BeforeUndoTopology,
            );
            self.revive_versioned_update(
                &collection,
                old_id,
                new_id,
                xid,
                previous_xmax,
                topology_lock.read(),
            )?;
        }
        Ok(())
    }

    pub(crate) fn revive_versioned_update(
        &self,
        collection: &str,
        old_id: crate::storage::EntityId,
        new_id: crate::storage::EntityId,
        xid: u64,
        previous_xmax: u64,
        _topology_guard: parking_lot::RwLockReadGuard<'_, ()>,
    ) -> RedDBResult<()> {
        let _indexed_mutation = self.index_store_ref().begin_indexed_mutation();
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        let epoch = store
            .get(collection, new_id)
            .and_then(|entity| entity.branch_version().map(|version| version.origin));
        if let Some(new) = store.get(collection, new_id) {
            let fields = crate::application::ports::entity_row_fields_snapshot(&new);
            self.index_store_ref()
                .index_entity_delete(collection, new_id, &fields)
                .map_err(RedDBError::Internal)?;
            #[cfg(test)]
            self.index_store_ref().mutation_test_hook(
                collection,
                super::index_store::MutationTestPhase::UndoIndexRemoved,
            );
        }
        store
            .delete_batch(collection, &[new_id])
            .map_err(|error| RedDBError::Internal(error.to_string()))?;
        if let Some(manager) = store.get_collection(collection) {
            if let Some(mut old) = manager.get(old_id) {
                if epoch.map_or(old.xmax, |epoch| old.branch_xmax(epoch)) == xid {
                    if let Some(epoch) = epoch {
                        old.set_branch_xmax(epoch, previous_xmax);
                    } else {
                        old.set_xmax(previous_xmax);
                    }
                    manager
                        .update(old.clone())
                        .map_err(|error| RedDBError::Internal(error.to_string()))?;
                    store.context_index().index_entity(collection, &old);
                    let fields = crate::application::ports::entity_row_fields_snapshot(&old);
                    self.index_store_ref()
                        .index_entity_insert(collection, old_id, &fields)
                        .map_err(RedDBError::Internal)?;
                }
            }
        }
        Ok(())
    }

    /// Flush tombstones on COMMIT. The xmax stamp is already the durable
    /// delete marker; commit only drops the rollback journal and emits
    /// side effects. Physical reclamation is left for VACUUM so old
    /// snapshots can still resolve the pre-delete row version.
    pub(crate) fn finalize_pending_tombstones(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_tombstones.write().remove(&conn_id) else {
            return;
        };
        if pending.is_empty() {
            return;
        }

        let store = self.inner.db.store();
        for (collection, id, _xid, _previous_xmax, _epoch) in pending {
            if let Some(manager) = store.get_collection(&collection) {
                manager.remove_metadata_expiry(id);
            }
            store.context_index().remove_entity(id);
            self.invalidate_result_cache_for_table(&collection);
            // Primary CDC identities are allocated before the durable outbox
            // captures their restart frontier.
            if self.inner.db.replication.is_none() {
                self.inner.cdc.emit(
                    crate::replication::cdc::ChangeOperation::Delete,
                    &collection,
                    id.raw(),
                    "entity",
                );
            }
        }
    }

    /// Revive tombstones on ROLLBACK — reset `xmax` to 0 so the tuples
    /// become visible again to future snapshots. Best-effort: a row
    /// already reclaimed by a concurrent VACUUM stays gone, but VACUUM
    /// never reclaims tuples whose xmax is still referenced by any
    /// active snapshot, so this case is only reachable via external
    /// storage corruption.
    pub(crate) fn revive_pending_tombstones(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_tombstones.write().remove(&conn_id) else {
            return;
        };

        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        for (collection, id, xid, previous_xmax, epoch) in pending {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            if let Some(mut entity) = manager.get(id) {
                if epoch.map_or(entity.xmax, |epoch| entity.branch_xmax(epoch)) == xid {
                    if let Some(epoch) = epoch {
                        entity.set_branch_xmax(epoch, previous_xmax);
                    } else {
                        entity.set_xmax(previous_xmax);
                    }
                    let _ = manager.update(entity);
                }
            }
        }
    }

    /// Slice C of PRD #718 — accessor for the local wait registry.
    pub fn queue_wait_registry(
        &self,
    ) -> std::sync::Arc<crate::runtime::queue_wait_registry::QueueWaitRegistry> {
        self.inner.queue_wait_registry.clone()
    }

    /// Buffer a `(scope, queue)` wake on the current connection so it
    /// fires post-COMMIT, or notify immediately if no transaction is
    /// open (autocommit path). The wait registry only ever observes
    /// notifies for committed work — rollback drops the buffer.
    pub(crate) fn record_queue_wake(&self, scope: &str, queue: &str) {
        if self.current_xid().is_some() {
            let conn_id = current_connection_id();
            self.inner
                .pending_queue_wakes
                .write()
                .entry(conn_id)
                .or_default()
                .push((scope.to_string(), queue.to_string()));
            return;
        }
        self.inner.queue_wait_registry.notify(scope, queue);
    }

    pub(crate) fn finalize_pending_queue_wakes(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_queue_wakes.write().remove(&conn_id) else {
            return;
        };
        for (scope, queue) in pending {
            self.inner.queue_wait_registry.notify(&scope, &queue);
        }
    }

    pub(crate) fn discard_pending_queue_wakes(&self, conn_id: u64) {
        self.inner.pending_queue_wakes.write().remove(&conn_id);
    }

    pub(crate) fn release_pending_claim_locks(&self, conn_id: u64) {
        self.inner
            .pending_claim_locks
            .write()
            .retain(|_, owner| *owner != conn_id);
    }

    pub(crate) fn finalize_pending_kv_watch_events(&self, conn_id: u64) {
        let Some(pending) = self.inner.pending_kv_watch_events.write().remove(&conn_id) else {
            return;
        };
        for (_, event) in pending {
            self.cdc_emit_kv(
                event.op,
                &event.collection,
                &event.key,
                0,
                event.before,
                event.after,
            );
        }
    }

    pub(crate) fn discard_pending_kv_watch_events(&self, conn_id: u64) {
        self.inner.pending_kv_watch_events.write().remove(&conn_id);
    }

    /// Phase 1.1 MVCC universal: post-save hook that stamps `xmin` on a
    /// freshly-inserted entity when the current connection holds an
    /// open transaction. Used by graph / queue / timeseries
    /// write paths that go through the DevX builder API (`db.node(...)
    /// .save()` and friends) — those live in the storage crate and
    /// can't reach `current_xid()` without crossing layers, so the
    /// application layer calls this helper right after `save()` to
    /// finalise the MVCC stamp.
    ///
    /// Autocommit (outside BEGIN) is a no-op — no extra lookup or
    /// write, so the non-transactional hot path stays untouched.
    ///
    /// Best-effort: if the collection or entity disappears between
    /// the save and the stamp (concurrent DROP), we silently skip.
    pub(crate) fn stamp_xmin_if_in_txn(
        &self,
        collection: &str,
        id: crate::storage::unified::entity::EntityId,
    ) {
        let Some(xid) = self.current_xid() else {
            return;
        };
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return;
        };
        if let Some(mut entity) = manager.get(id) {
            entity.set_xmin(xid);
            let _ = manager.update(entity);
        }
    }

    /// Revive tombstones stamped by `stamper_xid` or any sub-xid
    /// allocated after it (Phase 2.3.2e savepoint rollback). Any
    /// pending entries with `xid < stamper_xid` stay queued because
    /// they belong to the enclosing scope — they'll either flush on
    /// COMMIT or revive on an outer ROLLBACK TO SAVEPOINT.
    ///
    /// Returns the number of tuples whose `xmax` was wiped back to 0.
    pub(crate) fn revive_tombstones_since(&self, conn_id: u64, stamper_xid: u64) -> usize {
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        let mut guard = self.inner.pending_tombstones.write();
        let Some(pending) = guard.get_mut(&conn_id) else {
            return 0;
        };

        let mut revived = 0usize;
        pending.retain(|(collection, id, xid, previous_xmax, epoch)| {
            if *xid < stamper_xid {
                // Stamped before the savepoint — keep in queue.
                return true;
            }
            if let Some(manager) = store.get_collection(collection) {
                if let Some(mut entity) = manager.get(*id) {
                    if epoch.map_or(entity.xmax, |epoch| entity.branch_xmax(epoch)) == *xid {
                        if let Some(epoch) = epoch {
                            entity.set_branch_xmax(*epoch, *previous_xmax);
                        } else {
                            entity.set_xmax(*previous_xmax);
                        }
                        let _ = manager.update(entity);
                        revived += 1;
                    }
                }
            }
            false
        });
        if pending.is_empty() {
            guard.remove(&conn_id);
        }
        revived
    }

    /// Return the snapshot the current connection should use for visibility
    /// checks (Phase 2.3 PG parity).
    ///
    /// * If the connection is inside a READ COMMITTED transaction, capture
    ///   a fresh statement snapshot while preserving the transaction's xid
    ///   and write-set lifecycle.
    /// * If the connection is inside a SNAPSHOT transaction, reuse the
    ///   snapshot stored in its `TxnContext`.
    /// * Otherwise (autocommit), capture a fresh snapshot tied to an
    ///   implicit xid=0 — the read path treats pre-MVCC rows as always
    ///   visible so this degrades to "see everything committed".
    pub fn current_snapshot(&self) -> crate::storage::transaction::snapshot::Snapshot {
        self.inner
            .transaction_state
            .current_snapshot(current_connection_id())
    }

    /// Xid of the current connection's active transaction, or `None` when
    /// running outside a BEGIN/COMMIT block. Write paths call this to
    /// decide whether to stamp `xmin`/`xmax` on tuples.
    /// Phase 2.3.2e: when a savepoint is open, `writer_xid` returns the
    /// sub-xid so new writes can be selectively rolled back. Otherwise
    /// the parent txn's xid is returned, matching pre-savepoint
    /// behaviour. Callers that need the enclosing *transaction* xid
    /// (e.g. VACUUM min-active calculations) should read `ctx.xid`
    /// directly.
    pub fn current_xid(&self) -> Option<crate::storage::transaction::snapshot::Xid> {
        self.inner
            .transaction_state
            .writer_xid(current_connection_id())
    }

    pub(crate) fn own_transaction_xids(&self) -> std::collections::HashSet<u64> {
        let mut xids = self
            .inner
            .transaction_state
            .own_xids(current_connection_id());
        if let Some(xid) = self.graph_insert_xid() {
            xids.insert(xid);
        }
        xids
    }

    /// `true` when the given connection id has an open `BEGIN`. Issue
    /// #760 — `OpenStream` consults this to refuse output streams that
    /// would otherwise collide with an interactive transaction (see
    /// ADR 0029 "Transaction interaction"). HTTP requests pre-dating the
    /// connection-id plumbing run with id `0`, which never carries a
    /// transaction context, so this returns `false` on those paths.
    pub fn connection_in_transaction(&self, conn_id: u64) -> bool {
        self.inner.transaction_state.in_transaction(conn_id)
    }

    /// Access the shared `SnapshotManager` — useful for VACUUM to compute
    /// the oldest-active xid when reclaiming dead tuples.
    pub fn snapshot_manager(&self) -> Arc<crate::storage::transaction::snapshot::SnapshotManager> {
        self.inner.transaction_state.snapshot_manager()
    }

    pub(crate) fn mvcc_vacuum_cutoff_xid(&self) -> crate::storage::transaction::snapshot::Xid {
        let manager = self.inner.transaction_state.snapshot_manager();
        let next_xid = manager.peek_next_xid();
        let mut cutoff = next_xid;
        if let Some(oldest_required) = manager.oldest_required_xid() {
            cutoff = cutoff.min(oldest_required);
        }
        let retention_xids = self.config_u64("runtime.mvcc.vacuum_retention_xids", 0);
        if retention_xids > 0 {
            cutoff = cutoff.min(next_xid.saturating_sub(retention_xids));
        }
        cutoff
    }

    /// Own-tx xids (parent + open/released savepoints) for the current
    /// connection. Transports + tests that build a `SnapshotContext`
    /// manually (outside the `execute_query` scope) need this set so
    /// the writer's own uncommitted tuples stay visible to self.
    pub fn current_txn_own_xids(
        &self,
    ) -> std::collections::HashSet<crate::storage::transaction::snapshot::Xid> {
        self.inner
            .transaction_state
            .own_xids(current_connection_id())
    }
}
