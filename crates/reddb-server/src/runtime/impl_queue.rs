//! Queue DDL and command execution

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::runtime::audit_log::{AuditAuthSource, AuditEvent, AuditFieldEscaper, Outcome};
use crate::storage::queue::QueueMode;
use crate::storage::unified::entity::{QueueMessageData, RowData};
use crate::storage::unified::{Metadata, MetadataValue, UnifiedStore};

use super::*;

// ---------------------------------------------------------------------------
// Outbox metrics (exposed via /metrics)
// ---------------------------------------------------------------------------

/// Total event push attempts that failed (queue full or other error) and
/// triggered DLQ routing.
pub static EVENTS_DRAIN_RETRIES_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Total events routed to the dead-letter queue.
pub static EVENTS_DLQ_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Total events successfully enqueued to their target queue.
pub static EVENTS_ENQUEUED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Warn when total estimated outbox payload bytes exceed this value (1 GiB).
const OUTBOX_WARN_BYTES: u64 = 1 << 30;

/// Route all new events to DLQ when estimated outbox exceeds this value (10 GiB).
const OUTBOX_MAX_BYTES: u64 = 10 * (1 << 30);

/// Running estimate of bytes pending in event queues (approximate; not decremented on consume).
static OUTBOX_APPROX_BYTES: AtomicU64 = AtomicU64::new(0);

const QUEUE_META_COLLECTION: &str = "red_queue_meta";
const QUEUE_POSITION_CENTER: u64 = u64::MAX / 2;
const WORK_DEFAULT_GROUP: &str = "_work_default";
const FANOUT_GROUP_PREFIX: &str = "_fanout_";

#[derive(Debug, Clone)]
pub(super) struct QueueRuntimeConfig {
    pub(super) mode: QueueMode,
    pub(super) priority: bool,
    pub(super) max_size: Option<usize>,
    pub(super) ttl_ms: Option<u64>,
    pub(super) dlq: Option<String>,
    pub(super) max_attempts: u32,
    pub(super) lock_deadline_ms: u64,
    pub(super) in_flight_cap_per_group: u32,
}

#[derive(Debug, Clone)]
struct QueueGroupEntry {
    entity_id: EntityId,
    group: String,
}

#[derive(Debug, Clone)]
pub(super) struct QueuePendingEntry {
    pub(super) entity_id: EntityId,
    group: String,
    pub(super) message_id: EntityId,
    consumer: String,
    pub(super) delivered_at_ns: u64,
    pub(super) delivery_count: u32,
}

#[derive(Debug, Clone)]
pub(super) struct QueueAckEntry {
    entity_id: EntityId,
    group: String,
    pub(super) message_id: EntityId,
}

#[derive(Debug, Clone)]
pub(super) struct QueueMessageView {
    pub(super) id: EntityId,
    position: u64,
    priority: i32,
    pub(super) payload: Value,
    attempts: u32,
    pub(super) max_attempts: u32,
    enqueued_at_ns: u64,
}

impl RedDBRuntime {
    pub(crate) fn enqueue_event_payload(
        &self,
        queue: &str,
        payload: Value,
    ) -> RedDBResult<EntityId> {
        let store = self.inner.db.store();
        // Auto-create the queue if it does not exist yet.
        if store.get_collection(queue).is_none() {
            crate::runtime::impl_ddl::ensure_event_target_queue_pub(self, queue)?;
        }

        // Estimate payload bytes for outbox watermark checks.
        let payload_bytes = estimate_payload_bytes(&payload);
        let outbox_bytes = OUTBOX_APPROX_BYTES.fetch_add(payload_bytes, Ordering::Relaxed);

        // Hard limit: route directly to DLQ without even trying.
        if outbox_bytes > OUTBOX_MAX_BYTES {
            OUTBOX_APPROX_BYTES.fetch_sub(payload_bytes, Ordering::Relaxed);
            EVENTS_DRAIN_RETRIES_TOTAL.fetch_add(1, Ordering::Relaxed);
            return self.route_event_to_outbox_dlq(queue, payload, "outbox_max_bytes_exceeded");
        }

        // Soft limit: warn once per crossing.
        if outbox_bytes > OUTBOX_WARN_BYTES && outbox_bytes - payload_bytes <= OUTBOX_WARN_BYTES {
            tracing::warn!(
                outbox_bytes,
                warn_threshold = OUTBOX_WARN_BYTES,
                "event outbox approaching capacity warning threshold"
            );
            crate::telemetry::operator_event::OperatorEvent::OutboxDlqActivated {
                queue: queue.to_string(),
                dlq: format!("{queue}_outbox_dlq"),
                reason: "outbox_warn_bytes_exceeded".to_string(),
            }
            .emit_global();
        }

        let config = load_queue_config(store.as_ref(), queue);

        // If the target queue has a max_size and is full, route to DLQ.
        if let Some(max_size) = config.max_size {
            let current_len = load_queue_message_views(store.as_ref(), queue)
                .unwrap_or_default()
                .len();
            if current_len >= max_size {
                OUTBOX_APPROX_BYTES.fetch_sub(payload_bytes, Ordering::Relaxed);
                EVENTS_DRAIN_RETRIES_TOTAL.fetch_add(1, Ordering::Relaxed);
                return self.route_event_to_outbox_dlq(queue, payload, "queue_full");
            }
            // Warn at 80% capacity.
            if current_len * 10 >= max_size * 8 {
                tracing::warn!(
                    queue = %queue,
                    size = current_len,
                    max = max_size,
                    "event target queue near capacity"
                );
            }
        }

        let id = self.enqueue_event_payload_raw(store.as_ref(), queue, &config, payload)?;
        EVENTS_ENQUEUED_TOTAL.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }

    /// Route a failed event to `<queue>_outbox_dlq`, auto-creating it if needed.
    fn route_event_to_outbox_dlq(
        &self,
        queue: &str,
        payload: Value,
        reason: &str,
    ) -> RedDBResult<EntityId> {
        let dlq_name = format!("{queue}_outbox_dlq");
        EVENTS_DLQ_TOTAL.fetch_add(1, Ordering::Relaxed);

        crate::telemetry::operator_event::OperatorEvent::OutboxDlqActivated {
            queue: queue.to_string(),
            dlq: dlq_name.clone(),
            reason: reason.to_string(),
        }
        .emit_global();

        let store = self.inner.db.store();
        if store.get_collection(&dlq_name).is_none() {
            crate::runtime::impl_ddl::ensure_event_target_queue_pub(self, &dlq_name)?;
        }
        let dlq_config = load_queue_config(store.as_ref(), &dlq_name);
        let id = self.enqueue_event_payload_raw(store.as_ref(), &dlq_name, &dlq_config, payload)?;
        EVENTS_ENQUEUED_TOTAL.fetch_add(1, Ordering::Relaxed);
        Ok(id)
    }

    /// Low-level event message insert — no size checks, no DLQ routing.
    fn enqueue_event_payload_raw(
        &self,
        store: &UnifiedStore,
        queue: &str,
        config: &QueueRuntimeConfig,
        payload: Value,
    ) -> RedDBResult<EntityId> {
        let position = next_queue_position(store, queue, QueueSide::Right)?;
        let mut entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::QueueMessage {
                queue: queue.to_string(),
                position,
            },
            EntityData::QueueMessage(QueueMessageData {
                payload,
                priority: None,
                enqueued_at_ns: now_ns(),
                attempts: 0,
                max_attempts: config.max_attempts,
                acked: false,
            }),
        );
        if let Some(xid) = self.current_xid() {
            entity.set_xmin(xid);
        }
        let id = store
            .insert_auto(queue, entity)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if let Some(ttl_ms) = config.ttl_ms {
            store
                .set_metadata(queue, id, queue_message_ttl_metadata(ttl_ms))
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        self.invalidate_result_cache_for_table(queue);
        Ok(id)
    }

