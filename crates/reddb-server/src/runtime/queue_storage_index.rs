//! Durable internal queue postings. Source rows remain authoritative.

use super::RedDBRuntime;
use crate::catalog::CollectionModel;
use crate::storage::unified::store::secondary_index::{
    SecondaryIndexIdentity, SecondaryIndexProjector, SECONDARY_KEY_BYTES_MAX,
};
use crate::storage::{
    EntityData, EntityId, EntityKind, RowData, SegmentManager, StoreError, UnifiedEntity,
    UnifiedStore,
};
use crate::{RedDBError, RedDBResult};
use reddb_types::Value;
use sha2::{Digest, Sha256};
use std::ops::Bound;
use std::sync::Arc;

pub(super) const META_COLLECTION: &str = "red.queue.meta";
const CAPACITY: &str = "\0reddb/queue-capacity-v1";
const CAPACITY_DEFINITION: &[u8] = b"queue-capacity-v1:message:xmin:xmax:hidden";
const POSITION_ASC: &str = "\0reddb/queue-position-asc-v1";
const POSITION_DESC: &str = "\0reddb/queue-position-desc-v1";
const META_MESSAGE: &str = "\0reddb/queue-meta-message-v1";
const META_GROUP: &str = "\0reddb/queue-meta-group-v1";
const META_DELIVERY: &str = "\0reddb/queue-meta-delivery-v1";
const META_DEDUP: &str = "\0reddb/queue-dedup-v1";
const META_DEDUP_EXPIRY: &str = "\0reddb/queue-dedup-expiry-v1";
const META_DEDUP_DEFINITION: &[u8] = b"queue-dedup-v1:queue-sha256:key-sha256";
const META_DEDUP_EXPIRY_DEFINITION: &[u8] =
    b"queue-dedup-v1:queue-sha256:expiry-be64:missing-zero:id-be64";
const META_DEADLINE: &str = "\0reddb/queue-meta-deadline-v1";
const POSITION_ASC_DEFINITION: &[u8] = b"queue-position-v1:position-be64:id-be64";
const POSITION_DESC_DEFINITION: &[u8] = b"queue-position-v1:not-position-be64:not-id-be64";
const META_MESSAGE_DEFINITION: &[u8] =
    b"queue-meta-v1:kind-sha256:queue-sha256:message-be64:group-sha256";
const META_GROUP_DEFINITION: &[u8] =
    b"queue-meta-v1:kind-sha256:queue-sha256:group-sha256:message-be64";
const META_DELIVERY_DEFINITION: &[u8] = b"queue-meta-v1:delivery-sha256";
const META_DEADLINE_DEFINITION: &[u8] = b"queue-meta-v1:queue-sha256:deadline-be64";

fn identity(definition: &[u8]) -> SecondaryIndexIdentity {
    SecondaryIndexIdentity {
        codec_version: 1,
        definition: Sha256::digest(definition).into(),
    }
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

fn row_text(row: &RowData, field: &str) -> Option<String> {
    match row.get_field(field)? {
        Value::Text(value) => Some(value.to_string()),
        _ => None,
    }
}

fn row_u64(row: &RowData, field: &str) -> Option<u64> {
    match row.get_field(field)? {
        Value::UnsignedInteger(value) => Some(*value),
        Value::Integer(value) => u64::try_from(*value).ok(),
        _ => None,
    }
}

struct PositionProjection {
    descending: bool,
}

impl SecondaryIndexProjector for PositionProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        let EntityKind::QueueMessage { position, .. } = entity.kind else {
            return Ok(None);
        };
        if !matches!(entity.data, EntityData::QueueMessage(_)) {
            return Ok(None);
        }
        key[0] = 1;
        let position = if self.descending { !position } else { position };
        let id = if self.descending {
            !entity.id.raw()
        } else {
            entity.id.raw()
        };
        key[1..9].copy_from_slice(&position.to_be_bytes());
        key[9..17].copy_from_slice(&id.to_be_bytes());
        Ok(Some(17))
    }
}

