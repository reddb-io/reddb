//! Primary `QueueStore` adapter (slice 7 of PRD #527 / issue #534).
//!
//! Production-grade implementation of `lifecycle::QueueStore` against the
//! real `UnifiedStore`. Lets `QueueLifecycle` drive deliver/ack/nack/dlq
//! through an actual engine instead of the in-memory fake.
//!
//! The adapter owns the production storage implementation for
//! `QueueLifecycle`. It still understands legacy `queue_pending` rows so
//! in-flight deliveries created before the cutover can be resolved, but
//! new lifecycle POP/MOVE/delete paths are owned here.
//!
//! Lifecycle policy comes from the indexed queue configuration row, which also
//! supplies the catalog descriptor. Hot operations never build a full catalog.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::storage::queue::lifecycle::{
    BumpedAttempt, DeliveryId, MessageId, PendingDeliveryView, QueueSide, QueueStore,
    QueueStoreError, QueueTxn, QueueTxnContext, Result, DEFAULT_READ_MAX_ATTEMPTS,
};
use crate::storage::unified::entity::{QueueMessageData, RowData};
use crate::storage::{EntityData, EntityId, EntityKind, UnifiedEntity, UnifiedStore};
use reddb_types::Value;

use super::impl_core::current_connection_id;
use super::queue_lifecycle::LifecycleConfig;
use super::RedDBRuntime;

const QUEUE_META_COLLECTION: &str = super::queue_storage_index::META_COLLECTION;
const KIND_PENDING_LC: &str = "queue_pending_lc";
const KIND_ACKED_LC: &str = "queue_acked_lc";
const KIND_ATTEMPTS_LC: &str = "queue_attempts_lc";

