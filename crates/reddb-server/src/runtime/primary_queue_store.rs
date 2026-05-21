//! Primary `QueueStore` adapter (slice 7 of PRD #527 / issue #534).
//!
//! Production-grade implementation of `lifecycle::QueueStore` against the
//! real `UnifiedStore`. Lets `QueueLifecycle` drive deliver/ack/nack/dlq
//! through an actual engine instead of the in-memory fake.
//!
//! Parallel implementation: this adapter writes its own meta-row kinds
//! (`queue_pending_lc`, `queue_acked_lc`, `queue_attempts_lc`) so the
//! legacy plumbing in `impl_queue.rs` / `queue_delivery.rs` (which uses
//! `queue_pending` / `queue_ack`) keeps working untouched. Atomic cutover
//! is slice 12.
//!
//! Policy fields (`max_attempts`, `lock_deadline_ms`,
//! `in_flight_cap_per_group`, `dlq_target`) are read from the
//! `CollectionDescriptor` hot-fields tier (landed in slice 6).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::catalog::CollectionDescriptor;
use crate::storage::queue::lifecycle::{
    BumpedAttempt, DeliveryId, MessageId, PendingDeliveryView, QueueSide, QueueStore,
    QueueStoreError, QueueTxn, Result, DEFAULT_READ_MAX_ATTEMPTS,
};
use crate::storage::queue::QueueMode;
use crate::storage::schema::Value;
use crate::storage::unified::entity::{QueueMessageData, RowData};
use crate::storage::{EntityData, EntityId, EntityKind, UnifiedEntity, UnifiedStore};

use super::queue_lifecycle::LifecycleConfig;
use super::RedDBRuntime;

const QUEUE_META_COLLECTION: &str = "red_queue_meta";
const KIND_PENDING_LC: &str = "queue_pending_lc";
const KIND_ACKED_LC: &str = "queue_acked_lc";
const KIND_ATTEMPTS_LC: &str = "queue_attempts_lc";

/// `QueueStore` implementation backed by `UnifiedStore`. Holds a clone of
/// the runtime so it can reach the store + catalog without `&Engine`
/// leaking into `QueueLifecycle`.
pub(crate) struct PrimaryQueueStore {
    runtime: RedDBRuntime,
}

impl PrimaryQueueStore {
    pub(crate) fn new(runtime: RedDBRuntime) -> Self {
        Self { runtime }
    }

    fn store(&self) -> Arc<UnifiedStore> {
        self.runtime.db().store()
    }

    fn descriptor(&self, queue: &str) -> Option<CollectionDescriptor> {
        self.runtime
            .db()
            .catalog_model_snapshot()
            .collections
            .into_iter()
            .find(|c| c.name == queue)
    }

    /// Build a `LifecycleConfig` for `queue` from the catalog descriptor's
    /// hot-fields tier. Falls back to crate defaults when the descriptor is
    /// absent or a field is unset. `max_attempts` is no longer a config
    /// knob — it lives on each message and is read at decision time via
    /// [`QueueStore::read_max_attempts`].
    pub(crate) fn lifecycle_config(&self, queue: &str) -> LifecycleConfig {
        use crate::storage::query::DEFAULT_QUEUE_LOCK_DEADLINE_MS;

        let desc = self.descriptor(queue);
        let lock_ms = desc
            .as_ref()
            .and_then(|d| d.queue_lock_deadline_ms)
            .unwrap_or(DEFAULT_QUEUE_LOCK_DEADLINE_MS);
        let dlq_target = desc.as_ref().and_then(|d| d.queue_dlq_target.clone());
        let mode = desc
            .as_ref()
            .and_then(|d| d.queue_mode)
            .unwrap_or(QueueMode::Work);

        LifecycleConfig {
            lock_duration: Duration::from_millis(lock_ms),
            dlq_target,
            mode,
        }
    }

    fn queue_exists(&self, queue: &str) -> bool {
        self.store().get_collection(queue).is_some()
    }