#[derive(Clone, Copy)]
enum MetaOrder {
    Message,
    Group,
}

struct MetaSubjectProjection {
    order: MetaOrder,
}

impl SecondaryIndexProjector for MetaSubjectProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        let Some(row) = entity.data.as_row() else {
            return Ok(None);
        };
        let (Some(kind), Some(queue)) = (row_text(row, "kind"), row_text(row, "queue")) else {
            return Ok(None);
        };
        let group = row_text(row, "group").unwrap_or_default();
        let message = row_u64(row, "message_id").unwrap_or(0);
        key[0] = 1;
        key[1..33].copy_from_slice(&digest(&kind));
        key[33..65].copy_from_slice(&digest(&queue));
        match self.order {
            MetaOrder::Message => {
                key[65..73].copy_from_slice(&message.to_be_bytes());
                key[73..105].copy_from_slice(&digest(&group));
            }
            MetaOrder::Group => {
                key[65..97].copy_from_slice(&digest(&group));
                key[97..105].copy_from_slice(&message.to_be_bytes());
            }
        }
        Ok(Some(105))
    }
}

struct MetaDeliveryProjection;

impl SecondaryIndexProjector for MetaDeliveryProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        let Some(delivery) = entity
            .data
            .as_row()
            .and_then(|row| row_text(row, "delivery_id"))
        else {
            return Ok(None);
        };
        key[0] = 1;
        key[1..33].copy_from_slice(&digest(&delivery));
        Ok(Some(33))
    }
}

struct MetaDeadlineProjection;

impl SecondaryIndexProjector for MetaDeadlineProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        let Some(row) = entity.data.as_row() else {
            return Ok(None);
        };
        let (Some(queue), Some(deadline)) =
            (row_text(row, "queue"), row_u64(row, "lock_deadline_ns"))
        else {
            return Ok(None);
        };
        key[0] = 1;
        key[1..33].copy_from_slice(&digest(&queue));
        key[33..41].copy_from_slice(&deadline.to_be_bytes());
        Ok(Some(41))
    }
}

struct DedupProjection {
    expiry: bool,
}

impl SecondaryIndexProjector for DedupProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        let Some((queue, dedup_key, expires)) =
            reddb_engine::storage::segment_key_lookup::SegmentKeyLookup::queue_dedup(entity)
        else {
            return Ok(None);
        };
        key[0] = 1;
        key[1..33].copy_from_slice(&digest(queue));
        if self.expiry {
            key[33..41].copy_from_slice(&expires.to_be_bytes());
            key[41..49].copy_from_slice(&entity.id.raw().to_be_bytes());
            Ok(Some(49))
        } else if let Some(dedup_key) = dedup_key {
            key[33..65].copy_from_slice(&digest(dedup_key));
            Ok(Some(65))
        } else {
            Ok(None)
        }
    }
}

struct CapacityProjection;

impl SecondaryIndexProjector for CapacityProjection {
    fn project(
        &self,
        entity: &UnifiedEntity,
        key: &mut [u8; SECONDARY_KEY_BYTES_MAX],
    ) -> Result<Option<usize>, StoreError> {
        if !matches!(entity.kind, EntityKind::QueueMessage { .. })
            || !matches!(entity.data, EntityData::QueueMessage(_))
        {
            return Ok(None);
        }
        key[..8].copy_from_slice(&entity.xmin.to_be_bytes());
        key[8..16].copy_from_slice(&entity.xmax.to_be_bytes());
        key[16] = u8::from(reddb_engine::storage::visibility::entity_moderation_hidden(
            entity,
        ));
        Ok(Some(17))
    }
}

fn enabled(runtime: &RedDBRuntime) -> bool {
    let store = runtime.db().store();
    store.config().resident_memory_bytes_max.is_some() && store.pager().is_some()
}

fn store_enabled(store: &UnifiedStore) -> bool {
    store.config().resident_memory_bytes_max.is_some() && store.pager().is_some()
}

