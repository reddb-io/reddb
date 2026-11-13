//! Queue payloads participate in VCS. Delivery leases belong to one writer
//! epoch and are never copied by merge or replayed when moving HEAD.
use super::execution_context::{capture_current_snapshot, CurrentSnapshotGuard};
use super::RedDBRuntime;
use crate::storage::unified::store::RowPublication;
use crate::storage::{EntityId, UnifiedEntity, UnifiedStore};
use crate::{RedDBError, RedDBResult};
use reddb_types::Value;
use std::cell::RefCell;

const EPOCH: &str = "_vcs_queue_epoch";
const META: &str = super::queue_storage_index::META_COLLECTION;

struct Scope {
    runtime: RedDBRuntime,
    error: Option<String>,
}

thread_local! {
    static SCOPE: RefCell<Option<Scope>> = const { RefCell::new(None) };
}

struct ScopeGuard(Option<Scope>);

impl Drop for ScopeGuard {
    fn drop(&mut self) {
        SCOPE.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

pub(super) fn runtime(store: &UnifiedStore) -> Option<RedDBRuntime> {
    SCOPE.with(|slot| {
        slot.borrow().as_ref().and_then(|scope| {
            std::ptr::eq(scope.runtime.db().store().as_ref(), store).then(|| scope.runtime.clone())
        })
    })
}

/// QueueLifecycle has legacy Option/Vec read methods. An I/O failure in those
/// methods must fail the enclosing versioned operation, never publish a partial
/// result or a successful no-op.
pub(super) fn record_error(error: impl std::fmt::Display) {
    SCOPE.with(|slot| {
        if let Some(scope) = slot.borrow_mut().as_mut() {
            scope.error.get_or_insert_with(|| error.to_string());
        }
    });
}

pub(super) fn with_operation<T>(
    runtime: &RedDBRuntime,
    collections: &[&str],
    write: bool,
    run: impl FnOnce() -> RedDBResult<T>,
) -> RedDBResult<T> {
    let mut versioned = None;
    for collection in collections {
        if runtime.vcs_is_versioned(collection)? {
            versioned = Some(*collection);
            break;
        }
    }
    let Some(collection) = versioned else {
        return run();
    };
    let store = runtime.db().store();
    let _publication = store.vault_publication_lock.lock();
    let (mut snapshot, _retention) = if write {
        runtime.vcs_state_write_snapshot(collection)?
    } else {
        runtime.native_read_snapshot()?
    };
    let execute = || {
        snapshot.own_xids = runtime.own_transaction_xids();
        let _snapshot = CurrentSnapshotGuard::install(snapshot);
        let _scope = ScopeGuard(SCOPE.with(|slot| {
            slot.replace(Some(Scope {
                runtime: runtime.clone(),
                error: None,
            }))
        }));
        let result = run();
        let error = SCOPE.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .and_then(|scope| scope.error.take())
        });
        match error {
            Some(error) => Err(RedDBError::Internal(error)),
            None => result,
        }
    };
    if write {
        runtime.with_versioned_graph_insert(execute)
    } else {
        execute()
    }
}

fn operational(row: &crate::storage::unified::entity::RowData) -> bool {
    matches!(row.get_field("kind"), Some(Value::Text(kind)) if !matches!(kind.as_ref(), "queue_config" | "queue_group"))
}

pub(super) fn metadata_visible(entity: &UnifiedEntity) -> bool {
    let snapshot = capture_current_snapshot();
    let epoch = snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.branch.as_ref())
        .map_or(0, |view| view.writer_epoch);
    if let Some(Value::UnsignedInteger(owner)) =
        entity.data.as_row().and_then(|row| row.get_field(EPOCH))
    {
        if *owner != epoch {
            return false;
        }
    }
    super::execution_context::entity_visible_with_context(snapshot.as_ref(), entity)
}

pub(super) fn metadata_rows(
    store: &UnifiedStore,
    predicate: impl Fn(&crate::storage::unified::entity::RowData) -> bool,
) -> RedDBResult<Vec<(EntityId, crate::storage::unified::entity::RowData)>> {
    let Some(manager) = store.get_collection(META) else {
        return Ok(Vec::new());
    };
    let mut rows = Vec::new();
    manager.try_for_each_entity(usize::MAX, |entity| {
        if metadata_visible(entity) {
            if let Some(row) = entity.data.as_row().filter(|row| predicate(row)) {
                rows.push((entity.id, row.clone()));
            }
        }
        true
    })?;
    Ok(rows)
}

