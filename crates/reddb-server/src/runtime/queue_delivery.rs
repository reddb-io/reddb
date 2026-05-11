//! Queue delivery lifecycle helpers.
//!
//! This module owns queue retirement semantics so callers do not repeat
//! WORK/FANOUT, DLQ, and MVCC delete decisions.

use crate::storage::query::ast::QueueSide;
use crate::storage::queue::QueueMode;
use crate::storage::unified::entity::QueueMessageData;
use crate::storage::unified::UnifiedStore;

use super::*;

pub(super) enum NackOutcome {
    Requeued,
    MovedToDlq(String),
    Dropped,
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
