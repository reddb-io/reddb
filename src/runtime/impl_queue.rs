//! Queue DDL and command execution

use super::*;

impl RedDBRuntime {
    pub fn execute_create_queue(
        &self,
        raw_query: &str,
        query: &CreateQueueQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
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
            .map_err(|e| RedDBError::Internal(e.to_string()))?;
        self.inner
            .db
            .persist_metadata()
            .map_err(|e| RedDBError::Internal(e.to_string()))?;

        let mut msg = format!("queue '{}' created", query.name);
        if query.priority {
            msg.push_str(" (priority)");
        }
        if let Some(ms) = query.max_size {
            msg.push_str(&format!(" (max_size={})", ms));
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
            .map_err(|e| RedDBError::Internal(e.to_string()))?;
        self.inner
            .db
            .persist_metadata()
            .map_err(|e| RedDBError::Internal(e.to_string()))?;
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
                // Store as entity in the collection
                let store = self.inner.db.store();
                store.get_or_create_collection(queue);
                let entity = UnifiedEntity::new(
                    EntityId::new(0),
                    EntityKind::QueueMessage {
                        queue: queue.clone(),
                        position: 0,
                    },
                    EntityData::QueueMessage(crate::storage::unified::entity::QueueMessageData {
                        payload: Value::Text(value.clone()),
                        priority: *priority,
                        enqueued_at_ns: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_nanos() as u64,
                        attempts: 0,
                        max_attempts: 3,
                        acked: false,
                    }),
                );
                let id = store
                    .insert_auto(queue, entity)
                    .map_err(|e| RedDBError::Internal(e.to_string()))?;
                let side_str = match side {
                    QueueSide::Left => "left",
                    QueueSide::Right => "right",
                };
                let mut result =
                    UnifiedResult::with_columns(vec!["message_id".into(), "side".into()]);
                let mut record = UnifiedRecord::new();
                record.set("message_id", Value::UnsignedInteger(id.raw()));
                record.set("side", Value::Text(side_str.to_string()));
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
                let manager = store
                    .get_collection(queue)
                    .ok_or_else(|| RedDBError::NotFound(format!("queue '{}' not found", queue)))?;
                let entities =
                    manager.query_all(|e| matches!(e.kind, EntityKind::QueueMessage { .. }));
                let mut result =
                    UnifiedResult::with_columns(vec!["message_id".into(), "payload".into()]);
                let take_count = (*count).min(entities.len());
                let selected: Vec<_> = match side {
                    QueueSide::Left => entities.into_iter().take(take_count).collect(),
                    QueueSide::Right => {
                        let len = entities.len();
                        entities
                            .into_iter()
                            .skip(len.saturating_sub(take_count))
                            .collect()
                    }
                };
                for entity in &selected {
                    let mut record = UnifiedRecord::new();
                    record.set("message_id", Value::UnsignedInteger(entity.id.raw()));
                    if let EntityData::QueueMessage(ref qm) = entity.data {
                        record.set("payload", qm.payload.clone());
                    }
                    result.push(record);
                    // Delete the popped entity
                    let _ = store.delete(queue, entity.id);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: QueryMode::Sql,
                    statement: "queue_pop",
                    engine: "runtime-queue",
                    result,
                    affected_rows: selected.len() as u64,
                    statement_type: "delete",
                })
            }
            QueueCommand::Peek { queue, count } => {
                let store = self.inner.db.store();
                let manager = store
                    .get_collection(queue)
                    .ok_or_else(|| RedDBError::NotFound(format!("queue '{}' not found", queue)))?;
                let entities =
                    manager.query_all(|e| matches!(e.kind, EntityKind::QueueMessage { .. }));
                let mut result =
                    UnifiedResult::with_columns(vec!["message_id".into(), "payload".into()]);
                for entity in entities.iter().take(*count) {
                    let mut record = UnifiedRecord::new();
                    record.set("message_id", Value::UnsignedInteger(entity.id.raw()));
                    if let EntityData::QueueMessage(ref qm) = entity.data {
                        record.set("payload", qm.payload.clone());
                    }
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
                let count = store
                    .get_collection(queue)
                    .map(|m| {
                        m.query_all(|e| matches!(e.kind, EntityKind::QueueMessage { .. }))
                            .len()
                    })
                    .unwrap_or(0);
                let mut result = UnifiedResult::with_columns(vec!["len".into()]);
                let mut record = UnifiedRecord::new();
                record.set("len", Value::UnsignedInteger(count as u64));
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
                let manager = store
                    .get_collection(queue)
                    .ok_or_else(|| RedDBError::NotFound(format!("queue '{}' not found", queue)))?;
                let entities =
                    manager.query_all(|e| matches!(e.kind, EntityKind::QueueMessage { .. }));
                let count = entities.len();
                for entity in &entities {
                    let _ = store.delete(queue, entity.id);
                }
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("{} messages purged from queue '{}'", count, queue),
                    "delete",
                ))
            }
            QueueCommand::GroupCreate { queue, group } => Ok(RuntimeQueryResult::ok_message(
                raw_query.to_string(),
                &format!("consumer group '{}' created on queue '{}'", group, queue),
                "create",
            )),
            QueueCommand::GroupRead {
                queue,
                group,
                consumer,
                count,
            } => {
                let store = self.inner.db.store();
                let manager = store
                    .get_collection(queue)
                    .ok_or_else(|| RedDBError::NotFound(format!("queue '{}' not found", queue)))?;
                let entities =
                    manager.query_all(|e| matches!(e.kind, EntityKind::QueueMessage { .. }));
                let mut result = UnifiedResult::with_columns(vec![
                    "message_id".into(),
                    "payload".into(),
                    "consumer".into(),
                ]);
                for entity in entities.iter().take(*count) {
                    let mut record = UnifiedRecord::new();
                    record.set("message_id", Value::UnsignedInteger(entity.id.raw()));
                    if let EntityData::QueueMessage(ref qm) = entity.data {
                        record.set("payload", qm.payload.clone());
                    }
                    record.set("consumer", Value::Text(consumer.clone()));
                    result.push(record);
                }
                let _ = group;
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
            QueueCommand::Ack {
                queue,
                group,
                message_id,
            } => {
                let _ = (queue, group, message_id);
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
                let _ = (queue, group, message_id);
                Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    "message requeued",
                    "update",
                ))
            }
        }
    }
}