/// Legacy meta-row kind written by the pre-lifecycle delivery path
/// (`impl_queue::save_queue_pending`). The lifecycle adapter reads these
/// so a message reserved through the legacy path is visible to
/// `QueueLifecycle` instead of surfacing `UnknownDelivery` — closing the
/// parallel-meta-row divergence (issue #625, prereq B for the atomic flip).
const KIND_PENDING_LEGACY: &str = "queue_pending";

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

    /// Mint a [`QueueTxn`] bound to this adapter's runtime connection.
    ///
    /// Production callers (the eventual Statement-frame wiring, slice 12)
    /// hand this txn to `QueueLifecycle` so ack/purge retire the
    /// underlying message through the runtime's MVCC path
    /// (`current_xid → set_xmax → record_pending_tombstone`) — making the
    /// retirement participate in the caller's transaction and revive on
    /// rollback. No new internal transaction is opened.
    pub(crate) fn new_txn(&self) -> QueueTxn {
        QueueTxn::with_context(Arc::new(RuntimeQueueBridge {
            runtime: self.runtime.clone(),
        }))
    }

    fn store(&self) -> Arc<UnifiedStore> {
        self.runtime.db().store()
    }

    /// Retire the underlying queue message through the runtime MVCC path.
    ///
    /// Routes the deletion through `delete_message_with_state`, which —
    /// inside an open transaction — stamps `xmax` and records a pending
    /// tombstone (rollback-safe), and under autocommit falls back to a
    /// hard delete. When the caller supplied a context-bound `txn`
    /// (production), the retire is driven by `record_pending_tombstone`
    /// itself; otherwise (no-context unit-test path) we drive it here off
    /// this adapter's own runtime handle so the adapter stays correct
    /// standalone. Either way the message is retired exactly once.
    fn retire_message_mvcc(
        &self,
        txn: &QueueTxn,
        queue: &str,
        message_id: MessageId,
    ) -> Result<()> {
        let store = self.store();
        if super::vcs_queue::runtime(store.as_ref()).is_some() {
            return delete_message_with_state(
                Some(&self.runtime),
                store.as_ref(),
                queue,
                message_id,
            );
        }
        txn.record_pending_tombstone(queue, message_id);
        if !txn.has_context() {
            let store = self.store();
            let _ =
                delete_message_with_state(Some(&self.runtime), store.as_ref(), queue, message_id);
        }
        Ok(())
    }

    /// Validate the entire ACK set before retiring any message. The publication
    /// fence protects preimages and keeps all removals in one durable WAL batch.
    pub(crate) fn ack_many(
        &self,
        txn: &QueueTxn,
        delivery_ids: &[String],
        fanout: bool,
    ) -> Result<()> {
        let store = self.store();
        if UnifiedStore::deferred_store_wal_capture_active() || self.runtime.current_xid().is_some()
        {
            let _publication = store.vault_publication_lock.lock();
            self.prepare_ack_rows(delivery_ids, fanout, &[])?;
            for delivery in delivery_ids {
                if fanout {
                    self.retire_for_group(txn, delivery)?;
                } else {
                    self.ack_pending(txn, delivery)?;
                }
            }
            return Ok(());
        }
        let adapter = Self::new(self.runtime.clone());
        let delivery_ids = delivery_ids.to_vec();
        let pending = store
            .prepare_and_publish_rows(move |_, previous| {
                match adapter.prepare_ack_rows(&delivery_ids, fanout, previous) {
                    Ok((rows, pending)) => Ok((rows, Ok(pending))),
                    Err(error) => Ok((
                        crate::storage::unified::store::RowPublication::default(),
                        Err(error),
                    )),
                }
            })
            .map_err(|error| QueueStoreError::UnknownQueue(error.to_string()))??;
        if !fanout {
            for (_, row) in pending {
                self.retire_message_mvcc(txn, &row.queue, row.message_id)?;
            }
        }
        Ok(())
    }

    fn prepare_ack_rows(
        &self,
        delivery_ids: &[String],
        fanout: bool,
        previous: &[crate::storage::unified::store::RowPublication],
    ) -> Result<(
        crate::storage::unified::store::RowPublication,
        Vec<(EntityId, PendingRow)>,
    )> {
        let mut seen = std::collections::HashSet::new();
        let pending = delivery_ids
            .iter()
            .map(|delivery| {
                if delivery_ids.len() > 1 && !seen.insert(delivery) {
                    return Err(QueueStoreError::UnknownDelivery(format!(
                        "duplicate delivery: {delivery}"
                    )));
                }
                self.find_pending_by_delivery(delivery)
                    .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery.clone()))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut rows = crate::storage::unified::store::RowPublication::default();
        for (pending_id, row) in &pending {
            if fanout {
                rows.deletes
                    .push((QUEUE_META_COLLECTION.to_string(), *pending_id));
                rows.deletes.extend(
                    self.subject_rows(
                        KIND_ATTEMPTS_LC,
                        &row.queue,
                        Some(&row.group),
                        Some(row.message_id),
                    )
                    .into_iter()
                    .map(|(id, _)| (QUEUE_META_COLLECTION.to_string(), id)),
                );
                let fields = HashMap::from([
                    ("kind".into(), Value::text(KIND_ACKED_LC)),
                    ("queue".into(), Value::text(row.queue.clone())),
                    ("group".into(), Value::text(row.group.clone())),
                    ("message_id".into(), Value::UnsignedInteger(row.message_id)),
                ]);
                rows.inserts.push((
                    QUEUE_META_COLLECTION.to_string(),
                    queue_meta_entity(fields),
                    None,
                ));
            } else {
                rows.deletes
                    .push((row.queue.clone(), EntityId::new(row.message_id)));
                for kind in [
                    KIND_PENDING_LC,
                    KIND_ACKED_LC,
                    KIND_ATTEMPTS_LC,
                    KIND_PENDING_LEGACY,
                ] {
                    rows.deletes.extend(
                        self.subject_rows(kind, &row.queue, None, Some(row.message_id))
                            .into_iter()
                            .map(|(id, _)| (QUEUE_META_COLLECTION.to_string(), id)),
                    );
                }
            }
        }
        // Earlier accepted ACKs still exist in the store until the shared
        // publication. Reject overlap per request, before submitting any rows.
        if rows
            .deletes
            .iter()
            .any(|deleted| previous.iter().any(|plan| plan.deletes.contains(deleted)))
        {
            return Err(QueueStoreError::UnknownDelivery(
                "delivery already acknowledged in this publication".into(),
            ));
        }
        Ok((rows, pending))
    }

    // New READ reservations have already excluded legacy pending rows.
    // Call under the publication fence so validation and planning share a view.
    fn prepare_pending_read_rows(
        &self,
        queue: &str,
        message_ids: &[MessageId],
        group: &str,
        consumer: &str,
        deadline: Instant,
        count_attempt: bool,
    ) -> (
        crate::storage::unified::store::RowPublication,
        Vec<(DeliveryId, u32)>,
    ) {
        let mut rows = crate::storage::unified::store::RowPublication::default();
        let mut deliveries = Vec::with_capacity(message_ids.len());
        for &message_id in message_ids {
            let existing = self.find_pending_entry_by_key(queue, message_id, group);
            let delivery = existing
                .as_ref()
                .map(|(_, row)| row.delivery_id.clone())
                .unwrap_or_else(new_delivery_id);
            if let Some((id, _)) = existing {
                rows.deletes.push((QUEUE_META_COLLECTION.to_string(), id));
            }
            let fields = HashMap::from([
                ("kind".into(), Value::text(KIND_PENDING_LC)),
                ("queue".into(), Value::text(queue)),
                ("group".into(), Value::text(group)),
                ("message_id".into(), Value::UnsignedInteger(message_id)),
                ("delivery_id".into(), Value::text(delivery.clone())),
                ("consumer".into(), Value::text(consumer)),
                (
                    "lock_deadline_ns".into(),
                    Value::UnsignedInteger(instant_to_unix_ns(deadline)),
                ),
            ]);
            rows.inserts.push((
                QUEUE_META_COLLECTION.to_string(),
                queue_meta_entity(fields),
                None,
            ));
            let attempts = if count_attempt {
                let attempts = self
                    .read_attempts(queue, message_id, group)
                    .saturating_add(1);
                for (id, _) in
                    self.subject_rows(KIND_ATTEMPTS_LC, queue, Some(group), Some(message_id))
                {
                    rows.deletes.push((QUEUE_META_COLLECTION.to_string(), id));
                }
                let fields = HashMap::from([
                    ("kind".into(), Value::text(KIND_ATTEMPTS_LC)),
                    ("queue".into(), Value::text(queue)),
                    ("group".into(), Value::text(group)),
                    ("message_id".into(), Value::UnsignedInteger(message_id)),
                    ("attempts".into(), Value::UnsignedInteger(attempts as u64)),
                ]);
                rows.inserts.push((
                    QUEUE_META_COLLECTION.to_string(),
                    queue_meta_entity(fields),
                    None,
                ));
                attempts
            } else {
                1
            };
            deliveries.push((delivery, attempts));
        }
        (rows, deliveries)
    }

    /// Batch ordinary autocommit READs before taking the publication fence.
    /// Expiry, ordered keys, exhausted attempts and explicit transactions keep
    /// their existing lifecycle path. No provisional reservation escapes RAM.
    pub(crate) fn try_group_read_batched(
        &self,
        queue: &str,
        group: &str,
        consumer: &str,
        count: usize,
    ) -> Result<Option<Vec<super::queue_lifecycle::DeliveredMessage>>> {
        if count == 0
            || count > 256
            || self.runtime.current_xid().is_some()
            || UnifiedStore::deferred_store_wal_capture_active()
            || self.runtime.is_rls_enabled(queue)
        {
            return Ok(None);
        }
        let store = self.store();
        let adapter = Self::new(self.runtime.clone());
        let snapshot = super::execution_context::snapshot_bundle();
        let queue_owned = queue.to_string();
        let group_owned = group.to_string();
        let consumer_owned = consumer.to_string();
        let delivered = store
            .prepare_and_publish_rows(move |_, previous| {
                super::execution_context::with_snapshot_bundle(&snapshot, || {
                    adapter.prepare_group_read(
                        &queue_owned,
                        &group_owned,
                        &consumer_owned,
                        count,
                        previous,
                    )
                })
                .map_err(|error| {
                    crate::storage::StoreError::Io(std::io::Error::other(error.to_string()))
                })
            })
            .map_err(|error| QueueStoreError::UnknownQueue(error.to_string()))?;
        if let Some((mode, messages)) = delivered {
            // This cache is not authoritative. Another durable operation may
            // have advanced attempts since publication; copy the current value.
            let _publication = store.vault_publication_lock.lock();
            if mode == crate::storage::queue::QueueMode::Work {
                if let Some(manager) = store.get_collection(queue) {
                    for message in &messages {
                        if let Some(mut entity) = manager.get(EntityId::new(message.message_id)) {
                            if let EntityData::QueueMessage(data) = &mut entity.data {
                                data.attempts =
                                    self.read_attempts(queue, message.message_id, group);
                                let _ = manager.update(entity);
                            }
                        }
                    }
                }
            }
            if !messages.is_empty() {
                self.runtime.queue_telemetry().record_delivered(
                    queue,
                    group,
                    mode.as_str(),
                    messages.len() as u64,
                );
            }
            return Ok(Some(messages));
        }
        Ok(None)
    }

    fn prepare_group_read(
        &self,
        queue: &str,
        group: &str,
        consumer: &str,
        count: usize,
        previous: &[crate::storage::unified::store::RowPublication],
    ) -> Result<(
        crate::storage::unified::store::RowPublication,
        Option<(
            crate::storage::queue::QueueMode,
            Vec<super::queue_lifecycle::DeliveredMessage>,
        )>,
    )> {
        use crate::storage::queue::QueueMode;
        use crate::storage::unified::store::RowPublication;
        let now = Instant::now();
        let expired = super::queue_storage_index::deadline_rows(
            &self.runtime,
            queue,
            instant_to_unix_ns(now),
        )
        .map_err(|error| QueueStoreError::UnknownQueue(error.to_string()))?;
        if expired.is_none_or(|rows| !rows.is_empty()) {
            return Ok((RowPublication::default(), None));
        }
        let config = self.lifecycle_config(queue);
        let available = match config.mode {
            QueueMode::Work => self.available_messages(queue, QueueSide::Left),
            QueueMode::Fanout => self.available_messages_for_group(queue, group, QueueSide::Left),
        };
        let mut reserved = std::collections::HashSet::new();
        for plan in previous {
            for (collection, id) in &plan.deletes {
                if collection == queue {
                    reserved.insert(id.raw());
                }
            }
            for (collection, entity, _) in &plan.inserts {
                if collection != QUEUE_META_COLLECTION {
                    continue;
                }
                let Some(row) = entity.data.as_row() else {
                    continue;
                };
                if row_text(row, "kind").as_deref() == Some(KIND_PENDING_LC)
                    && row_text(row, "queue").as_deref() == Some(queue)
                    && (config.mode == QueueMode::Work
                        || row_text(row, "group").as_deref() == Some(group))
                {
                    if let Some(id) = row_u64(row, "message_id") {
                        reserved.insert(id);
                    }
                }
            }
        }
        let selected: Vec<_> = available
            .into_iter()
            .filter(|id| !reserved.contains(id))
            .take(count)
            .collect();
        let mut payloads = Vec::with_capacity(selected.len());
        for &message_id in &selected {
            let attempts = if config.mode == QueueMode::Work {
                self.read_attempts(queue, message_id, group)
                    .saturating_add(1)
            } else {
                1
            };
            if self.read_ordering_key(queue, message_id).is_some()
                || attempts > self.read_max_attempts(queue, message_id)
            {
                return Ok((RowPublication::default(), None));
            }
            payloads.push(
                self.read_message(queue, message_id)
                    .ok_or_else(|| QueueStoreError::UnknownQueue(queue.to_string()))?,
            );
        }
        let (rows, deliveries) = self.prepare_pending_read_rows(
            queue,
            &selected,
            group,
            consumer,
            now + config.lock_duration,
            config.mode == QueueMode::Work,
        );
        let messages = selected
            .into_iter()
            .zip(deliveries)
            .zip(payloads)
            .map(|((message_id, (delivery_id, delivery_count)), payload)| {
                super::queue_lifecycle::DeliveredMessage {
                    delivery_id,
                    message_id,
                    payload,
                    consumer: consumer.to_string(),
                    delivery_count,
                }
            })
            .collect();
        Ok((rows, Some((config.mode, messages))))
    }

    /// Read only this queue's indexed configuration, including its cold base.
    /// Do not cache the result: ALTER must affect the next lifecycle operation.
    /// The loader supplies defaults for missing fields/queues. `max_attempts`
    /// lives on each message and is read via [`QueueStore::read_max_attempts`].
    pub(crate) fn lifecycle_config(&self, queue: &str) -> LifecycleConfig {
        let config = super::impl_queue::load_queue_config(self.store().as_ref(), queue);
        LifecycleConfig {
            lock_duration: Duration::from_millis(config.lock_deadline_ms),
            dlq_target: config.dlq,
            mode: config.mode,
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
        match super::vcs_queue::metadata_rows(store.as_ref(), predicate) {
            Ok(rows) => rows,
            Err(error) => {
                super::vcs_queue::record_error(&error);
                tracing::warn!(error = %error, "queue metadata scan failed");
                Vec::new()
            }
        }
    }

    fn subject_rows(
        &self,
        kind: &str,
        queue: &str,
        group: Option<&str>,
        message_id: Option<MessageId>,
    ) -> Vec<(EntityId, RowData)> {
        match super::queue_storage_index::subject_rows(
            &self.runtime,
            kind,
            queue,
            group,
            message_id,
        ) {
            Ok(Some(rows)) => rows,
            Ok(None) => self.meta_rows(|row| {
                row_text(row, "kind").as_deref() == Some(kind)
                    && row_text(row, "queue").as_deref() == Some(queue)
                    && group
                        .map(|group| row_text(row, "group").as_deref() == Some(group))
                        .unwrap_or(true)
                    && message_id
                        .map(|id| row_u64(row, "message_id") == Some(id))
                        .unwrap_or(true)
            }),
            Err(error) => {
                super::vcs_queue::record_error(&error);
                tracing::warn!(error = %error, "queue meta index read failed; using resident fallback");
                self.meta_rows(|row| {
                    row_text(row, "kind").as_deref() == Some(kind)
                        && row_text(row, "queue").as_deref() == Some(queue)
                        && group
                            .map(|group| row_text(row, "group").as_deref() == Some(group))
                            .unwrap_or(true)
                        && message_id
                            .map(|id| row_u64(row, "message_id") == Some(id))
                            .unwrap_or(true)
                })
            }
        }
    }

    fn delivery_rows(&self, delivery_id: &str) -> Vec<(EntityId, RowData)> {
        match super::queue_storage_index::delivery_rows(&self.runtime, delivery_id) {
            Ok(Some(rows)) => rows,
            Ok(None) => {
                self.meta_rows(|row| row_text(row, "delivery_id").as_deref() == Some(delivery_id))
            }
            Err(error) => {
                super::vcs_queue::record_error(&error);
                tracing::warn!(error = %error, "queue delivery index read failed; using resident fallback");
                self.meta_rows(|row| row_text(row, "delivery_id").as_deref() == Some(delivery_id))
            }
        }
    }

    fn insert_meta_row(&self, fields: HashMap<String, Value>) -> Result<()> {
        super::impl_queue::insert_meta_row(self.store().as_ref(), fields)
            .map(|_| ())
            .map_err(|error| QueueStoreError::UnknownQueue(error.to_string()))
    }

    fn find_pending_by_delivery(&self, delivery_id: &str) -> Option<(EntityId, PendingRow)> {
        self.delivery_rows(delivery_id)
            .into_iter()
            .find(|(_, row)| row_text(row, "kind").as_deref() == Some(KIND_PENDING_LC))
            .and_then(|(eid, row)| Some((eid, PendingRow::from_row(&row)?)))
            .filter(|(_, row)| self.read_message(&row.queue, row.message_id).is_some())
    }

    fn find_pending_entry_by_key(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
    ) -> Option<(EntityId, PendingRow)> {
        let lifecycle_hit = self
            .subject_rows(KIND_PENDING_LC, queue, Some(group), Some(message_id))
            .into_iter()
            .next()
            .and_then(|(eid, row)| Some((eid, PendingRow::from_row(&row)?)));
        if lifecycle_hit.is_some() {
            return lifecycle_hit;
        }
        // Compat path: no lifecycle row exists, but the legacy delivery
        // path may have reserved this message under a `queue_pending` row.
        // Migrate it into the lifecycle representation so the rest of the
        // adapter (ack / release / bump / read_lock_deadline, all keyed by
        // delivery_id) resolves it through the normal `*_lc` machinery.
        self.migrate_legacy_pending(queue, message_id, group)
    }

    /// One-shot migration of a single legacy `queue_pending` row into the
    /// lifecycle representation. Forges a `delivery_id`, writes the
    /// `queue_pending_lc` row (anchoring the lock deadline to the legacy
    /// `delivered_at_ns` plus the queue's configured lock window), carries
    /// the legacy `delivery_count` over to `queue_attempts_lc` so the
    /// attempt counters agree across both views, then deletes the legacy
    /// row — leaving the message pending in exactly one representation.
    /// Returns the freshly-written lifecycle entry, or `None` when no
    /// matching legacy row exists.
    fn migrate_legacy_pending(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
    ) -> Option<(EntityId, PendingRow)> {
        let queue_owned = queue.to_string();
        let group_owned = group.to_string();
        let (legacy_eid, delivered_at_ns, delivery_count, consumer) = self
            .subject_rows(
                KIND_PENDING_LEGACY,
                &queue_owned,
                Some(&group_owned),
                Some(message_id),
            )
            .into_iter()
            .next()
            .map(|(eid, row)| {
                let delivered = row_u64(&row, "delivered_at_ns").unwrap_or_else(now_unix_ns);
                let count = row_u64(&row, "delivery_count").unwrap_or(1) as u32;
                let consumer = row_text(&row, "consumer").unwrap_or_default();
                (eid, delivered, count, consumer)
            })?;

        let delivery_id = new_delivery_id();
        let lock_ms = self.lifecycle_config(queue).lock_duration.as_millis() as u64;
        let deadline_ns = delivered_at_ns.saturating_add(lock_ms.saturating_mul(1_000_000));

        let mut fields = HashMap::new();
        fields.insert("kind".into(), Value::text(KIND_PENDING_LC.to_string()));
        fields.insert("queue".into(), Value::text(queue.to_string()));
        fields.insert("group".into(), Value::text(group.to_string()));
        fields.insert("message_id".into(), Value::UnsignedInteger(message_id));
        fields.insert("delivery_id".into(), Value::text(delivery_id.clone()));
        fields.insert("consumer".into(), Value::text(consumer));
        fields.insert(
            "lock_deadline_ns".into(),
            Value::UnsignedInteger(deadline_ns),
        );
        self.insert_meta_row(fields).ok()?;

        // Carry the legacy delivery_count across so the attempt counter
        // is consistent between the two views for the same message.
        if delivery_count > 0 {
            let _ = self.write_attempts(queue, message_id, group, delivery_count);
        }

        // Drop the legacy row last — only after the lifecycle row is
        // durably in place, so a failure mid-migration never strands the
        // message in *neither* representation.
        let _ = super::vcs_queue::delete_metadata(self.store().as_ref(), legacy_eid);

        self.find_pending_by_delivery(&delivery_id)
    }

    /// Message ids currently reserved through the legacy `queue_pending`
    /// path on `queue` (optionally scoped to `group`). Read-only — used to
    /// keep legacy-pending messages out of the lifecycle "available" view
    /// without migrating them (migration only happens on explicit resolve).
    fn legacy_pending_message_ids(&self, queue: &str, group: Option<&str>) -> Vec<MessageId> {
        let queue_owned = queue.to_string();
        let group_owned = group.map(|g| g.to_string());
        self.subject_rows(
            KIND_PENDING_LEGACY,
            &queue_owned,
            group_owned.as_deref(),
            None,
        )
        .into_iter()
        .filter_map(|(_, row)| row_u64(&row, "message_id"))
        .collect()
    }

    fn read_attempts(&self, queue: &str, message_id: MessageId, group: &str) -> u32 {
        self.subject_rows(KIND_ATTEMPTS_LC, queue, Some(group), Some(message_id))
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
        for (id, _) in self.subject_rows(KIND_ATTEMPTS_LC, queue, Some(group), Some(message_id)) {
            let _ = super::vcs_queue::delete_metadata(self.store().as_ref(), id);
        }
        let mut fields = HashMap::new();
        fields.insert("kind".into(), Value::text(KIND_ATTEMPTS_LC.to_string()));
        fields.insert("queue".into(), Value::text(queue.to_string()));
        fields.insert("group".into(), Value::text(group.to_string()));
        fields.insert("message_id".into(), Value::UnsignedInteger(message_id));
        fields.insert("attempts".into(), Value::UnsignedInteger(attempts as u64));
        self.insert_meta_row(fields)?;

        let store = self.store();
        if super::vcs_queue::runtime(store.as_ref()).is_some() {
            return Ok(());
        }
        if let Some(manager) = store.get_collection(queue) {
            if let Some(mut entity) = manager.get(EntityId::new(message_id)) {
                if let EntityData::QueueMessage(message) = &mut entity.data {
                    message.attempts = attempts;
                    let _ = manager.update(entity);
                }
            }
        }
        Ok(())
    }

    fn clear_attempts(&self, queue: &str, message_id: MessageId, group: &str) {
        for (id, _) in self.subject_rows(KIND_ATTEMPTS_LC, queue, Some(group), Some(message_id)) {
            let _ = super::vcs_queue::delete_metadata(self.store().as_ref(), id);
        }
    }

    fn pending_message_ids(&self, queue: &str, group: Option<&str>) -> Vec<MessageId> {
        let queue_owned = queue.to_string();
        let group_owned = group.map(|g| g.to_string());
        let mut ids: Vec<MessageId> = self
            .subject_rows(KIND_PENDING_LC, &queue_owned, group_owned.as_deref(), None)
            .into_iter()
            .filter_map(|(_, row)| row_u64(&row, "message_id"))
            .collect();
        // Union the legacy `queue_pending` reservations so a message
        // reserved through the legacy delivery path stays out of the
        // lifecycle "available" set — otherwise `deliver` would forge a
        // second, parallel pending row for the same message (issue #625).
        ids.extend(self.legacy_pending_message_ids(queue, group));
        ids
    }

    fn acked_message_ids(&self, queue: &str, group: &str) -> Vec<MessageId> {
        self.subject_rows(KIND_ACKED_LC, queue, Some(group), None)
            .into_iter()
            .filter_map(|(_, row)| row_u64(&row, "message_id"))
            .collect()
    }

    fn list_queue_messages(&self, queue: &str) -> Vec<QueueMessageOrdered> {
        match super::queue_storage_index::ordered_messages(&self.runtime, queue) {
            Ok(Some(rows)) => {
                return rows
                    .into_iter()
                    .map(|(id, position)| QueueMessageOrdered { id, position })
                    .collect();
            }
            Ok(None) => {}
            Err(error) => {
                super::vcs_queue::record_error(&error);
                tracing::warn!(error = %error, "queue position index read failed; using resident fallback");
            }
        }
        let store = self.store();
        let Some(manager) = store.get_collection(queue) else {
            return Vec::new();
        };
        // Honour the calling connection's MVCC snapshot: a queue message
        // retired through `delete_message_with_state` inside a transaction
        // stamps `xmax` and is only invisible to consumers once the txn
        // commits — same contract the legacy `load_queue_message_views_with_runtime`
        // applies. Without this, a committed lifecycle ack still surfaces
        // the message on subsequent reads.
        let snap_ctx = crate::runtime::impl_core::capture_current_snapshot();
        let (mut out, _) =
            manager.scan_project_zoned_with_stats(snap_ctx.as_ref(), &[], |entity| {
                let position = match &entity.kind {
                    EntityKind::QueueMessage { position, .. } => *position,
                    _ => return None,
                };
                let data = match &entity.data {
                    EntityData::QueueMessage(d) => d,
                    _ => return None,
                };
                if data.acked {
                    return None;
                }
                Some(QueueMessageOrdered {
                    id: entity.id,
                    position,
                })
            });
        out.sort_by_key(|m| m.position);
        out
    }

    fn next_position(&self, queue: &str) -> Result<u64> {
        let store = self.store();
        match super::queue_storage_index::position_boundary(store.as_ref(), queue, true) {
            Ok(Some(boundary)) => {
                return Ok(boundary.map(|position| position + 1).unwrap_or(1 << 32));
            }
            Ok(None) => {}
            Err(error) => {
                super::vcs_queue::record_error(&error);
                tracing::warn!(error = %error, "queue boundary index read failed; using resident fallback");
            }
        }
        let Some(manager) = store.get_collection(queue) else {
            return Err(QueueStoreError::UnknownQueue(queue.to_string()));
        };
        let mut max = None;
        let snapshot = super::execution_context::capture_current_snapshot();
        manager
            .try_for_each_entity(usize::MAX, |entity| {
                if super::execution_context::entity_visible_with_context(snapshot.as_ref(), entity)
                {
                    if let EntityKind::QueueMessage { position, .. } = entity.kind {
                        max = Some(max.map_or(position, |previous: u64| previous.max(position)));
                    }
                }
                true
            })
            .map_err(|error| QueueStoreError::UnknownQueue(error.to_string()))?;
        Ok(max.map(|p| p + 1).unwrap_or(1 << 32))
    }
}

