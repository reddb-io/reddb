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

impl RedDBRuntime {
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
    ) {
        self.inner
            .pending_tombstones
            .write()
            .entry(conn_id)
            .or_default()
            .push((collection.to_string(), id, stamper_xid, previous_xmax));
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

    pub(crate) fn with_deferred_store_wal_if_transaction<T>(
        &self,
        f: impl FnOnce() -> RedDBResult<T>,
    ) -> RedDBResult<T> {
        let conn_id = current_connection_id();
        if !self.inner.tx_contexts.read().contains_key(&conn_id) {
            return f();
        }

        crate::storage::UnifiedStore::begin_deferred_store_wal_capture();
        let result = f();
        let captured = crate::storage::UnifiedStore::take_deferred_store_wal_capture();
        match result {
            Ok(value) => {
                self.record_pending_store_wal_actions(conn_id, captured);
                Ok(value)
            }
            Err(err) => Err(err),
        }
    }

    pub(crate) fn with_deferred_store_wal_for_dml<T>(
        &self,
        capture_autocommit_events: bool,
        f: impl FnOnce() -> RedDBResult<T>,
    ) -> RedDBResult<T> {
        let conn_id = current_connection_id();
        if self.inner.tx_contexts.read().contains_key(&conn_id) {
            return self.with_deferred_store_wal_if_transaction(f);
        }
        if !capture_autocommit_events {
            return f();
        }

        crate::storage::UnifiedStore::begin_deferred_store_wal_capture();
        let result = f();
        let captured = crate::storage::UnifiedStore::take_deferred_store_wal_capture();
        self.inner
            .db
            .store()
            .append_deferred_store_wal_actions(captured)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
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

    fn record_pending_store_wal_actions(
        &self,
        conn_id: u64,
        actions: crate::storage::unified::DeferredStoreWalActions,
    ) {
        if actions.is_empty() {
            return;
        }
        let mut guard = self.inner.pending_store_wal_actions.write();
        guard.entry(conn_id).or_default().extend(actions);
    }

    pub(crate) fn flush_pending_store_wal_actions(&self, conn_id: u64) -> RedDBResult<()> {
        let Some(actions) = self
            .inner
            .pending_store_wal_actions
            .write()
            .remove(&conn_id)
        else {
            return Ok(());
        };
        self.inner
            .db
            .store()
            .append_deferred_store_wal_actions(actions)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub(crate) fn discard_pending_store_wal_actions(&self, conn_id: u64) {
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
            && !self.inner.snapshot_manager.is_aborted(xid)
            && !self.inner.snapshot_manager.is_active(xid)
            && (xid > snapshot.xid || snapshot.in_progress.contains(&xid))
    }

    fn conflict_error(
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
    ) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(());
        };

        for candidate in manager.query_all(|_| true) {
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
            serializable_write_set.insert((collection.clone(), logical_id));
            if self.xid_conflicts_with_snapshot(previous_xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, previous_xmax));
            }
            if old.xmax != xid && self.xid_conflicts_with_snapshot(old.xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, old.xmax));
            }
            self.check_logical_row_conflict(
                &collection,
                logical_id,
                &[old_id, new_id],
                snapshot,
                own_xids,
            )?;
        }

        for (collection, id, xid, previous_xmax) in tombstones {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            let Some(entity) = manager.get(id) else {
                continue;
            };
            let logical_id = entity.logical_id();
            serializable_write_set.insert((collection.clone(), logical_id));
            if self.xid_conflicts_with_snapshot(previous_xmax, snapshot, own_xids) {
                return Err(Self::conflict_error(&collection, logical_id, previous_xmax));
            }
            if entity.xmax != xid
                && self.xid_conflicts_with_snapshot(entity.xmax, snapshot, own_xids)
            {
                return Err(Self::conflict_error(&collection, logical_id, entity.xmax));
            }
            self.check_logical_row_conflict(&collection, logical_id, &[id], snapshot, own_xids)?;
        }

        if isolation == crate::storage::transaction::IsolationLevel::Serializable
            && self
                .inner
                .snapshot_manager
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
        for (collection, old_id, _new_id, xid, _previous_xmax) in versioned_updates {
            if let Some(manager) = store.get_collection(&collection) {
                if let Some(mut entity) = manager.get(old_id) {
                    entity.set_xmax(xid);
                    let _ = manager.update(entity);
                }
            }
        }
        for (collection, id, xid, _previous_xmax) in tombstones {
            if let Some(manager) = store.get_collection(&collection) {
                if let Some(mut entity) = manager.get(id) {
                    entity.set_xmax(xid);
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

    pub(crate) fn revive_pending_versioned_updates(&self, conn_id: u64) {
        let Some(pending) = self
            .inner
            .pending_versioned_updates
            .write()
            .remove(&conn_id)
        else {
            return;
        };

        let store = self.inner.db.store();
        for (collection, old_id, new_id, xid, previous_xmax) in pending {
            if let Some(manager) = store.get_collection(&collection) {
                if let Some(mut old) = manager.get(old_id) {
                    if old.xmax == xid {
                        old.set_xmax(previous_xmax);
                        let _ = manager.update(old);
                    }
                }
            }
            let _ = store.delete_batch(&collection, &[new_id]);
        }
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
        for (collection, id, _xid, _previous_xmax) in pending {
            store.context_index().remove_entity(id);
            self.cdc_emit(
                crate::replication::cdc::ChangeOperation::Delete,
                &collection,
                id.raw(),
                "entity",
            );
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
        for (collection, id, xid, previous_xmax) in pending {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            if let Some(mut entity) = manager.get(id) {
                if entity.xmax == xid {
                    entity.set_xmax(previous_xmax);
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
        for event in pending {
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
    /// open transaction. Used by graph / vector / queue / timeseries
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
        let mut guard = self.inner.pending_tombstones.write();
        let Some(pending) = guard.get_mut(&conn_id) else {
            return 0;
        };

        let store = self.inner.db.store();
        let mut revived = 0usize;
        pending.retain(|(collection, id, xid, previous_xmax)| {
            if *xid < stamper_xid {
                // Stamped before the savepoint — keep in queue.
                return true;
            }
            if let Some(manager) = store.get_collection(collection) {
                if let Some(mut entity) = manager.get(*id) {
                    if entity.xmax == *xid {
                        entity.set_xmax(*previous_xmax);
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
    /// * If the connection is inside a BEGIN-wrapped transaction, reuse
    ///   the snapshot stored in its `TxnContext`.
    /// * Otherwise (autocommit), capture a fresh snapshot tied to an
    ///   implicit xid=0 — the read path treats pre-MVCC rows as always
    ///   visible so this degrades to "see everything committed".
    pub fn current_snapshot(&self) -> crate::storage::transaction::snapshot::Snapshot {
        let conn_id = current_connection_id();
        if let Some(ctx) = self.inner.tx_contexts.read().get(&conn_id).cloned() {
            return ctx.snapshot;
        }
        // Autocommit: take a fresh snapshot bounded by `peek_next_xid` so
        // every already-committed xid (which is strictly less) passes the
        // `xmin <= snap.xid` gate, while concurrently-active xids land in
        // the `in_progress` set and stay hidden until they commit. Using
        // xid=0 would incorrectly hide every MVCC-stamped tuple.
        let high_water = self.inner.snapshot_manager.peek_next_xid();
        self.inner.snapshot_manager.snapshot(high_water)
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
        let conn_id = current_connection_id();
        self.inner
            .tx_contexts
            .read()
            .get(&conn_id)
            .map(|ctx| ctx.writer_xid())
    }

    /// `true` when the given connection id has an open `BEGIN`. Issue
    /// #760 — `OpenStream` consults this to refuse output streams that
    /// would otherwise collide with an interactive transaction (see
    /// ADR 0029 "Transaction interaction"). HTTP requests pre-dating the
    /// connection-id plumbing run with id `0`, which never carries a
    /// transaction context, so this returns `false` on those paths.
    pub fn connection_in_transaction(&self, conn_id: u64) -> bool {
        self.inner.tx_contexts.read().contains_key(&conn_id)
    }

    /// Access the shared `SnapshotManager` — useful for VACUUM to compute
    /// the oldest-active xid when reclaiming dead tuples.
    pub fn snapshot_manager(&self) -> Arc<crate::storage::transaction::snapshot::SnapshotManager> {
        Arc::clone(&self.inner.snapshot_manager)
    }

    pub(crate) fn mvcc_vacuum_cutoff_xid(&self) -> crate::storage::transaction::snapshot::Xid {
        let manager = &self.inner.snapshot_manager;
        let next_xid = manager.peek_next_xid();
        let mut cutoff = next_xid;
        if let Some(oldest_active) = manager.oldest_active_xid() {
            cutoff = cutoff.min(oldest_active);
        }
        if let Some(oldest_pinned) = manager.oldest_pinned_xid() {
            cutoff = cutoff.min(oldest_pinned);
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
        let mut set = std::collections::HashSet::new();
        if let Some(ctx) = self.inner.tx_contexts.read().get(&current_connection_id()) {
            set.insert(ctx.xid);
            for (_, sub) in &ctx.savepoints {
                set.insert(*sub);
            }
            for sub in &ctx.released_sub_xids {
                set.insert(*sub);
            }
        }
        set
    }
}