pub(super) fn prepare(runtime: &RedDBRuntime) -> RedDBResult<()> {
    if !enabled(runtime) {
        return Ok(());
    }
    if runtime
        .db()
        .store()
        .get_collection(META_COLLECTION)
        .is_some()
    {
        prepare_meta(runtime)?;
    }
    for collection in runtime.db().catalog_model_snapshot().collections {
        if collection.model == CollectionModel::Queue {
            prepare_queue(runtime, &collection.name)?;
        }
    }
    Ok(())
}

pub(super) fn prepare_queue(runtime: &RedDBRuntime, queue: &str) -> RedDBResult<()> {
    let store = runtime.db().store();
    prepare_queue_store(store.as_ref(), queue)
}

pub(super) fn prepare_queue_store(store: &UnifiedStore, queue: &str) -> RedDBResult<()> {
    if !store_enabled(store) || store.get_collection(queue).is_none() {
        return Ok(());
    }
    store.register_secondary_index(
        queue,
        POSITION_ASC,
        identity(POSITION_ASC_DEFINITION),
        Arc::new(PositionProjection { descending: false }),
    )?;
    store.register_secondary_index(
        queue,
        POSITION_DESC,
        identity(POSITION_DESC_DEFINITION),
        Arc::new(PositionProjection { descending: true }),
    )?;
    store.register_secondary_index(
        queue,
        CAPACITY,
        identity(CAPACITY_DEFINITION),
        Arc::new(CapacityProjection),
    )?;
    Ok(())
}

pub(super) fn prepare_meta(runtime: &RedDBRuntime) -> RedDBResult<()> {
    let store = runtime.db().store();
    prepare_meta_store(store.as_ref())
}

pub(super) fn prepare_meta_store(store: &UnifiedStore) -> RedDBResult<()> {
    if !store_enabled(store) || store.get_collection(META_COLLECTION).is_none() {
        return Ok(());
    }
    store.register_secondary_index(
        META_COLLECTION,
        META_MESSAGE,
        identity(META_MESSAGE_DEFINITION),
        Arc::new(MetaSubjectProjection {
            order: MetaOrder::Message,
        }),
    )?;
    store.register_secondary_index(
        META_COLLECTION,
        META_GROUP,
        identity(META_GROUP_DEFINITION),
        Arc::new(MetaSubjectProjection {
            order: MetaOrder::Group,
        }),
    )?;
    store.register_secondary_index(
        META_COLLECTION,
        META_DELIVERY,
        identity(META_DELIVERY_DEFINITION),
        Arc::new(MetaDeliveryProjection),
    )?;
    store.register_secondary_index(
        META_COLLECTION,
        META_DEADLINE,
        identity(META_DEADLINE_DEFINITION),
        Arc::new(MetaDeadlineProjection),
    )?;
    store.register_secondary_index(
        META_COLLECTION,
        META_DEDUP,
        identity(META_DEDUP_DEFINITION),
        Arc::new(DedupProjection { expiry: false }),
    )?;
    store.register_secondary_index(
        META_COLLECTION,
        META_DEDUP_EXPIRY,
        identity(META_DEDUP_EXPIRY_DEFINITION),
        Arc::new(DedupProjection { expiry: true }),
    )?;
    Ok(())
}

fn prefix_end(mut prefix: Vec<u8>) -> Option<Vec<u8>> {
    let position = prefix.iter().rposition(|byte| *byte != u8::MAX)?;
    prefix[position] += 1;
    prefix.truncate(position + 1);
    Some(prefix)
}

fn subject_key(
    order: MetaOrder,
    kind: &str,
    queue: &str,
    group: Option<&str>,
    message_id: Option<u64>,
) -> Vec<u8> {
    let mut key = Vec::with_capacity(105);
    key.push(1);
    key.extend_from_slice(&digest(kind));
    key.extend_from_slice(&digest(queue));
    match order {
        MetaOrder::Message => {
            if let Some(message_id) = message_id {
                key.extend_from_slice(&message_id.to_be_bytes());
                if let Some(group) = group {
                    key.extend_from_slice(&digest(group));
                }
            }
        }
        MetaOrder::Group => {
            if let Some(group) = group {
                key.extend_from_slice(&digest(group));
                if let Some(message_id) = message_id {
                    key.extend_from_slice(&message_id.to_be_bytes());
                }
            }
        }
    }
    key
}