/// Production [`QueueTxnContext`]: retires the underlying queue message
/// through the runtime MVCC path (`delete_message_with_state` →
/// `current_xid → set_xmax → record_pending_tombstone`), so a lifecycle
/// ack/purge that rolls back leaves the message visible again and a
/// committed one removes it — parity with the legacy delivery path. Holds
/// a clone of the runtime; the live xid / connection are resolved from
/// the caller's Statement-frame thread at retire time. No new transaction
/// is opened.
pub(crate) struct RuntimeQueueBridge {
    runtime: RedDBRuntime,
}

impl QueueTxnContext for RuntimeQueueBridge {
    fn retire_message(&self, queue: &str, message_id: MessageId) {
        let store = self.runtime.db().store();
        let _ = delete_message_with_state(Some(&self.runtime), store.as_ref(), queue, message_id);
    }
}

fn delete_message_with_state(
    runtime: Option<&RedDBRuntime>,
    store: &UnifiedStore,
    queue: &str,
    message_id: MessageId,
) -> Result<()> {
    if super::vcs_queue::retire(store, queue, EntityId::new(message_id))
        .map_err(|error| QueueStoreError::UnknownQueue(error.to_string()))?
    {
        remove_message_state(store, queue, message_id);
        return Ok(());
    }
    let entity_id = EntityId::new(message_id);
    if let Some(runtime) = runtime {
        if let Some(xid) = runtime.current_xid() {
            if let Some(manager) = store.get_collection(queue) {
                if let Some(mut entity) = manager.get(entity_id) {
                    if entity.xmax == 0 {
                        let previous_xmax = entity.xmax;
                        entity.set_xmax(xid);
                        if manager.update(entity).is_ok() {
                            let conn_id = crate::runtime::impl_core::current_connection_id();
                            runtime.record_pending_tombstone(
                                conn_id,
                                queue,
                                entity_id,
                                xid,
                                previous_xmax,
                                None,
                            );
                            super::impl_queue::forget_queue_message_lock(runtime, queue, entity_id);
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    remove_message_state(store, queue, message_id);
    store
        .delete(queue, entity_id)
        .map_err(|_| QueueStoreError::UnknownQueue(queue.to_string()))?;
    if let Some(runtime) = runtime {
        super::impl_queue::forget_queue_message_lock(runtime, queue, entity_id);
    }
    Ok(())
}

fn remove_message_state(store: &UnifiedStore, queue: &str, message_id: MessageId) {
    let kinds = [
        KIND_PENDING_LC,
        KIND_ACKED_LC,
        KIND_ATTEMPTS_LC,
        KIND_PENDING_LEGACY,
        "queue_retry",
    ];
    let indexed = kinds
        .iter()
        .map(|kind| {
            super::queue_storage_index::subject_rows_store(
                store,
                kind,
                queue,
                None,
                Some(message_id),
            )
        })
        .collect::<crate::RedDBResult<Vec<_>>>();
    match indexed {
        Ok(results) if results.iter().all(Option::is_some) => {
            for (id, _) in results.into_iter().flatten().flatten() {
                let _ = super::vcs_queue::delete_metadata(store, id);
            }
        }
        Ok(_) | Err(_) => super::impl_queue::remove_meta_rows(store, |row| {
            let kind = super::impl_queue::row_text(row, "kind");
            matches!(
                kind.as_deref(),
                Some(KIND_PENDING_LC)
                    | Some(KIND_ACKED_LC)
                    | Some(KIND_ATTEMPTS_LC)
                    | Some(KIND_PENDING_LEGACY)
                    | Some("queue_retry")
            ) && super::impl_queue::row_text(row, "queue").as_deref() == Some(queue)
                && super::impl_queue::row_u64(row, "message_id") == Some(message_id)
        }),
    }
}

#[derive(Debug, Clone)]
struct PendingRow {
    queue: String,
    group: String,
    message_id: MessageId,
    delivery_id: DeliveryId,
    consumer: String,
    lock_deadline_ns: u64,
}

impl PendingRow {
    fn from_row(row: &RowData) -> Option<Self> {
        Some(Self {
            queue: row_text(row, "queue")?,
            group: row_text(row, "group")?,
            message_id: row_u64(row, "message_id")?,
            delivery_id: row_text(row, "delivery_id")?,
            consumer: row_text(row, "consumer").unwrap_or_default(),
            lock_deadline_ns: row_u64(row, "lock_deadline_ns")?,
        })
    }
}

#[derive(Debug, Clone)]
struct QueueMessageOrdered {
    id: EntityId,
    position: u64,
}

impl QueueStore for PrimaryQueueStore {
    fn available_messages(&self, queue: &str, side: QueueSide) -> Vec<MessageId> {
        let pending: std::collections::HashSet<MessageId> =
            self.pending_message_ids(queue, None).into_iter().collect();
        let now_ns = now_unix_ns();
        let store = self.store();
        let mut out: Vec<MessageId> = self
            .list_queue_messages(queue)
            .into_iter()
            .map(|m| m.id.raw())
            .filter(|id| !pending.contains(id))
            .filter(|id| {
                // Issue #722: skip messages with a future `available_at_ns`.
                super::impl_queue::read_message_available_at_ns(
                    store.as_ref(),
                    queue,
                    EntityId::new(*id),
                )
                .is_none_or(|at| at <= now_ns)
            })
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
        let now_ns = now_unix_ns();
        let store = self.store();
        let mut out: Vec<MessageId> = self
            .list_queue_messages(queue)
            .into_iter()
            .map(|m| m.id.raw())
            .filter(|id| !pending.contains(id) && !acked.contains(id))
            .filter(|id| {
                // Issue #722: skip delayed messages.
                super::impl_queue::read_message_available_at_ns(
                    store.as_ref(),
                    queue,
                    EntityId::new(*id),
                )
                .is_none_or(|at| at <= now_ns)
            })
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
        consumer: &str,
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
            let _ = super::vcs_queue::delete_metadata(store.as_ref(), entity_id);
            let mut fields = HashMap::new();
            fields.insert("kind".into(), Value::text(KIND_PENDING_LC.to_string()));
            fields.insert("queue".into(), Value::text(queue.to_string()));
            fields.insert("group".into(), Value::text(group.to_string()));
            fields.insert("message_id".into(), Value::UnsignedInteger(message_id));
            fields.insert(
                "delivery_id".into(),
                Value::text(existing.delivery_id.clone()),
            );
            fields.insert("consumer".into(), Value::text(consumer.to_string()));
            fields.insert(
                "lock_deadline_ns".into(),
                Value::UnsignedInteger(deadline_ns),
            );
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
        fields.insert("consumer".into(), Value::text(consumer.to_string()));
        fields.insert(
            "lock_deadline_ns".into(),
            Value::UnsignedInteger(deadline_ns),
        );
        self.insert_meta_row(fields)?;
        Ok(delivery_id)
    }

    fn mark_pending_for_read(
        &self,
        txn: &QueueTxn,
        queue: &str,
        message_id: MessageId,
        group: &str,
        consumer: &str,
        deadline: Instant,
        count_attempt: bool,
    ) -> Result<(DeliveryId, u32)> {
        self.mark_pending_many_for_read(
            txn,
            queue,
            &[message_id],
            group,
            consumer,
            deadline,
            count_attempt,
        )
        .map(|mut deliveries| deliveries.pop().expect("one requested delivery"))
    }

    fn mark_pending_many_for_read(
        &self,
        txn: &QueueTxn,
        queue: &str,
        message_ids: &[MessageId],
        group: &str,
        consumer: &str,
        deadline: Instant,
        count_attempt: bool,
    ) -> Result<Vec<(DeliveryId, u32)>> {
        if UnifiedStore::deferred_store_wal_capture_active() || self.runtime.current_xid().is_some()
        {
            return message_ids
                .iter()
                .map(|&message_id| {
                    let delivery =
                        self.mark_pending(txn, queue, message_id, group, consumer, deadline)?;
                    let attempts = if count_attempt {
                        self.bump_attempt(txn, &delivery)?.attempts
                    } else {
                        1
                    };
                    Ok((delivery, attempts))
                })
                .collect();
        }
        let store = self.store();
        let _publication = store.vault_publication_lock.lock();
        if !self.queue_exists(queue) {
            return Err(QueueStoreError::UnknownQueue(queue.to_string()));
        }
        let (rows, deliveries) = self.prepare_pending_read_rows(
            queue,
            message_ids,
            group,
            consumer,
            deadline,
            count_attempt,
        );
        store
            .publish_rows(rows)
            .map_err(|error| QueueStoreError::UnknownQueue(error.to_string()))?;
        for (&message_id, (_, attempts)) in message_ids.iter().zip(&deliveries) {
            if count_attempt {
                if let Some(manager) = store.get_collection(queue) {
                    if let Some(mut entity) = manager.get(EntityId::new(message_id)) {
                        if let EntityData::QueueMessage(message) = &mut entity.data {
                            message.attempts = *attempts;
                            let _ = manager.update(entity);
                        }
                    }
                }
            }
        }
        Ok(deliveries)
    }

    fn release_pending(&self, _txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        if let Some((entity_id, _)) = self.find_pending_by_delivery(delivery_id) {
            let _ = super::vcs_queue::delete_metadata(self.store().as_ref(), entity_id);
        }
        Ok(())
    }

    fn ack_pending(&self, txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        if !UnifiedStore::deferred_store_wal_capture_active()
            && self.runtime.current_xid().is_none()
        {
            return self.ack_many(txn, &[delivery_id.to_string()], false);
        }
        let (entity_id, row) = self
            .find_pending_by_delivery(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        // Clear the adapter's own bookkeeping rows eagerly (pending +
        // attempts): these are lifecycle meta-state, not the durable
        // message.
        let _ = super::vcs_queue::delete_metadata(self.store().as_ref(), entity_id);
        self.clear_attempts(&row.queue, row.message_id, &row.group);
        // Retire the underlying message through the runtime MVCC path
        // (current_xid → set_xmax → record_pending_tombstone) rather than
        // an immediate hard delete, so an ack inside a statement that
        // rolls back leaves the message visible again.
        self.retire_message_mvcc(txn, &row.queue, row.message_id)?;
        Ok(())
    }

    fn retire_for_group(&self, _txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        if !UnifiedStore::deferred_store_wal_capture_active()
            && self.runtime.current_xid().is_none()
        {
            return self.ack_many(_txn, &[delivery_id.to_string()], true);
        }
        let (entity_id, row) = self
            .find_pending_by_delivery(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        let _ = super::vcs_queue::delete_metadata(self.store().as_ref(), entity_id);
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
            group: row.group,
            consumer: row.consumer,
        })
    }

    fn read_pending_attempt(&self, delivery_id: &str) -> Result<BumpedAttempt> {
        let (_, row) = self
            .find_pending_by_delivery(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        Ok(BumpedAttempt {
            attempts: self.read_attempts(&row.queue, row.message_id, &row.group),
            queue: row.queue,
            message_id: row.message_id,
            group: row.group,
            consumer: row.consumer,
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
        let Some(entity) = store.get(queue, EntityId::new(message_id)) else {
            return DEFAULT_READ_MAX_ATTEMPTS;
        };
        match entity.data {
            EntityData::QueueMessage(data) => data.max_attempts,
            _ => DEFAULT_READ_MAX_ATTEMPTS,
        }
    }

    fn enqueue_dlq(
        &self,
        _txn: &QueueTxn,
        dlq_target: &str,
        original: Value,
        ordering_key: Option<&str>,
    ) -> Result<()> {
        let store = self.store();
        if store.get_collection(dlq_target).is_none() {
            store
                .create_collection(dlq_target)
                .map_err(|err| QueueStoreError::UnknownQueue(err.to_string()))?;
            super::queue_storage_index::prepare_queue(&self.runtime, dlq_target)
                .map_err(|err| QueueStoreError::UnknownQueue(err.to_string()))?;
        }
        let position = self.next_position(dlq_target)?;
        let mut entity = UnifiedEntity::new(
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
        super::vcs_queue::stamp_entity(store.as_ref(), dlq_target, &mut entity)
            .map_err(|error| QueueStoreError::UnknownQueue(error.to_string()))?;
        let id = store
            .insert_auto(dlq_target, entity)
            .map_err(|err| QueueStoreError::UnknownQueue(err.to_string()))?;
        if ordering_key.is_some() {
            store
                .set_metadata(
                    dlq_target,
                    id,
                    super::impl_queue::queue_message_metadata(None, None, ordering_key),
                )
                .map_err(|err| QueueStoreError::UnknownQueue(err.to_string()))?;
        }
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
        // Queue selection can return a disk-only message after cold boot.
        // The store lookup merges the resident overlay with the paged base.
        let entity = match store.try_get(queue, EntityId::new(message_id), usize::MAX) {
            Ok(entity) => entity?,
            Err(error) => {
                super::vcs_queue::record_error(error);
                return None;
            }
        };
        if !super::execution_context::entity_visible_with_context(
            super::execution_context::capture_current_snapshot().as_ref(),
            &entity,
        ) {
            return None;
        }
        match entity.data {
            EntityData::QueueMessage(data) if !data.acked => Some(data.payload),
            _ => None,
        }
    }

    fn read_ordering_key(&self, queue: &str, message_id: MessageId) -> Option<String> {
        let store = self.store();
        super::impl_queue::read_message_ordering_key(
            store.as_ref(),
            queue,
            EntityId::new(message_id),
        )
    }

    fn ordering_key_in_flight(&self, queue: &str, group: &str, ordering_key: &str) -> bool {
        self.pending_message_ids(queue, Some(group))
            .into_iter()
            .any(|message_id| {
                self.read_ordering_key(queue, message_id)
                    .is_some_and(|key| key == ordering_key)
            })
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

        // Retire each message through the runtime MVCC path (rollback-safe)
        // rather than an immediate hard delete; `retire_message_mvcc` also
        // records the per-message tombstone on `txn`, in the same shape
        // `ack_pending` uses.
        for message_id in &ids {
            self.retire_message_mvcc(txn, queue, *message_id)?;
        }
        // Sweep every meta-row keyed off this queue (pending, acked,
        // attempts) — none of them are meaningful once the underlying
        // messages are gone.
        for kind in [KIND_PENDING_LC, KIND_ACKED_LC, KIND_ATTEMPTS_LC] {
            for (id, _) in self.subject_rows(kind, queue, None, None) {
                let _ = super::vcs_queue::delete_metadata(self.store().as_ref(), id);
            }
        }

        Ok(ids.len())
    }

    fn pending_deliveries_for_queue(&self, queue: &str) -> Vec<PendingDeliveryView> {
        // Walk every `queue_pending_lc` meta-row scoped to this queue and
        // hydrate it into the trait-level view. The persisted deadline is
        // unix-ns; convert to a live `Instant` the same way
        // `read_lock_deadline` does so the caller compares against
        // `Clock::now()` in a single domain.
        let now_i = Instant::now();
        let now_w = now_unix_ns();
        self.subject_rows(KIND_PENDING_LC, queue, None, None)
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
        let rows = match super::queue_storage_index::deadline_rows(&self.runtime, queue, now_ns) {
            Ok(Some(rows)) => rows,
            Ok(None) => self.subject_rows(KIND_PENDING_LC, queue, None, None),
            Err(error) => {
                super::vcs_queue::record_error(&error);
                tracing::warn!(error = %error, "queue deadline index read failed; using resident fallback");
                self.subject_rows(KIND_PENDING_LC, queue, None, None)
            }
        };
        for (id, row) in rows {
            if row_text(&row, "kind").as_deref() == Some(KIND_PENDING_LC)
                && row_u64(&row, "lock_deadline_ns").is_some_and(|deadline| deadline <= now_ns)
            {
                let _ = super::vcs_queue::delete_metadata(self.store().as_ref(), id);
            }
        }
        Ok(())
    }

    fn pop_available(
        &self,
        txn: &QueueTxn,
        queue: &str,
        side: QueueSide,
        count: usize,
    ) -> Result<Vec<(MessageId, Value)>> {
        if !self.queue_exists(queue) {
            return Err(QueueStoreError::UnknownQueue(queue.to_string()));
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut popped = Vec::new();
        for message_id in self.available_messages(queue, side) {
            if popped.len() >= count {
                break;
            }

            let entity_id = EntityId::new(message_id);
            let message_lock =
                super::impl_queue::queue_message_lock_handle(&self.runtime, queue, entity_id);
            let Some(_guard) = message_lock.try_lock() else {
                continue;
            };
            if self.pending_message_ids(queue, None).contains(&message_id) {
                continue;
            }
            let Some(payload) = self.read_message(queue, message_id) else {
                continue;
            };

            popped.push((message_id, payload));
            self.retire_message_mvcc(txn, queue, message_id)?;
        }
        Ok(popped)
    }

    fn delete_with_state(&self, txn: &QueueTxn, queue: &str, message_id: MessageId) -> Result<()> {
        if !self.queue_exists(queue) {
            return Err(QueueStoreError::UnknownQueue(queue.to_string()));
        }
        if super::vcs_queue::runtime(self.store().as_ref()).is_some() {
            return delete_message_with_state(
                Some(&self.runtime),
                self.store().as_ref(),
                queue,
                message_id,
            );
        }
        if txn.has_context() {
            txn.record_pending_tombstone(queue, message_id);
            return Ok(());
        }
        let store = self.runtime.db().store();
        delete_message_with_state(Some(&self.runtime), store.as_ref(), queue, message_id)
    }

    fn move_to_queue(
        &self,
        txn: &QueueTxn,
        source: &str,
        dest: &str,
        side: QueueSide,
        count: usize,
    ) -> Result<usize> {
        // Atomic-ish move: pop the source-side messages (retiring them
        // through the MVCC tombstone path), then insert each payload on
        // `dest` via the same `enqueue` machinery the inline
        // `impl_queue::execute_queue_move` uses. On insert failure the
        // already-inserted destination rows are rolled back; the source
        // pops have already retired, so on rollback they revive via
        // MVCC when the surrounding statement aborts.
        if count == 0 {
            return Ok(0);
        }
        let store = self.runtime.db().store();
        let popped = self.pop_available(txn, source, side, count)?;
        if popped.is_empty() {
            return Ok(0);
        }
        let mut inserted: Vec<EntityId> = Vec::with_capacity(popped.len());
        for (_, payload) in &popped {
            match super::impl_queue::insert_moved_queue_message_payload(&store, dest, payload) {
                Ok(id) => inserted.push(id),
                Err(_) => {
                    for id in inserted {
                        let _ = store.delete(dest, id);
                    }
                    return Err(QueueStoreError::UnknownQueue(dest.to_string()));
                }
            }
        }
        Ok(popped.len())
    }

    fn reclaim_expired_dedup(&self, _txn: &QueueTxn, queue: &str, now_ns: u64) -> Result<usize> {
        let store = self.runtime.db().store();
        super::impl_queue::reclaim_expired_queue_dedup(store.as_ref(), queue, now_ns)
            .map_err(|err| QueueStoreError::UnknownQueue(err.to_string()))
    }

    fn find_dedup(&self, queue: &str, dedup_key: &str, now_ns: u64) -> Result<Option<MessageId>> {
        let store = self.runtime.db().store();
        super::impl_queue::find_queue_dedup(store.as_ref(), queue, dedup_key, now_ns)
            .map(|id| id.map(|id| id.raw()))
            .map_err(|err| QueueStoreError::UnknownQueue(err.to_string()))
    }

    fn record_dedup(
        &self,
        _txn: &QueueTxn,
        queue: &str,
        dedup_key: &str,
        message_id: MessageId,
        expires_at_ns: u64,
    ) -> Result<()> {
        let store = self.runtime.db().store();
        let metadata_id = super::impl_queue::record_queue_dedup(
            store.as_ref(),
            queue,
            dedup_key,
            EntityId::new(message_id),
            expires_at_ns,
        )
        .map_err(|err| QueueStoreError::UnknownQueue(err.to_string()))?;
        self.runtime
            .stamp_xmin_if_in_txn(super::impl_queue::QUEUE_META_COLLECTION, metadata_id);
        if self.runtime.current_xid().is_some() || UnifiedStore::deferred_store_wal_capture_active()
        {
            self.runtime.record_pending_queue_dedup(
                current_connection_id(),
                queue,
                dedup_key,
                metadata_id,
            );
        }
        Ok(())
    }
}

fn queue_meta_entity(fields: HashMap<String, Value>) -> UnifiedEntity {
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
    )
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
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
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
    use crate::runtime::queue_lifecycle::{QueueLifecycle, RetirementOutcome};
    use crate::storage::queue::QueueMode;
    use crate::RedDBRuntime;

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

    fn batch_ids(rt: &RedDBRuntime, queue: &str, group: &str, count: usize) -> Vec<String> {
        rt.execute_query(&format!(
            "QUEUE READ {queue} GROUP {group} CONSUMER c COUNT {count}"
        ))
        .expect("read")
        .result
        .records
        .iter()
        .map(|row| {
            let Some(Value::Text(id)) = row.get("message_id") else {
                panic!("message id")
            };
            format!("'{id}'")
        })
        .collect()
    }

    #[cfg(unix)]
    fn committed_queue_batches(path: &std::path::Path) -> usize {
        crate::storage::wal::WalReader::open(reddb_file::unified_wal_path(path))
            .expect("WAL")
            .iter()
            .collect::<std::io::Result<Vec<_>>>()
            .expect("records")
            .into_iter()
            .filter(|(_, record)| {
                matches!(record, crate::storage::wal::WalRecord::TxCommitBatch { .. })
            })
            .count()
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_queue_delivery_and_ack_preserve_ownership_after_cold_boot() {
        const CONSUMERS: usize = 8;
        const PER_CONSUMER: usize = 4;
        for bounded in [false, true] {
            let directory = tempfile::tempdir().expect("directory");
            let path = directory.path().join("concurrent.rdb");
            let options = || {
                RedDBOptions::persistent(&path)
                    .with_memory_budget(64 * 1024 * 1024)
                    .with_bounded_disk_residency(bounded)
                    .with_durability_mode(crate::api::DurabilityMode::WalDurableGrouped)
                    .with_group_commit_window_ms(20)
                    .with_group_commit_max_statements(CONSUMERS)
            };
            let runtime = RedDBRuntime::with_options(options()).expect("runtime");
            runtime
                .execute_query("CREATE QUEUE competing")
                .expect("queue");
            runtime
                .execute_query("QUEUE GROUP CREATE competing workers")
                .expect("group");
            for message in 0..CONSUMERS * PER_CONSUMER {
                push(&runtime, "competing", &format!("message-{message}"));
            }
            runtime.checkpoint().expect("cold source rows");
            let before_delivery = bounded.then(|| committed_queue_batches(&path));
            let start = Arc::new(std::sync::Barrier::new(CONSUMERS));
            let deliveries = std::thread::scope(|scope| {
                let readers: Vec<_> = (0..CONSUMERS)
                    .map(|consumer| {
                        let runtime = &runtime;
                        let start = Arc::clone(&start);
                        scope.spawn(move || {
                            super::super::impl_core::set_current_connection_id(
                                80_000 + consumer as u64,
                            );
                            start.wait();
                            let result = runtime
                                .execute_query(&format!(
                                    "QUEUE READ competing GROUP workers CONSUMER c{consumer} COUNT {PER_CONSUMER}"
                                ))
                                .expect("concurrent read");
                            assert_eq!(result.result.records.len(), PER_CONSUMER);
                            result.result.records.into_iter().map(|row| {
                                let Some(Value::Text(id)) = row.get("message_id") else {
                                    panic!("message id")
                                };
                                id.to_string()
                            }).collect::<Vec<_>>()
                        })
                    })
                    .collect();
                readers
                    .into_iter()
                    .map(|reader| reader.join().expect("reader"))
                    .collect::<Vec<_>>()
            });
            let unique: std::collections::HashSet<_> = deliveries.iter().flatten().collect();
            assert_eq!(
                unique.len(),
                CONSUMERS * PER_CONSUMER,
                "exclusive ownership"
            );
            if let Some(before) = before_delivery {
                let delivery_batches = committed_queue_batches(&path) - before;
                assert!(
                    delivery_batches > 0 && delivery_batches < CONSUMERS,
                    "concurrent READs must share durable publications: {delivery_batches}"
                );
            }
            assert!(batch_ids(&runtime, "competing", "workers", 1).is_empty());

            // Race the same ACK set twice. Exactly one caller may retire it;
            // unrelated ACKs must still commit successfully.
            let start = Arc::new(std::sync::Barrier::new(CONSUMERS + 1));
            let before_ack = bounded.then(|| committed_queue_batches(&path));
            std::thread::scope(|scope| {
                let writers: Vec<_> = (0..=CONSUMERS)
                    .map(|writer| {
                        let runtime = &runtime;
                        let start = Arc::clone(&start);
                        let ids = &deliveries[writer % CONSUMERS];
                        scope.spawn(move || {
                            super::super::impl_core::set_current_connection_id(
                                81_000 + writer as u64,
                            );
                            let ids = ids
                                .iter()
                                .map(|id| format!("'{id}'"))
                                .collect::<Vec<_>>()
                                .join(",");
                            start.wait();
                            runtime
                                .execute_query(&format!(
                                    "QUEUE ACK competing BATCH GROUP workers ({ids})"
                                ))
                                .is_ok()
                        })
                    })
                    .collect();
                let successes: Vec<_> = writers
                    .into_iter()
                    .map(|writer| writer.join().expect("writer"))
                    .collect();
                assert_ne!(successes[0], successes[CONSUMERS], "one ACK wins");
                assert!(successes[1..CONSUMERS].iter().all(|success| *success));
            });
            if let Some(before) = before_ack {
                let ack_batches = committed_queue_batches(&path) - before;
                assert!(
                    ack_batches > 0 && ack_batches < CONSUMERS,
                    "concurrent ACKs must share durable publications: {ack_batches}"
                );
            }
            drop(runtime);
            // First boot replays WAL; later boots read checkpointed state.
            for _ in 0..3 {
                let runtime = RedDBRuntime::with_options(options()).expect("cold boot");
                assert!(
                    batch_ids(&runtime, "competing", "workers", CONSUMERS * PER_CONSUMER)
                        .is_empty()
                );
                assert!(runtime
                    .execute_query("QUEUE PENDING competing GROUP workers")
                    .expect("pending")
                    .result
                    .records
                    .is_empty());
                assert!(runtime
                    .execute_query("QUEUE PEEK competing 100")
                    .expect("retired payloads")
                    .result
                    .records
                    .is_empty());
                runtime.checkpoint().expect("checkpoint");
            }
        }
    }

    #[test]
    fn ack_batch_validates_all_handles_and_preserves_fanout_groups() {
        let rt = boot();
        for mode in ["WORK", "FANOUT"] {
            let q = format!("batch_{}", mode.to_lowercase());
            rt.execute_query(&format!("CREATE QUEUE {q} {mode}"))
                .expect("queue");
            for group in ["a", "b"] {
                rt.execute_query(&format!("QUEUE GROUP CREATE {q} {group}"))
                    .expect("group");
            }
            for value in ["one", "two", "three"] {
                push(&rt, &q, value);
            }
            let ids = batch_ids(&rt, &q, "a", 3);
            assert_eq!(ids.len(), 3);
            for bad in [
                format!("{}, '99999999'", ids[0]),
                format!("{}, {}", ids[0], ids[0]),
            ] {
                assert!(rt
                    .execute_query(&format!("QUEUE ACK {q} BATCH GROUP a ({bad})"))
                    .is_err());
                assert_eq!(
                    rt.execute_query(&format!("QUEUE PENDING {q} GROUP a"))
                        .expect("pending")
                        .result
                        .records
                        .len(),
                    3
                );
            }
            rt.execute_query(&format!("QUEUE ACK {q} BATCH GROUP a ({})", ids.join(",")))
                .expect("ack");
            assert!(batch_ids(&rt, &q, "a", 3).is_empty());
            assert_eq!(
                batch_ids(&rt, &q, "b", 3).len(),
                if mode == "FANOUT" { 3 } else { 0 }
            );
        }
    }

    #[test]
    fn ack_batch_rollback_restores_messages() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE batch_tx").expect("queue");
        rt.execute_query("QUEUE GROUP CREATE batch_tx workers")
            .expect("group");
        for value in ["one", "two"] {
            push(&rt, "batch_tx", value);
        }
        let ids = batch_ids(&rt, "batch_tx", "workers", 2);
        rt.execute_query("BEGIN").expect("begin");
        rt.execute_query(&format!(
            "QUEUE ACK batch_tx BATCH GROUP workers ({})",
            ids.join(",")
        ))
        .expect("ack");
        rt.execute_query("ROLLBACK").expect("rollback");
        assert_eq!(
            rt.execute_query("SELECT payload FROM QUEUE batch_tx")
                .expect("messages")
                .result
                .records
                .len(),
            2
        );
        let redelivered = batch_ids(&rt, "batch_tx", "workers", 2);
        assert_eq!(redelivered.len(), 2);
        rt.execute_query(&format!(
            "QUEUE ACK batch_tx BATCH GROUP workers ({})",
            redelivered.join(",")
        ))
        .expect("ack redelivery after rollback");
    }

    #[test]
    fn queue_table_join_filters_relevance_without_consuming_messages() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE email_jobs").expect("queue");
        rt.execute_query("CREATE TABLE recipients (email TEXT, opened BOOLEAN)")
            .expect("table");
        rt.execute_query("INSERT INTO recipients (email, opened) VALUES ('a@example.com', false), ('b@example.com', true)").expect("rows");
        push(&rt, "email_jobs", "a@example.com");
        push(&rt, "email_jobs", "b@example.com");
        let sql = "SELECT q.payload FROM email_jobs q JOIN recipients r ON q.payload = r.email WHERE r.opened = false";
        let relevant = rt.execute_query(sql).expect("join");
        assert_eq!(relevant.result.records.len(), 1);
        assert_eq!(
            relevant.result.records[0].get("q.payload"),
            Some(&Value::text("a@example.com"))
        );
        rt.execute_query("UPDATE recipients SET opened = true WHERE email = 'a@example.com'")
            .expect("opened");
        assert!(rt
            .execute_query(sql)
            .expect("fresh relevance")
            .result
            .records
            .is_empty());
        assert_eq!(
            rt.execute_query("QUEUE PEEK email_jobs 10")
                .expect("unconsumed")
                .result
                .records
                .len(),
            2
        );
    }

    #[cfg(unix)]
    #[test]
    fn paged_delivery_and_ack_each_publish_one_wal_batch() {
        for mode in [
            crate::api::DurabilityMode::Strict,
            crate::api::DurabilityMode::WalDurableGrouped,
        ] {
            for count in [3usize, 257] {
                let directory = tempfile::tempdir().expect("directory");
                let path = directory.path().join("data.rdb");
                let rt = RedDBRuntime::with_options(
                    crate::RedDBOptions::persistent(&path)
                        .with_memory_budget(64 * 1024 * 1024)
                        .with_bounded_disk_residency(true)
                        .with_durability_mode(mode),
                )
                .expect("runtime");
                rt.execute_query("CREATE QUEUE batches").expect("queue");
                rt.execute_query("QUEUE GROUP CREATE batches workers")
                    .expect("group");
                for _ in 0..count {
                    push(&rt, "batches", "payload");
                }
                rt.checkpoint().expect("base checkpoint");
                let batches = || {
                    crate::storage::wal::WalReader::open(reddb_file::unified_wal_path(&path))
                        .expect("WAL")
                        .iter()
                        .collect::<std::io::Result<Vec<_>>>()
                        .expect("records")
                        .into_iter()
                        .filter(|(_, record)| {
                            matches!(record, crate::storage::wal::WalRecord::TxCommitBatch { .. })
                        })
                        .count()
                };
                let before = batches();
                let result = rt
                    .execute_query(&format!(
                        "QUEUE READ batches GROUP workers CONSUMER c COUNT {count}"
                    ))
                    .expect("read");
                assert_eq!(
                    batches() - before,
                    count.div_ceil(256),
                    "bounded reservation and attempt batches"
                );
                assert_eq!(result.result.records.len(), count);
                let before = batches();
                for chunk in result.result.records.chunks(256) {
                    let ids = chunk
                        .iter()
                        .map(|row| {
                            let Some(Value::Text(id)) = row.get("message_id") else {
                                panic!("message id")
                            };
                            format!("'{id}'")
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    rt.execute_query(&format!("QUEUE ACK batches BATCH GROUP workers ({ids})"))
                        .expect("ACK batch");
                }
                assert_eq!(
                    batches() - before,
                    count.div_ceil(256),
                    "bounded message and metadata retirement batches"
                );
            }
        }
    }

    #[test]
    fn failed_delivery_and_ack_do_not_publish_partial_queue_state() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE publication_queue")
            .expect("queue");
        push(&rt, "publication_queue", "payload");
        let ps = PrimaryQueueStore::new(rt.clone());
        let message = ps.list_queue_messages("publication_queue")[0].id.raw();
        let store = rt.db().store();
        let txn = ps.new_txn();
        store
            .test_fail_deferred_wal_append
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(ps
            .mark_pending_for_read(
                &txn,
                "publication_queue",
                message,
                "workers",
                "c",
                Instant::now() + Duration::from_secs(60),
                true
            )
            .is_err());
        assert!(ps
            .find_pending_by_key("publication_queue", message, "workers")
            .is_none());
        assert_eq!(ps.read_attempts("publication_queue", message, "workers"), 0);
        let (delivery, attempts) = ps
            .mark_pending_for_read(
                &txn,
                "publication_queue",
                message,
                "workers",
                "c",
                Instant::now() + Duration::from_secs(60),
                true,
            )
            .expect("reserve");
        assert_eq!(attempts, 1);
        store
            .test_fail_deferred_wal_append
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(ps.ack_pending(&txn, &delivery).is_err());
        assert!(ps.read_message("publication_queue", message).is_some());
        assert_eq!(
            ps.find_pending_by_key("publication_queue", message, "workers"),
            Some(delivery.clone())
        );
        assert_eq!(ps.read_attempts("publication_queue", message, "workers"), 1);
        ps.ack_pending(&txn, &delivery).expect("retry ACK");
        assert!(ps.read_message("publication_queue", message).is_none());
        assert!(ps
            .find_pending_by_key("publication_queue", message, "workers")
            .is_none());
    }

    #[test]
    fn failed_multi_message_publication_restores_every_delivery_and_ack() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE batch_failure")
            .expect("queue");
        for value in ["one", "two", "three"] {
            push(&rt, "batch_failure", value);
        }
        let ps = PrimaryQueueStore::new(rt.clone());
        let messages = list_message_ids(&ps, "batch_failure");
        let store = rt.db().store();
        let txn = ps.new_txn();
        store
            .test_fail_deferred_wal_append
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(ps
            .mark_pending_many_for_read(
                &txn,
                "batch_failure",
                &messages,
                "workers",
                "c",
                Instant::now() + Duration::from_secs(60),
                true
            )
            .is_err());
        for &message in &messages {
            assert!(ps
                .find_pending_by_key("batch_failure", message, "workers")
                .is_none());
            assert_eq!(ps.read_attempts("batch_failure", message, "workers"), 0);
        }
        let deliveries = ps
            .mark_pending_many_for_read(
                &txn,
                "batch_failure",
                &messages,
                "workers",
                "c",
                Instant::now() + Duration::from_secs(60),
                true,
            )
            .expect("reserve")
            .into_iter()
            .map(|(delivery, attempts)| {
                assert_eq!(attempts, 1);
                delivery
            })
            .collect::<Vec<_>>();
        store
            .test_fail_deferred_wal_append
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(ps.ack_many(&txn, &deliveries, false).is_err());
        for (&message, delivery) in messages.iter().zip(&deliveries) {
            assert!(ps.read_message("batch_failure", message).is_some());
            assert_eq!(
                ps.find_pending_by_key("batch_failure", message, "workers"),
                Some(delivery.clone())
            );
            assert_eq!(ps.read_attempts("batch_failure", message, "workers"), 1);
        }
        ps.ack_many(&txn, &deliveries, false).expect("retry ack");
        assert!(list_message_ids(&ps, "batch_failure").is_empty());
    }

    #[test]
    fn queue_selection_and_next_position_do_not_copy_payloads() {
        use crate::storage::no_malloc_hot_paths::measure_allocations;
        let observe = |payload_bytes| {
            let runtime = boot();
            let store = runtime.db().store();
            let manager = store.get_or_create_collection("projection");
            for id in (1..=128).rev() {
                manager
                    .insert(UnifiedEntity::new(
                        EntityId::new(id),
                        EntityKind::QueueMessage {
                            queue: "projection".into(),
                            position: id * 10,
                        },
                        EntityData::QueueMessage(QueueMessageData {
                            payload: Value::Blob(vec![7; payload_bytes]),
                            priority: None,
                            enqueued_at_ns: 0,
                            attempts: 0,
                            max_attempts: 0,
                            acked: id == 64,
                        }),
                    ))
                    .expect("queue message");
            }
            let adapter = PrimaryQueueStore::new(runtime);
            let ((left, right, next), allocations) = measure_allocations(|| {
                (
                    adapter.available_messages("projection", QueueSide::Left),
                    adapter.available_messages("projection", QueueSide::Right),
                    adapter.next_position("projection").expect("position"),
                )
            });
            let expected: Vec<_> = (1..=128).filter(|id| *id != 64).collect();
            assert_eq!(left, expected);
            assert_eq!(right, expected.into_iter().rev().collect::<Vec<_>>());
            assert_eq!(next, 1281);
            allocations
        };
        let empty = observe(0);
        let large = observe(32 * 1024);
        assert!(
            large.allocated_bytes <= empty.allocated_bytes + 64 * 1024,
            "queue selection copied payloads: {empty:?} -> {large:?}"
        );
        eprintln!(
            "queue selection allocated bytes, empty/32 KiB payload: {} / {}",
            empty.allocated_bytes, large.allocated_bytes
        );
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
    fn queue_delivery_and_ack_do_not_scan_catalog_payloads() {
        let rt = boot();
        rt.execute_query("CREATE TABLE unrelated (id INT)")
            .expect("table");
        for id in 0..64 {
            rt.execute_query(&format!("INSERT INTO unrelated (id) VALUES ({id})"))
                .expect("unrelated data");
        }
        rt.execute_query("CREATE QUEUE hot_queue").expect("queue");
        rt.execute_query("QUEUE GROUP CREATE hot_queue workers")
            .expect("group");
        for batch in [false, true] {
            push(&rt, "hot_queue", "payload");
            crate::catalog::take_observed_entities_for_test();
            let ids = batch_ids(&rt, "hot_queue", "workers", 1);
            let sql = if batch {
                format!("QUEUE ACK hot_queue BATCH GROUP workers ({})", ids[0])
            } else {
                format!("QUEUE ACK hot_queue GROUP workers {}", ids[0])
            };
            rt.execute_query(&sql).expect("ACK");
            assert_eq!(
                crate::catalog::take_observed_entities_for_test(),
                0,
                "delivery/ACK must not inspect unrelated collection payloads"
            );
        }
    }

    #[test]
    fn queue_lifecycle_policy_tracks_alter_and_cold_boot() {
        let directory = tempfile::tempdir().expect("fixture");
        let options = RedDBOptions::persistent(directory.path().join("data.rdb"))
            .with_memory_budget(64 * 1024 * 1024)
            .with_bounded_disk_residency(true);
        for boot_index in 0..3 {
            let rt = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
                RedDBRuntime::with_options(options.clone())
            })
            .expect("runtime");
            if boot_index == 0 {
                rt.execute_query(
                    "CREATE QUEUE policies FANOUT LOCK_DEADLINE_MS 4000 WITH DLQ failures",
                )
                .expect("queue");
            }
            let ps = PrimaryQueueStore::new(rt.clone());
            let config = ps.lifecycle_config("policies");
            assert_eq!(
                config.mode,
                if boot_index < 2 {
                    QueueMode::Fanout
                } else {
                    QueueMode::Work
                }
            );
            assert_eq!(
                config.lock_duration,
                Duration::from_millis(if boot_index < 2 { 4000 } else { 9000 })
            );
            assert_eq!(
                config.dlq_target.as_deref(),
                Some(if boot_index < 2 {
                    "failures"
                } else {
                    "other_failures"
                })
            );
            if boot_index == 1 {
                rt.execute_query("ALTER QUEUE policies SET MODE WORK")
                    .expect("mode");
                rt.execute_query("ALTER QUEUE policies SET LOCK_DEADLINE_MS 9000")
                    .expect("deadline");
                rt.execute_query("ALTER QUEUE policies SET DLQ other_failures")
                    .expect("DLQ");
                let updated = ps.lifecycle_config("policies");
                assert_eq!(updated.mode, QueueMode::Work);
                assert_eq!(updated.lock_duration, Duration::from_millis(9000));
                assert_eq!(updated.dlq_target.as_deref(), Some("other_failures"));
            }
            rt.checkpoint().expect("checkpoint policy");
        }
    }

    #[test]
    fn lifecycle_config_falls_back_to_defaults_for_unknown_queue() {
        use reddb_rql::ast::DEFAULT_QUEUE_LOCK_DEADLINE_MS;
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

        let first = lc
            .deliver(&QueueTxn::new(), "qround", "workers", 1)
            .expect("deliver");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].payload, Value::text("alpha"));
        assert!(!first[0].delivery_id.is_empty());

        // Pending row blocks redelivery of the same message.
        let second = lc
            .deliver(&QueueTxn::new(), "qround", "workers", 1)
            .expect("deliver-2");
        assert_eq!(second.len(), 1);
        assert_ne!(second[0].delivery_id, first[0].delivery_id);
        assert_eq!(second[0].payload, Value::text("beta"));

        lc.ack(&QueueTxn::new(), &first[0].delivery_id)
            .expect("ack");

        // After ack the underlying message is gone — only `beta` remains and it's pending.
        let remaining = list_message_ids(&lc.store_ref(), "qround");
        assert_eq!(remaining.len(), 1);

        lc.ack(&QueueTxn::new(), &second[0].delivery_id)
            .expect("ack-2");
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
        let a = lc
            .deliver(&QueueTxn::new(), "qdlq", "workers", 1)
            .expect("deliver-a");
        assert_eq!(a[0].payload, Value::text("payload"));
        lc.nack(&QueueTxn::new(), &a[0].delivery_id)
            .expect("nack-a");

        // Second nack → MovedToDlq.
        let b = lc
            .deliver(&QueueTxn::new(), "qdlq", "workers", 1)
            .expect("deliver-b");
        assert_eq!(b[0].payload, Value::text("payload"), "redelivered original");
        lc.nack(&QueueTxn::new(), &b[0].delivery_id)
            .expect("nack-b");

        assert_eq!(
            lc.recorded_outcomes(),
            vec![
                RetirementOutcome::Requeued,
                RetirementOutcome::MovedToDlq("qdlq_dlq".to_string()),
            ]
        );

        // Source queue is now empty.
        assert!(lc
            .deliver(&QueueTxn::new(), "qdlq", "workers", 1)
            .unwrap()
            .is_empty());

        // DLQ has the original payload.
        let dlq_msgs = lc.store_ref().list_queue_messages("qdlq_dlq");
        assert_eq!(dlq_msgs.len(), 1);
        assert_eq!(
            lc.store_ref()
                .read_message("qdlq_dlq", dlq_msgs[0].id.raw()),
            Some(Value::text("payload"))
        );
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
            .mark_pending(&t, "qpersist", mid, "g", "c", deadline)
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
            .mark_pending(
                &t,
                "qpersist",
                mid,
                "g",
                "c",
                deadline + Duration::from_millis(500),
            )
            .expect("mark-2");
        assert_eq!(id, id2);
    }

    #[test]
    fn mark_pending_on_unknown_queue_errors() {
        let rt = boot();
        let ps = PrimaryQueueStore::new(rt);
        let t = QueueTxn::new();
        let err = ps
            .mark_pending(
                &t,
                "nope",
                1,
                "g",
                "c",
                Instant::now() + Duration::from_secs(1),
            )
            .unwrap_err();
        assert!(matches!(err, QueueStoreError::UnknownQueue(_)));
    }

    #[test]
    fn cold_reopen_uses_persisted_queue_indices_without_changing_ack_retry() {
        let directory = tempfile::tempdir().expect("queue index fixture");
        let path = directory.path().join("queues.rdb");
        let options = RedDBOptions::persistent(path.clone())
            .with_memory_budget(64 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("profile");
        let runtime = RedDBRuntime::with_options(options.clone()).expect("seed runtime");
        runtime
            .execute_query("CREATE QUEUE indexed_queue MAX_ATTEMPTS 7")
            .expect("queue");
        push(&runtime, "indexed_queue", "first");
        push(&runtime, "indexed_queue", "second");
        let seed = PrimaryQueueStore::new(runtime.clone());
        let message_id = seed.list_queue_messages("indexed_queue")[0].id.raw();
        let transaction = QueueTxn::new();
        let delivery_id = seed
            .mark_pending(
                &transaction,
                "indexed_queue",
                message_id,
                "workers",
                "worker-1",
                Instant::now() + Duration::from_secs(30),
            )
            .expect("pending");
        assert_eq!(
            seed.bump_attempt(&transaction, &delivery_id)
                .expect("attempt")
                .attempts,
            1
        );
        // Publish the one-time result-cache metadata before repeated cold
        // boots start comparing queue index roots. The queue contract is that
        // unchanged queue data reuses its roots; a store-format activation is
        // an unrelated structural checkpoint.
        assert!(runtime.result_cache_snapshot().0.is_some());
        runtime.flush().expect("queue checkpoint");
        drop(seed);
        drop(runtime);

        let reopened = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
            RedDBRuntime::with_options(options.clone())
        })
        .expect("cold runtime");
        assert!(
            crate::runtime::queue_storage_index::ordered_messages(&reopened, "indexed_queue")
                .expect("position index")
                .is_some()
        );
        assert!(
            crate::runtime::queue_storage_index::delivery_rows(&reopened, &delivery_id)
                .expect("delivery index")
                .is_some()
        );
        let store = PrimaryQueueStore::new(reopened);
        assert_eq!(
            store.read_message("indexed_queue", message_id),
            Some(Value::text("first"))
        );
        assert_eq!(store.read_max_attempts("indexed_queue", message_id), 7);
        let restored = store
            .read_pending_attempt(&delivery_id)
            .expect("pending after reopen");
        assert_eq!(restored.message_id, message_id);
        assert_eq!(restored.attempts, 1);
        store
            .ack_pending(&QueueTxn::new(), &delivery_id)
            .expect("ack after reopen");
        assert_eq!(
            store
                .available_messages("indexed_queue", QueueSide::Left)
                .len(),
            1
        );
        store.runtime.flush().expect("ack checkpoint");
        assert!(
            store
                .runtime
                .db()
                .store()
                .pager()
                .expect("paged queue store")
                .header()
                .expect("queue file header")
                .version
                >= reddb_file::file_format::PAGE_RESULT_CACHE_DISK_FILE_VERSION,
            "disk-resident checkpoint must promote the cache-aware file format"
        );
        drop(store);

        let mut warmed_pages = 0;
        let mut warmed_bytes = 0;
        let mut published_roots = None;
        for boot in 0..5 {
            let runtime = crate::storage::RedDB::with_disk_residency_for_qualification(|| {
                RedDBRuntime::with_options(options.clone())
            })
            .expect("repeat cold runtime");
            assert!(crate::runtime::queue_storage_index::ordered_messages(
                &runtime,
                "indexed_queue"
            )
            .expect("published queue position index")
            .is_some());
            runtime.flush().expect("repeat cold checkpoint");
            let roots = crate::runtime::queue_storage_index::published_roots(
                runtime.db().store().as_ref(),
                "indexed_queue",
            )
            .expect("all queue index roots published");
            match &published_roots {
                Some(expected) => assert_eq!(
                    &roots, expected,
                    "unchanged queue cold boot replaced a published index root at boot {boot}"
                ),
                None => published_roots = Some(roots),
            }
            let pages = runtime
                .db()
                .store()
                .pager()
                .expect("paged queue store")
                .page_count()
                .expect("queue page count");
            let bytes = std::fs::metadata(&path).expect("queue database file").len();
            if boot == 1 {
                warmed_pages = pages;
                warmed_bytes = bytes;
            } else if boot > 1 {
                assert!(
                    pages <= warmed_pages + 32,
                    "unchanged queue cold boot grew from {warmed_pages} to {pages} pages at boot {boot}"
                );
                assert!(
                    bytes <= warmed_bytes + 32 * crate::storage::engine::PAGE_SIZE as u64,
                    "unchanged queue cold boot grew from {warmed_bytes} to {bytes} bytes at boot {boot}"
                );
            }
            drop(runtime);
        }
    }

    #[test]
    fn ack_unknown_delivery_errors() {
        let rt = boot();
        let ps = PrimaryQueueStore::new(rt);
        let t = QueueTxn::new();
        let err = ps.ack_pending(&t, "nope").unwrap_err();
        assert!(matches!(err, QueueStoreError::UnknownDelivery(_)));
    }

    // Issue #625 — legacy `queue_pending` divergence. A message reserved
    // through the pre-lifecycle delivery path (`QUEUE READ`, which persists
    // a `queue_pending` meta-row) must be resolvable and ack-able through
    // the lifecycle adapter. `_work_default` is the WORK-mode default
    // consumer group the legacy read path auto-creates when GROUP is
    // omitted (see `impl_queue::WORK_DEFAULT_GROUP`).
    const WORK_DEFAULT_GROUP: &str = "_work_default";

    #[test]
    fn legacy_pending_is_resolvable_and_ackable_via_lifecycle() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE qlegacy").expect("create");
        push(&rt, "qlegacy", "legacy-payload");

        // Reserve via the LEGACY delivery path — persists a `queue_pending`
        // row, never a `queue_pending_lc` row.
        rt.execute_query("QUEUE READ qlegacy CONSUMER c1 COUNT 1")
            .expect("legacy read reserves the message");

        let ps = PrimaryQueueStore::new(rt);
        let msgs = ps.list_queue_messages("qlegacy");
        assert_eq!(msgs.len(), 1);
        let mid = msgs[0].id.raw();

        // The lifecycle adapter must SEE the legacy pending state.
        let delivery_id = ps
            .find_pending_by_key("qlegacy", mid, WORK_DEFAULT_GROUP)
            .expect("legacy-reserved message must resolve through the lifecycle adapter");
        assert!(!delivery_id.is_empty());

        // Idempotent: a second resolve returns the same handle (the first
        // call migrated the row into the lifecycle representation).
        assert_eq!(
            ps.find_pending_by_key("qlegacy", mid, WORK_DEFAULT_GROUP)
                .as_deref(),
            Some(delivery_id.as_str()),
        );

        // ...and the handle is ack-able through the lifecycle adapter.
        let t = QueueTxn::new();
        ps.ack_pending(&t, &delivery_id)
            .expect("ack legacy-reserved message via lifecycle adapter");
        assert!(
            ps.list_queue_messages("qlegacy").is_empty(),
            "message must be gone after lifecycle ack",
        );
    }

    #[test]
    fn legacy_pending_excluded_from_available_and_counters_agree() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE qcount").expect("create");
        push(&rt, "qcount", "p");

        // Legacy read reserves the message and stamps delivery_count = 1.
        rt.execute_query("QUEUE READ qcount CONSUMER c1 COUNT 1")
            .expect("legacy read");

        let ps = PrimaryQueueStore::new(rt);
        let mid = ps.list_queue_messages("qcount")[0].id.raw();

        // No double-counting: a legacy-pending message must not look
        // available to the lifecycle adapter, or `deliver` would reserve a
        // *second*, parallel `queue_pending_lc` row for the same message.
        assert!(
            ps.available_messages("qcount", QueueSide::Left).is_empty(),
            "legacy-pending message must not appear available to the lifecycle adapter",
        );

        // Resolving migrates the row; the lifecycle attempts counter must
        // agree with the legacy delivery_count (1) for the same message.
        let _ = ps
            .find_pending_by_key("qcount", mid, WORK_DEFAULT_GROUP)
            .expect("resolve");
        assert_eq!(
            ps.read_attempts("qcount", mid, WORK_DEFAULT_GROUP),
            1,
            "lifecycle attempts must match the legacy delivery_count",
        );

        // Exactly one representation after the slice: the message is
        // pending once (the legacy row was migrated away, not duplicated).
        let pending = ps.pending_message_ids("qcount", None);
        assert_eq!(
            pending.iter().filter(|&&m| m == mid).count(),
            1,
            "message must be pending in exactly one representation",
        );
    }

    // Issue #624 — QueueTxn wired to runtime MVCC (rollback-safe ack).
    //
    // A lifecycle ack runs the underlying message retirement through
    // `current_xid → set_xmax → record_pending_tombstone` instead of an
    // immediate hard delete. Inside a transaction that rolls back, the
    // message must be visible again; on commit it must be gone — parity
    // with the pre-lifecycle delete-with-state behavior.
    //
    // Observable through the legacy `QUEUE READ` path, which applies MVCC
    // snapshot visibility (so a committed tombstone hides the message and
    // a rolled-back one revives it). `new_txn()` binds the live runtime
    // connection so `ack_pending` drives the real MVCC retire.

    #[test]
    fn lifecycle_ack_rolled_back_revives_message() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE qrollback").expect("create");
        push(&rt, "qrollback", "m");

        let ps = PrimaryQueueStore::new(rt.clone());
        let mid = ps.list_queue_messages("qrollback")[0].id.raw();

        // Reserve a lifecycle delivery, then ack it *inside* a transaction
        // and roll back. The MVCC retire must be undone.
        let txn = ps.new_txn();
        let did = ps
            .mark_pending(
                &txn,
                "qrollback",
                mid,
                "g",
                "c",
                Instant::now() + Duration::from_secs(30),
            )
            .expect("reserve");

        rt.execute_query("BEGIN").expect("begin");
        ps.ack_pending(&txn, &did).expect("ack inside txn");
        rt.execute_query("ROLLBACK").expect("rollback");

        let read = rt
            .execute_query("QUEUE READ qrollback CONSUMER c COUNT 5")
            .expect("read after rollback");
        assert_eq!(
            read.result.len(),
            1,
            "rolled-back lifecycle ack must leave the message visible again",
        );
    }

    #[test]
    fn lifecycle_ack_committed_removes_message_matches_legacy() {
        let rt = boot();
        rt.execute_query("CREATE QUEUE qcommit").expect("create");
        push(&rt, "qcommit", "m");

        let ps = PrimaryQueueStore::new(rt.clone());
        let mid = ps.list_queue_messages("qcommit")[0].id.raw();

        let txn = ps.new_txn();
        let did = ps
            .mark_pending(
                &txn,
                "qcommit",
                mid,
                "g",
                "c",
                Instant::now() + Duration::from_secs(30),
            )
            .expect("reserve");

        rt.execute_query("BEGIN").expect("begin");
        ps.ack_pending(&txn, &did).expect("ack inside txn");
        rt.execute_query("COMMIT").expect("commit");

        let read = rt
            .execute_query("QUEUE READ qcommit CONSUMER c COUNT 5")
            .expect("read after commit");
        assert!(
            read.result.is_empty(),
            "committed lifecycle ack must remove the message \
             (parity with delete_message_with_state)",
        );
        // The retire is also observable on the bound txn — exactly one
        // tombstone for the acked message.
        assert_eq!(
            txn.recorded_tombstones(),
            vec![crate::storage::queue::lifecycle::TombstoneRecord {
                queue: "qcommit".to_string(),
                message_id: mid,
            }],
        );
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

        let a = lc
            .deliver(&QueueTxn::new(), "qfan", "subs.a", 1)
            .expect("deliver-a");
        let b = lc
            .deliver(&QueueTxn::new(), "qfan", "subs.b", 1)
            .expect("deliver-b");
        assert_eq!(a[0].payload, Value::text("shared"));
        assert_eq!(b[0].payload, Value::text("shared"));
        assert_ne!(a[0].delivery_id, b[0].delivery_id);

        lc.ack(&QueueTxn::new(), &a[0].delivery_id).expect("ack-a");
        // A no longer sees the message; B's pending row remains valid.
        assert!(lc
            .deliver(&QueueTxn::new(), "qfan", "subs.a", 1)
            .unwrap()
            .is_empty());
        lc.ack(&QueueTxn::new(), &b[0].delivery_id).expect("ack-b");
    }
}