    fn meta_rows<F>(&self, predicate: F) -> Vec<(EntityId, RowData)>
    where
        F: Fn(&RowData) -> bool + Sync,
    {
        let store = self.store();
        let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
            return Vec::new();
        };
        manager
            .query_all(|entity| entity.data.as_row().is_some_and(&predicate))
            .into_iter()
            .filter_map(|entity| {
                let row = entity.data.as_row()?.clone();
                Some((entity.id, row))
            })
            .collect()
    }

    fn delete_meta_where<F>(&self, predicate: F)
    where
        F: Fn(&RowData) -> bool + Sync,
    {
        let store = self.store();
        let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
            return;
        };
        let hits = manager.query_all(|entity| entity.data.as_row().is_some_and(&predicate));
        for entity in hits {
            let _ = store.delete(QUEUE_META_COLLECTION, entity.id);
        }
    }

    fn insert_meta_row(&self, fields: HashMap<String, Value>) -> Result<()> {
        let store = self.store();
        let _ = store.get_or_create_collection(QUEUE_META_COLLECTION);
        store
            .insert_auto(
                QUEUE_META_COLLECTION,
                UnifiedEntity::new(
                    EntityId::new(0),
                    EntityKind::TableRow {
                        table: Arc::from(QUEUE_META_COLLECTION),
                        row_id: 0,
                    },
                    EntityData::Row(RowData {
                        columns: Vec::new(),
                        named: Some(fields),
                        schema: None,
                    }),
                ),
            )
            .map_err(|err| QueueStoreError::UnknownQueue(err.to_string()))?;
        Ok(())
    }

    fn find_pending_by_delivery(&self, delivery_id: &str) -> Option<(EntityId, PendingRow)> {
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_PENDING_LC)
                && row_text(row, "delivery_id").as_deref() == Some(delivery_id)
        })
        .into_iter()
        .next()
        .and_then(|(eid, row)| Some((eid, PendingRow::from_row(&row)?)))
    }

    fn find_pending_entry_by_key(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
    ) -> Option<(EntityId, PendingRow)> {
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_PENDING_LC)
                && row_text(row, "queue").as_deref() == Some(queue)
                && row_text(row, "group").as_deref() == Some(group)
                && row_u64(row, "message_id") == Some(message_id)
        })
        .into_iter()
        .next()
        .and_then(|(eid, row)| Some((eid, PendingRow::from_row(&row)?)))
    }

    fn read_attempts(&self, queue: &str, message_id: MessageId, group: &str) -> u32 {
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_ATTEMPTS_LC)
                && row_text(row, "queue").as_deref() == Some(queue)
                && row_text(row, "group").as_deref() == Some(group)
                && row_u64(row, "message_id") == Some(message_id)
        })
        .into_iter()
        .next()
        .and_then(|(_, row)| row_u64(&row, "attempts").map(|v| v as u32))
        .unwrap_or(0)
    }

    fn write_attempts(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
        attempts: u32,
    ) -> Result<()> {
        let queue_owned = queue.to_string();
        let group_owned = group.to_string();
        self.delete_meta_where(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_ATTEMPTS_LC)
                && row_text(row, "queue").as_deref() == Some(&queue_owned)
                && row_text(row, "group").as_deref() == Some(&group_owned)
                && row_u64(row, "message_id") == Some(message_id)
        });
        let mut fields = HashMap::new();
        fields.insert("kind".into(), Value::text(KIND_ATTEMPTS_LC.to_string()));
        fields.insert("queue".into(), Value::text(queue.to_string()));
        fields.insert("group".into(), Value::text(group.to_string()));
        fields.insert("message_id".into(), Value::UnsignedInteger(message_id));
        fields.insert("attempts".into(), Value::UnsignedInteger(attempts as u64));
        self.insert_meta_row(fields)
    }

    fn clear_attempts(&self, queue: &str, message_id: MessageId, group: &str) {
        let queue_owned = queue.to_string();
        let group_owned = group.to_string();
        self.delete_meta_where(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_ATTEMPTS_LC)
                && row_text(row, "queue").as_deref() == Some(&queue_owned)
                && row_text(row, "group").as_deref() == Some(&group_owned)
                && row_u64(row, "message_id") == Some(message_id)
        });
    }

    fn pending_message_ids(&self, queue: &str, group: Option<&str>) -> Vec<MessageId> {
        let queue_owned = queue.to_string();
        let group_owned = group.map(|g| g.to_string());
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_PENDING_LC)
                && row_text(row, "queue").as_deref() == Some(&queue_owned)
                && group_owned
                    .as_ref()
                    .map(|g| row_text(row, "group").as_deref() == Some(g))
                    .unwrap_or(true)
        })
        .into_iter()
        .filter_map(|(_, row)| row_u64(&row, "message_id"))
        .collect()
    }

    fn acked_message_ids(&self, queue: &str, group: &str) -> Vec<MessageId> {
        let queue_owned = queue.to_string();
        let group_owned = group.to_string();
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_ACKED_LC)
                && row_text(row, "queue").as_deref() == Some(&queue_owned)
                && row_text(row, "group").as_deref() == Some(&group_owned)
        })
        .into_iter()
        .filter_map(|(_, row)| row_u64(&row, "message_id"))
        .collect()
    }

    fn list_queue_messages(&self, queue: &str) -> Vec<QueueMessageOrdered> {
        let store = self.store();
        let Some(manager) = store.get_collection(queue) else {
            return Vec::new();
        };
        let mut out: Vec<QueueMessageOrdered> = manager
            .query_all(|entity| {
                matches!(entity.kind, EntityKind::QueueMessage { .. })
                    && matches!(entity.data, EntityData::QueueMessage(_))
            })
            .into_iter()
            .filter_map(|entity| {
                let position = match &entity.kind {
                    EntityKind::QueueMessage { position, .. } => *position,
                    _ => return None,
                };
                let data = match &entity.data {
                    EntityData::QueueMessage(d) => d.clone(),
                    _ => return None,
                };
                if data.acked {
                    return None;
                }
                Some(QueueMessageOrdered {
                    id: entity.id,
                    position,
                    payload: data.payload,
                })
            })
            .collect();
        out.sort_by_key(|m| m.position);
        out
    }

    fn delete_message(&self, queue: &str, message_id: EntityId) {
        let store = self.store();
        let _ = store.delete(queue, message_id);
        let queue_owned = queue.to_string();
        let raw = message_id.raw();
        self.delete_meta_where(|row| {
            row_text(row, "queue").as_deref() == Some(&queue_owned)
                && row_u64(row, "message_id") == Some(raw)
        });
    }

    /// Retire the queue message backing `message_id` on `queue`,
    /// rollback-safely when the caller's `txn` carries a live transaction
    /// context. Mirrors `queue_delivery::delete_message_with_state`:
    ///
    /// * Inside a transaction (`txn.xid()` is `Some`) the tuple is stamped
    ///   `xmax = xid` and a runtime pending tombstone is recorded against
    ///   the owning connection. COMMIT finalises it (the row is removed
    ///   from the context index, invisible to every future snapshot);
    ///   ROLLBACK revives it (`xmax` reset to its previous value) so the
    ///   message becomes visible again — parity with the legacy path.
    /// * Outside a transaction, on an already-tombstoned tuple, or when
    ///   the MVCC update fails, fall back to the immediate hard delete.
    ///
    /// The queue meta-rows keyed off the message (pending/acked/attempts)
    /// are lifecycle bookkeeping, not MVCC tuples, so they are swept in
    /// both branches.
    fn retire_message_mvcc(&self, txn: &QueueTxn, queue: &str, message_id: MessageId) {
        let entity_id = EntityId::new(message_id);
        if let Some(xid) = txn.xid() {
            let store = self.store();
            if let Some(manager) = store.get_collection(queue) {
                if let Some(mut entity) = manager.get(entity_id) {
                    if entity.xmax == 0 {
                        let previous_xmax = entity.xmax;
                        entity.set_xmax(xid);
                        if manager.update(entity).is_ok() {
                            self.runtime.record_pending_tombstone(
                                txn.conn_id(),
                                queue,
                                entity_id,
                                xid,
                                previous_xmax,
                            );
                            let queue_owned = queue.to_string();
                            self.delete_meta_where(|row| {
                                row_text(row, "queue").as_deref() == Some(&queue_owned)
                                    && row_u64(row, "message_id") == Some(message_id)
                            });
                            return;
                        }
                    }
                }
            }
        }
        self.delete_message(queue, entity_id);
    }

    /// Build a `QueueTxn` from the runtime's live connection context — the
    /// xid of the active transaction (if any) plus the owning connection
    /// id. This is the production bridge that threads the caller's
    /// Statement frame into the lifecycle store; autocommit callers get a
    /// context-less txn (immediate hard delete on retire). No stub: the
    /// xid is read straight off the running transaction.
    pub(crate) fn current_txn(&self) -> QueueTxn {
        let conn_id = crate::runtime::impl_core::current_connection_id();
        match self.runtime.current_xid() {
            Some(xid) => QueueTxn::with_runtime_context(xid, conn_id),
            None => QueueTxn::new(),
        }
    }

    fn next_position(&self, queue: &str) -> Result<u64> {
        let store = self.store();
        let Some(manager) = store.get_collection(queue) else {
            return Err(QueueStoreError::UnknownQueue(queue.to_string()));
        };
        let max = manager
            .query_all(|e| matches!(e.kind, EntityKind::QueueMessage { .. }))
            .into_iter()
            .filter_map(|e| match e.kind {
                EntityKind::QueueMessage { position, .. } => Some(position),
                _ => None,
            })
            .max();
        Ok(max.map(|p| p + 1).unwrap_or(1 << 32))
    }
}

