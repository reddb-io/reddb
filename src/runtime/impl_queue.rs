//! Queue DDL and command execution

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::storage::unified::entity::{QueueMessageData, RowData};
use crate::storage::unified::{Metadata, MetadataValue, UnifiedStore};

use super::*;

const QUEUE_META_COLLECTION: &str = "red_queue_meta";
const QUEUE_POSITION_CENTER: u64 = u64::MAX / 2;

#[derive(Debug, Clone)]
struct QueueRuntimeConfig {
    priority: bool,
    max_size: Option<usize>,
    ttl_ms: Option<u64>,
    dlq: Option<String>,
    max_attempts: u32,
}

#[derive(Debug, Clone)]
struct QueueGroupEntry {
    entity_id: EntityId,
    group: String,
}

#[derive(Debug, Clone)]
struct QueuePendingEntry {
    entity_id: EntityId,
    group: String,
    message_id: EntityId,
    consumer: String,
    delivered_at_ns: u64,
    delivery_count: u32,
}

#[derive(Debug, Clone)]
struct QueueAckEntry {
    entity_id: EntityId,
    group: String,
    message_id: EntityId,
}

#[derive(Debug, Clone)]
struct QueueMessageView {
    id: EntityId,
    position: u64,
    priority: i32,
    payload: Value,
    attempts: u32,
    max_attempts: u32,
}