    pub fn execute_create_queue(
        &self,
        raw_query: &str,
        query: &CreateQueueQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        if query.dlq.as_deref() == Some(query.name.as_str()) {
            return Err(RedDBError::Query(
                "dead-letter queue must be different from the source queue".to_string(),
            ));
        }

        let store = self.inner.db.store();
        let exists = store.get_collection(&query.name).is_some();
        if exists {
            if query.if_not_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("queue '{}' already exists", query.name),
                    "create",
                ));
            }
            return Err(RedDBError::Query(format!(
                "queue '{}' already exists",
                query.name
            )));
        }

        store
            .create_collection(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if let Some(ttl_ms) = query.ttl_ms {
            self.inner
                .db
                .set_collection_default_ttl_ms(&query.name, ttl_ms);
        }
        self.inner
            .db
            .save_collection_contract(queue_collection_contract(
                &query.name,
                query.priority,
                query.ttl_ms,
            ))
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        save_queue_config(
            store.as_ref(),
            &query.name,
            &QueueRuntimeConfig {
                mode: query.mode,
                priority: query.priority,
                max_size: query.max_size,
                ttl_ms: query.ttl_ms,
                dlq: query.dlq.clone(),
                max_attempts: query.max_attempts,
                lock_deadline_ms: query.lock_deadline_ms,
                in_flight_cap_per_group: query.in_flight_cap_per_group,
            },
        )?;

        if let Some(dlq) = &query.dlq {
            if store.get_collection(dlq).is_none() {
                store
                    .create_collection(dlq)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                self.inner
                    .db
                    .save_collection_contract(queue_collection_contract(dlq, false, None))
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
            }
        }

        self.invalidate_result_cache();
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        // Issue #120 — feed the queue into the schema-vocabulary so
        // AskPipeline (#121) can resolve queue references. Queues
        // have an opaque payload column, so we expose `payload` and
        // (when configured) the DLQ partner as type-tag context.
        let mut type_tags = Vec::new();
        if let Some(dlq) = &query.dlq {
            type_tags.push(format!("dlq:{}", dlq));
        }
        self.schema_vocabulary_apply(
            crate::runtime::schema_vocabulary::DdlEvent::CreateCollection {
                collection: query.name.clone(),
                columns: vec!["payload".to_string()],
                type_tags,
                description: None,
            },
        );

        let mut msg = format!("queue '{}' created", query.name);
        msg.push_str(&format!(" (mode={})", query.mode.as_str()));
        if query.priority {
            msg.push_str(" (priority)");
        }
        if let Some(max_size) = query.max_size {
            msg.push_str(&format!(" (max_size={max_size})"));
        }
        if let Some(ttl_ms) = query.ttl_ms {
            msg.push_str(&format!(" (ttl={ttl_ms}ms)"));
        }
        if let Some(dlq) = &query.dlq {
            msg.push_str(&format!(
                " (dlq={dlq}, max_attempts={})",
                query.max_attempts
            ));
        }

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &msg,
            "create",
        ))
    }

    pub fn execute_alter_queue(
        &self,
        raw_query: &str,
        query: &AlterQueueQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();
        ensure_queue_exists(store.as_ref(), &query.name)?;

        let mut config = load_queue_config(store.as_ref(), &query.name);
        let mut summary: Vec<String> = Vec::new();

        if let Some(new_mode) = query.mode {
            let pending =
                load_pending_entries(store.as_ref(), &query.name, None, None).unwrap_or_default();
            if !pending.is_empty() {
                tracing::warn!(
                    queue = %query.name,
                    pending_count = pending.len(),
                    new_mode = %new_mode.as_str(),
                    "ALTER QUEUE SET MODE: {} in-flight messages will drain with old mode; \
                     new reads use {}",
                    pending.len(),
                    new_mode.as_str(),
                );
            }
            config.mode = new_mode;
            summary.push(format!("mode={}", new_mode.as_str()));
        }
        if let Some(max_attempts) = query.max_attempts {
            config.max_attempts = max_attempts;
            summary.push(format!("max_attempts={max_attempts}"));
        }
        if let Some(lock_deadline_ms) = query.lock_deadline_ms {
            config.lock_deadline_ms = lock_deadline_ms;
            summary.push(format!("lock_deadline_ms={lock_deadline_ms}"));
        }
        if let Some(in_flight_cap) = query.in_flight_cap_per_group {
            config.in_flight_cap_per_group = in_flight_cap;
            summary.push(format!("in_flight_cap_per_group={in_flight_cap}"));
        }
        if let Some(dlq) = &query.dlq {
            if dlq == &query.name {
                return Err(RedDBError::Query(
                    "dead-letter queue must be different from the source queue".to_string(),
                ));
            }
            config.dlq = Some(dlq.clone());
            summary.push(format!("dlq={dlq}"));
        }

        save_queue_config(store.as_ref(), &query.name, &config)?;

        self.invalidate_result_cache();
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("queue '{}' altered: {}", query.name, summary.join(", ")),
            "alter",
        ))
    }

    pub fn execute_drop_queue(
        &self,
        raw_query: &str,
        query: &DropQueueQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let store = self.inner.db.store();
        if super::impl_ddl::is_system_schema_name(&query.name) {
            return Err(RedDBError::Query("system schema is read-only".to_string()));
        }
        if store.get_collection(&query.name).is_none() {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("queue '{}' does not exist", query.name),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "queue '{}' not found",
                query.name
            )));
        }
        let actual = crate::runtime::ddl::polymorphic_resolver::resolve(
            &query.name,
            &self.inner.db.catalog_model_snapshot(),
        )?;
        crate::runtime::ddl::polymorphic_resolver::ensure_model_match(
            crate::catalog::CollectionModel::Queue,
            actual,
        )?;

        store
            .drop_collection(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner.db.clear_collection_default_ttl_ms(&query.name);
        self.inner
            .db
            .remove_collection_contract(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        remove_queue_metadata(store.as_ref(), &query.name);
        self.invalidate_result_cache();
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        // Issue #120 — invalidate the schema-vocabulary entry.
        self.schema_vocabulary_apply(
            crate::runtime::schema_vocabulary::DdlEvent::DropCollection {
                collection: query.name.clone(),
            },
        );

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("queue '{}' dropped", query.name),
            "drop",
        ))
    }

    pub fn execute_queue_command(
        &self,
        raw_query: &str,
        cmd: &QueueCommand,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        match cmd {
            QueueCommand::Push {
                queue,
                value,
                side,
                priority,
            } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let config = load_queue_config(store.as_ref(), queue);
                if priority.is_some() && !config.priority {
                    return Err(RedDBError::Query(format!(
                        "queue '{}' is not a priority queue",
                        queue
                    )));
                }
                if let Some(max_size) = config.max_size {
                    let current_len =
                        load_queue_message_views_with_runtime(Some(self), store.as_ref(), queue)?
                            .len();
                    if current_len >= max_size {
                        return Err(RedDBError::Query(format!(
                            "queue '{}' is full (max_size={max_size})",
                            queue
                        )));
                    }
                }

                let position = next_queue_position(store.as_ref(), queue, *side)?;
                let mut entity = UnifiedEntity::new(
                    EntityId::new(0),
                    EntityKind::QueueMessage {
                        queue: queue.clone(),
                        position,
                    },
                    EntityData::QueueMessage(QueueMessageData {
                        payload: value.clone(),
                        priority: if config.priority { *priority } else { None },
                        enqueued_at_ns: now_ns(),
                        attempts: 0,
                        max_attempts: config.max_attempts,
                        acked: false,
                    }),
                );
                // Phase 1.1 MVCC universal: stamp xmin so other
                // connections don't see this message until COMMIT.
                if let Some(xid) = self.current_xid() {
                    entity.set_xmin(xid);
                }
                let id = store
                    .insert_auto(queue, entity)
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                if let Some(ttl_ms) = config.ttl_ms {
                    store
                        .set_metadata(queue, id, queue_message_ttl_metadata(ttl_ms))
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                }
                self.invalidate_result_cache();

                let mut result = UnifiedResult::with_columns(vec![
                    "message_id".into(),
                    "side".into(),
                    "queue".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("message_id", Value::text(message_id_string(id)));
                record.set(
                    "side",
                    Value::text(match side {
                        QueueSide::Left => "left".to_string(),
                        QueueSide::Right => "right".to_string(),
                    }),
                );
                record.set("queue", Value::text(queue.clone()));
                result.push(record);

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "queue_push",
                    engine: "runtime-queue",
                    result,
                    affected_rows: 1,
                    statement_type: "insert",
                })
            }
            QueueCommand::Pop { queue, side, count } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let popped = super::queue_delivery::pop_messages(
                    self,
                    store.as_ref(),
                    queue,
                    *side,
                    *count,
                )?;

                let mut result =
                    UnifiedResult::with_columns(vec!["message_id".into(), "payload".into()]);
                for message in &popped {
                    let mut record = UnifiedRecord::new();
                    record.set(
                        "message_id",
                        Value::text(message_id_string(message.message_id)),
                    );
                    record.set("payload", message.payload.clone());
                    result.push(record);
                }
                let popped_count = popped.len() as u64;
                if popped_count > 0 {
                    self.invalidate_result_cache();
                }

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "queue_pop",
                    engine: "runtime-queue",
                    result,
                    affected_rows: popped_count,
                    statement_type: "delete",
                })
            }
            QueueCommand::Peek { queue, count } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let messages =
                    super::queue_delivery::peek_messages(self, store.as_ref(), queue, *count)?;

                let mut result =
                    UnifiedResult::with_columns(vec!["message_id".into(), "payload".into()]);
                for message in messages {
                    let mut record = UnifiedRecord::new();
                    record.set(
                        "message_id",
                        Value::text(message_id_string(message.message_id)),
                    );
                    record.set("payload", message.payload);
                    result.push(record);
                }

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "queue_peek",
                    engine: "runtime-queue",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            QueueCommand::Len { queue } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let count =
                    load_queue_message_views_with_runtime(Some(self), store.as_ref(), queue)?.len()
                        as u64;
                let mut result = UnifiedResult::with_columns(vec!["len".into()]);
                let mut record = UnifiedRecord::new();
                record.set("len", Value::UnsignedInteger(count));
                result.push(record);

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "queue_len",
                    engine: "runtime-queue",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            QueueCommand::Purge { queue } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let count = super::queue_delivery::purge_messages(self, store.as_ref(), queue)?;
                if count > 0 {
                    self.invalidate_result_cache();
                }

                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("{count} messages purged from queue '{queue}'"),
                    "delete",
                ))
            }
            QueueCommand::GroupCreate { queue, group } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                if queue_group_exists(store.as_ref(), queue, group)? {
                    return Ok(RuntimeQueryResult::ok_message(
                        raw_query.to_string(),
                        &format!(
                            "consumer group '{}' already exists on queue '{}'",
                            group, queue
                        ),
                        "create",
                    ));
                }
                save_queue_group(store.as_ref(), queue, group)?;
                self.invalidate_result_cache();

                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("consumer group '{}' created on queue '{}'", group, queue),
                    "create",
                ))
            }
            QueueCommand::GroupRead {
                queue,
                group,
                consumer,
                count,
            } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let delivered = super::queue_delivery::read_messages(
                    self,
                    store.as_ref(),
                    queue,
                    group.as_deref(),
                    consumer,
                    *count,
                )?;

                let mut result = UnifiedResult::with_columns(vec![
                    "message_id".into(),
                    "payload".into(),
                    "consumer".into(),
                    "delivery_count".into(),
                    "attempts".into(),
                ]);

                for message in delivered {
                    let mut record = UnifiedRecord::new();
                    record.set(
                        "message_id",
                        Value::text(message_id_string(message.message_id)),
                    );
                    record.set("payload", message.payload);
                    record.set("consumer", Value::text(message.consumer));
                    record.set(
                        "delivery_count",
                        Value::UnsignedInteger(u64::from(message.delivery_count)),
                    );
                    record.set(
                        "attempts",
                        Value::UnsignedInteger(u64::from(message.delivery_count)),
                    );
                    result.push(record);
                }
                if !result.records.is_empty() {
                    self.invalidate_result_cache();
                }

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "queue_group_read",
                    engine: "runtime-queue",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            QueueCommand::Pending { queue, group } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                require_queue_group(store.as_ref(), queue, group)?;
                let mut pending = load_pending_entries(store.as_ref(), queue, Some(group), None)?;
                pending.sort_by_key(|entry| entry.delivered_at_ns);
                let current_time_ns = now_ns();

                let mut result = UnifiedResult::with_columns(vec![
                    "message_id".into(),
                    "consumer".into(),
                    "delivered_at_ns".into(),
                    "delivery_count".into(),
                    "idle_ms".into(),
                ]);
                for entry in pending {
                    let mut record = UnifiedRecord::new();
                    record.set(
                        "message_id",
                        Value::text(message_id_string(entry.message_id)),
                    );
                    record.set("consumer", Value::text(entry.consumer));
                    record.set(
                        "delivered_at_ns",
                        Value::UnsignedInteger(entry.delivered_at_ns),
                    );
                    record.set(
                        "delivery_count",
                        Value::UnsignedInteger(u64::from(entry.delivery_count)),
                    );
                    record.set(
                        "idle_ms",
                        Value::UnsignedInteger(
                            current_time_ns.saturating_sub(entry.delivered_at_ns) / 1_000_000,
                        ),
                    );
                    result.push(record);
                }

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "queue_pending",
                    engine: "runtime-queue",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            QueueCommand::Claim {
                queue,
                group,
                consumer,
                min_idle_ms,
            } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let delivered = super::queue_delivery::claim_messages(
                    self,
                    store.as_ref(),
                    queue,
                    group,
                    consumer,
                    *min_idle_ms,
                )?;

                let mut result = UnifiedResult::with_columns(vec![
                    "message_id".into(),
                    "payload".into(),
                    "consumer".into(),
                    "delivery_count".into(),
                ]);

                for message in delivered {
                    let mut record = UnifiedRecord::new();
                    record.set(
                        "message_id",
                        Value::text(message_id_string(message.message_id)),
                    );
                    record.set("payload", message.payload);
                    record.set("consumer", Value::text(message.consumer));
                    record.set(
                        "delivery_count",
                        Value::UnsignedInteger(u64::from(message.delivery_count)),
                    );
                    result.push(record);
                }
                if !result.records.is_empty() {
                    self.invalidate_result_cache();
                }
                let affected_rows = result.records.len() as u64;

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "queue_claim",
                    engine: "runtime-queue",
                    result,
                    affected_rows,
                    statement_type: "update",
                })
            }
            QueueCommand::Ack {
                queue,
                group,
                message_id,
            } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                require_queue_group(store.as_ref(), queue, group)?;
                let message_id = parse_message_id(message_id)?;
                let config = load_queue_config(store.as_ref(), queue);
                super::queue_delivery::ack_message(
                    self,
                    store.as_ref(),
                    queue,
                    group,
                    message_id,
                    &config,
                )?;
                self.invalidate_result_cache();

                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    "message acknowledged",
                    "update",
                ))
            }
            QueueCommand::Nack {
                queue,
                group,
                message_id,
            } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                require_queue_group(store.as_ref(), queue, group)?;
                let message_id = parse_message_id(message_id)?;
                let config = load_queue_config(store.as_ref(), queue);
                let message = match super::queue_delivery::nack_message(
                    self,
                    store.as_ref(),
                    queue,
                    group,
                    message_id,
                    &config,
                )? {
                    super::queue_delivery::NackOutcome::Requeued => "message requeued".to_string(),
                    super::queue_delivery::NackOutcome::MovedToDlq(dlq) => {
                        format!("message moved to dead-letter queue '{}'", dlq)
                    }
                    super::queue_delivery::NackOutcome::Dropped => {
                        "message dropped after max attempts".to_string()
                    }
                };
                self.invalidate_result_cache();

                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &message,
                    "update",
                ))
            }
            QueueCommand::Move {
                source,
                destination,
                filter,
                limit,
            } => self.execute_queue_move(raw_query, source, destination, filter.as_ref(), *limit),
        }
    }

    pub fn execute_queue_select(
        &self,
        raw_query: &str,
        query: &QueueSelectQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();
        ensure_queue_exists(store.as_ref(), &query.queue)?;
        let config = load_queue_config(store.as_ref(), &query.queue);
        let dlq = queue_is_dead_letter_target(store.as_ref(), &query.queue);
        let columns = if query.columns.is_empty() {
            queue_projection_default_columns()
        } else {
            query.columns.clone()
        };

        let mut messages =
            load_queue_message_views_with_runtime(Some(self), store.as_ref(), &query.queue)?;
        sort_queue_messages(&mut messages, &config, QueueSide::Left);

        let mut result = UnifiedResult::with_columns(columns.clone());
        for message in messages {
            if query
                .filter
                .as_ref()
                .is_some_and(|filter| !queue_message_matches_filter(&message, dlq, filter))
            {
                continue;
            }
            let record = queue_projection_record(&columns, &message, dlq)?;
            result.push(record);
            if query
                .limit
                .is_some_and(|limit| result.records.len() >= limit as usize)
            {
                break;
            }
        }

        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: QueryMode::Sql,
            statement: "queue_select",
            engine: "runtime-queue",
            result,
            affected_rows: 0,
            statement_type: "select",
        })
    }

    fn execute_queue_move(
        &self,
        raw_query: &str,
        source: &str,
        destination: &str,
        filter: Option<&Filter>,
        limit: usize,
    ) -> RedDBResult<RuntimeQueryResult> {
        if source == destination {
            return Err(RedDBError::Query(
                "QUEUE MOVE source and destination must be different".to_string(),
            ));
        }
        let store = self.inner.db.store();
        ensure_queue_exists(store.as_ref(), source)?;
        ensure_queue_exists(store.as_ref(), destination)?;
        let source_config = load_queue_config(store.as_ref(), source);
        let destination_config = load_queue_config(store.as_ref(), destination);
        let source_dlq = queue_is_dead_letter_target(store.as_ref(), source);

        let mut messages =
            load_queue_message_views_with_runtime(Some(self), store.as_ref(), source)?;
        sort_queue_messages(&mut messages, &source_config, QueueSide::Left);
        let selected = messages
            .into_iter()
            .filter(|message| {
                filter
                    .map(|f| queue_message_matches_filter(message, source_dlq, f))
                    .unwrap_or(true)
            })
            .take(limit)
            .collect::<Vec<_>>();

        if let Some(max_size) = destination_config.max_size {
            let current_len =
                load_queue_message_views_with_runtime(Some(self), store.as_ref(), destination)?
                    .len();
            if current_len + selected.len() > max_size {
                return Err(RedDBError::Query(format!(
                    "queue '{}' is full (max_size={max_size})",
                    destination
                )));
            }
        }

        for message in &selected {
            let lock = queue_message_lock_handle(self, source, message.id);
            let Some(_guard) = lock.try_lock() else {
                return Err(RedDBError::Query(format!(
                    "message '{}' is locked on queue '{}'",
                    message.id.raw(),
                    source
                )));
            };
            if queue_message_view_by_id(store.as_ref(), source, message.id)?.is_none() {
                return Err(RedDBError::Query(format!(
                    "message '{}' is no longer available on queue '{}'",
                    message.id.raw(),
                    source
                )));
            }
        }

        let mut inserted = Vec::new();
        for message in &selected {
            match insert_moved_queue_message(
                store.as_ref(),
                destination,
                &destination_config,
                message,
            ) {
                Ok(id) => inserted.push(id),
                Err(err) => {
                    for id in inserted {
                        let _ = store.delete(destination, id);
                    }
                    return Err(err);
                }
            }
        }

        for message in &selected {
            super::queue_delivery::delete_message_with_state(
                Some(self),
                store.as_ref(),
                source,
                message.id,
            )?;
        }
        if !selected.is_empty() {
            self.invalidate_result_cache();
        }

        let selected_count = selected.len() as u64;
        self.audit_log().record_event(
            AuditEvent::builder("queue/move")
                .source(AuditAuthSource::System)
                .outcome(Outcome::Success)
                .resource(format!("queue:{source}->{destination}"))
                .fields([
                    AuditFieldEscaper::field("source", source),
                    AuditFieldEscaper::field("destination", destination),
                    AuditFieldEscaper::field("selected", selected_count),
                    AuditFieldEscaper::field("committed", selected_count),
                ])
                .build(),
        );

        let mut result = UnifiedResult::with_columns(vec![
            "source".into(),
            "destination".into(),
            "selected".into(),
            "committed".into(),
        ]);
        let mut record = UnifiedRecord::new();
        record.set("source", Value::text(source.to_string()));
        record.set("destination", Value::text(destination.to_string()));
        record.set("selected", Value::UnsignedInteger(selected_count));
        record.set("committed", Value::UnsignedInteger(selected_count));
        result.push(record);

        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: QueryMode::Sql,
            statement: "queue_move",
            engine: "runtime-queue",
            result,
            affected_rows: selected_count,
            statement_type: "update",
        })
    }
}