fn row_matches_subject(
    row: &RowData,
    kind: &str,
    queue: &str,
    group: Option<&str>,
    message_id: Option<u64>,
) -> bool {
    row_text(row, "kind").as_deref() == Some(kind)
        && row_text(row, "queue").as_deref() == Some(queue)
        && group.is_none_or(|group| row_text(row, "group").as_deref() == Some(group))
        && message_id.is_none_or(|id| row_u64(row, "message_id") == Some(id))
}

fn read_limit(manager: &SegmentManager) -> usize {
    manager.backing_read_bytes_max().unwrap_or(usize::MAX)
}

pub(super) fn subject_rows(
    runtime: &RedDBRuntime,
    kind: &str,
    queue: &str,
    group: Option<&str>,
    message_id: Option<u64>,
) -> RedDBResult<Option<Vec<(EntityId, RowData)>>> {
    let store = runtime.db().store();
    subject_rows_store(store.as_ref(), kind, queue, group, message_id)
}

pub(super) fn subject_rows_store(
    store: &UnifiedStore,
    kind: &str,
    queue: &str,
    group: Option<&str>,
    message_id: Option<u64>,
) -> RedDBResult<Option<Vec<(EntityId, RowData)>>> {
    let _publication = store.vault_publication_lock.lock();
    let manager = match store.get_collection(META_COLLECTION) {
        Some(manager) => manager,
        None => return Ok(Some(Vec::new())),
    };
    let (name, order) = if group.is_some() && message_id.is_none() {
        (META_GROUP, MetaOrder::Group)
    } else {
        (META_MESSAGE, MetaOrder::Message)
    };
    let identity = match order {
        MetaOrder::Message => identity(META_MESSAGE_DEFINITION),
        MetaOrder::Group => identity(META_GROUP_DEFINITION),
    };
    let Some(index) = store.secondary_index(META_COLLECTION, name, &identity) else {
        return Ok(None);
    };
    let key = subject_key(order, kind, queue, group, message_id);
    let mut ids = Vec::new();
    let mut posting = [0; SECONDARY_KEY_BYTES_MAX];
    if key.len() == 105 {
        let mut cursor = index.candidates(&key)?;
        while let Some(id) = cursor.next_id()? {
            ids.push(id);
        }
    } else {
        let upper = prefix_end(key.clone()).ok_or_else(|| {
            RedDBError::Internal("queue meta prefix has no finite upper bound".into())
        })?;
        let mut cursor = index.ordered_range(
            Bound::Included(key.as_slice()),
            Bound::Excluded(upper.as_slice()),
        )?;
        while let Some((_, id)) = cursor.next_posting(&mut posting)? {
            ids.push(id);
        }
    }
    let mut out = Vec::new();
    for id in ids {
        if manager.backing_row_is_shadowed(id) {
            continue;
        }
        let Some(entity) = manager.try_get(id, read_limit(&manager))? else {
            continue;
        };
        if !SegmentManager::scan_entity_visible(None, &entity) {
            continue;
        }
        let EntityData::Row(row) = entity.data else {
            continue;
        };
        if row_matches_subject(&row, kind, queue, group, message_id) {
            out.push((id, row));
        }
    }
    manager.scan_for_each(None, |entity| {
        let Some(row) = entity.data.as_row() else {
            return true;
        };
        if row_matches_subject(row, kind, queue, group, message_id) {
            out.push((entity.id, row.clone()));
        }
        true
    });
    Ok(Some(out))
}

pub(super) fn delivery_rows(
    runtime: &RedDBRuntime,
    delivery_id: &str,
) -> RedDBResult<Option<Vec<(EntityId, RowData)>>> {
    let store = runtime.db().store();
    delivery_rows_store(store.as_ref(), delivery_id)
}

