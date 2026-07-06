//! Analytics / ops `red.*` snapshot builders.
//!
//! Extracted from the `red_schema` dispatcher (issue #1639). Serves
//! `red.analytics.metrics`, `red.analytics.slos`, `red.analytics.sources`,
//! `red.schema_registry`, `red.subscriptions`, `red.retention`,
//! `red.materialized_views`, `red.queue_pending`, and `red.queues`.

use super::helpers::*;
use super::*;

pub(super) fn schema_registry_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let store = runtime.db().store();
    let schema = Arc::new(
        SCHEMA_REGISTRY_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    super::analytics_schema_registry::list(store.as_ref())
        .into_iter()
        .map(|entry| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.event_name),
                    Value::UnsignedInteger(entry.version as u64),
                    Value::text(entry.schema_json),
                    Value::TimestampMs(entry.registered_at_ms as i64),
                ],
            )
        })
        .collect()
}
pub(super) fn analytics_metrics_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let store = runtime.db().store();
    let schema = Arc::new(
        ANALYTICS_METRIC_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    super::metric_descriptor_catalog::list(store.as_ref())
        .into_iter()
        .map(|entry| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.path),
                    Value::text(entry.kind),
                    Value::text(entry.role),
                    timestamp_ms_value(entry.created_at_ms),
                    entry.source.map(Value::text).unwrap_or(Value::Null),
                    entry.query.map(Value::text).unwrap_or(Value::Null),
                    entry
                        .window_ms
                        .map(|ms| Value::Integer(ms as i64))
                        .unwrap_or(Value::Null),
                    entry.time_field.map(Value::text).unwrap_or(Value::Null),
                ],
            )
        })
        .collect()
}
pub(super) fn analytics_slos_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let store = runtime.db().store();
    let schema = Arc::new(
        ANALYTICS_SLO_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    super::slo_descriptor_catalog::list(store.as_ref())
        .into_iter()
        .map(|entry| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.path),
                    Value::text(entry.metric_path),
                    Value::Float(entry.target),
                    Value::Integer(entry.window_ms as i64),
                    timestamp_ms_value(entry.created_at_ms),
                ],
            )
        })
        .collect()
}
pub(super) fn analytics_sources_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let store = runtime.db().store();
    let schema = Arc::new(
        ANALYTICS_SOURCE_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    super::analytics_source_catalog::list(store.as_ref())
        .into_iter()
        .filter(|entry| {
            visible_collections
                .map(|visible| visible.contains(&entry.collection))
                .unwrap_or(true)
        })
        .map(|entry| {
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(entry.name),
                    Value::text(entry.collection),
                    Value::text(entry.time_field),
                    Value::text(entry.event_field),
                    Value::text(entry.actor_field),
                    entry.session_field.map(Value::text).unwrap_or(Value::Null),
                    entry
                        .properties_field
                        .map(Value::text)
                        .unwrap_or(Value::Null),
                    timestamp_ms_value(entry.created_at_ms),
                ],
            )
        })
        .collect()
}
pub(super) fn subscriptions_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        SUBSCRIPTION_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let contracts = runtime.db().collection_contracts();
    let created_at_by_collection: HashMap<&str, u128> = contracts
        .iter()
        .map(|contract| (contract.name.as_str(), contract.created_at_unix_ms))
        .collect();
    let mut records = Vec::new();

    for collection in snapshot.collections {
        if !collection_is_visible(&collection.name, visible_collections) {
            continue;
        }

        let created_at = created_at_by_collection
            .get(collection.name.as_str())
            .copied()
            .unwrap_or(0);
        for subscription in collection.subscriptions {
            let mode = subscription_queue_mode(store.as_ref(), &subscription.target_queue)
                .to_ascii_uppercase();
            let name = if subscription.name.is_empty() {
                format!("{}_to_{}", subscription.source, subscription.target_queue)
            } else {
                subscription.name.clone()
            };
            records.push(UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(name),
                    Value::text(subscription.source),
                    Value::text(subscription.target_queue.clone()),
                    Value::text(mode),
                    Value::Array(
                        subscription
                            .ops_filter
                            .iter()
                            .map(|op| Value::text(op.as_str()))
                            .collect(),
                    ),
                    subscription
                        .where_filter
                        .map(Value::text)
                        .unwrap_or(Value::Null),
                    Value::Array(
                        subscription
                            .redact_fields
                            .into_iter()
                            .map(Value::text)
                            .collect(),
                    ),
                    Value::Boolean(subscription.enabled),
                    Value::UnsignedInteger(0),
                    Value::UnsignedInteger(outbox_dlq_count(
                        store.as_ref(),
                        &subscription.target_queue,
                    )),
                    Value::TimestampMs(created_at as i64),
                ],
            ));
        }
    }

    records
}
fn outbox_dlq_count(store: &UnifiedStore, target_queue: &str) -> u64 {
    let dlq = format!("{target_queue}_outbox_dlq");
    let Some(manager) = store.get_collection(&dlq) else {
        return 0;
    };
    manager
        .query_all(|entity| matches!(&entity.kind, crate::storage::EntityKind::QueueMessage { queue, .. } if queue == &dlq))
        .len() as u64
}
fn subscription_queue_mode(store: &UnifiedStore, queue: &str) -> String {
    match store.get_config(&format!("queue.{queue}.mode")) {
        Some(Value::Text(value)) => value.to_string(),
        _ => super::impl_queue::queue_mode_str(store, queue).to_string(),
    }
}
/// Issue #580 — DeclarativeRetention slice 1. Per-collection retention
/// state: `(name, retention_duration, oldest_row_ts,
/// expired_row_count_estimate)`. Materialised views are not subject to
/// source retention in this slice.
pub(super) fn retention_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&std::collections::HashSet<String>>,
) -> Vec<UnifiedRecord> {
    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        RETENTION_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let db = runtime.db();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    snapshot
        .collections
        .into_iter()
        .filter(|collection| {
            visible_collections.is_none_or(|visible| visible.contains(&collection.name))
        })
        .map(|collection| {
            let contract = db.collection_contract(&collection.name);
            let retention_ms = contract.as_ref().and_then(|c| c.retention_duration_ms);
            let ts_column = contract
                .as_ref()
                .and_then(crate::runtime::retention_filter::resolve_timestamp_column);

            // Cheap-ish single pass: walk the collection once,
            // tracking the min timestamp and counting expired rows.
            // The acceptance criterion explicitly allows an
            // approximation here; we deliberately keep the scan
            // simple rather than reach for zone-map min lookups
            // that don't exist on the schemaless `created_at`
            // axis yet.
            let cutoff = retention_ms.map(|ret| (now_ms as i64).saturating_sub(ret as i64));
            let mut oldest_ts: Option<i64> = None;
            let mut expired_count: u64 = 0;
            if let Some(manager) = store.get_collection(&collection.name) {
                manager.for_each_entity(|entity| {
                    let ts = match ts_column.as_deref() {
                        Some("created_at") => Some(entity.created_at as i64),
                        Some("updated_at") => Some(entity.updated_at as i64),
                        Some(name) => entity
                            .data
                            .as_row()
                            .and_then(|row| row.get_field(name))
                            .and_then(value_as_ms),
                        None => Some(entity.created_at as i64),
                    };
                    if let Some(t) = ts {
                        oldest_ts = Some(match oldest_ts {
                            Some(prev) => prev.min(t),
                            None => t,
                        });
                        if let Some(c) = cutoff {
                            if t < c {
                                expired_count = expired_count.saturating_add(1);
                            }
                        }
                    }
                    true
                });
            }

            let retention_value = retention_ms
                .map(Value::UnsignedInteger)
                .unwrap_or(Value::Null);
            let oldest_value = oldest_ts.map(Value::BigInt).unwrap_or(Value::Null);
            // Issue #584 slice 12 — sweeper state. `last_sweep_at == 0`
            // means the collection has never been ticked; surface as
            // NULL rather than the unix epoch.
            let sweeper_state = runtime.inner.retention_sweeper.read().get(&collection.name);
            let last_sweep_at = if sweeper_state.last_sweep_at_ms == 0 {
                Value::Null
            } else {
                Value::TimestampMs(sweeper_state.last_sweep_at_ms as i64)
            };
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name),
                    retention_value,
                    oldest_value,
                    Value::UnsignedInteger(expired_count),
                    last_sweep_at,
                    Value::UnsignedInteger(sweeper_state.rows_swept_total),
                    Value::UnsignedInteger(sweeper_state.last_pending_estimate),
                ],
            )
        })
        .collect()
}
pub(super) fn materialized_views_snapshot(runtime: &RedDBRuntime) -> Vec<UnifiedRecord> {
    let schema = Arc::new(
        MATERIALIZED_VIEW_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let entries = runtime.materialized_view_metadata();
    entries
        .into_iter()
        .map(|m| {
            let refresh_every = m
                .refresh_every_ms
                .map(Value::UnsignedInteger)
                .unwrap_or(Value::Null);
            let last_refresh_at = if m.last_refresh_at_ms == 0 {
                Value::Null
            } else {
                Value::TimestampMs(m.last_refresh_at_ms as i64)
            };
            let last_error = m.last_error.clone().map(Value::text).unwrap_or(Value::Null);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(m.name),
                    Value::text(m.query_text),
                    refresh_every,
                    last_refresh_at,
                    Value::UnsignedInteger(m.last_refresh_duration_ms),
                    last_error,
                    Value::UnsignedInteger(m.current_row_count),
                ],
            )
        })
        .collect()
}
/// Issue #536 — per-row pending-delivery drill-down.
///
/// Reads from the `red_queue_meta` rows that the user-facing
/// `QUEUE READ` path writes (`kind = "queue_pending"` legacy and
/// `kind = "queue_pending_lc"` lifecycle). Cold scan, no caching:
/// every read walks the live meta collection.
///
/// Field mapping from the meta-row schema to the public columns:
/// - `consumer`            -> `locked_by`
/// - `delivery_count - 1`  -> `attempts` (delivery_count is incremented
///   to 1 on the first deliver, so attempts
///   starts at 0 and rises on NACK/redelivery)
/// - `delivered_at_ns + queue.lock_deadline_ms`
///   -> `lock_deadline` (the legacy plumbing does
///   not persist the deadline; derive it
///   from the queue descriptor's
///   `lock_deadline_ms`).
/// - opaque `delivery_id`  composed from `(queue, group, message_id,
///   delivery_count)`. The legacy plumbing has
///   no first-class delivery_id; this string
///   is stable for a given delivery instance
///   and changes when the message is
///   re-delivered (delivery_count bumps).
pub(super) fn queue_pending_snapshot(
    runtime: &RedDBRuntime,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    use crate::storage::query::DEFAULT_QUEUE_LOCK_DEADLINE_MS;

    let schema = Arc::new(
        QUEUE_PENDING_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let Some(manager) = store.get_collection("red_queue_meta") else {
        return Vec::new();
    };

    // Queue → lock_deadline_ms lookup from the catalog descriptor hot
    // fields. Falls back to the engine-wide default when unset.
    let snapshot = runtime.db().catalog_model_snapshot();
    let queue_lock_ms: HashMap<String, u64> = snapshot
        .collections
        .iter()
        .filter_map(|c| c.queue_lock_deadline_ms.map(|ms| (c.name.clone(), ms)))
        .collect();

    let mut records = Vec::new();
    let attempts_by_key: HashMap<(String, String, u64), u64> = manager
        .query_all(|entity| {
            entity
                .data
                .as_row()
                .is_some_and(|row| row_text(row, "kind").as_deref() == Some("queue_attempts_lc"))
        })
        .into_iter()
        .filter_map(|entity| {
            let row = entity.data.as_row()?;
            Some((
                (
                    row_text(row, "queue")?,
                    row_text(row, "group")?,
                    row_u64(row, "message_id")?,
                ),
                row_u64(row, "attempts").unwrap_or(1),
            ))
        })
        .collect();
    let mut seen_pending: HashSet<(String, String, u64)> = HashSet::new();
    let entities = manager.query_all(|entity| {
        entity.data.as_row().is_some_and(|row| {
            matches!(
                row_text(row, "kind").as_deref(),
                Some("queue_pending") | Some("queue_pending_lc")
            )
        })
    });
    for entity in entities {
        let Some(row) = entity.data.as_row() else {
            continue;
        };
        let Some(queue) = row_text(row, "queue") else {
            continue;
        };
        if !collection_is_visible(&queue, visible_collections) {
            continue;
        }
        let Some(group) = row_text(row, "group") else {
            continue;
        };
        let Some(message_id) = row_u64(row, "message_id") else {
            continue;
        };
        if !seen_pending.insert((queue.clone(), group.clone(), message_id)) {
            continue;
        }
        let kind = row_text(row, "kind").unwrap_or_default();
        let consumer = row_text(row, "consumer").unwrap_or_default();

        let lock_ms = queue_lock_ms
            .get(&queue)
            .copied()
            .unwrap_or(DEFAULT_QUEUE_LOCK_DEADLINE_MS);
        let (lock_deadline_ms, delivery_count, delivery_id) = if kind == "queue_pending_lc" {
            let deadline_ns = row_u64(row, "lock_deadline_ns").unwrap_or(0);
            let delivery_count = attempts_by_key
                .get(&(queue.clone(), group.clone(), message_id))
                .copied()
                .unwrap_or(1);
            let delivery_id = row_text(row, "delivery_id").unwrap_or_default();
            (deadline_ns / 1_000_000, delivery_count, delivery_id)
        } else {
            let delivered_at_ns = row_u64(row, "delivered_at_ns").unwrap_or(0);
            let delivery_count = row_u64(row, "delivery_count").unwrap_or(1);
            let lock_deadline_ms = (delivered_at_ns / 1_000_000).saturating_add(lock_ms);
            let delivery_id = format!("{queue}:{group}:{message_id}:{delivery_count}");
            (lock_deadline_ms, delivery_count, delivery_id)
        };
        let attempts = delivery_count.saturating_sub(1);
        let ordering_key = super::super::impl_queue::read_message_ordering_key(
            store.as_ref(),
            &queue,
            EntityId::new(message_id),
        )
        .map(Value::text)
        .unwrap_or(Value::Null);

        records.push(UnifiedRecord::with_schema(
            Arc::clone(&schema),
            vec![
                Value::text(queue),
                Value::text(group),
                Value::UnsignedInteger(message_id),
                Value::text(delivery_id),
                ordering_key,
                Value::UnsignedInteger(attempts),
                Value::TimestampMs(lock_deadline_ms as i64),
                Value::text(consumer),
            ],
        ));
    }
    records
}
/// Issue #535 — QueueLifecycle slice 8.
///
/// Per-queue introspection backing `red.queues` and the repointed
/// `SHOW QUEUES` desugar. Hot fields (`mode`, `depth`, `dlq_target`,
/// `attention`) come from the catalog descriptor — sub-ms reads, no
/// B-tree walk per row. `total_pending` and `oldest_pending_age` are
/// derived from a single pass over `red_queue_meta` queue_pending
/// rows so they cannot drift from the source of truth that
/// `red.queue_pending` and `queue_pending_gauge` already render
/// from.
pub(super) fn queues_snapshot(
    runtime: &RedDBRuntime,
    tenant: Option<&str>,
    visible_collections: Option<&HashSet<String>>,
) -> Vec<UnifiedRecord> {
    use crate::storage::query::DEFAULT_QUEUE_LOCK_DEADLINE_MS;

    let snapshot = runtime.db().catalog_model_snapshot();
    let schema = Arc::new(
        QUEUE_COLUMNS
            .iter()
            .map(|name| Arc::<str>::from(*name))
            .collect::<Vec<_>>(),
    );
    let store = runtime.db().store();
    let internal_registry = InternalCollectionRegistry::from_store(store.as_ref());

    let now_ms: u64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // Single pass: per-queue (count, oldest delivered_at_ns).
    let queue_lock_ms: HashMap<String, u64> = snapshot
        .collections
        .iter()
        .filter_map(|c| c.queue_lock_deadline_ms.map(|ms| (c.name.clone(), ms)))
        .collect();
    let mut per_queue: HashMap<String, (u64, u64)> = HashMap::new();
    let mut seen_pending: HashSet<(String, String, u64)> = HashSet::new();
    if let Some(manager) = store.get_collection("red_queue_meta") {
        let entities = manager.query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                matches!(
                    row_text(row, "kind").as_deref(),
                    Some("queue_pending") | Some("queue_pending_lc")
                )
            })
        });
        for entity in entities {
            let Some(row) = entity.data.as_row() else {
                continue;
            };
            let Some(queue) = row_text(row, "queue") else {
                continue;
            };
            let group = row_text(row, "group").unwrap_or_default();
            let message_id = row_u64(row, "message_id").unwrap_or(0);
            if !seen_pending.insert((queue.clone(), group, message_id)) {
                continue;
            }
            let delivered_at_ns = match row_text(row, "kind").as_deref() {
                Some("queue_pending_lc") => {
                    let lock_ms = queue_lock_ms
                        .get(&queue)
                        .copied()
                        .unwrap_or(DEFAULT_QUEUE_LOCK_DEADLINE_MS);
                    row_u64(row, "lock_deadline_ns")
                        .unwrap_or(0)
                        .saturating_sub(lock_ms.saturating_mul(1_000_000))
                }
                _ => row_u64(row, "delivered_at_ns").unwrap_or(0),
            };
            let entry = per_queue.entry(queue).or_insert((0, u64::MAX));
            entry.0 = entry.0.saturating_add(1);
            if delivered_at_ns > 0 && delivered_at_ns < entry.1 {
                entry.1 = delivered_at_ns;
            }
        }
    }

    snapshot
        .collections
        .into_iter()
        .filter(|c| c.model == CollectionModel::Queue)
        .filter(|c| visible_collections.is_none_or(|visible| visible.contains(&c.name)))
        .filter(|c| {
            tenant.is_none_or(|tenant| {
                collection_tenant(store.as_ref(), &c.name)
                    .as_deref()
                    .is_none_or(|owner| owner == tenant)
            })
        })
        .map(|collection| {
            let mode_value = collection
                .queue_mode
                .map(|m| Value::text(m.as_str().to_ascii_uppercase()))
                .unwrap_or_else(|| {
                    Value::text(
                        super::impl_queue::queue_mode_str(store.as_ref(), &collection.name)
                            .to_ascii_uppercase(),
                    )
                });
            let (total_pending, oldest_age_ms) = match per_queue.get(&collection.name) {
                Some(&(count, oldest_ns)) if count > 0 && oldest_ns != u64::MAX => {
                    let oldest_ms = oldest_ns / 1_000_000;
                    let age = now_ms.saturating_sub(oldest_ms);
                    (count, Some(age))
                }
                Some(&(count, _)) => (count, None),
                None => (0, None),
            };
            let oldest_value = oldest_age_ms
                .map(Value::UnsignedInteger)
                .unwrap_or(Value::Null);
            let dlq_value = collection
                .queue_dlq_target
                .clone()
                .map(Value::text)
                .unwrap_or(Value::Null);
            let internal = internal_registry.is_internal(&collection.name);
            UnifiedRecord::with_schema(
                Arc::clone(&schema),
                vec![
                    Value::text(collection.name.clone()),
                    mode_value,
                    Value::UnsignedInteger(collection.entities as u64),
                    Value::UnsignedInteger(total_pending),
                    oldest_value,
                    dlq_value,
                    Value::Boolean(collection.attention_required),
                    Value::Boolean(internal),
                ],
            )
        })
        .collect()
}
fn row_u64(row: &crate::storage::unified::entity::RowData, field: &str) -> Option<u64> {
    match row.get_field(field)? {
        Value::UnsignedInteger(v) => Some(*v),
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        _ => None,
    }
}