fn ensure_queue_exists(store: &UnifiedStore, queue: &str) -> RedDBResult<()> {
    if store.get_collection(queue).is_some() {
        Ok(())
    } else {
        Err(RedDBError::NotFound(format!("queue '{}' not found", queue)))
    }
}

pub(super) fn load_queue_config(store: &UnifiedStore, queue: &str) -> QueueRuntimeConfig {
    let default = QueueRuntimeConfig {
        mode: QueueMode::Work,
        priority: false,
        max_size: None,
        ttl_ms: None,
        dlq: None,
        max_attempts: crate::storage::query::DEFAULT_QUEUE_MAX_ATTEMPTS,
        lock_deadline_ms: crate::storage::query::DEFAULT_QUEUE_LOCK_DEADLINE_MS,
        in_flight_cap_per_group: crate::storage::query::DEFAULT_QUEUE_IN_FLIGHT_CAP_PER_GROUP,
    };

    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return default;
    };
    manager
        .query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                row_text(row, "kind").as_deref() == Some("queue_config")
                    && row_text(row, "queue").as_deref() == Some(queue)
            })
        })
        .into_iter()
        .find_map(|entity| {
            let row = entity.data.as_row()?;
            Some(QueueRuntimeConfig {
                mode: row_text(row, "mode")
                    .as_deref()
                    .and_then(QueueMode::parse)
                    .unwrap_or_default(),
                priority: row_bool(row, "priority").unwrap_or(false),
                max_size: row_u64(row, "max_size").map(|value| value as usize),
                ttl_ms: row_u64(row, "ttl_ms"),
                dlq: row_text(row, "dlq"),
                max_attempts: row_u64(row, "max_attempts")
                    .map(|value| value as u32)
                    .unwrap_or(crate::storage::query::DEFAULT_QUEUE_MAX_ATTEMPTS),
                lock_deadline_ms: row_u64(row, "lock_deadline_ms")
                    .unwrap_or(crate::storage::query::DEFAULT_QUEUE_LOCK_DEADLINE_MS),
                in_flight_cap_per_group: row_u64(row, "in_flight_cap_per_group")
                    .map(|value| value as u32)
                    .unwrap_or(crate::storage::query::DEFAULT_QUEUE_IN_FLIGHT_CAP_PER_GROUP),
            })
        })
        .unwrap_or(default)
}

