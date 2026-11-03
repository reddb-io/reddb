//! Tombstone rollback ownership through the durable autocommit boundary.
use super::memory_admission::OwnedMemoryReservation;
use super::*;
use std::cell::RefCell;

thread_local! {
    static DEFERRED_DELETES: RefCell<Option<Vec<DeletePublication>>> = const { RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn pending_lock_keys(runtime: &RedDBRuntime, collection: &str) -> Vec<String> {
    DEFERRED_DELETES.with(|slot| {
        slot.borrow()
            .as_ref()
            .into_iter()
            .flatten()
            .filter(|pending| {
                pending.collection == collection
                    && Arc::ptr_eq(&pending.runtime.inner, &runtime.inner)
            })
            .flat_map(|pending| pending.row_locks.iter().map(|(key, _)| key.clone()))
            .collect()
    })
}

use std::rc::Rc;

pub(super) type DeleteRowLocks = Vec<(String, Rc<super::OwnedRmwRowLockGuard>)>;

pub(super) fn event_writer(runtime: &RedDBRuntime) -> Option<u64> {
    DEFERRED_DELETES.with(|slot| {
        slot.borrow()
            .as_ref()?
            .iter()
            .rev()
            .find(|deletion| Arc::ptr_eq(&deletion.runtime.inner, &runtime.inner))
            .map(|deletion| deletion.stamp)
    })
}

/// Outbox inserts inherit the DELETE writer, so neither readers nor a
/// checkpoint can observe a false event from a failed WAL append. The temporary
/// own-writer context also preserves queue ordering across captured batches.
pub(super) fn event_context(
    runtime: &RedDBRuntime,
) -> Option<super::execution_context::SnapshotContext> {
    event_writer(runtime)?;
    let mut context = super::execution_context::capture_current_snapshot()?;
    DEFERRED_DELETES.with(|slot| {
        for deletion in slot.borrow().as_ref().expect("delete event scope") {
            if Arc::ptr_eq(&deletion.runtime.inner, &runtime.inner) {
                context.own_xids.insert(deletion.stamp);
            }
        }
    });
    Some(context)
}

/// Captured callbacks retain earlier batches' locks. Establish one global
/// order before splitting targets, sharing UPDATE's exact identity key.
pub(super) fn order_target_ids(
    runtime: &RedDBRuntime,
    collection: &str,
    mut ids: Vec<EntityId>,
) -> RedDBResult<Vec<EntityId>> {
    let store = runtime.db().store();
    let manager = store
        .get_collection(collection)
        .ok_or_else(|| RedDBError::NotFound(collection.into()))?;
    // A decimal u64 lock key fits in 24 bytes. Allow 64 bytes of capacity
    // per key plus its slot; reuse the incoming ID vector for the result.
    let bytes =
        (ids.len() as u64).saturating_mul((std::mem::size_of::<(String, EntityId)>() + 64) as u64);
    let _scratch = runtime.admit_non_evictable_growth(
        crate::storage::memory_pools::MemoryPool::SegmentArena,
        "delete lock ordering",
        bytes,
    )?;
    let mut ordered = Vec::with_capacity(ids.len());
    manager.try_for_each_id(&ids, usize::MAX, |_, entity| {
        ordered.push((super::RmwLockTable::row_key(entity.logical_id()), entity.id));
        true
    })?;
    ordered.sort_unstable_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let retained = ordered.len();
    for (destination, (_, id)) in ids.iter_mut().zip(ordered) {
        *destination = id;
    }
    ids.truncate(retained);
    Ok(ids)
}

/// Use UPDATE's same logical-row locks, in the same global order, before
/// taking the store publication fence. Captured batches retain their locks.
pub(super) fn lock_delete_rows(
    runtime: &RedDBRuntime,
    collection: &str,
    manager: &crate::storage::SegmentManager,
    ids: &[EntityId],
) -> RedDBResult<DeleteRowLocks> {
    let mut keys = Vec::with_capacity(ids.len());
    for &id in ids {
        if let Some(entity) = manager.try_get(id, usize::MAX)? {
            keys.push(super::RmwLockTable::row_key(entity.logical_id()));
        }
    }
    keys.sort_unstable();
    keys.dedup();
    let mut locks = Vec::with_capacity(keys.len());
    for key in keys {
        let previous = DEFERRED_DELETES.with(|slot| {
            slot.borrow()
                .as_ref()
                .into_iter()
                .flatten()
                .filter(|pending| {
                    pending.collection == collection
                        && Arc::ptr_eq(&pending.runtime.inner, &runtime.inner)
                })
                .find_map(|pending| {
                    pending
                        .row_locks
                        .iter()
                        .find(|(held, _)| held == &key)
                        .map(|(_, guard)| Rc::clone(guard))
                })
        });
        let guard = previous.unwrap_or_else(|| {
            Rc::new(
                runtime
                    .inner
                    .rmw_locks
                    .lock_for(collection, &key)
                    .lock_owned(),
            )
        });
        locks.push((key, guard));
    }
    Ok(locks)
}

pub(super) struct DeletePublication {
    runtime: RedDBRuntime,
    collection: String,
    manager: Arc<crate::storage::SegmentManager>,
    preimages: Vec<UnifiedEntity>,
    _reservations: Vec<Rc<OwnedMemoryReservation>>,
    row_locks: DeleteRowLocks,
    autocommit_xid: Option<u64>,
    stamp: u64,
    published: usize,
    finished: bool,
}

impl DeletePublication {
    pub(super) fn new(
        runtime: &RedDBRuntime,
        collection: &str,
        manager: Arc<crate::storage::SegmentManager>,
        preimages: Vec<UnifiedEntity>,
        reservations: Vec<Rc<OwnedMemoryReservation>>,
        active_xid: Option<u64>,
        row_locks: DeleteRowLocks,
    ) -> Self {
        let autocommit_xid = active_xid
            .is_none()
            .then(|| runtime.snapshot_manager().begin());
        Self {
            runtime: runtime.clone(),
            collection: collection.into(),
            manager,
            preimages,
            _reservations: reservations,
            row_locks,
            autocommit_xid,
            stamp: active_xid.or(autocommit_xid).expect("delete has a writer"),
            published: 0,
            finished: false,
        }
    }

    pub(super) fn apply(&mut self) -> RedDBResult<Vec<UnifiedEntity>> {
        let mut postimages = Vec::with_capacity(self.preimages.len());
        for original in &self.preimages {
            let mut entity = original.clone();
            entity.set_xmax(self.stamp);
            self.manager
                .update(entity.clone())
                .map_err(|e| RedDBError::Internal(e.to_string()))?;
            self.published += 1;
            #[cfg(test)]
            self.runtime.index_store_ref().mutation_test_hook(
                &self.collection,
                super::index_store::MutationTestPhase::StoragePublished,
            );
            postimages.push(entity);
        }
        Ok(postimages)
    }

    pub(super) fn finish_transaction(mut self, connection_id: u64) {
        for original in &self.preimages {
            self.runtime.record_pending_tombstone(
                connection_id,
                &self.collection,
                original.id,
                self.stamp,
                original.xmax,
            );
        }
        self.finished = true;
    }

    pub(super) fn finish_autocommit(self) -> RedDBResult<()> {
        DEFERRED_DELETES.with(|slot| {
            if let Some(pending) = slot.borrow_mut().as_mut() {
                pending.push(self);
                Ok(())
            } else {
                self.commit()
            }
        })
    }

    /// Explicit constant UPDATEs do not retain the row RMW lock. Reject their
    /// interference before WAL, including an active competitor: this avoids
    /// acknowledging a DELETE whose stamp has already been replaced. Aborted
    /// competing stamps may safely be replaced under the publication fence.
    fn prepare(&mut self) -> RedDBResult<()> {
        let snapshots = self.runtime.snapshot_manager();
        for original in &self.preimages[..self.published] {
            let mut current = self
                .manager
                .try_get(original.id, usize::MAX)?
                .ok_or_else(|| {
                    RedDBRuntime::conflict_error(
                        &self.collection,
                        original.logical_id(),
                        self.stamp,
                    )
                })?;
            if current.xmin != original.xmin || current.logical_id() != original.logical_id() {
                return Err(RedDBRuntime::conflict_error(
                    &self.collection,
                    original.logical_id(),
                    current.xmin,
                ));
            }
            for xid in [original.xmax, current.xmax] {
                if xid != 0 && xid != self.stamp && !snapshots.is_aborted(xid) {
                    return Err(RedDBRuntime::conflict_error(
                        &self.collection,
                        original.logical_id(),
                        xid,
                    ));
                }
            }
            if current.xmax != self.stamp {
                current.set_xmax(self.stamp);
                self.manager
                    .update(current)
                    .map_err(|error| RedDBError::Internal(error.to_string()))?;
            }
        }
        Ok(())
    }

    fn commit(mut self) -> RedDBResult<()> {
        let store = self.runtime.db().store();
        let _publication = store.vault_publication_lock.lock();
        self.runtime
            .snapshot_manager()
            .commit(self.autocommit_xid.expect("autocommit writer"));
        // Durable visibility is irreversible even if a later index repair fails.
        self.finished = true;
        let mut failure = None;
        for original in &self.preimages {
            let fields = crate::application::ports::entity_row_fields_snapshot(original);
            if !fields.is_empty() {
                if let Err(error) = self.runtime.index_store_ref().index_entity_delete(
                    &self.collection,
                    original.id,
                    &fields,
                ) {
                    failure.get_or_insert(RedDBError::Internal(error));
                }
            }
            store.context_index().remove_entity(original.id);
        }
        failure.map_or(Ok(()), Err)
    }

    pub(super) fn abort(&mut self) -> RedDBResult<()> {
        let store = self.runtime.db().store();
        let _publication = store.vault_publication_lock.lock();
        let mut failure = None;
        for original in self.preimages[..self.published].iter().rev() {
            if self
                .manager
                .get_with(original.id, |entity| entity.xmax == self.stamp)
                == Some(true)
            {
                if let Err(error) = self.manager.update(original.clone()) {
                    failure.get_or_insert(RedDBError::Internal(format!(
                        "delete rollback failed: {error}"
                    )));
                }
            }
        }
        if let Some(xid) = self.autocommit_xid {
            self.runtime.snapshot_manager().rollback(xid);
        }
        self.finished = true;
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for DeletePublication {
    fn drop(&mut self) {
        if !self.finished {
            if let Err(error) = self.abort() {
                tracing::error!(%error, "delete publication rollback failed");
            }
        }
    }
}

pub(super) struct DeletePublicationScope {
    active: bool,
}

impl DeletePublicationScope {
    pub(super) fn begin() -> Self {
        DEFERRED_DELETES.with(|slot| {
            assert!(slot.borrow().is_none(), "delete WAL scopes cannot nest");
            *slot.borrow_mut() = Some(Vec::new());
        });
        Self { active: true }
    }

    pub(super) fn has_pending(&self) -> bool {
        DEFERRED_DELETES.with(|slot| {
            slot.borrow()
                .as_ref()
                .is_some_and(|pending| !pending.is_empty())
        })
    }

    /// Take topology readers only after callbacks finish. Hold them and the
    /// checkpoint fence through validation, WAL append and visibility/index
    /// publication. Any failure drops the rollback owner inside that fence.
    pub(super) fn publish(
        self,
        runtime: &RedDBRuntime,
        append: impl FnOnce() -> RedDBResult<()>,
    ) -> RedDBResult<()> {
        let topologies = DEFERRED_DELETES.with(|slot| {
            let pending = slot.borrow();
            let mut topologies = std::collections::BTreeMap::new();
            for deletion in pending.as_ref().expect("delete WAL scope") {
                topologies
                    .entry(deletion.collection.clone())
                    .or_insert_with(|| {
                        deletion
                            .runtime
                            .index_store_ref()
                            .collection_topology_lock(&deletion.collection)
                    });
            }
            topologies.into_values().collect::<Vec<_>>()
        });
        let _topology_guards: Vec<_> = topologies
            .iter()
            .map(|lock| lock.read_recursive())
            .collect();
        let store = runtime.db().store();
        let _publication = store.vault_publication_lock.lock();
        (|| {
            DEFERRED_DELETES.with(|slot| {
                for deletion in slot.borrow_mut().as_mut().expect("delete WAL scope") {
                    deletion.prepare()?;
                }
                Ok::<(), RedDBError>(())
            })?;
            append()?;
            self.commit()
        })()
    }

    fn commit(mut self) -> RedDBResult<()> {
        let pending =
            DEFERRED_DELETES.with(|slot| slot.borrow_mut().take().expect("delete WAL scope"));
        self.active = false;
        let mut failure = None;
        for deletion in pending {
            if let Err(error) = deletion.commit() {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

impl Drop for DeletePublicationScope {
    fn drop(&mut self) {
        if self.active {
            let pending = DEFERRED_DELETES.with(|slot| slot.borrow_mut().take());
            if let Some(pending) = pending {
                for deletion in pending.into_iter().rev() {
                    drop(deletion);
                }
            }
        }
    }
}