pub(super) fn delivery_rows_store(
    store: &UnifiedStore,
    delivery_id: &str,
) -> RedDBResult<Option<Vec<(EntityId, RowData)>>> {
    let _publication = store.vault_publication_lock.lock();
    let manager = match store.get_collection(META_COLLECTION) {
        Some(manager) => manager,
        None => return Ok(Some(Vec::new())),
    };
    let identity = identity(META_DELIVERY_DEFINITION);
    let Some(index) = store.secondary_index(META_COLLECTION, META_DELIVERY, &identity) else {
        return Ok(None);
    };
    let mut key = Vec::with_capacity(33);
    key.push(1);
    key.extend_from_slice(&digest(delivery_id));
    let mut ids = Vec::new();
    let mut cursor = index.candidates(&key)?;
    while let Some(id) = cursor.next_id()? {
        ids.push(id);
    }
    let mut out = Vec::new();
    for id in ids {
        if manager.backing_row_is_shadowed(id) {
            continue;
        }
        let Some(entity) = manager.try_get(id, read_limit(&manager))? else {
            continue;
        };
        if !SegmentManager::scan_entity_visible(None, &entity) {
            continue;
        }
        let EntityData::Row(row) = entity.data else {
            continue;
        };
        if row_text(&row, "delivery_id").as_deref() == Some(delivery_id) {
            out.push((id, row));
        }
    }
    manager.scan_for_each(None, |entity| {
        let Some(row) = entity.data.as_row() else {
            return true;
        };
        if row_text(row, "delivery_id").as_deref() == Some(delivery_id) {
            out.push((entity.id, row.clone()));
        }
        true
    });
    Ok(Some(out))
}

pub(super) fn deadline_rows(
    runtime: &RedDBRuntime,
    queue: &str,
    deadline_at_or_before: u64,
) -> RedDBResult<Option<Vec<(EntityId, RowData)>>> {
    let store = runtime.db().store();
    let _publication = store.vault_publication_lock.lock();
    let manager = match store.get_collection(META_COLLECTION) {
        Some(manager) => manager,
        None => return Ok(Some(Vec::new())),
    };
    let identity = identity(META_DEADLINE_DEFINITION);
    let Some(index) = store.secondary_index(META_COLLECTION, META_DEADLINE, &identity) else {
        return Ok(None);
    };
    let mut lower = Vec::with_capacity(33);
    lower.push(1);
    lower.extend_from_slice(&digest(queue));
    let mut upper = lower.clone();
    upper.extend_from_slice(&deadline_at_or_before.to_be_bytes());
    let mut ids = Vec::new();
    let mut posting = [0; SECONDARY_KEY_BYTES_MAX];
    let mut cursor = index.ordered_range(
        Bound::Included(lower.as_slice()),
        Bound::Included(upper.as_slice()),
    )?;
    while let Some((_, id)) = cursor.next_posting(&mut posting)? {
        ids.push(id);
    }
    let matches = |row: &RowData| {
        row_text(row, "queue").as_deref() == Some(queue)
            && row_u64(row, "lock_deadline_ns")
                .is_some_and(|deadline| deadline <= deadline_at_or_before)
    };
    let mut out = Vec::new();
    for id in ids {
        if manager.backing_row_is_shadowed(id) {
            continue;
        }
        let Some(entity) = manager.try_get(id, read_limit(&manager))? else {
            continue;
        };
        if !SegmentManager::scan_entity_visible(None, &entity) {
            continue;
        }
        let EntityData::Row(row) = entity.data else {
            continue;
        };
        if matches(&row) {
            out.push((id, row));
        }
    }
    manager.scan_for_each(None, |entity| {
        let Some(row) = entity.data.as_row() else {
            return true;
        };
        if matches(row) {
            out.push((entity.id, row.clone()));
        }
        true
    });
    Ok(Some(out))
}