pub(super) fn queue_mode_str(store: &UnifiedStore, queue: &str) -> &'static str {
    load_queue_config(store, queue).mode.as_str()
}

fn save_queue_config(
    store: &UnifiedStore,
    queue: &str,
    config: &QueueRuntimeConfig,
) -> RedDBResult<()> {
    remove_meta_rows(store, |row| {
        row_text(row, "kind").as_deref() == Some("queue_config")
            && row_text(row, "queue").as_deref() == Some(queue)
    });

    let mut fields = HashMap::new();
    fields.insert("kind".to_string(), Value::text("queue_config".to_string()));
    fields.insert("queue".to_string(), Value::text(queue.to_string()));
    fields.insert(
        "mode".to_string(),
        Value::text(config.mode.as_str().to_string()),
    );
    fields.insert("priority".to_string(), Value::Boolean(config.priority));
    fields.insert(
        "max_size".to_string(),
        config
            .max_size
            .map(|value| Value::UnsignedInteger(value as u64))
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "ttl_ms".to_string(),
        config
            .ttl_ms
            .map(Value::UnsignedInteger)
            .unwrap_or(Value::Null),
    );
    fields.insert(
        "dlq".to_string(),
        config.dlq.clone().map(Value::text).unwrap_or(Value::Null),
    );
    fields.insert(
        "max_attempts".to_string(),
        Value::UnsignedInteger(u64::from(config.max_attempts)),
    );
    fields.insert(
        "lock_deadline_ms".to_string(),
        Value::UnsignedInteger(config.lock_deadline_ms),
    );
    fields.insert(
        "in_flight_cap_per_group".to_string(),
        Value::UnsignedInteger(u64::from(config.in_flight_cap_per_group)),
    );
    insert_meta_row(store, fields)
}

