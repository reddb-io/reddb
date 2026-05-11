//! Queue delivery lifecycle helpers.
//!
//! This module owns queue retirement semantics so callers do not repeat
//! WORK/FANOUT, DLQ, and MVCC delete decisions.

use crate::storage::query::ast::QueueSide;
use crate::storage::queue::QueueMode;
use crate::storage::unified::entity::QueueMessageData;
use crate::storage::unified::UnifiedStore;

use super::*;

pub(super) struct DeliveredMessage {
    pub(super) message_id: EntityId,
    pub(super) payload: Value,
    pub(super) consumer: String,
    pub(super) delivery_count: u32,
}

pub(super) struct QueuePayloadMessage {
    pub(super) message_id: EntityId,
    pub(super) payload: Value,
}

pub(super) enum NackOutcome {
    Requeued,
    MovedToDlq(String),
    Dropped,
}

pub(super) fn pop_messages(
    runtime: &RedDBRuntime,
    store: &UnifiedStore,
    queue: &str,
    side: QueueSide,
    count: usize,
) -> RedDBResult<Vec<QueuePayloadMessage>> {
    let mut messages = available_messages(runtime, store, queue, side)?;
    let mut popped = Vec::new();
    for message in messages.drain(..) {
        if popped.len() >= count {
            break;
        }

        let message_lock = super::impl_queue::queue_message_lock_handle(runtime, queue, message.id);
        let Some(_guard) = message_lock.try_lock() else {
            continue;
        };
        if super::impl_queue::queue_message_pending_any(store, queue, message.id)? {
            continue;
        }
        let Some(current) = super::impl_queue::queue_message_view_by_id(store, queue, message.id)?
        else {
            continue;
        };

        popped.push(QueuePayloadMessage {
            message_id: current.id,
            payload: current.payload.clone(),
        });
        delete_message_with_state(Some(runtime), store, queue, current.id)?;
    }
    Ok(popped)
}

pub(super) fn peek_messages(
    runtime: &RedDBRuntime,
    store: &UnifiedStore,
    queue: &str,
    count: usize,
) -> RedDBResult<Vec<QueuePayloadMessage>> {
    Ok(available_messages(runtime, store, queue, QueueSide::Left)?
        .into_iter()
        .take(count)
        .map(|message| QueuePayloadMessage {
            message_id: message.id,
            payload: message.payload,
        })
        .collect())
}

pub(super) fn read_messages(
    runtime: &RedDBRuntime,
    store: &UnifiedStore,
    queue: &str,
    group: Option<&str>,
    consumer: &str,
    count: usize,
) -> RedDBResult<Vec<DeliveredMessage>> {
    let config = super::impl_queue::load_queue_config(store, queue);
    let group = super::impl_queue::resolve_read_group(store, queue, group, consumer, &config)?;
    let group = group.as_str();
    let pending = super::impl_queue::load_pending_entries(store, queue, Some(group), None)?;
    let pending_ids = pending
        .iter()
        .map(|entry| entry.message_id)
        .collect::<std::collections::HashSet<_>>();
    let acked_ids = super::impl_queue::load_ack_entries(store, queue, Some(group), None)?
        .into_iter()
        .map(|entry| entry.message_id)
        .collect::<std::collections::HashSet<_>>();
    let mut messages =
        super::impl_queue::load_queue_message_views_with_runtime(Some(runtime), store, queue)?
            .into_iter()
            .filter(|message| {
                !pending_ids.contains(&message.id) && !acked_ids.contains(&message.id)
            })
            .collect::<Vec<_>>();
    super::impl_queue::sort_queue_messages(&mut messages, &config, QueueSide::Left);

    let mut delivered = Vec::new();
    for message in messages {
        if delivered.len() >= count {
            break;
        }

        let message_lock = super::impl_queue::queue_message_lock_handle(runtime, queue, message.id);
        let Some(_guard) = message_lock.try_lock() else {
            continue;
        };
        if super::impl_queue::queue_message_pending_for_group(store, queue, group, message.id)?
            || super::impl_queue::queue_message_acked_for_group(store, queue, group, message.id)?
        {
            continue;
        }
        let Some(current) = super::impl_queue::queue_message_view_by_id(store, queue, message.id)?
        else {
            continue;
        };

        let delivery_count = if config.mode == QueueMode::Fanout {
            1u32
        } else {
            super::impl_queue::increment_queue_attempts(store, queue, current.id)?
        };
        if delivery_count > current.max_attempts {
            let _ = move_message_to_dlq_or_drop(
                Some(runtime),
                store,
                queue,
                current.id,
                &config,
                Some(group),
                "max_attempts_exceeded",
            )?;
            continue;
        }

        let delivered_at_ns = super::impl_queue::now_ns();
        super::impl_queue::save_queue_pending(
            store,
            queue,
            group,
            current.id,
            consumer,
            delivered_at_ns,
            delivery_count,
        )?;
        delivered.push(DeliveredMessage {
            message_id: current.id,
            payload: current.payload,
            consumer: consumer.to_string(),
            delivery_count,
        });
    }

    Ok(delivered)
}