/// At most `limit` matching source rows leave this function. A missing old
/// disk projection uses the fallible source visitor until its first checkpoint.
pub(super) fn dedup_rows(
    store: &UnifiedStore,
    queue: &str,
    dedup_key: Option<&str>,
    now: u64,
    limit: usize,
) -> RedDBResult<Vec<(EntityId, Option<u64>)>> {
    assert!(limit > 0, "dedup read limit must be positive");
    let _publication = store.vault_publication_lock.lock();
    let Some(manager) = store.get_collection(META_COLLECTION) else {
        return Ok(Vec::new());
    };
    let snapshot = dedup_key.and_then(|_| super::impl_core::capture_current_snapshot());
    let mut out = Vec::new();
    let mut visit = |entity: &UnifiedEntity| {
        let Some((candidate_queue, candidate_key, expires)) =
            reddb_engine::storage::segment_key_lookup::SegmentKeyLookup::queue_dedup(entity)
        else {
            return true;
        };
        if candidate_queue == queue
            && dedup_key.map_or(expires <= now, |key| {
                candidate_key == Some(key) && expires > now
            })
        {
            let message = entity
                .data
                .as_row()
                .and_then(|row| row_u64(row, "message_id"));
            if dedup_key.is_none() || message.is_some() {
                out.push((entity.id, message));
            }
        }
        out.len() < limit
    };
    let (name, definition) = if dedup_key.is_some() {
        (META_DEDUP, META_DEDUP_DEFINITION)
    } else {
        (META_DEDUP_EXPIRY, META_DEDUP_EXPIRY_DEFINITION)
    };
    let index = store.secondary_index(META_COLLECTION, name, &identity(definition));
    if manager.backing_root_page().is_some() && index.is_none() {
        if dedup_key.is_some() {
            manager.try_scan_for_each(snapshot.as_ref(), read_limit(&manager), &mut visit)?;
        } else {
            manager.try_for_each_entity(read_limit(&manager), &mut visit)?;
        }
        return Ok(out);
    }
    if let Some(index) = index {
        let queue_hash = digest(queue);
        let mut lower = vec![1];
        lower.extend_from_slice(&queue_hash);
        if let Some(key) = dedup_key {
            lower.extend_from_slice(&digest(key));
            let mut cursor = index.candidates(&lower)?;
            while let Some(id) = cursor.next_id()? {
                if manager.backing_row_is_shadowed(id) {
                    continue;
                }
                let Some(entity) = manager.try_get(id, read_limit(&manager))? else {
                    continue;
                };
                if SegmentManager::scan_entity_visible(snapshot.as_ref(), &entity)
                    && !visit(&entity)
                {
                    return Ok(out);
                }
            }
        } else {
            // Include the physical ID in the projection so equal deadlines
            // can resume without rescanning their prefix or skipping siblings.
            let mut upper = lower.clone();
            upper.extend_from_slice(&now.to_be_bytes());
            upper.extend_from_slice(&u64::MAX.to_be_bytes());
            let root = index.root_page_id();
            let mut cursors = store.queue_dedup_cursors.lock();
            let resume = cursors
                .get(&queue_hash)
                .filter(|(prior_root, _)| *prior_root == root)
                .map(|(_, key)| *key);
            let lower_bound = resume
                .as_ref()
                .map_or(Bound::Included(lower.as_slice()), |key| {
                    Bound::Excluded(key.as_slice())
                });
            let mut cursor = index.ordered_range(lower_bound, Bound::Included(upper.as_slice()))?;
            let mut posting = [0; SECONDARY_KEY_BYTES_MAX];
            for _ in 0..limit {
                let Some((length, id)) = cursor.next_posting(&mut posting)? else {
                    cursors.remove(&queue_hash);
                    break;
                };
                if length != 49 {
                    return Err(RedDBError::Internal(
                        "invalid dedup expiration projection".into(),
                    ));
                }
                let entity = if manager.backing_row_is_shadowed(id) {
                    None
                } else {
                    manager.try_get(id, read_limit(&manager))?
                };
                // Cap cursor memory independently of the number/name sizes of queues.
                if cursors.len() >= 256 && !cursors.contains_key(&queue_hash) {
                    if let Some(victim) = cursors.keys().next().copied() {
                        cursors.remove(&victim);
                    }
                }
                cursors.insert(
                    queue_hash,
                    (root, posting[..49].try_into().expect("49-byte cursor")),
                );
                if let Some(entity) = entity {
                    if !visit(&entity) {
                        return Ok(out);
                    }
                }
            }
        }
    }
    manager.visit_queue_dedup(queue, dedup_key, now, snapshot.as_ref(), visit);
    Ok(out)
}