fn remove_queue_metadata(store: &UnifiedStore, queue: &str) {
    remove_meta_rows(store, |row| {
        row_text(row, "queue").as_deref() == Some(queue)
    });
}

fn queue_group_exists(store: &UnifiedStore, queue: &str, group: &str) -> RedDBResult<bool> {
    Ok(load_queue_groups(store, queue)?
        .into_iter()
        .any(|entry| entry.group == group))
}

pub(super) fn require_queue_group(
    store: &UnifiedStore,
    queue: &str,
    group: &str,
) -> RedDBResult<()> {
    if queue_group_exists(store, queue, group)? {
        Ok(())
    } else {
        Err(RedDBError::NotFound(format!(
            "consumer group '{}' not found on queue '{}'",
            group, queue
        )))
    }
}

pub(super) fn resolve_read_group(
    store: &UnifiedStore,
    queue: &str,
    group: Option<&str>,
    consumer: &str,
    config: &QueueRuntimeConfig,
) -> RedDBResult<String> {
    if let Some(group) = group {
        require_queue_group(store, queue, group)?;
        return Ok(group.to_string());
    }

    match config.mode {
        QueueMode::Work => {
            if !queue_group_exists(store, queue, WORK_DEFAULT_GROUP)? {
                save_queue_group(store, queue, WORK_DEFAULT_GROUP)?;
            }
            Ok(WORK_DEFAULT_GROUP.to_string())
        }
        QueueMode::Fanout => {
            let fanout_group = format!("{FANOUT_GROUP_PREFIX}{consumer}");
            if !queue_group_exists(store, queue, &fanout_group)? {
                save_queue_group(store, queue, &fanout_group)?;
            }
            Ok(fanout_group)
        }
    }
}

fn load_queue_groups(store: &UnifiedStore, queue: &str) -> RedDBResult<Vec<QueueGroupEntry>> {
    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return Ok(Vec::new());
    };
    Ok(manager
        .query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                row_text(row, "kind").as_deref() == Some("queue_group")
                    && row_text(row, "queue").as_deref() == Some(queue)
            })
        })
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            Some(QueueGroupEntry {
                entity_id: entity.id,
                group: row_text(row, "group")?,
            })
        })
        .collect())
}

fn save_queue_group(store: &UnifiedStore, queue: &str, group: &str) -> RedDBResult<()> {
    let mut fields = HashMap::new();
    fields.insert("kind".to_string(), Value::text("queue_group".to_string()));
    fields.insert("queue".to_string(), Value::text(queue.to_string()));
    fields.insert("group".to_string(), Value::text(group.to_string()));
    fields.insert(
        "created_at_ns".to_string(),
        Value::UnsignedInteger(now_ns()),
    );
    insert_meta_row(store, fields)
}

pub(super) fn load_pending_entries(
    store: &UnifiedStore,
    queue: &str,
    group: Option<&str>,
    message_id: Option<EntityId>,
) -> RedDBResult<Vec<QueuePendingEntry>> {
    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return Ok(Vec::new());
    };
    Ok(manager
        .query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                row_text(row, "kind").as_deref() == Some("queue_pending")
                    && row_text(row, "queue").as_deref() == Some(queue)
                    && group
                        .map(|group_name| row_text(row, "group").as_deref() == Some(group_name))
                        .unwrap_or(true)
                    && message_id
                        .map(|candidate| row_u64(row, "message_id") == Some(candidate.raw()))
                        .unwrap_or(true)
            })
        })
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            Some(QueuePendingEntry {
                entity_id: entity.id,
                group: row_text(row, "group")?,
                message_id: EntityId::new(row_u64(row, "message_id")?),
                consumer: row_text(row, "consumer")?,
                delivered_at_ns: row_u64(row, "delivered_at_ns")?,
                delivery_count: row_u64(row, "delivery_count")
                    .map(|value| value as u32)
                    .unwrap_or(1),
            })
        })
        .collect())
}

pub(super) fn save_queue_pending(
    store: &UnifiedStore,
    queue: &str,
    group: &str,
    message_id: EntityId,
    consumer: &str,
    delivered_at_ns: u64,
    delivery_count: u32,
) -> RedDBResult<()> {
    remove_meta_rows(store, |row| {
        row_text(row, "kind").as_deref() == Some("queue_pending")
            && row_text(row, "queue").as_deref() == Some(queue)
            && row_text(row, "group").as_deref() == Some(group)
            && row_u64(row, "message_id") == Some(message_id.raw())
    });

    let mut fields = HashMap::new();
    fields.insert("kind".to_string(), Value::text("queue_pending".to_string()));
    fields.insert("queue".to_string(), Value::text(queue.to_string()));
    fields.insert("group".to_string(), Value::text(group.to_string()));
    fields.insert(
        "message_id".to_string(),
        Value::UnsignedInteger(message_id.raw()),
    );
    fields.insert("consumer".to_string(), Value::text(consumer.to_string()));
    fields.insert(
        "delivered_at_ns".to_string(),
        Value::UnsignedInteger(delivered_at_ns),
    );
    fields.insert(
        "delivery_count".to_string(),
        Value::UnsignedInteger(u64::from(delivery_count)),
    );
    insert_meta_row(store, fields)
}

pub(super) fn require_pending_entry(
    store: &UnifiedStore,
    queue: &str,
    group: &str,
    message_id: EntityId,
) -> RedDBResult<QueuePendingEntry> {
    load_pending_entries(store, queue, Some(group), Some(message_id))?
        .into_iter()
        .next()
        .ok_or_else(|| {
            RedDBError::NotFound(format!(
                "message '{}' is not pending in group '{}' on queue '{}'",
                message_id.raw(),
                group,
                queue
            ))
        })
}