fn available_messages(
    runtime: &RedDBRuntime,
    store: &UnifiedStore,
    queue: &str,
    side: QueueSide,
) -> RedDBResult<Vec<super::impl_queue::QueueMessageView>> {
    let config = super::impl_queue::load_queue_config(store, queue);
    let pending_ids = super::impl_queue::load_pending_entries(store, queue, None, None)?
        .into_iter()
        .map(|entry| entry.message_id)
        .collect::<std::collections::HashSet<_>>();
    let mut messages =
        super::impl_queue::load_queue_message_views_with_runtime(Some(runtime), store, queue)?
            .into_iter()
            .filter(|message| !pending_ids.contains(&message.id))
            .collect::<Vec<_>>();
    super::impl_queue::sort_queue_messages(&mut messages, &config, side);
    Ok(messages)
}

pub(super) fn claim_messages(
    runtime: &RedDBRuntime,
    store: &UnifiedStore,
    queue: &str,
    group: &str,
    consumer: &str,
    min_idle_ms: u64,
) -> RedDBResult<Vec<DeliveredMessage>> {
    super::impl_queue::require_queue_group(store, queue, group)?;
    let config = super::impl_queue::load_queue_config(store, queue);
    let current_time_ns = super::impl_queue::now_ns();
    let min_idle_ns = min_idle_ms.saturating_mul(1_000_000);
    let mut pending = super::impl_queue::load_pending_entries(store, queue, Some(group), None)?
        .into_iter()
        .filter(|entry| current_time_ns.saturating_sub(entry.delivered_at_ns) >= min_idle_ns)
        .collect::<Vec<_>>();
    pending.sort_by_key(|entry| entry.delivered_at_ns);

    let mut delivered = Vec::new();
    for entry in pending {
        let message_lock =
            super::impl_queue::queue_message_lock_handle(runtime, queue, entry.message_id);
        let Some(_guard) = message_lock.try_lock() else {
            continue;
        };
        let Some(current) = super::impl_queue::load_pending_entries(
            store,
            queue,
            Some(group),
            Some(entry.message_id),
        )?
        .into_iter()
        .next() else {
            continue;
        };
        if current_time_ns.saturating_sub(current.delivered_at_ns) < min_idle_ns {
            continue;
        }

        let payload = super::impl_queue::queue_message_payload(store, queue, current.message_id)?;
        let next_delivery_count = current.delivery_count.saturating_add(1);
        let max_attempts =
            super::impl_queue::queue_message_max_attempts(store, queue, current.message_id)?;
        let attempts = if config.mode == QueueMode::Fanout {
            next_delivery_count
        } else {
            super::impl_queue::increment_queue_attempts(store, queue, current.message_id)?
        };
        if attempts > max_attempts {
            super::impl_queue::delete_meta_entity(store, current.entity_id);
            let _ = move_message_to_dlq_or_drop(
                Some(runtime),
                store,
                queue,
                current.message_id,
                &config,
                Some(group),
                "claim_max_attempts_exceeded",
            )?;
            continue;
        }

        super::impl_queue::save_queue_pending(
            store,
            queue,
            group,
            current.message_id,
            consumer,
            current_time_ns,
            next_delivery_count,
        )?;
        delivered.push(DeliveredMessage {
            message_id: current.message_id,
            payload,
            consumer: consumer.to_string(),
            delivery_count: next_delivery_count,
        });
    }

    Ok(delivered)
}

pub(super) fn ack_message(
    runtime: &RedDBRuntime,
    store: &UnifiedStore,
    queue: &str,
    group: &str,
    message_id: EntityId,
    config: &super::impl_queue::QueueRuntimeConfig,
) -> RedDBResult<()> {
    let message_lock = super::impl_queue::queue_message_lock_handle(runtime, queue, message_id);
    let _guard = message_lock.lock();
    let pending = super::impl_queue::require_pending_entry(store, queue, group, message_id)?;
    super::impl_queue::delete_meta_entity(store, pending.entity_id);
    super::impl_queue::save_queue_ack(store, queue, group, message_id)?;

    if config.mode != QueueMode::Fanout
        && super::impl_queue::queue_message_completed_for_all_groups(store, queue, message_id)?
    {
        delete_message_with_state(Some(runtime), store, queue, message_id)?;
    }
    Ok(())
}