/// Capacity checks need visibility, never message bodies or ordering metadata.
/// Policy/SSI callers retain their source-row checks instead of using this path.
pub(super) fn capacity_reached(
    runtime: &RedDBRuntime,
    queue: &str,
    limit: usize,
) -> RedDBResult<Option<bool>> {
    let snapshot = super::impl_core::capture_current_snapshot();
    if runtime.is_rls_enabled(queue)
        || snapshot
            .as_ref()
            .is_some_and(|context| context.serializable_reader.is_some())
    {
        return Ok(None);
    }
    let store = runtime.db().store();
    let _publication = store.vault_publication_lock.lock();
    let Some(manager) = store.get_collection(queue) else {
        return Ok(None);
    };
    let index = store.secondary_index(queue, CAPACITY, &identity(CAPACITY_DEFINITION));
    if manager.backing_root_page().is_some() && index.is_none() {
        return Ok(None);
    }
    let mut count = 0usize;
    if let Some(index) = index {
        let mut cursor = index.ordered_range(Bound::Unbounded, Bound::Unbounded)?;
        let mut key = [0; SECONDARY_KEY_BYTES_MAX];
        while let Some((length, id)) = cursor.next_posting(&mut key)? {
            if length != 17 || key[16] > 1 {
                return Err(RedDBError::Internal(
                    "invalid queue capacity projection".into(),
                ));
            }
            if manager.backing_row_is_shadowed(id) || key[16] != 0 {
                continue;
            }
            let xmin = u64::from_be_bytes(key[..8].try_into().expect("eight-byte xmin"));
            let xmax = u64::from_be_bytes(key[8..16].try_into().expect("eight-byte xmax"));
            let visible = snapshot.as_ref().map_or(xmax == 0, |context| {
                reddb_engine::transaction::read::visibility_check(context, xmin, xmax)
            });
            count += usize::from(visible);
            if count >= limit {
                return Ok(Some(true));
            }
        }
    }
    manager.scan_for_each(snapshot.as_ref(), |entity| {
        if matches!(entity.kind, EntityKind::QueueMessage { .. })
            && matches!(entity.data, EntityData::QueueMessage(_))
        {
            count += 1;
        }
        count < limit
    });
    Ok(Some(count >= limit))
}

pub(super) fn ordered_messages(
    runtime: &RedDBRuntime,
    queue: &str,
) -> RedDBResult<Option<Vec<(EntityId, u64)>>> {
    let store = runtime.db().store();
    let _publication = store.vault_publication_lock.lock();
    let Some(manager) = store.get_collection(queue) else {
        return Ok(Some(Vec::new()));
    };
    let identity = identity(POSITION_ASC_DEFINITION);
    let Some(index) = store.secondary_index(queue, POSITION_ASC, &identity) else {
        return Ok(None);
    };
    let snapshot = super::impl_core::capture_current_snapshot();
    let mut out = Vec::new();
    let mut key = [0; SECONDARY_KEY_BYTES_MAX];
    let mut cursor = index.ordered_range(Bound::Included(&[1]), Bound::Excluded(&[2]))?;
    while let Some((_, id)) = cursor.next_posting(&mut key)? {
        if manager.backing_row_is_shadowed(id) {
            continue;
        }
        let Some(entity) = manager.try_get(id, read_limit(&manager))? else {
            continue;
        };
        if !SegmentManager::scan_entity_visible(snapshot.as_ref(), &entity) {
            continue;
        }
        let EntityKind::QueueMessage { position, .. } = entity.kind else {
            continue;
        };
        if matches!(entity.data, EntityData::QueueMessage(ref data) if !data.acked) {
            out.push((id, position));
        }
    }
    manager.scan_for_each(snapshot.as_ref(), |entity| {
        let EntityKind::QueueMessage { position, .. } = entity.kind else {
            return true;
        };
        if matches!(entity.data, EntityData::QueueMessage(ref data) if !data.acked) {
            out.push((entity.id, position));
        }
        true
    });
    out.sort_unstable_by_key(|(id, position)| (*position, id.raw()));
    out.dedup_by_key(|(id, _)| *id);
    Ok(Some(out))
}