pub(super) fn load_ack_entries(
    store: &UnifiedStore,
    queue: &str,
    group: Option<&str>,
    message_id: Option<EntityId>,
) -> RedDBResult<Vec<QueueAckEntry>> {
    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return Ok(Vec::new());
    };
    Ok(manager
        .query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                row_text(row, "kind").as_deref() == Some("queue_ack")
                    && row_text(row, "queue").as_deref() == Some(queue)
                    && group
                        .map(|group_name| row_text(row, "group").as_deref() == Some(group_name))
                        .unwrap_or(true)
                    && message_id
                        .map(|candidate| row_u64(row, "message_id") == Some(candidate.raw()))
                        .unwrap_or(true)
            })
        })
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            Some(QueueAckEntry {
                entity_id: entity.id,
                group: row_text(row, "group")?,
                message_id: EntityId::new(row_u64(row, "message_id")?),
            })
        })
        .collect())
}

pub(super) fn save_queue_ack(
    store: &UnifiedStore,
    queue: &str,
    group: &str,
    message_id: EntityId,
) -> RedDBResult<()> {
    let existing = load_ack_entries(store, queue, Some(group), Some(message_id))?;
    if !existing.is_empty() {
        return Ok(());
    }

    let mut fields = HashMap::new();
    fields.insert("kind".to_string(), Value::text("queue_ack".to_string()));
    fields.insert("queue".to_string(), Value::text(queue.to_string()));
    fields.insert("group".to_string(), Value::text(group.to_string()));
    fields.insert(
        "message_id".to_string(),
        Value::UnsignedInteger(message_id.raw()),
    );
    fields.insert("acked_at_ns".to_string(), Value::UnsignedInteger(now_ns()));
    insert_meta_row(store, fields)
}

pub(super) fn queue_message_completed_for_all_groups(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<bool> {
    let groups = load_queue_groups(store, queue)?;
    let pending = load_pending_entries(store, queue, None, Some(message_id))?;
    if !pending.is_empty() {
        return Ok(false);
    }
    if groups.is_empty() {
        return Ok(true);
    }

    let acked_groups = load_ack_entries(store, queue, None, Some(message_id))?
        .into_iter()
        .map(|entry| entry.group)
        .collect::<HashSet<_>>();
    Ok(groups
        .into_iter()
        .all(|group| acked_groups.contains(&group.group)))
}

fn load_queue_message_views(
    store: &UnifiedStore,
    queue: &str,
) -> RedDBResult<Vec<QueueMessageView>> {
    load_queue_message_views_with_runtime(None, store, queue)
}

/// Kind-aware queue scan (Phase 2.5.5 RLS universal). When the
/// caller has a `RedDBRuntime` reference, the gate also applies
/// any `CREATE POLICY ... ON MESSAGES OF <queue>` predicate. In
/// autocommit / embedded paths that only have the raw store (e.g.
/// purge loops) we skip RLS because there's no session identity
/// to match against.
pub(super) fn load_queue_message_views_with_runtime(
    runtime: Option<&RedDBRuntime>,
    store: &UnifiedStore,
    queue: &str,
) -> RedDBResult<Vec<QueueMessageView>> {
    let manager = store
        .get_collection(queue)
        .ok_or_else(|| RedDBError::NotFound(format!("queue '{}' not found", queue)))?;
    // Phase 1.2 MVCC universal: capture before parallel scan. Messages
    // inserted by another connection's open txn stay invisible to
    // consumers until that txn commits (prevents phantom POPs).
    let snap_ctx = crate::runtime::impl_core::capture_current_snapshot();
    let rls_filter = runtime.and_then(|rt| {
        crate::runtime::impl_core::rls_policy_filter_for_kind(
            rt,
            queue,
            crate::storage::query::ast::PolicyAction::Select,
            crate::storage::query::ast::PolicyTargetKind::Messages,
        )
    });
    let rls_enabled_but_denied = runtime.map(|rt| rt.is_rls_enabled(queue)).unwrap_or(false)
        && rls_filter.is_none()
        && runtime.is_some();
    if rls_enabled_but_denied {
        // RLS on + no Messages policy for this role = deny-default.
        return Ok(Vec::new());
    }
    let filter_arc = rls_filter.map(std::sync::Arc::new);
    let rt_arc = runtime;
    Ok(manager
        .query_all(move |entity| {
            if !matches!(entity.kind, EntityKind::QueueMessage { .. }) {
                return false;
            }
            if !crate::runtime::impl_core::entity_visible_with_context(snap_ctx.as_ref(), entity) {
                return false;
            }
            if let (Some(filter), Some(rt)) = (filter_arc.as_ref(), rt_arc) {
                return crate::runtime::query_exec::evaluate_entity_filter_with_db(
                    Some(&rt.inner.db),
                    entity,
                    filter,
                    queue,
                    queue,
                );
            }
            true
        })
        .into_iter()
        .filter_map(queue_message_view_from_entity)
        .collect())
}

fn queue_message_view_from_entity(entity: UnifiedEntity) -> Option<QueueMessageView> {
    let (position, _) = match &entity.kind {
        EntityKind::QueueMessage { position, queue } => (*position, queue),
        _ => return None,
    };
    let data = match entity.data {
        EntityData::QueueMessage(data) => data,
        _ => return None,
    };
    Some(QueueMessageView {
        id: entity.id,
        position,
        priority: data.priority.unwrap_or(0),
        payload: data.payload,
        attempts: data.attempts,
        max_attempts: data.max_attempts,
        enqueued_at_ns: data.enqueued_at_ns,
    })
}

fn insert_moved_queue_message(
    store: &UnifiedStore,
    queue: &str,
    config: &QueueRuntimeConfig,
    message: &QueueMessageView,
) -> RedDBResult<EntityId> {
    let position = next_queue_position(store, queue, QueueSide::Right)?;
    let entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::QueueMessage {
            queue: queue.to_string(),
            position,
        },
        EntityData::QueueMessage(QueueMessageData {
            payload: message.payload.clone(),
            priority: if config.priority {
                Some(message.priority)
            } else {
                None
            },
            enqueued_at_ns: message.enqueued_at_ns,
            attempts: message.attempts,
            max_attempts: message.max_attempts,
            acked: false,
        }),
    );
    let id = store
        .insert_auto(queue, entity)
        .map_err(|err| RedDBError::Internal(err.to_string()))?;
    if let Some(ttl_ms) = config.ttl_ms {
        store
            .set_metadata(queue, id, queue_message_ttl_metadata(ttl_ms))
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
    }
    Ok(id)
}

fn queue_projection_default_columns() -> Vec<String> {
    [
        "id",
        "payload",
        "priority",
        "attempts",
        "last_error",
        "enqueued_at",
        "available_at",
        "dlq",
        "tenant",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn queue_projection_record(
    columns: &[String],
    message: &QueueMessageView,
    dlq: bool,
) -> RedDBResult<UnifiedRecord> {
    let mut record = UnifiedRecord::new();
    for column in columns {
        let value = queue_projection_value(message, dlq, column).ok_or_else(|| {
            RedDBError::Query(format!("unknown queue projection column '{}'", column))
        })?;
        record.set(column, value);
    }
    Ok(record)
}

fn queue_projection_value(message: &QueueMessageView, dlq: bool, column: &str) -> Option<Value> {
    match column {
        "id" => Some(Value::text(message_id_string(message.id))),
        "payload" => Some(message.payload.clone()),
        "priority" => Some(Value::Integer(i64::from(message.priority))),
        "attempts" => Some(Value::UnsignedInteger(u64::from(message.attempts))),
        "last_error" => Some(Value::Null),
        "enqueued_at" => Some(Value::UnsignedInteger(message.enqueued_at_ns)),
        "available_at" => Some(Value::UnsignedInteger(message.enqueued_at_ns)),
        "dlq" => Some(Value::Boolean(dlq)),
        "tenant" => queue_message_tenant(&message.payload).or(Some(Value::Null)),
        _ => None,
    }
}

fn queue_message_tenant(payload: &Value) -> Option<Value> {
    let Value::Json(bytes) = payload else {
        return None;
    };
    let json: crate::json::Value = crate::json::from_slice(bytes).ok()?;
    json.get("tenant")
        .and_then(crate::json::Value::as_str)
        .map(|tenant| Value::text(tenant.to_string()))
}

fn queue_is_dead_letter_target(store: &UnifiedStore, queue: &str) -> bool {
    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return false;
    };
    !manager
        .query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                row_text(row, "kind").as_deref() == Some("queue_config")
                    && row_text(row, "dlq").as_deref() == Some(queue)
            })
        })
        .is_empty()
}