#[derive(Debug, Clone)]
struct PendingRow {
    queue: String,
    group: String,
    message_id: MessageId,
    delivery_id: DeliveryId,
    lock_deadline_ns: u64,
}

impl PendingRow {
    fn from_row(row: &RowData) -> Option<Self> {
        Some(Self {
            queue: row_text(row, "queue")?,
            group: row_text(row, "group")?,
            message_id: row_u64(row, "message_id")?,
            delivery_id: row_text(row, "delivery_id")?,
            lock_deadline_ns: row_u64(row, "lock_deadline_ns")?,
        })
    }
}

#[derive(Debug, Clone)]
struct QueueMessageOrdered {
    id: EntityId,
    position: u64,
    payload: Value,
}

impl QueueStore for PrimaryQueueStore {
    fn available_messages(&self, queue: &str, side: QueueSide) -> Vec<MessageId> {
        let pending: std::collections::HashSet<MessageId> =
            self.pending_message_ids(queue, None).into_iter().collect();
        let mut out: Vec<MessageId> = self
            .list_queue_messages(queue)
            .into_iter()
            .map(|m| m.id.raw())
            .filter(|id| !pending.contains(id))
            .collect();
        if matches!(side, QueueSide::Right) {
            out.reverse();
        }
        out
    }

    fn available_messages_for_group(
        &self,
        queue: &str,
        group: &str,
        side: QueueSide,
    ) -> Vec<MessageId> {
        let pending: std::collections::HashSet<MessageId> = self
            .pending_message_ids(queue, Some(group))
            .into_iter()
            .collect();
        let acked: std::collections::HashSet<MessageId> =
            self.acked_message_ids(queue, group).into_iter().collect();
        let mut out: Vec<MessageId> = self
            .list_queue_messages(queue)
            .into_iter()
            .map(|m| m.id.raw())
            .filter(|id| !pending.contains(id) && !acked.contains(id))
            .collect();
        if matches!(side, QueueSide::Right) {
            out.reverse();
        }
        out
    }