pub(super) fn position_boundary(
    store: &UnifiedStore,
    queue: &str,
    right: bool,
) -> RedDBResult<Option<Option<u64>>> {
    let _publication = store.vault_publication_lock.lock();
    let Some(manager) = store.get_collection(queue) else {
        return Ok(Some(None));
    };
    let (name, identity) = if right {
        (POSITION_DESC, identity(POSITION_DESC_DEFINITION))
    } else {
        (POSITION_ASC, identity(POSITION_ASC_DEFINITION))
    };
    let Some(index) = store.secondary_index(queue, name, &identity) else {
        return Ok(None);
    };
    let snapshot = super::impl_core::capture_current_snapshot();
    let mut boundary = None;
    manager.scan_for_each(snapshot.as_ref(), |entity| {
        if let EntityKind::QueueMessage { position, .. } = entity.kind {
            if matches!(entity.data, EntityData::QueueMessage(ref data) if !data.acked) {
                boundary = Some(boundary.map_or(position, |current: u64| {
                    if right {
                        current.max(position)
                    } else {
                        current.min(position)
                    }
                }));
            }
        }
        true
    });
    let mut key = [0; SECONDARY_KEY_BYTES_MAX];
    let mut cursor = index.ordered_range(Bound::Included(&[1]), Bound::Excluded(&[2]))?;
    while let Some((_, id)) = cursor.next_posting(&mut key)? {
        if manager.backing_row_is_shadowed(id) {
            continue;
        }
        let Some(entity) = manager.try_get(id, read_limit(&manager))? else {
            continue;
        };
        if !SegmentManager::scan_entity_visible(snapshot.as_ref(), &entity) {
            continue;
        }
        let EntityKind::QueueMessage { position, .. } = entity.kind else {
            continue;
        };
        if matches!(entity.data, EntityData::QueueMessage(ref data) if !data.acked) {
            boundary = Some(boundary.map_or(position, |current: u64| {
                if right {
                    current.max(position)
                } else {
                    current.min(position)
                }
            }));
            break;
        }
    }
    Ok(Some(boundary))
}

#[cfg(test)]
pub(super) fn published_roots(store: &UnifiedStore, queue: &str) -> Option<Vec<u32>> {
    [
        (queue, CAPACITY, identity(CAPACITY_DEFINITION)),
        (META_COLLECTION, META_DEDUP, identity(META_DEDUP_DEFINITION)),
        (
            META_COLLECTION,
            META_DEDUP_EXPIRY,
            identity(META_DEDUP_EXPIRY_DEFINITION),
        ),
        (queue, POSITION_ASC, identity(POSITION_ASC_DEFINITION)),
        (queue, POSITION_DESC, identity(POSITION_DESC_DEFINITION)),
        (
            META_COLLECTION,
            META_MESSAGE,
            identity(META_MESSAGE_DEFINITION),
        ),
        (META_COLLECTION, META_GROUP, identity(META_GROUP_DEFINITION)),
        (
            META_COLLECTION,
            META_DELIVERY,
            identity(META_DELIVERY_DEFINITION),
        ),
        (
            META_COLLECTION,
            META_DEADLINE,
            identity(META_DEADLINE_DEFINITION),
        ),
    ]
    .into_iter()
    .map(|(collection, name, identity)| {
        store
            .secondary_index(collection, name, &identity)
            .map(|index| index.root_page_id())
    })
    .collect()
}

#[cfg(test)]
mod tests;