fn queue_message_matches_filter(message: &QueueMessageView, dlq: bool, filter: &Filter) -> bool {
    match filter {
        Filter::Compare { field, op, value } => queue_filter_field_value(message, dlq, field)
            .is_some_and(|candidate| queue_compare_values(&candidate, value, *op)),
        Filter::CompareFields { left, op, right } => {
            match (
                queue_filter_field_value(message, dlq, left),
                queue_filter_field_value(message, dlq, right),
            ) {
                (Some(left), Some(right)) => queue_compare_values(&left, &right, *op),
                _ => false,
            }
        }
        Filter::And(left, right) => {
            queue_message_matches_filter(message, dlq, left)
                && queue_message_matches_filter(message, dlq, right)
        }
        Filter::Or(left, right) => {
            queue_message_matches_filter(message, dlq, left)
                || queue_message_matches_filter(message, dlq, right)
        }
        Filter::Not(inner) => !queue_message_matches_filter(message, dlq, inner),
        Filter::IsNull(field) => queue_filter_field_value(message, dlq, field)
            .is_none_or(|value| matches!(value, Value::Null)),
        Filter::IsNotNull(field) => queue_filter_field_value(message, dlq, field)
            .is_some_and(|value| !matches!(value, Value::Null)),
        Filter::In { field, values } => {
            queue_filter_field_value(message, dlq, field).is_some_and(|candidate| {
                values
                    .iter()
                    .any(|value| queue_values_equal(&candidate, value))
            })
        }
        Filter::Between { field, low, high } => queue_filter_field_value(message, dlq, field)
            .is_some_and(|candidate| {
                queue_compare_values(&candidate, low, CompareOp::Ge)
                    && queue_compare_values(&candidate, high, CompareOp::Le)
            }),
        Filter::Like { field, pattern } => queue_filter_text(message, dlq, field)
            .is_some_and(|value| queue_like_matches(&value, pattern)),
        Filter::StartsWith { field, prefix } => {
            queue_filter_text(message, dlq, field).is_some_and(|value| value.starts_with(prefix))
        }
        Filter::EndsWith { field, suffix } => {
            queue_filter_text(message, dlq, field).is_some_and(|value| value.ends_with(suffix))
        }
        Filter::Contains { field, substring } => {
            queue_filter_text(message, dlq, field).is_some_and(|value| value.contains(substring))
        }
        Filter::CompareExpr { .. } => false,
    }
}

fn queue_filter_field_value(
    message: &QueueMessageView,
    dlq: bool,
    field: &FieldRef,
) -> Option<Value> {
    match field {
        FieldRef::TableColumn { table, column } if table.is_empty() => {
            queue_projection_value(message, dlq, column)
                .or_else(|| queue_payload_field_value(&message.payload, column))
        }
        FieldRef::TableColumn { column, .. } => queue_projection_value(message, dlq, column)
            .or_else(|| queue_payload_field_value(&message.payload, column)),
        _ => None,
    }
}

fn queue_payload_field_value(payload: &Value, field: &str) -> Option<Value> {
    let Value::Json(bytes) = payload else {
        return None;
    };
    let json: crate::json::Value = crate::json::from_slice(bytes).ok()?;
    let value = json.get(field)?;
    json_value_to_schema_value(value)
}

fn json_value_to_schema_value(value: &crate::json::Value) -> Option<Value> {
    if matches!(value, crate::json::Value::Null) {
        Some(Value::Null)
    } else if let Some(value) = value.as_bool() {
        Some(Value::Boolean(value))
    } else if let Some(value) = value.as_i64() {
        Some(Value::Integer(value))
    } else if let Some(value) = value.as_u64() {
        Some(Value::UnsignedInteger(value))
    } else if let Some(value) = value.as_f64() {
        Some(Value::Float(value))
    } else if let Some(value) = value.as_str() {
        Some(Value::text(value.to_string()))
    } else {
        Some(Value::Json(value.to_string_compact().into_bytes()))
    }
}

fn queue_filter_text(message: &QueueMessageView, dlq: bool, field: &FieldRef) -> Option<String> {
    queue_filter_field_value(message, dlq, field).and_then(|value| match value {
        Value::Text(value) => Some(value.to_string()),
        Value::NodeRef(value) | Value::EdgeRef(value) | Value::TableRef(value) => Some(value),
        Value::Integer(value) => Some(value.to_string()),
        Value::UnsignedInteger(value) => Some(value.to_string()),
        Value::Float(value) => Some(value.to_string()),
        Value::Boolean(value) => Some(value.to_string()),
        _ => None,
    })
}

fn queue_compare_values(left: &Value, right: &Value, op: CompareOp) -> bool {
    match op {
        CompareOp::Eq => queue_values_equal(left, right),
        CompareOp::Ne => !queue_values_equal(left, right),
        CompareOp::Lt => queue_partial_cmp(left, right).is_some_and(|ord| ord.is_lt()),
        CompareOp::Le => queue_partial_cmp(left, right).is_some_and(|ord| !ord.is_gt()),
        CompareOp::Gt => queue_partial_cmp(left, right).is_some_and(|ord| ord.is_gt()),
        CompareOp::Ge => queue_partial_cmp(left, right).is_some_and(|ord| !ord.is_lt()),
    }
}

fn queue_values_equal(left: &Value, right: &Value) -> bool {
    if let (Some(left), Some(right)) = (queue_value_number(left), queue_value_number(right)) {
        return (left - right).abs() < f64::EPSILON;
    }
    match (left, right) {
        (Value::Text(left), Value::Text(right)) => left == right,
        (Value::Boolean(left), Value::Boolean(right)) => left == right,
        _ => left == right,
    }
}

fn queue_partial_cmp(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    if let (Some(left), Some(right)) = (queue_value_number(left), queue_value_number(right)) {
        return left.partial_cmp(&right);
    }
    match (left, right) {
        (Value::Text(left), Value::Text(right)) => Some(left.cmp(right)),
        _ => None,
    }
}

fn queue_value_number(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        Value::Float(value) => Some(*value),
        Value::Text(value) => value.parse().ok(),
        _ => None,
    }
}

fn queue_like_matches(value: &str, pattern: &str) -> bool {
    if pattern == "%" {
        return true;
    }
    let starts_wild = pattern.starts_with('%');
    let ends_wild = pattern.ends_with('%');
    let needle = pattern.trim_matches('%');
    match (starts_wild, ends_wild) {
        (true, true) => value.contains(needle),
        (true, false) => value.ends_with(needle),
        (false, true) => value.starts_with(needle),
        (false, false) => value == needle,
    }
}