    fn find_pending_by_key(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
    ) -> Option<DeliveryId> {
        self.find_pending_entry_by_key(queue, message_id, group)
            .map(|(_, row)| row.delivery_id)
    }

    fn mark_pending(
        &self,
        _txn: &QueueTxn,
        queue: &str,
        message_id: MessageId,
        group: &str,
        deadline: Instant,
    ) -> Result<DeliveryId> {
        if !self.queue_exists(queue) {
            return Err(QueueStoreError::UnknownQueue(queue.to_string()));
        }
        let deadline_ns = instant_to_unix_ns(deadline);
        if let Some((entity_id, existing)) =
            self.find_pending_entry_by_key(queue, message_id, group)
        {
            // Refresh deadline; same delivery_id.
            let store = self.store();
            let _ = store.delete(QUEUE_META_COLLECTION, entity_id);
            let mut fields = HashMap::new();
            fields.insert("kind".into(), Value::text(KIND_PENDING_LC.to_string()));
            fields.insert("queue".into(), Value::text(queue.to_string()));
            fields.insert("group".into(), Value::text(group.to_string()));
            fields.insert("message_id".into(), Value::UnsignedInteger(message_id));
            fields.insert("delivery_id".into(), Value::text(existing.delivery_id.clone()));
            fields.insert("lock_deadline_ns".into(), Value::UnsignedInteger(deadline_ns));
            self.insert_meta_row(fields)?;
            return Ok(existing.delivery_id);
        }

        let delivery_id = new_delivery_id();
        let mut fields = HashMap::new();
        fields.insert("kind".into(), Value::text(KIND_PENDING_LC.to_string()));
        fields.insert("queue".into(), Value::text(queue.to_string()));
        fields.insert("group".into(), Value::text(group.to_string()));
        fields.insert("message_id".into(), Value::UnsignedInteger(message_id));
        fields.insert("delivery_id".into(), Value::text(delivery_id.clone()));
        fields.insert("lock_deadline_ns".into(), Value::UnsignedInteger(deadline_ns));
        self.insert_meta_row(fields)?;
        Ok(delivery_id)
    }

    fn release_pending(&self, _txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        if let Some((entity_id, _)) = self.find_pending_by_delivery(delivery_id) {
            let _ = self.store().delete(QUEUE_META_COLLECTION, entity_id);
        }
        Ok(())
    }