impl RedDBRuntime {
    pub fn execute_create_queue(
        &self,
        raw_query: &str,
        query: &CreateQueueQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
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
                priority: query.priority,
                max_size: query.max_size,
                ttl_ms: query.ttl_ms,
                dlq: query.dlq.clone(),
                max_attempts: query.max_attempts,
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

        let mut msg = format!("queue '{}' created", query.name);
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

    pub fn execute_drop_queue(
        &self,
        raw_query: &str,
        query: &DropQueueQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();
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
                    let current_len = load_queue_message_views(store.as_ref(), queue)?.len();
                    if current_len >= max_size {
                        return Err(RedDBError::Query(format!(
                            "queue '{}' is full (max_size={max_size})",
                            queue
                        )));
                    }
                }

                let position = next_queue_position(store.as_ref(), queue, *side)?;
                let entity = UnifiedEntity::new(
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
                record.set("message_id", Value::Text(message_id_string(id)));
                record.set(
                    "side",
                    Value::Text(match side {
                        QueueSide::Left => "left".to_string(),
                        QueueSide::Right => "right".to_string(),
                    }),
                );
                record.set("queue", Value::Text(queue.clone()));
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
                let config = load_queue_config(store.as_ref(), queue);
                let pending_ids = load_pending_entries(store.as_ref(), queue, None, None)?
                    .into_iter()
                    .map(|entry| entry.message_id)
                    .collect::<HashSet<_>>();
                let mut messages = load_queue_message_views(store.as_ref(), queue)?
                    .into_iter()
                    .filter(|message| !pending_ids.contains(&message.id))
                    .collect::<Vec<_>>();
                sort_queue_messages(&mut messages, &config, *side);

                let mut result =
                    UnifiedResult::with_columns(vec!["message_id".into(), "payload".into()]);
                let mut popped = 0u64;
                for message in messages.into_iter().take(*count) {
                    let mut record = UnifiedRecord::new();
                    record.set("message_id", Value::Text(message_id_string(message.id)));
                    record.set("payload", message.payload.clone());
                    result.push(record);
                    delete_message_with_state(store.as_ref(), queue, message.id)?;
                    popped += 1;
                }
                if popped > 0 {
                    self.invalidate_result_cache();
                }

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "queue_pop",
                    engine: "runtime-queue",
                    result,
                    affected_rows: popped,
                    statement_type: "delete",
                })
            }
            QueueCommand::Peek { queue, count } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let config = load_queue_config(store.as_ref(), queue);
                let pending_ids = load_pending_entries(store.as_ref(), queue, None, None)?
                    .into_iter()
                    .map(|entry| entry.message_id)
                    .collect::<HashSet<_>>();
                let mut messages = load_queue_message_views(store.as_ref(), queue)?
                    .into_iter()
                    .filter(|message| !pending_ids.contains(&message.id))
                    .collect::<Vec<_>>();
                sort_queue_messages(&mut messages, &config, QueueSide::Left);

                let mut result =
                    UnifiedResult::with_columns(vec!["message_id".into(), "payload".into()]);
                for message in messages.into_iter().take(*count) {
                    let mut record = UnifiedRecord::new();
                    record.set("message_id", Value::Text(message_id_string(message.id)));
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
                let count = load_queue_message_views(store.as_ref(), queue)?.len() as u64;
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
                let messages = load_queue_message_views(store.as_ref(), queue)?;
                let count = messages.len();
                for message in messages {
                    delete_message_with_state(store.as_ref(), queue, message.id)?;
                }
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
                require_queue_group(store.as_ref(), queue, group)?;
                let config = load_queue_config(store.as_ref(), queue);
                let pending = load_pending_entries(store.as_ref(), queue, Some(group), None)?;
                let pending_ids = pending
                    .iter()
                    .map(|entry| entry.message_id)
                    .collect::<HashSet<_>>();
                let acked_ids = load_ack_entries(store.as_ref(), queue, Some(group), None)?
                    .into_iter()
                    .map(|entry| entry.message_id)
                    .collect::<HashSet<_>>();
                let mut messages = load_queue_message_views(store.as_ref(), queue)?
                    .into_iter()
                    .filter(|message| {
                        !pending_ids.contains(&message.id) && !acked_ids.contains(&message.id)
                    })
                    .collect::<Vec<_>>();
                sort_queue_messages(&mut messages, &config, QueueSide::Left);

                let mut result = UnifiedResult::with_columns(vec![
                    "message_id".into(),
                    "payload".into(),
                    "consumer".into(),
                    "delivery_count".into(),
                    "attempts".into(),
                ]);

                for message in messages.into_iter().take(*count) {
                    let attempts = increment_queue_attempts(store.as_ref(), queue, message.id)?;
                    if attempts > message.max_attempts {
                        let _ = move_message_to_dlq_or_drop(
                            store.as_ref(),
                            queue,
                            message.id,
                            &config,
                            "max_attempts_exceeded",
                        )?;
                        continue;
                    }

                    let delivered_at_ns = now_ns();
                    save_queue_pending(
                        store.as_ref(),
                        queue,
                        group,
                        message.id,
                        consumer,
                        delivered_at_ns,
                        attempts,
                    )?;

                    let mut record = UnifiedRecord::new();
                    record.set("message_id", Value::Text(message_id_string(message.id)));
                    record.set("payload", message.payload);
                    record.set("consumer", Value::Text(consumer.clone()));
                    record.set(
                        "delivery_count",
                        Value::UnsignedInteger(u64::from(attempts)),
                    );
                    record.set("attempts", Value::UnsignedInteger(u64::from(attempts)));
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
                        Value::Text(message_id_string(entry.message_id)),
                    );
                    record.set("consumer", Value::Text(entry.consumer));
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
                require_queue_group(store.as_ref(), queue, group)?;
                let config = load_queue_config(store.as_ref(), queue);
                let current_time_ns = now_ns();
                let min_idle_ns = min_idle_ms.saturating_mul(1_000_000);
                let mut pending = load_pending_entries(store.as_ref(), queue, Some(group), None)?
                    .into_iter()
                    .filter(|entry| {
                        current_time_ns.saturating_sub(entry.delivered_at_ns) >= min_idle_ns
                    })
                    .collect::<Vec<_>>();
                pending.sort_by_key(|entry| entry.delivered_at_ns);

                let mut result = UnifiedResult::with_columns(vec![
                    "message_id".into(),
                    "payload".into(),
                    "consumer".into(),
                    "delivery_count".into(),
                ]);

                for entry in pending {
                    let payload = queue_message_payload(store.as_ref(), queue, entry.message_id)?;
                    let attempts =
                        increment_queue_attempts(store.as_ref(), queue, entry.message_id)?;
                    if attempts
                        > queue_message_max_attempts(store.as_ref(), queue, entry.message_id)?
                    {
                        delete_meta_entity(store.as_ref(), entry.entity_id);
                        let _ = move_message_to_dlq_or_drop(
                            store.as_ref(),
                            queue,
                            entry.message_id,
                            &config,
                            "claim_max_attempts_exceeded",
                        )?;
                        continue;
                    }

                    save_queue_pending(
                        store.as_ref(),
                        queue,
                        group,
                        entry.message_id,
                        consumer,
                        current_time_ns,
                        entry.delivery_count.saturating_add(1),
                    )?;

                    let mut record = UnifiedRecord::new();
                    record.set(
                        "message_id",
                        Value::Text(message_id_string(entry.message_id)),
                    );
                    record.set("payload", payload);
                    record.set("consumer", Value::Text(consumer.clone()));
                    record.set(
                        "delivery_count",
                        Value::UnsignedInteger(u64::from(entry.delivery_count.saturating_add(1))),
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
                let pending = require_pending_entry(store.as_ref(), queue, group, message_id)?;
                delete_meta_entity(store.as_ref(), pending.entity_id);
                save_queue_ack(store.as_ref(), queue, group, message_id)?;

                if queue_message_completed_for_all_groups(store.as_ref(), queue, message_id)? {
                    delete_message_with_state(store.as_ref(), queue, message_id)?;
                }
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
                let config = load_queue_config(store.as_ref(), queue);
                let message_id = parse_message_id(message_id)?;
                let pending = require_pending_entry(store.as_ref(), queue, group, message_id)?;
                delete_meta_entity(store.as_ref(), pending.entity_id);

                let attempts = queue_message_attempts(store.as_ref(), queue, message_id)?;
                let max_attempts = queue_message_max_attempts(store.as_ref(), queue, message_id)?;
                let message = if attempts >= max_attempts {
                    let target = move_message_to_dlq_or_drop(
                        store.as_ref(),
                        queue,
                        message_id,
                        &config,
                        "nack_max_attempts_exceeded",
                    )?;
                    match target {
                        Some(dlq) => format!("message moved to dead-letter queue '{}'", dlq),
                        None => "message dropped after max attempts".to_string(),
                    }
                } else {
                    "message requeued".to_string()
                };
                self.invalidate_result_cache();

                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &message,
                    "update",
                ))
            }
        }
    }
}