pub(super) fn stamp_entity(
    store: &UnifiedStore,
    collection: &str,
    entity: &mut UnifiedEntity,
) -> RedDBResult<()> {
    let Some(runtime) = runtime(store) else {
        return Ok(());
    };
    let is_metadata = collection == META || collection == super::impl_queue::QUEUE_META_COLLECTION;
    let parent = if is_metadata {
        let Some(row) = entity.data.as_row().filter(|row| operational(row)) else {
            return Ok(());
        };
        let Some(Value::Text(queue)) = row.get_field("queue") else {
            return Ok(());
        };
        queue.to_string()
    } else {
        collection.to_string()
    };
    let epoch = runtime.vcs_writer_epoch(&parent)?;
    if is_metadata {
        if let Some(epoch) = epoch {
            if let crate::storage::EntityData::Row(row) = &mut entity.data {
                row.named
                    .as_mut()
                    .expect("queue metadata uses named rows")
                    .insert(EPOCH.into(), Value::UnsignedInteger(epoch));
            }
        }
    } else if let Some(epoch) = epoch {
        entity.set_branch_origin(epoch);
    }
    if let Some(xid) = runtime.current_xid().or_else(|| runtime.graph_insert_xid()) {
        entity.set_xmin(xid);
    }
    entity.field_bloom = reddb_engine::storage::entity::compute_entity_field_bloom(&entity.data);
    Ok(())
}

/// Returns false outside a versioned queue operation, preserving the ordinary
/// queue's existing deletion path. Versioned operations retain physical rows.
pub(super) fn retire(store: &UnifiedStore, collection: &str, id: EntityId) -> RedDBResult<bool> {
    let Some(runtime) = runtime(store) else {
        return Ok(false);
    };
    let xid = runtime
        .current_xid()
        .or_else(|| runtime.graph_insert_xid())
        .ok_or_else(|| {
            RedDBError::InvalidOperation("queue mutation requires a WAL writer".into())
        })?;
    let Some(mut entity) = store.try_get(collection, id, usize::MAX)? else {
        return Ok(true);
    };
    let snapshot = capture_current_snapshot();
    if !super::execution_context::entity_visible_with_context(snapshot.as_ref(), &entity) {
        return Err(RedDBError::NotFound(format!(
            "queue version {id} is not visible"
        )));
    }
    let epoch = if collection == META {
        None
    } else {
        runtime.vcs_writer_epoch(collection)?
    };
    let previous = epoch.map_or(entity.xmax, |epoch| entity.branch_xmax(epoch));
    if let Some(epoch) = epoch {
        entity.set_branch_xmax(epoch, xid);
    } else {
        entity.set_xmax(xid);
    }
    store
        .get_collection(collection)
        .expect("queue version collection")
        .update(entity.clone())
        .map_err(|error| RedDBError::Internal(error.to_string()))?;
    store.persist_entities_to_pager_wal_only(collection, &[entity])?;
    if runtime.current_xid().is_some() {
        runtime.record_pending_tombstone(
            super::execution_context::current_connection_id(),
            collection,
            id,
            xid,
            previous,
            epoch,
        );
    }
    Ok(true)
}

pub(super) fn delete_metadata(store: &UnifiedStore, id: EntityId) -> RedDBResult<()> {
    let result = (|| {
        if !retire(store, META, id)? {
            store.delete(META, id)?;
        }
        Ok(())
    })();
    if let Err(error) = &result {
        record_error(error);
    }
    result
}

pub(super) fn stage_metadata_scope(
    store: &UnifiedStore,
    publication: &mut RowPublication,
    queue: &str,
    enabled: bool,
) -> RedDBResult<()> {
    let Some(manager) = store.get_collection(META) else {
        return Ok(());
    };
    let mut entities = Vec::new();
    manager.try_for_each_entity(usize::MAX, |entity| {
        if entity.data.as_row().is_some_and(|row| operational(row) && matches!(row.get_field("queue"), Some(Value::Text(name)) if name.as_ref() == queue)) {
            entities.push(entity.clone());
        }
        true
    })?;
    for mut entity in entities {
        let crate::storage::EntityData::Row(row) = &mut entity.data else {
            unreachable!()
        };
        let fields = row.named.as_mut().expect("queue metadata uses named rows");
        if enabled {
            fields
                .entry(EPOCH.into())
                .or_insert(Value::UnsignedInteger(0));
        } else {
            fields.remove(EPOCH);
        }
        entity.field_bloom =
            reddb_engine::storage::entity::compute_entity_field_bloom(&entity.data);
        let metadata = store.try_get_metadata(META, entity.id, usize::MAX)?;
        publication
            .replacements
            .push((META.into(), entity, metadata));
    }
    Ok(())
}