    fn ack_pending(&self, txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        let (entity_id, row) = self
            .find_pending_by_delivery(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        let _ = self.store().delete(QUEUE_META_COLLECTION, entity_id);
        self.clear_attempts(&row.queue, row.message_id, &row.group);
        // WORK-mode ack retires the underlying message through the runtime
        // MVCC path when the caller's txn is live, so an aborted statement
        // revives the message (parity with `delete_message_with_state`).
        self.retire_message_mvcc(txn, &row.queue, row.message_id);
        // Keep the in-memory observability contract: record the tombstone
        // on the threaded txn so the seam is exercised end-to-end.
        txn.record_pending_tombstone(&row.queue, row.message_id);
        Ok(())
    }

    fn retire_for_group(&self, _txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        let (entity_id, row) = self
            .find_pending_by_delivery(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        let _ = self.store().delete(QUEUE_META_COLLECTION, entity_id);
        self.clear_attempts(&row.queue, row.message_id, &row.group);

        let mut fields = HashMap::new();
        fields.insert("kind".into(), Value::text(KIND_ACKED_LC.to_string()));
        fields.insert("queue".into(), Value::text(row.queue.clone()));
        fields.insert("group".into(), Value::text(row.group.clone()));
        fields.insert("message_id".into(), Value::UnsignedInteger(row.message_id));
        self.insert_meta_row(fields)
    }

    fn bump_attempt(&self, _txn: &QueueTxn, delivery_id: &str) -> Result<BumpedAttempt> {
        let (_, row) = self
            .find_pending_by_delivery(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        let current = self.read_attempts(&row.queue, row.message_id, &row.group);
        let next = current.saturating_add(1);
        self.write_attempts(&row.queue, row.message_id, &row.group, next)?;
        Ok(BumpedAttempt {
            attempts: next,
            queue: row.queue,
            message_id: row.message_id,
        })
    }

    fn read_max_attempts(&self, queue: &str, message_id: MessageId) -> u32 {
        // Mirrors `impl_queue::queue_message_max_attempts`: the value is
        // stamped onto each `QueueMessageData` at push time from the
        // descriptor's `max_attempts`. Falling back to the crate-wide
        // default when the message is missing or the entity isn't a
        // queue message keeps the trait surface total — the caller
        // shouldn't have to handle "deleted underneath us" here.
        let store = self.store();
        let Some(manager) = store.get_collection(queue) else {
            return DEFAULT_READ_MAX_ATTEMPTS;
        };
        let Some(entity) = manager.get(EntityId::new(message_id)) else {
            return DEFAULT_READ_MAX_ATTEMPTS;
        };
        match entity.data {
            EntityData::QueueMessage(data) => data.max_attempts,
            _ => DEFAULT_READ_MAX_ATTEMPTS,
        }
    }

    fn enqueue_dlq(&self, _txn: &QueueTxn, dlq_target: &str, original: Value) -> Result<()> {
        let store = self.store();
        if store.get_collection(dlq_target).is_none() {
            store
                .create_collection(dlq_target)
                .map_err(|err| QueueStoreError::UnknownQueue(err.to_string()))?;
        }
        let position = self.next_position(dlq_target)?;
        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::QueueMessage {
                queue: dlq_target.to_string(),
                position,
            },
            EntityData::QueueMessage(QueueMessageData {
                payload: original,
                priority: None,
                enqueued_at_ns: now_unix_ns(),
                attempts: 0,
                max_attempts: 0,
                acked: false,
            }),
        );
        store
            .insert_auto(dlq_target, entity)
            .map_err(|err| QueueStoreError::UnknownQueue(err.to_string()))?;
        Ok(())
    }

    fn read_lock_deadline(&self, delivery_id: &str) -> Option<Instant> {
        let (_, row) = self.find_pending_by_delivery(delivery_id)?;
        let now_ns = now_unix_ns();
        // Persisted deadline is unix-ns; convert to a live Instant by
        // anchoring to Instant::now(). Sub-second drift across the
        // conversion is acceptable — `Instant` is intrinsically wall-
        // clock-unsafe and lock expiry semantics use this only as an
        // approximation for the deadline-eviction loop (slice 8).
        let remaining_ns = row.lock_deadline_ns.saturating_sub(now_ns);
        Some(Instant::now() + Duration::from_nanos(remaining_ns))
    }

    fn read_message(&self, queue: &str, message_id: MessageId) -> Option<Value> {
        let store = self.store();
        let manager = store.get_collection(queue)?;
        let entity = manager.get(EntityId::new(message_id))?;
        match entity.data {
            EntityData::QueueMessage(data) if !data.acked => Some(data.payload),
            _ => None,
        }
    }

    fn read_pending_payload(&self, delivery_id: &str) -> Option<Value> {
        let (_, row) = self.find_pending_by_delivery(delivery_id)?;
        self.read_message(&row.queue, row.message_id)
    }

    fn purge_queue(&self, txn: &QueueTxn, queue: &str) -> Result<usize> {
        // Snapshot every message id on the queue so the tombstones we
        // record match the rows we actually remove. Pending rows may
        // reference message ids that are still present in
        // `list_queue_messages` (mark_pending does not remove the
        // underlying message) so the queue's own listing is the
        // authoritative source. Anything that *only* lives in the
        // pending meta-row stream (no backing queue message) gets
        // tombstoned too — matches the in-memory contract.
        let mut ids: Vec<MessageId> = self
            .list_queue_messages(queue)
            .into_iter()
            .map(|m| m.id.raw())
            .collect();
        for pending_id in self.pending_message_ids(queue, None) {
            if !ids.contains(&pending_id) {
                ids.push(pending_id);
            }
        }
        ids.sort_unstable();
        ids.dedup();

        for message_id in &ids {
            self.retire_message_mvcc(txn, queue, *message_id);
        }
        // Sweep every meta-row keyed off this queue (pending, acked,
        // attempts) — none of them are meaningful once the underlying
        // messages are gone.
        let queue_owned = queue.to_string();
        self.delete_meta_where(|row| {
            let kind = row_text(row, "kind");
            let kind_matches = matches!(
                kind.as_deref(),
                Some(KIND_PENDING_LC) | Some(KIND_ACKED_LC) | Some(KIND_ATTEMPTS_LC)
            );
            kind_matches && row_text(row, "queue").as_deref() == Some(&queue_owned)
        });

        for message_id in &ids {
            txn.record_pending_tombstone(queue, *message_id);
        }
        Ok(ids.len())
    }

    fn pending_deliveries_for_queue(&self, queue: &str) -> Vec<PendingDeliveryView> {
        // Walk every `queue_pending_lc` meta-row scoped to this queue and
        // hydrate it into the trait-level view. The persisted deadline is
        // unix-ns; convert to a live `Instant` the same way
        // `read_lock_deadline` does so the caller compares against
        // `Clock::now()` in a single domain.
        let queue_owned = queue.to_string();
        let now_i = Instant::now();
        let now_w = now_unix_ns();
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_PENDING_LC)
                && row_text(row, "queue").as_deref() == Some(&queue_owned)
        })
        .into_iter()
        .filter_map(|(_, row)| {
            let pending = PendingRow::from_row(&row)?;
            let deadline = if pending.lock_deadline_ns >= now_w {
                now_i + Duration::from_nanos(pending.lock_deadline_ns - now_w)
            } else {
                now_i - Duration::from_nanos(now_w - pending.lock_deadline_ns)
            };
            Some(PendingDeliveryView {
                delivery_id: pending.delivery_id,
                queue: pending.queue,
                message_id: pending.message_id,
                group: pending.group,
                deadline,
            })
        })
        .collect()
    }

    fn reclaim_expired(&self, _txn: &QueueTxn, queue: &str, now: Instant) -> Result<()> {
        // Persisted deadlines are wall-clock unix-ns (see
        // `instant_to_unix_ns` at `mark_pending` time). Convert the
        // monotonic `now` argument the same way so the comparison
        // happens in a single domain.
        let now_ns = instant_to_unix_ns(now);
        let queue_owned = queue.to_string();
        self.delete_meta_where(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_PENDING_LC)
                && row_text(row, "queue").as_deref() == Some(&queue_owned)
                && row_u64(row, "lock_deadline_ns")
                    .map(|d| d <= now_ns)
                    .unwrap_or(false)
        });
        Ok(())
    }
}