fn ensure_queue_exists(store: &UnifiedStore, queue: &str) -> RedDBResult<()> {
    if store.get_collection(queue).is_some() {
        Ok(())
    } else {
        Err(RedDBError::NotFound(format!("queue '{}' not found", queue)))
    }
}

fn load_queue_config(store: &UnifiedStore, queue: &str) -> QueueRuntimeConfig {
    let default = QueueRuntimeConfig {
        priority: false,
        max_size: None,
        ttl_ms: None,
        dlq: None,
        max_attempts: 3,
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
                priority: row_bool(row, "priority").unwrap_or(false),
                max_size: row_u64(row, "max_size").map(|value| value as usize),
                ttl_ms: row_u64(row, "ttl_ms"),
                dlq: row_text(row, "dlq"),
                max_attempts: row_u64(row, "max_attempts")
                    .map(|value| value as u32)
                    .unwrap_or(3),
            })
        })
        .unwrap_or(default)
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
    fields.insert("kind".to_string(), Value::Text("queue_config".to_string()));
    fields.insert("queue".to_string(), Value::Text(queue.to_string()));
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
        config.dlq.clone().map(Value::Text).unwrap_or(Value::Null),
    );
    fields.insert(
        "max_attempts".to_string(),
        Value::UnsignedInteger(u64::from(config.max_attempts)),
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

fn require_queue_group(store: &UnifiedStore, queue: &str, group: &str) -> RedDBResult<()> {
    if queue_group_exists(store, queue, group)? {
        Ok(())
    } else {
        Err(RedDBError::NotFound(format!(
            "consumer group '{}' not found on queue '{}'",
            group, queue
        )))
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
    fields.insert("kind".to_string(), Value::Text("queue_group".to_string()));
    fields.insert("queue".to_string(), Value::Text(queue.to_string()));
    fields.insert("group".to_string(), Value::Text(group.to_string()));
    fields.insert(
        "created_at_ns".to_string(),
        Value::UnsignedInteger(now_ns()),
    );
    insert_meta_row(store, fields)
}

fn load_pending_entries(
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

fn save_queue_pending(
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
    fields.insert("kind".to_string(), Value::Text("queue_pending".to_string()));
    fields.insert("queue".to_string(), Value::Text(queue.to_string()));
    fields.insert("group".to_string(), Value::Text(group.to_string()));
    fields.insert(
        "message_id".to_string(),
        Value::UnsignedInteger(message_id.raw()),
    );
    fields.insert("consumer".to_string(), Value::Text(consumer.to_string()));
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

fn require_pending_entry(
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

fn load_ack_entries(
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

fn save_queue_ack(
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
    fields.insert("kind".to_string(), Value::Text("queue_ack".to_string()));
    fields.insert("queue".to_string(), Value::Text(queue.to_string()));
    fields.insert("group".to_string(), Value::Text(group.to_string()));
    fields.insert(
        "message_id".to_string(),
        Value::UnsignedInteger(message_id.raw()),
    );
    fields.insert("acked_at_ns".to_string(), Value::UnsignedInteger(now_ns()));
    insert_meta_row(store, fields)
}

fn queue_message_completed_for_all_groups(
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
    let manager = store
        .get_collection(queue)
        .ok_or_else(|| RedDBError::NotFound(format!("queue '{}' not found", queue)))?;
    Ok(manager
        .query_all(|entity| matches!(entity.kind, EntityKind::QueueMessage { .. }))
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
    })
}

fn sort_queue_messages(
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

fn next_queue_position(store: &UnifiedStore, queue: &str, side: QueueSide) -> RedDBResult<u64> {
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

fn increment_queue_attempts(
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

fn queue_message_attempts(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<u32> {
    Ok(queue_message_data(store, queue, message_id)?.attempts)
}

fn queue_message_max_attempts(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<u32> {
    Ok(queue_message_data(store, queue, message_id)?.max_attempts)
}

fn queue_message_payload(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<Value> {
    Ok(queue_message_data(store, queue, message_id)?.payload)
}

fn delete_message_with_state(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<()> {
    remove_message_state(store, queue, message_id);
    store
        .delete(queue, message_id)
        .map_err(|err| RedDBError::Internal(err.to_string()))?;
    Ok(())
}

fn remove_message_state(store: &UnifiedStore, queue: &str, message_id: EntityId) {
    remove_meta_rows(store, |row| {
        row_text(row, "queue").as_deref() == Some(queue)
            && row_u64(row, "message_id") == Some(message_id.raw())
    });
}

fn move_message_to_dlq_or_drop(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
    config: &QueueRuntimeConfig,
    _reason: &str,
) -> RedDBResult<Option<String>> {
    let data = queue_message_data(store, queue, message_id)?;

    if let Some(dlq) = &config.dlq {
        if store.get_collection(dlq).is_none() {
            store
                .create_collection(dlq)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        let position = next_queue_position(store, dlq, QueueSide::Right)?;
        let dlq_entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::QueueMessage {
                queue: dlq.clone(),
                position,
            },
            EntityData::QueueMessage(QueueMessageData {
                payload: data.payload,
                priority: data.priority,
                enqueued_at_ns: data.enqueued_at_ns,
                attempts: data.attempts,
                max_attempts: data.max_attempts,
                acked: false,
            }),
        );
        let inserted_id = store
            .insert_auto(dlq, dlq_entity)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        let dlq_config = load_queue_config(store, dlq);
        if let Some(ttl_ms) = dlq_config.ttl_ms {
            store
                .set_metadata(dlq, inserted_id, queue_message_ttl_metadata(ttl_ms))
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        delete_message_with_state(store, queue, message_id)?;
        Ok(Some(dlq.clone()))
    } else {
        delete_message_with_state(store, queue, message_id)?;
        Ok(None)
    }
}

fn queue_manager(
    store: &UnifiedStore,
    queue: &str,
) -> RedDBResult<Arc<crate::storage::unified::SegmentManager>> {
    store
        .get_collection(queue)
        .ok_or_else(|| RedDBError::NotFound(format!("queue '{}' not found", queue)))
}

fn queue_message_data(
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

fn remove_meta_rows(store: &UnifiedStore, predicate: impl Fn(&RowData) -> bool + Sync) {
    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return;
    };
    let rows = manager.query_all(|entity| entity.data.as_row().is_some_and(&predicate));
    for row in rows {
        let _ = store.delete(QUEUE_META_COLLECTION, row.id);
    }
}

fn delete_meta_entity(store: &UnifiedStore, entity_id: EntityId) {
    let _ = store.delete(QUEUE_META_COLLECTION, entity_id);
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

fn row_text(row: &RowData, field: &str) -> Option<String> {
    match row.get_field(field)?.clone() {
        Value::Text(value) => Some(value),
        Value::NodeRef(value) => Some(value),
        Value::EdgeRef(value) => Some(value),
        Value::TableRef(value) => Some(value),
        _ => None,
    }
}

fn row_u64(row: &RowData, field: &str) -> Option<u64> {
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
        context_index_fields,
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
    }
}

fn current_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

fn queue_message_ttl_metadata(ttl_ms: u64) -> Metadata {
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