pub(super) fn queue_message_view_by_id(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<Option<QueueMessageView>> {
    let manager = queue_manager(store, queue)?;
    Ok(manager
        .get(message_id)
        .and_then(queue_message_view_from_entity))
}

pub(super) fn sort_queue_messages(
    messages: &mut [QueueMessageView],
    config: &QueueRuntimeConfig,
    side: QueueSide,
) {
    messages.sort_by(|left, right| {
        if config.priority {
            right
                .priority
                .cmp(&left.priority)
                .then_with(|| match side {
                    QueueSide::Left => left.position.cmp(&right.position),
                    QueueSide::Right => right.position.cmp(&left.position),
                })
                .then_with(|| left.id.raw().cmp(&right.id.raw()))
        } else {
            match side {
                QueueSide::Left => left.position.cmp(&right.position),
                QueueSide::Right => right.position.cmp(&left.position),
            }
            .then_with(|| left.id.raw().cmp(&right.id.raw()))
        }
    });
}

pub(super) fn next_queue_position(
    store: &UnifiedStore,
    queue: &str,
    side: QueueSide,
) -> RedDBResult<u64> {
    let messages = load_queue_message_views(store, queue)?;
    if messages.is_empty() {
        return Ok(QUEUE_POSITION_CENTER);
    }
    match side {
        QueueSide::Left => Ok(messages
            .iter()
            .map(|message| message.position)
            .min()
            .unwrap_or(QUEUE_POSITION_CENTER)
            .saturating_sub(1)),
        QueueSide::Right => Ok(messages
            .iter()
            .map(|message| message.position)
            .max()
            .unwrap_or(QUEUE_POSITION_CENTER)
            .saturating_add(1)),
    }
}

pub(super) fn increment_queue_attempts(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<u32> {
    let manager = queue_manager(store, queue)?;
    let mut entity = manager
        .get(message_id)
        .ok_or_else(|| RedDBError::NotFound(format!("message '{}' not found", message_id.raw())))?;
    match &mut entity.data {
        EntityData::QueueMessage(message) => {
            message.attempts = message.attempts.saturating_add(1);
            let attempts = message.attempts;
            manager
                .update(entity)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            Ok(attempts)
        }
        _ => Err(RedDBError::Query(format!(
            "entity '{}' is not a queue message",
            message_id.raw()
        ))),
    }
}

pub(super) fn queue_message_attempts(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<u32> {
    Ok(queue_message_data(store, queue, message_id)?.attempts)
}

pub(super) fn queue_message_max_attempts(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<u32> {
    Ok(queue_message_data(store, queue, message_id)?.max_attempts)
}

pub(super) fn queue_message_payload(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<Value> {
    Ok(queue_message_data(store, queue, message_id)?.payload)
}

pub(super) fn queue_message_pending_any(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<bool> {
    Ok(!load_pending_entries(store, queue, None, Some(message_id))?.is_empty())
}

pub(super) fn queue_message_pending_for_group(
    store: &UnifiedStore,
    queue: &str,
    group: &str,
    message_id: EntityId,
) -> RedDBResult<bool> {
    Ok(!load_pending_entries(store, queue, Some(group), Some(message_id))?.is_empty())
}

pub(super) fn queue_message_acked_for_group(
    store: &UnifiedStore,
    queue: &str,
    group: &str,
    message_id: EntityId,
) -> RedDBResult<bool> {
    Ok(!load_ack_entries(store, queue, Some(group), Some(message_id))?.is_empty())
}

fn queue_manager(
    store: &UnifiedStore,
    queue: &str,
) -> RedDBResult<Arc<crate::storage::unified::SegmentManager>> {
    store
        .get_collection(queue)
        .ok_or_else(|| RedDBError::NotFound(format!("queue '{}' not found", queue)))
}

pub(super) fn queue_message_data(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<QueueMessageData> {
    let manager = queue_manager(store, queue)?;
    let entity = manager
        .get(message_id)
        .ok_or_else(|| RedDBError::NotFound(format!("message '{}' not found", message_id.raw())))?;
    match entity.data {
        EntityData::QueueMessage(message) => Ok(message),
        _ => Err(RedDBError::Query(format!(
            "entity '{}' is not a queue message",
            message_id.raw()
        ))),
    }
}

fn insert_meta_row(store: &UnifiedStore, fields: HashMap<String, Value>) -> RedDBResult<()> {
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
        .map_err(|err| RedDBError::Internal(err.to_string()))?;
    Ok(())
}

pub(super) fn remove_meta_rows(store: &UnifiedStore, predicate: impl Fn(&RowData) -> bool + Sync) {
    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return;
    };
    let rows = manager.query_all(|entity| entity.data.as_row().is_some_and(&predicate));
    for row in rows {
        let _ = store.delete(QUEUE_META_COLLECTION, row.id);
    }
}

pub(super) fn delete_meta_entity(store: &UnifiedStore, entity_id: EntityId) {
    let _ = store.delete(QUEUE_META_COLLECTION, entity_id);
}

fn queue_message_lock_key(queue: &str, message_id: EntityId) -> String {
    format!("{queue}:{}", message_id.raw())
}

pub(super) fn queue_message_lock_handle(
    runtime: &RedDBRuntime,
    queue: &str,
    message_id: EntityId,
) -> Arc<parking_lot::Mutex<()>> {
    let key = queue_message_lock_key(queue, message_id);
    if let Some(lock) = runtime.inner.queue_message_locks.read().get(&key).cloned() {
        return lock;
    }

    let mut locks = runtime.inner.queue_message_locks.write();
    locks
        .entry(key)
        .or_insert_with(|| Arc::new(parking_lot::Mutex::new(())))
        .clone()
}

pub(super) fn forget_queue_message_lock(runtime: &RedDBRuntime, queue: &str, message_id: EntityId) {
    runtime
        .inner
        .queue_message_locks
        .write()
        .remove(&queue_message_lock_key(queue, message_id));
}

fn parse_message_id(value: &str) -> RedDBResult<EntityId> {
    let raw = value.strip_prefix('e').unwrap_or(value);
    raw.parse::<u64>()
        .map(EntityId::new)
        .map_err(|_| RedDBError::Query(format!("invalid message id '{}'", value)))
}

fn message_id_string(message_id: EntityId) -> String {
    message_id.raw().to_string()
}

pub(super) fn row_text(row: &RowData, field: &str) -> Option<String> {
    match row.get_field(field)?.clone() {
        Value::Text(value) => Some(value.to_string()),
        Value::NodeRef(value) => Some(value),
        Value::EdgeRef(value) => Some(value),
        Value::TableRef(value) => Some(value),
        _ => None,
    }
}

pub(super) fn row_u64(row: &RowData, field: &str) -> Option<u64> {
    match row.get_field(field)?.clone() {
        Value::UnsignedInteger(value) => Some(value),
        Value::Integer(value) if value >= 0 => Some(value as u64),
        Value::Float(value) if value >= 0.0 => Some(value as u64),
        Value::Text(value) => value.parse().ok(),
        _ => None,
    }
}

fn row_bool(row: &RowData, field: &str) -> Option<bool> {
    match row.get_field(field)?.clone() {
        Value::Boolean(value) => Some(value),
        Value::Text(value) => match value.to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn queue_collection_contract(
    name: &str,
    priority: bool,
    ttl_ms: Option<u64>,
) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    let mut context_index_fields = Vec::new();
    if priority {
        context_index_fields.push("priority".to_string());
    }

    crate::physical::CollectionContract {
        name: name.to_string(),
        declared_model: crate::catalog::CollectionModel::Queue,
        schema_mode: crate::catalog::SchemaMode::Dynamic,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: ttl_ms,
        vector_dimension: None,
        vector_metric: None,
        context_index_fields,
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        // Queues manipulate messages via push/pop/ack — the row DML
        // paths never apply. Flag it as append_only so inadvertent
        // `UPDATE/DELETE FROM queue_name` statements fail loudly.
        append_only: true,
        subscriptions: Vec::new(),
        session_key: None,
        session_gap_ms: None,
    }
}

fn current_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

pub(super) fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

pub(super) fn queue_message_ttl_metadata(ttl_ms: u64) -> Metadata {
    Metadata::with_fields(
        [(
            "_ttl_ms".to_string(),
            if ttl_ms <= i64::MAX as u64 {
                MetadataValue::Int(ttl_ms as i64)
            } else {
                MetadataValue::Timestamp(ttl_ms)
            },
        )]
        .into_iter()
        .collect(),
    )
}

/// Rough payload byte estimate for outbox watermark tracking.
fn estimate_payload_bytes(payload: &Value) -> u64 {
    match payload {
        Value::Json(v) => v.len() as u64,
        Value::Text(s) => s.len() as u64,
        _ => 64,
    }
}