fn row_text(row: &RowData, field: &str) -> Option<String> {
    match row.get_field(field)? {
        Value::Text(value) => Some(value.to_string()),
        _ => None,
    }
}

fn row_u64(row: &RowData, field: &str) -> Option<u64> {
    match row.get_field(field)? {
        Value::UnsignedInteger(v) => Some(*v),
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        _ => None,
    }
}

fn now_unix_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn instant_to_unix_ns(deadline: Instant) -> u64 {
    let now_i = Instant::now();
    let now_w = now_unix_ns();
    if deadline >= now_i {
        now_w + (deadline - now_i).as_nanos() as u64
    } else {
        now_w.saturating_sub((now_i - deadline).as_nanos() as u64)
    }
}

fn new_delivery_id() -> DeliveryId {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = now_unix_ns();
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&n.to_le_bytes());
    bytes[8..].copy_from_slice(&nanos.to_le_bytes());
    let hash = blake3::hash(&bytes);
    base32_lower(&hash.as_bytes()[..15])
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity((bytes.len() * 8 + 4) / 5);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buf >> bits) & 0x1f) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buf << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RedDBOptions;
    use crate::RedDBRuntime;
    use crate::runtime::queue_lifecycle::{QueueLifecycle, RetirementOutcome};

    fn boot() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
    }

    fn push(rt: &RedDBRuntime, queue: &str, payload: &str) {
        rt.execute_query(&format!("QUEUE PUSH {queue} '{payload}'"))
            .expect("push");
    }

    fn list_message_ids(store: &PrimaryQueueStore, queue: &str) -> Vec<MessageId> {
        store
            .list_queue_messages(queue)
            .into_iter()
            .map(|m| m.id.raw())
            .collect()
    }

    #[test]
    fn lifecycle_config_reads_from_descriptor() {
        let rt = boot();
        rt.execute_query(
            "CREATE QUEUE qcfg MAX_ATTEMPTS 7 LOCK_DEADLINE_MS 4000 WITH DLQ qcfg_dlq",
        )
        .expect("create");
        // Push a message so the descriptor's `MAX_ATTEMPTS 7` lands on
        // a `QueueMessageData` row — that's where `read_max_attempts`
        // now sources the per-message budget.
        push(&rt, "qcfg", "p");

        let ps = PrimaryQueueStore::new(rt);
        let cfg = ps.lifecycle_config("qcfg");
        assert_eq!(cfg.lock_duration, Duration::from_millis(4000));
        assert_eq!(cfg.dlq_target.as_deref(), Some("qcfg_dlq"));

        let msgs = ps.list_queue_messages("qcfg");
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            ps.read_max_attempts("qcfg", msgs[0].id.raw()),
            7,
            "per-message max_attempts must match the descriptor's value",
        );
    }

    #[test]
    fn lifecycle_config_falls_back_to_defaults_for_unknown_queue() {
        use crate::storage::query::DEFAULT_QUEUE_LOCK_DEADLINE_MS;
        let rt = boot();
        let ps = PrimaryQueueStore::new(rt);
        let cfg = ps.lifecycle_config("missing");
        assert_eq!(
            cfg.lock_duration,
            Duration::from_millis(DEFAULT_QUEUE_LOCK_DEADLINE_MS)
        );
        assert!(cfg.dlq_target.is_none());
        // No queue → `read_max_attempts` falls back to the crate default
        // rather than erroring; keeps the trait surface total.
        assert_eq!(
            ps.read_max_attempts("missing", 0),
            DEFAULT_READ_MAX_ATTEMPTS,
        );
    }

    #[test]
    fn deliver_then_ack_round_trip_against_real_engine() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE qround").expect("create");
        push(&rt, "qround", "alpha");
        push(&rt, "qround", "beta");

        let ps = PrimaryQueueStore::new(rt);
        let lc = QueueLifecycle::new(ps, LifecycleConfig::default());

        let first = lc.deliver(&QueueTxn::new(),"qround", "workers", 1).expect("deliver");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].payload, Value::text("alpha"));
        assert!(!first[0].delivery_id.is_empty());

        // Pending row blocks redelivery of the same message.
        let second = lc.deliver(&QueueTxn::new(),"qround", "workers", 1).expect("deliver-2");
        assert_eq!(second.len(), 1);
        assert_ne!(second[0].delivery_id, first[0].delivery_id);
        assert_eq!(second[0].payload, Value::text("beta"));

        lc.ack(&QueueTxn::new(),&first[0].delivery_id).expect("ack");

        // After ack the underlying message is gone — only `beta` remains and it's pending.
        let remaining = list_message_ids(&lc.store_ref(), "qround");
        assert_eq!(remaining.len(), 1);

        lc.ack(&QueueTxn::new(),&second[0].delivery_id).expect("ack-2");
        assert!(list_message_ids(&lc.store_ref(), "qround").is_empty());
    }

    #[test]
    fn nack_requeues_below_max_then_promotes_to_dlq_against_real_engine() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE qdlq MAX_ATTEMPTS 2 WITH DLQ qdlq_dlq")
            .expect("create");
        push(&rt, "qdlq", "payload");

        let ps = PrimaryQueueStore::new(rt);
        let cfg = ps.lifecycle_config("qdlq");
        let lc = QueueLifecycle::new(ps, cfg);

        // First nack → Requeued.
        let a = lc.deliver(&QueueTxn::new(),"qdlq", "workers", 1).expect("deliver-a");
        assert_eq!(a[0].payload, Value::text("payload"));
        lc.nack(&QueueTxn::new(),&a[0].delivery_id).expect("nack-a");

        // Second nack → MovedToDlq.
        let b = lc.deliver(&QueueTxn::new(),"qdlq", "workers", 1).expect("deliver-b");
        assert_eq!(b[0].payload, Value::text("payload"), "redelivered original");
        lc.nack(&QueueTxn::new(),&b[0].delivery_id).expect("nack-b");

        assert_eq!(
            lc.recorded_outcomes(),
            vec![
                RetirementOutcome::Requeued,
                RetirementOutcome::MovedToDlq("qdlq_dlq".to_string()),
            ]
        );

        // Source queue is now empty.
        assert!(lc.deliver(&QueueTxn::new(),"qdlq", "workers", 1).unwrap().is_empty());

        // DLQ has the original payload.
        let dlq_msgs = lc.store_ref().list_queue_messages("qdlq_dlq");
        assert_eq!(dlq_msgs.len(), 1);
        assert_eq!(dlq_msgs[0].payload, Value::text("payload"));
    }

    #[test]
    fn mark_pending_persists_delivery_id_and_lock_deadline() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE qpersist").expect("create");
        push(&rt, "qpersist", "p1");

        let ps = PrimaryQueueStore::new(rt);
        let msgs = ps.list_queue_messages("qpersist");
        assert_eq!(msgs.len(), 1);
        let mid = msgs[0].id.raw();

        let deadline = Instant::now() + Duration::from_millis(1500);
        let t = QueueTxn::new();
        let id = ps
            .mark_pending(&t, "qpersist", mid, "g", deadline)
            .expect("mark");
        assert!(!id.is_empty());
        // Persisted delivery_id is base32-lower.
        assert!(id.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7')));

        // read_lock_deadline returns a live Instant within the expected window.
        let observed = ps.read_lock_deadline(&id).expect("deadline");
        let now = Instant::now();
        assert!(observed >= now, "deadline must be in the future");
        assert!(
            observed - now <= Duration::from_millis(2500),
            "deadline should be within original window"
        );

        // Idempotent on same key.
        let id2 = ps
            .mark_pending(&t, "qpersist", mid, "g", deadline + Duration::from_millis(500))
            .expect("mark-2");
        assert_eq!(id, id2);
    }

    #[test]
    fn mark_pending_on_unknown_queue_errors() {
        let rt = boot();
        let ps = PrimaryQueueStore::new(rt);
        let t = QueueTxn::new();
        let err = ps
            .mark_pending(&t, "nope", 1, "g", Instant::now() + Duration::from_secs(1))
            .unwrap_err();
        assert!(matches!(err, QueueStoreError::UnknownQueue(_)));
    }

    #[test]
    fn ack_unknown_delivery_errors() {
        let rt = boot();
        let ps = PrimaryQueueStore::new(rt);
        let t = QueueTxn::new();
        let err = ps.ack_pending(&t, "nope").unwrap_err();
        assert!(matches!(err, QueueStoreError::UnknownDelivery(_)));
    }

    /// Read the live `xmax` stamp on a queue message, or `None` when the
    /// tuple has been physically removed from the collection.
    fn message_xmax(rt: &RedDBRuntime, queue: &str, message_id: MessageId) -> Option<u64> {
        rt.db()
            .store()
            .get_collection(queue)
            .and_then(|m| m.get(EntityId::new(message_id)))
            .map(|e| e.xmax)
    }

    #[test]
    fn ack_inside_rolled_back_statement_revives_message() {
        // AC #3: a lifecycle ack inside a transaction that rolls back must
        // leave the underlying message visible again afterwards.
        let rt = boot();
        rt.execute_query("CREATE QUEUE qroll").expect("create");
        push(&rt, "qroll", "alpha");

        let ps = PrimaryQueueStore::new(rt.clone());
        let mid = ps.list_queue_messages("qroll")[0].id.raw();
        let lc = QueueLifecycle::new(ps, LifecycleConfig::default());

        // Deliver under autocommit to obtain a delivery handle.
        let d = lc.deliver(&QueueTxn::new(), "qroll", "workers", 1).expect("deliver");

        // Now open a transaction and ack inside it — the bridge lifts the
        // live xid + conn id off the running frame (no stub).
        rt.execute_query("BEGIN").expect("begin");
        let xid = rt.current_xid().expect("active xid");
        let txn = lc.store_ref().current_txn();
        assert_eq!(txn.xid(), Some(xid), "current_txn threads the live xid");
        lc.ack(&txn, &d[0].delivery_id).expect("ack");

        // Mid-transaction the tuple is xmax-stamped, not hard-deleted.
        assert_eq!(message_xmax(&rt, "qroll", mid), Some(xid));

        rt.execute_query("ROLLBACK").expect("rollback");

        // Rollback revives the tuple: xmax back to 0, message visible again.
        assert_eq!(
            message_xmax(&rt, "qroll", mid),
            Some(0),
            "rolled-back ack must revive the message",
        );
    }

    #[test]
    fn ack_committed_removes_message_and_records_tombstone() {
        // AC #4: a lifecycle ack that commits leaves the message gone
        // (xmax stamped with the committed xid → invisible to every future
        // snapshot) and records a tombstone on the threaded txn.
        let rt = boot();
        rt.execute_query("CREATE QUEUE qcommit").expect("create");
        push(&rt, "qcommit", "alpha");

        let ps = PrimaryQueueStore::new(rt.clone());
        let mid = ps.list_queue_messages("qcommit")[0].id.raw();
        let lc = QueueLifecycle::new(ps, LifecycleConfig::default());

        let d = lc.deliver(&QueueTxn::new(), "qcommit", "workers", 1).expect("deliver");

        rt.execute_query("BEGIN").expect("begin");
        let xid = rt.current_xid().expect("active xid");
        let txn = lc.store_ref().current_txn();
        lc.ack(&txn, &d[0].delivery_id).expect("ack");

        // Seam observability: the would-be tombstone is recorded.
        let recorded = txn.recorded_tombstones();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].queue, "qcommit");
        assert_eq!(recorded[0].message_id, mid);

        rt.execute_query("COMMIT").expect("commit");

        // After commit the tuple stays xmax-stamped with the committed xid
        // (the legacy contract — VACUUM reclaims it later; new snapshots
        // never see it). It is NOT revived.
        assert_eq!(
            message_xmax(&rt, "qcommit", mid),
            Some(xid),
            "committed ack must keep the message tombstoned",
        );
    }

    #[test]
    fn mvcc_ack_matches_legacy_delete_message_with_state() {
        // AC #4 (parity): the lifecycle MVCC retire stamps xmax + records a
        // pending tombstone exactly as the legacy
        // `delete_message_with_state` does, so both revive identically on
        // rollback.
        let rt = boot();
        rt.execute_query("CREATE QUEUE qpar").expect("create");
        push(&rt, "qpar", "m0");
        push(&rt, "qpar", "m1");

        let ps = PrimaryQueueStore::new(rt.clone());
        let mids: Vec<MessageId> = ps
            .list_queue_messages("qpar")
            .into_iter()
            .map(|m| m.id.raw())
            .collect();
        assert_eq!(mids.len(), 2);
        let (mid_lc, mid_legacy) = (mids[0], mids[1]);
        let lc = QueueLifecycle::new(ps, LifecycleConfig::default());

        // Deliver the first message so the lifecycle has a handle to ack.
        let d = lc.deliver(&QueueTxn::new(), "qpar", "workers", 1).expect("deliver");

        rt.execute_query("BEGIN").expect("begin");
        let xid = rt.current_xid().expect("active xid");

        // Retire mid_lc via the lifecycle adapter…
        let txn = lc.store_ref().current_txn();
        lc.ack(&txn, &d[0].delivery_id).expect("ack");

        // …and mid_legacy straight through the legacy path, same txn.
        let store = rt.db().store();
        crate::runtime::queue_delivery::delete_message_with_state(
            Some(&rt),
            store.as_ref(),
            "qpar",
            EntityId::new(mid_legacy),
        )
        .expect("legacy delete");

        // Both tuples carry the same xmax stamp.
        assert_eq!(message_xmax(&rt, "qpar", mid_lc), Some(xid));
        assert_eq!(message_xmax(&rt, "qpar", mid_legacy), Some(xid));

        rt.execute_query("ROLLBACK").expect("rollback");

        // Both revive identically.
        assert_eq!(message_xmax(&rt, "qpar", mid_lc), Some(0));
        assert_eq!(message_xmax(&rt, "qpar", mid_legacy), Some(0));
    }

    #[test]
    fn fanout_ack_retires_only_caller_group_against_real_engine() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE qfan FANOUT")
            .expect("create");
        push(&rt, "qfan", "shared");

        let ps = PrimaryQueueStore::new(rt);
        let cfg = ps.lifecycle_config("qfan");
        assert_eq!(cfg.mode, QueueMode::Fanout);
        let lc = QueueLifecycle::new(ps, cfg);

        let a = lc.deliver(&QueueTxn::new(),"qfan", "subs.a", 1).expect("deliver-a");
        let b = lc.deliver(&QueueTxn::new(),"qfan", "subs.b", 1).expect("deliver-b");
        assert_eq!(a[0].payload, Value::text("shared"));
        assert_eq!(b[0].payload, Value::text("shared"));
        assert_ne!(a[0].delivery_id, b[0].delivery_id);

        lc.ack(&QueueTxn::new(),&a[0].delivery_id).expect("ack-a");
        // A no longer sees the message; B's pending row remains valid.
        assert!(lc.deliver(&QueueTxn::new(),"qfan", "subs.a", 1).unwrap().is_empty());
        lc.ack(&QueueTxn::new(),&b[0].delivery_id).expect("ack-b");
    }
}