pub(super) fn nack_message(
    runtime: &RedDBRuntime,
    store: &UnifiedStore,
    queue: &str,
    group: &str,
    message_id: EntityId,
    config: &super::impl_queue::QueueRuntimeConfig,
) -> RedDBResult<NackOutcome> {
    let message_lock = super::impl_queue::queue_message_lock_handle(runtime, queue, message_id);
    let _guard = message_lock.lock();
    let pending = super::impl_queue::require_pending_entry(store, queue, group, message_id)?;
    super::impl_queue::delete_meta_entity(store, pending.entity_id);

    let attempts = if config.mode == QueueMode::Fanout {
        pending.delivery_count
    } else {
        super::impl_queue::queue_message_attempts(store, queue, message_id)?
    };
    let max_attempts = super::impl_queue::queue_message_max_attempts(store, queue, message_id)?;
    if attempts >= max_attempts {
        return match move_message_to_dlq_or_drop(
            Some(runtime),
            store,
            queue,
            message_id,
            config,
            Some(group),
            "nack_max_attempts_exceeded",
        )? {
            Some(dlq) => Ok(NackOutcome::MovedToDlq(dlq)),
            None => Ok(NackOutcome::Dropped),
        };
    }

    Ok(NackOutcome::Requeued)
}

pub(super) fn delete_message_with_state(
    runtime: Option<&RedDBRuntime>,
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> RedDBResult<()> {
    if let Some(runtime) = runtime {
        if let Some(xid) = runtime.current_xid() {
            if let Some(manager) = store.get_collection(queue) {
                if let Some(mut entity) = manager.get(message_id) {
                    if entity.xmax == 0 {
                        entity.set_xmax(xid);
                        if manager.update(entity).is_ok() {
                            let conn_id = crate::runtime::impl_core::current_connection_id();
                            runtime.record_pending_tombstone(conn_id, queue, message_id, xid);
                            super::impl_queue::forget_queue_message_lock(
                                runtime, queue, message_id,
                            );
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    remove_message_state(store, queue, message_id);
    store
        .delete(queue, message_id)
        .map_err(|err| RedDBError::Internal(err.to_string()))?;
    if let Some(runtime) = runtime {
        super::impl_queue::forget_queue_message_lock(runtime, queue, message_id);
    }
    Ok(())
}

fn remove_message_state(store: &UnifiedStore, queue: &str, message_id: EntityId) {
    super::impl_queue::remove_meta_rows(store, |row| {
        super::impl_queue::row_text(row, "queue").as_deref() == Some(queue)
            && super::impl_queue::row_u64(row, "message_id") == Some(message_id.raw())
    });
}

pub(super) fn move_message_to_dlq_or_drop(
    runtime: Option<&RedDBRuntime>,
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
    config: &super::impl_queue::QueueRuntimeConfig,
    group: Option<&str>,
    _reason: &str,
) -> RedDBResult<Option<String>> {
    let data = super::impl_queue::queue_message_data(store, queue, message_id)?;

    if let Some(dlq) = &config.dlq {
        if store.get_collection(dlq).is_none() {
            store
                .create_collection(dlq)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        let position = super::impl_queue::next_queue_position(store, dlq, QueueSide::Right)?;
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
        let dlq_config = super::impl_queue::load_queue_config(store, dlq);
        if let Some(ttl_ms) = dlq_config.ttl_ms {
            store
                .set_metadata(
                    dlq,
                    inserted_id,
                    super::impl_queue::queue_message_ttl_metadata(ttl_ms),
                )
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        retire_message_for_group(runtime, store, queue, message_id, group, config)?;
        Ok(Some(dlq.clone()))
    } else {
        retire_message_for_group(runtime, store, queue, message_id, group, config)?;
        Ok(None)
    }
}

fn retire_message_for_group(
    runtime: Option<&RedDBRuntime>,
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
    group: Option<&str>,
    config: &super::impl_queue::QueueRuntimeConfig,
) -> RedDBResult<()> {
    if let Some(g) = group {
        super::impl_queue::save_queue_ack(store, queue, g, message_id)?;
        if config.mode != QueueMode::Fanout
            && super::impl_queue::queue_message_completed_for_all_groups(store, queue, message_id)?
        {
            delete_message_with_state(runtime, store, queue, message_id)?;
        }
    } else {
        delete_message_with_state(runtime, store, queue, message_id)?;
    }
    Ok(())
}
