//! Event operator execution.

use std::collections::HashSet;

use super::*;

impl RedDBRuntime {
    pub fn execute_events_backfill(
        &self,
        raw_query: &str,
        query: &EventsBackfillQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;

        let contract = self
            .db()
            .collection_contract_arc(&query.collection)
            .ok_or_else(|| RedDBError::NotFound(query.collection.clone()))?;
        if contract.declared_model == crate::catalog::CollectionModel::Queue {
            return Err(RedDBError::Query(
                "queues cannot be event backfill sources".to_string(),
            ));
        }

        let subscription = contract
            .subscriptions
            .iter()
            .find(|sub| sub.enabled && sub.target_queue == query.target_queue)
            .cloned()
            .ok_or_else(|| {
                RedDBError::Query(format!(
                    "no enabled event subscription from '{}' to '{}'",
                    query.collection, query.target_queue
                ))
            })?;

        let command_filter = match query.where_filter.as_deref() {
            Some(sql) => Some(parse_backfill_filter(sql)?),
            None => None,
        };
        let subscription_filter = subscription
            .where_filter
            .as_deref()
            .and_then(parse_subscription_filter);
        let tenant_column = self.tenant_column(&query.collection);
        let rls_filter = if tenant_column.is_none()
            && crate::runtime::impl_core::rls_is_enabled(self, &query.collection)
        {
            match crate::runtime::impl_core::rls_policy_filter(
                self,
                &query.collection,
                crate::storage::query::ast::PolicyAction::Select,
            ) {
                Some(filter) => Some(filter),
                None => return Ok(backfill_result(raw_query, 0, 0, 0, &query.target_queue)),
            }
        } else {
            None
        };

        let store = self.inner.db.store();
        let manager = store
            .get_collection(&query.collection)
            .ok_or_else(|| RedDBError::NotFound(query.collection.clone()))?;
        let snap_ctx = crate::runtime::impl_core::capture_current_snapshot();
        let mut entities = manager.query_all(|entity| {
            crate::runtime::impl_core::entity_visible_with_context(snap_ctx.as_ref(), entity)
        });
        entities.sort_by_key(|entity| entity.id.raw());

        let effective_queue = crate::runtime::mutation::effective_queue_name(&subscription);
        let mut existing_event_ids = queue_event_ids(store.as_ref(), &effective_queue)?;
        let subscription_id = subscription_identity(&subscription);
        let mut matched = 0u64;
        let mut enqueued = 0u64;
        let mut skipped = 0u64;

        for entity in entities {
            if !row_matches_filter(self, &query.collection, &entity, command_filter.as_ref()) {
                continue;
            }
            if !row_matches_filter(
                self,
                &query.collection,
                &entity,
                subscription_filter.as_ref(),
            ) {
                continue;
            }
            if !row_matches_tenant_scope(&query.collection, &entity, tenant_column.as_deref()) {
                continue;
            }
            if !row_matches_filter(self, &query.collection, &entity, rls_filter.as_ref()) {
                continue;
            }

            if query.limit.is_some_and(|limit| matched >= limit) {
                break;
            }
            matched += 1;

            let after = crate::runtime::mutation::entity_row_json(&entity);
            let (event_id, payload) = crate::runtime::mutation::backfill_event_payload(
                &query.collection,
                entity.id.raw(),
                &after,
                &subscription_id,
                subscription.redact_fields.as_slice(),
            )?;
            if existing_event_ids.contains(&event_id) {
                skipped += 1;
                continue;
            }

            self.enqueue_event_payload(&effective_queue, Value::Json(payload))?;
            existing_event_ids.insert(event_id);
            enqueued += 1;
        }

        self.invalidate_result_cache_for_table(&effective_queue);
        Ok(backfill_result(
            raw_query,
            matched,
            enqueued,
            skipped,
            &effective_queue,
        ))
    }
}

fn parse_backfill_filter(sql: &str) -> RedDBResult<Filter> {
    crate::storage::query::Parser::new(sql)
        .and_then(|mut parser| parser.parse_filter())
        .map_err(|err| RedDBError::Query(format!("invalid EVENTS BACKFILL WHERE predicate: {err}")))
}

fn parse_subscription_filter(sql: &str) -> Option<Filter> {
    crate::storage::query::Parser::new(sql)
        .ok()
        .and_then(|mut parser| parser.parse_filter().ok())
}

fn row_matches_filter(
    runtime: &RedDBRuntime,
    collection: &str,
    entity: &UnifiedEntity,
    filter: Option<&Filter>,
) -> bool {
    filter.is_none_or(|filter| {
        crate::runtime::query_exec::evaluate_entity_filter_with_db(
            Some(&runtime.inner.db),
            entity,
            filter,
            collection,
            collection,
        )
    })
}

fn row_matches_tenant_scope(
    collection: &str,
    entity: &UnifiedEntity,
    tenant_column: Option<&str>,
) -> bool {
    let Some(tenant_column) = tenant_column else {
        return true;
    };
    let Some(tenant) = crate::runtime::impl_core::current_tenant() else {
        return true;
    };
    let row = crate::runtime::mutation::entity_row_json(entity);
    json_path_string(&row, tenant_column).is_some_and(|value| value == tenant)
        || json_path_string(&row, &format!("{collection}.{tenant_column}"))
            .is_some_and(|value| value == tenant)
}

fn json_path_string<'a>(value: &'a crate::json::Value, path: &str) -> Option<&'a str> {
    let mut current = value;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    current.as_str()
}

fn subscription_identity(subscription: &crate::catalog::SubscriptionDescriptor) -> String {
    if subscription.name.is_empty() {
        format!("{}->{}", subscription.source, subscription.target_queue)
    } else {
        subscription.name.clone()
    }
}

fn queue_event_ids(store: &UnifiedStore, queue: &str) -> RedDBResult<HashSet<String>> {
    let Some(manager) = store.get_collection(queue) else {
        return Ok(HashSet::new());
    };
    let mut ids = HashSet::new();
    for entity in manager.query_all(|entity| matches!(entity.kind, EntityKind::QueueMessage { .. }))
    {
        let EntityData::QueueMessage(message) = entity.data else {
            continue;
        };
        let Value::Json(bytes) = message.payload else {
            continue;
        };
        let Ok(json) = crate::json::from_slice::<crate::json::Value>(&bytes) else {
            continue;
        };
        if let Some(event_id) = json.get("event_id").and_then(|value| value.as_str()) {
            ids.insert(event_id.to_string());
        }
    }
    Ok(ids)
}

fn backfill_result(
    raw_query: &str,
    matched: u64,
    enqueued: u64,
    skipped: u64,
    queue: &str,
) -> RuntimeQueryResult {
    let mut result = UnifiedResult::with_columns(vec![
        "matched".into(),
        "enqueued".into(),
        "skipped".into(),
        "queue".into(),
    ]);
    let mut record = UnifiedRecord::new();
    record.set("matched", Value::UnsignedInteger(matched));
    record.set("enqueued", Value::UnsignedInteger(enqueued));
    record.set("skipped", Value::UnsignedInteger(skipped));
    record.set("queue", Value::text(queue.to_string()));
    result.push(record);

    RuntimeQueryResult {
        query: raw_query.to_string(),
        mode: QueryMode::Sql,
        statement: "events_backfill",
        engine: "runtime-events",
        result,
        affected_rows: enqueued,
        statement_type: "insert",
        bookmark: None,
    }
}
