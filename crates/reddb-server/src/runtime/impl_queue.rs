//! Queue DDL and command execution

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::runtime::audit_log::{AuditAuthSource, AuditEvent, AuditFieldEscaper, Outcome};
use crate::runtime::impl_core::{
    clear_current_auth_identity, clear_current_tenant, current_auth_identity, current_tenant,
    set_current_auth_identity, set_current_tenant,
};
use crate::runtime::queue_telemetry::NackOutcomeLabel;
use crate::storage::queue::QueueMode;
use crate::storage::unified::entity::{QueueMessageData, RowData};
use crate::storage::unified::{Metadata, MetadataValue, UnifiedStore};
use crate::telemetry::operator_event::OperatorEvent;

use super::*;

use super::primary_queue_store::PrimaryQueueStore;
use super::queue_lifecycle::{QueueLifecycle, RetirementOutcome};
use crate::storage::queue::lifecycle::{
    QueueSide as LcQueueSide, QueueStore as _, QueueStoreError, QueueTxn,
};

/// Build a [`QueueLifecycle`] backed by a fresh [`PrimaryQueueStore`] for
/// the given `queue`, plus a `QueueTxn` bound to the live runtime
/// connection. The store inside the lifecycle and the standalone
/// [`PrimaryQueueStore`] returned for ack/nack lookups share the same
/// underlying [`UnifiedStore`] — calls against either are observable on
/// the other.
pub(super) fn runtime_lifecycle(
    runtime: &RedDBRuntime,
    queue: &str,
) -> (
    QueueLifecycle<PrimaryQueueStore>,
    PrimaryQueueStore,
    QueueTxn,
) {
    let primary_for_lookup = PrimaryQueueStore::new(runtime.clone());
    let primary_for_lifecycle = PrimaryQueueStore::new(runtime.clone());
    let txn = primary_for_lifecycle.new_txn();
    let cfg = primary_for_lifecycle.lifecycle_config(queue);
    (
        QueueLifecycle::new(primary_for_lifecycle, cfg)
            .with_telemetry(Arc::clone(&runtime.inner.queue_telemetry)),
        primary_for_lookup,
        txn,
    )
}

/// Slice C of PRD #718 — error surfaced to callers when the wait
/// registry is cancelled (server shutdown) while a `QUEUE READ … WAIT`
/// is parked. Kept as a plain `RedDBError::Query` so transports
/// inherit the message unchanged — there is no separate `Cancelled`
/// variant on the public error today.
pub(crate) const QUEUE_READ_WAIT_CANCELLED: &str =
    "QUEUE READ WAIT cancelled — server shutting down";

/// Slice B of PRD #718 — `red.config` key naming the maximum WAIT
/// budget the runtime will honour. Values above the cap are rejected
/// before any waiter is registered.
pub(crate) const QUEUE_MAX_WAIT_MS_CONFIG_KEY: &str = "red.config.queue.max_wait_ms";

/// Default cap when the operator has not set
/// [`QUEUE_MAX_WAIT_MS_CONFIG_KEY`] — 60 seconds, in milliseconds.
pub(crate) const QUEUE_MAX_WAIT_MS_DEFAULT: u64 = 60_000;

/// Outcome of the async live queue-wait edge ([`RedDBRuntime::redwire_queue_wait_json`],
/// issue #919). Carries the three non-error terminal states the RedWire
/// session maps to distinct frames, so a timeout never aliases an empty
/// delivery and a cancellation never aliases a timeout:
///   - `Delivered` → one `QueueEventPush` per message.
///   - `TimedOut`  → a distinct `QueueWaitTimeout` frame.
///   - `Cancelled` → a `StreamError` with the cancellation code.
///
/// A genuine runtime failure (bad queue, read error) stays an `Err` on
/// the surrounding `RedDBResult`.
#[derive(Debug)]
pub(crate) enum RedwireWaitOutcome {
    Delivered(Vec<crate::serde_json::Value>),
    TimedOut,
    Cancelled,
}

/// Slice C of PRD #718 — scope key for the queue wait registry.
/// Today every connection in the process shares a single namespace;
/// the helper exists so multi-tenant scoping (e.g. tenant id) can be
/// threaded through later without touching every call site.
pub(super) fn queue_wait_scope() -> String {
    crate::runtime::impl_core::current_tenant().unwrap_or_default()
}

fn with_redwire_wait_context<T>(
    auth_identity: &Option<(String, crate::auth::Role)>,
    tenant: &Option<String>,
    f: impl FnOnce() -> T,
) -> T {
    let previous_auth = current_auth_identity();
    let previous_tenant = current_tenant();
    match tenant {
        Some(t) => set_current_tenant(t.clone()),
        None => clear_current_tenant(),
    }
    match auth_identity {
        Some((username, role)) => set_current_auth_identity(username.clone(), *role),
        None => clear_current_auth_identity(),
    }
    let result = f();
    match previous_tenant {
        Some(t) => set_current_tenant(t),
        None => clear_current_tenant(),
    }
    match previous_auth {
        Some((username, role)) => set_current_auth_identity(username, role),
        None => clear_current_auth_identity(),
    }
    result
}

/// Convert a lifecycle `QueueSide` view into the AST flavour we accept
/// from `QueueCommand` callers. Both enums are isomorphic but live in
/// different modules.
fn ast_side_to_lc(side: crate::storage::query::ast::QueueSide) -> LcQueueSide {
    use crate::storage::query::ast::QueueSide as Ast;
    match side {
        Ast::Left => LcQueueSide::Left,
        Ast::Right => LcQueueSide::Right,
    }
}

/// Map a `QueueStoreError` (returned by lifecycle methods) onto the
/// runtime-facing `RedDBError`.
fn map_qse(err: QueueStoreError) -> RedDBError {
    match err {
        QueueStoreError::UnknownDelivery(id) => RedDBError::NotFound(format!(
            "delivery_id '{id}' does not resolve to a live pending delivery"
        )),
        QueueStoreError::UnknownQueue(q) => RedDBError::NotFound(format!("queue '{q}' not found")),
        QueueStoreError::ReplicaImmutable => {
            RedDBError::Internal("replica QueueStore is immutable".to_string())
        }
    }
}

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
    /// Default retry delay (issue #723) applied to NACK-requeued
    /// messages before they become re-deliverable. `None` keeps the
    /// pre-#723 immediate-requeue behaviour. Overridden per-failure by
    /// an authorized `NACK ... WITH DELAY <duration>`.
    pub(super) retry_delay_ms: Option<u64>,
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
    /// First-delivery instant for delayed messages (issue #722). `None`
    /// means immediate availability. Sourced from the
    /// `_available_at_ns` metadata field, populated on push.
    pub(super) available_at_ns: Option<u64>,
    /// Optional grouped-delivery ordering key, sourced from message metadata.
    pub(super) ordering_key: Option<String>,
}

impl QueueMessageView {
    /// Whether this message is currently deliverable. Messages whose
    /// `available_at_ns` lies in the future remain durable and
    /// inspectable but are filtered out of `QUEUE READ` / `QUEUE POP`
    /// projections.
    pub(super) fn is_available_now(&self) -> bool {
        match self.available_at_ns {
            Some(at) => at <= now_ns(),
            None => true,
        }
    }
}

impl RedDBRuntime {
    /// Slice C of PRD #718 — non-blocking `group_read` plus optional
    /// `WAIT <duration>` retry. When `wait_ms` is `None` this is the
    /// pre-slice-C synchronous read. When `Some`, an immediate empty
    /// projection parks the caller on the shared
    /// [`crate::runtime::queue_wait_registry::QueueWaitRegistry`] and
    /// retries on wake until the deadline. Timeout returns an empty
    /// projection (zero records, no error). Shutdown cancellation
    /// returns [`QUEUE_READ_WAIT_CANCELLED`].
    pub(super) fn group_read_with_optional_wait(
        &self,
        queue: &str,
        group: &str,
        consumer: &str,
        count: usize,
        wait_ms: Option<u64>,
    ) -> RedDBResult<Vec<crate::runtime::queue_lifecycle::DeliveredMessage>> {
        let do_read =
            |runtime: &RedDBRuntime| -> RedDBResult<Vec<crate::runtime::queue_lifecycle::DeliveredMessage>> {
                // #1371 — serialize concurrent group consumers on this queue so
                // a single available message is claimed (mark_pending) and
                // returned by exactly one consumer. Without this, woken
                // competing waiters all observe the message as available and
                // each delivers it (double-delivery across the wake-all).
                let read_lock = runtime
                    .inner
                    .rmw_locks
                    .lock_for(queue, "__queue_group_read__");
                let _read_guard = read_lock.lock();
                let (lifecycle, _ps, txn) = runtime_lifecycle(runtime, queue);
                lifecycle
                    .group_read(&txn, queue, group, consumer, count)
                    .map_err(map_qse)
            };

        let delivered = do_read(self)?;
        let Some(wait_ms) = wait_ms else {
            return Ok(delivered);
        };
        if !delivered.is_empty() {
            return Ok(delivered);
        }
        // Empty under WAIT: park on the registry. WAIT 0 collapses to
        // a single re-probe of the registry's current state — useful
        // for tests but the timeout path returns immediately.
        //
        // Telemetry (slice D / PRD #718 / #729): we record exactly one
        // `wait_started` increment at entry, and exactly one terminal
        // outcome increment + histogram observation at exit, for the
        // (scope, queue) labels. The histogram measures wall-clock
        // started→resolved across all re-park iterations of this call.
        let registry = self.queue_wait_registry();
        let scope = queue_wait_scope();
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        let telemetry = self.queue_telemetry();
        telemetry.record_wait_started(&scope, queue);
        let wait_start = std::time::Instant::now();
        let observe = |outcome: crate::runtime::queue_telemetry::WaitOutcomeLabel| {
            let elapsed_ms = wait_start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            telemetry.record_wait_outcome(&scope, queue, outcome, elapsed_ms);
        };
        loop {
            // Snapshot BEFORE the re-probe so a notify that fires
            // between the probe and the park bumps the generation and
            // wait_until returns Woken without ever blocking.
            let snapshot = registry.snapshot(&scope, queue);
            let delivered = do_read(self)?;
            if !delivered.is_empty() {
                observe(crate::runtime::queue_telemetry::WaitOutcomeLabel::Woken);
                return Ok(delivered);
            }
            if registry.is_cancelled() {
                observe(crate::runtime::queue_telemetry::WaitOutcomeLabel::Cancelled);
                return Err(RedDBError::Query(QUEUE_READ_WAIT_CANCELLED.to_string()));
            }
            if std::time::Instant::now() >= deadline {
                observe(crate::runtime::queue_telemetry::WaitOutcomeLabel::Timeout);
                return Ok(Vec::new());
            }
            // Issue #722: a delayed message becomes deliverable when its
            // `available_at_ns` passes, but the registry only wakes on
            // producer commits — a quiet queue with a future due-time
            // would otherwise sit on the condvar until the user budget
            // expired. Cap the park horizon at the soonest future
            // `available_at_ns` so the next loop iteration probes the
            // queue at-or-just-after the message becomes due. A
            // `Timeout` from the capped park is not the final answer; we
            // loop and re-probe before deciding the user budget is up.
            let park_deadline = match earliest_future_available_at(&self.inner.db.store(), queue) {
                Some(at_ns) => {
                    let now_ns = now_ns();
                    if at_ns <= now_ns {
                        // Already due; re-probe immediately.
                        deadline.min(std::time::Instant::now())
                    } else {
                        let wait_ns = at_ns - now_ns;
                        let due_instant =
                            std::time::Instant::now() + std::time::Duration::from_nanos(wait_ns);
                        deadline.min(due_instant)
                    }
                }
                None => deadline,
            };
            match registry.wait_until(&snapshot, park_deadline) {
                crate::runtime::queue_wait_registry::WaitOutcome::Woken => continue,
                crate::runtime::queue_wait_registry::WaitOutcome::Timeout => {
                    // If this was the user-supplied deadline, give up;
                    // otherwise loop and re-probe (a delayed message may
                    // have just become due).
                    if std::time::Instant::now() >= deadline {
                        observe(crate::runtime::queue_telemetry::WaitOutcomeLabel::Timeout);
                        return Ok(Vec::new());
                    }
                    continue;
                }
                crate::runtime::queue_wait_registry::WaitOutcome::Cancelled => {
                    observe(crate::runtime::queue_telemetry::WaitOutcomeLabel::Cancelled);
                    return Err(RedDBError::Query(QUEUE_READ_WAIT_CANCELLED.to_string()));
                }
            }
        }
    }

    /// Issue #917 — async live-wait edge used by the RedWire session.
    ///
    /// Unlike [`group_read_with_optional_wait`](Self::group_read_with_optional_wait)
    /// (the synchronous HTTP/condvar caller), this parks on the
    /// registry's async wake head and never holds a blocking OS thread:
    /// the awaiting tokio worker is released back to the runtime for the
    /// wait duration. On every wake it re-probes the *normal* queue
    /// delivery path (`group_read`), so a delivered message is genuinely
    /// claimed, not merely observed. Returns the delivered messages
    /// rendered as JSON values (so the transport edge stays free of
    /// runtime queue types); [`RedwireWaitOutcome::TimedOut`] means the
    /// deadline elapsed without a delivery (issue #919 surfaces this as
    /// a distinct timeout frame rather than an empty push), and a
    /// cancellation surfaces as [`RedwireWaitOutcome::Cancelled`] — the
    /// async analogue of the sync path's [`QUEUE_READ_WAIT_CANCELLED`].
    /// A genuine runtime failure stays an `Err`.
    pub(crate) async fn redwire_queue_wait_json(
        &self,
        queue: &str,
        group: Option<&str>,
        consumer: &str,
        count: usize,
        wait_ms: u64,
        auth_identity: Option<(String, crate::auth::Role)>,
        tenant: Option<String>,
    ) -> RedDBResult<RedwireWaitOutcome> {
        let group_owned: RedDBResult<String> =
            with_redwire_wait_context(&auth_identity, &tenant, || {
                let expr = crate::storage::query::ast::QueryExpr::QueueCommand(
                    crate::storage::query::ast::QueueCommand::GroupRead {
                        queue: queue.to_string(),
                        group: group.map(str::to_string),
                        consumer: consumer.to_string(),
                        count,
                        wait_ms: Some(wait_ms),
                    },
                );
                self.check_query_privilege(&expr)
                    .map_err(RedDBError::Query)?;
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let config = load_queue_config(store.as_ref(), queue);
                resolve_read_group(store.as_ref(), queue, group, consumer, &config)
            });
        let group_owned = group_owned?;
        let group_ref = group_owned.as_str();

        let do_read =
            |runtime: &RedDBRuntime| -> RedDBResult<Vec<crate::runtime::queue_lifecycle::DeliveredMessage>> {
                with_redwire_wait_context(&auth_identity, &tenant, || {
                    let (lifecycle, _ps, txn) = runtime_lifecycle(runtime, queue);
                    lifecycle
                        .group_read(&txn, queue, group_ref, consumer, count)
                        .map_err(map_qse)
                })
            };

        let render = |delivered: Vec<crate::runtime::queue_lifecycle::DeliveredMessage>| {
            RedwireWaitOutcome::Delivered(
                delivered.into_iter().map(delivered_message_json).collect(),
            )
        };

        // Fast path: a message is already deliverable at open time.
        let delivered = do_read(self)?;
        if !delivered.is_empty() {
            return Ok(render(delivered));
        }

        let registry = self.queue_wait_registry();
        let scope = with_redwire_wait_context(&auth_identity, &tenant, queue_wait_scope);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        let telemetry = self.queue_telemetry();
        telemetry.record_wait_started(&scope, queue);
        let wait_start = std::time::Instant::now();
        tracing::debug!(
            target: "reddb::redwire::queue_wait",
            queue,
            group = group_ref,
            consumer,
            count,
            wait_ms,
            scope = scope.as_str(),
            "redwire queue wait parked"
        );
        let observe = |outcome: crate::runtime::queue_telemetry::WaitOutcomeLabel| {
            let elapsed_ms = wait_start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            telemetry.record_wait_outcome(&scope, queue, outcome, elapsed_ms);
            tracing::debug!(
                target: "reddb::redwire::queue_wait",
                queue,
                group = group_ref,
                consumer,
                count,
                wait_ms,
                scope = scope.as_str(),
                outcome = outcome.as_str(),
                duration_ms = elapsed_ms,
                "redwire queue wait resolved"
            );
        };
        loop {
            // Register the async waiter (snapshot the generation) BEFORE
            // the re-probe so a notify landing between probe and park is
            // observed as a generation move rather than a lost wake.
            let waiter = registry.async_waiter(&scope, queue);
            let delivered = do_read(self)?;
            if !delivered.is_empty() {
                observe(crate::runtime::queue_telemetry::WaitOutcomeLabel::Woken);
                return Ok(render(delivered));
            }
            if registry.is_cancelled() {
                observe(crate::runtime::queue_telemetry::WaitOutcomeLabel::Cancelled);
                return Ok(RedwireWaitOutcome::Cancelled);
            }
            if std::time::Instant::now() >= deadline {
                observe(crate::runtime::queue_telemetry::WaitOutcomeLabel::Timeout);
                return Ok(RedwireWaitOutcome::TimedOut);
            }
            let park_deadline = match earliest_future_available_at(&self.inner.db.store(), queue) {
                Some(at_ns) => {
                    let now_ns = now_ns();
                    if at_ns <= now_ns {
                        deadline.min(std::time::Instant::now())
                    } else {
                        let wait_ns = at_ns - now_ns;
                        let due_instant =
                            std::time::Instant::now() + std::time::Duration::from_nanos(wait_ns);
                        deadline.min(due_instant)
                    }
                }
                None => deadline,
            };
            // The async waiter (and its `Arc<Slot>` clone) is a local
            // dropped on every return below, so an expired or cancelled
            // wait releases its registry slot reference and frees the
            // tokio worker the moment this future resolves (AC #4).
            match registry.wait_until_async(&waiter, park_deadline).await {
                crate::runtime::queue_wait_registry::WaitOutcome::Woken => continue,
                crate::runtime::queue_wait_registry::WaitOutcome::Timeout => {
                    if std::time::Instant::now() >= deadline {
                        observe(crate::runtime::queue_telemetry::WaitOutcomeLabel::Timeout);
                        return Ok(RedwireWaitOutcome::TimedOut);
                    }
                    continue;
                }
                crate::runtime::queue_wait_registry::WaitOutcome::Cancelled => {
                    observe(crate::runtime::queue_telemetry::WaitOutcomeLabel::Cancelled);
                    return Ok(RedwireWaitOutcome::Cancelled);
                }
            }
        }
    }

    /// Reject a live queue-wait open whose requested budget exceeds the
    /// server's maximum wait cap (issue #919), mirroring the SQL
    /// `QUEUE READ … WAIT` cap (slice B of PRD #718). Returns the
    /// operator-actionable message (naming the `red.config` key and the
    /// active cap) when `wait_ms` is over the cap, or `Ok(())` to
    /// proceed. The transport calls this *before* spawning the wait
    /// task, so an over-cap request is refused with an explicit error
    /// and never parks — not silently shortened (AC #3).
    pub(crate) fn redwire_queue_wait_cap_check(&self, wait_ms: u64) -> Result<(), String> {
        let cap = self.config_u64(QUEUE_MAX_WAIT_MS_CONFIG_KEY, QUEUE_MAX_WAIT_MS_DEFAULT);
        if wait_ms > cap {
            Err(format!(
                "queue-wait WAIT {wait_ms}ms exceeds server cap {QUEUE_MAX_WAIT_MS_CONFIG_KEY} = {cap}ms"
            ))
        } else {
            Ok(())
        }
    }

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
                retry_delay_ms: query.retry_delay_ms,
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
        if let Some(retry_delay_ms) = query.retry_delay_ms {
            config.retry_delay_ms = if retry_delay_ms == 0 {
                None
            } else {
                Some(retry_delay_ms)
            };
            summary.push(format!("retry_delay_ms={retry_delay_ms}"));
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
                key,
                available,
            } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let config = load_queue_config(store.as_ref(), queue);
                if key.is_some() && config.priority {
                    return Err(RedDBError::Query(format!(
                        "ordering key is not supported on priority queue '{}'",
                        queue
                    )));
                }
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
                // Resolve per-message availability (issue #722): DELAY is
                // relative to the push instant, AVAILABLE AT carries an
                // absolute unix-ms. Both collapse to a unix-ns timestamp
                // delivery paths compare against. `None` means immediate.
                let available_at_ns = available.map(|a| match a {
                    crate::storage::query::ast::QueueAvailability::DelayMs(ms) => {
                        now_ns().saturating_add(ms.saturating_mul(1_000_000))
                    }
                    crate::storage::query::ast::QueueAvailability::AtUnixMs(ms) => {
                        ms.saturating_mul(1_000_000)
                    }
                });
                if config.ttl_ms.is_some() || available_at_ns.is_some() || key.is_some() {
                    store
                        .set_metadata(
                            queue,
                            id,
                            queue_message_metadata(config.ttl_ms, available_at_ns, key.as_deref()),
                        )
                        .map_err(|err| RedDBError::Internal(err.to_string()))?;
                }
                // Slice C of PRD #718 — wake `QUEUE READ … WAIT` waiters.
                // Under autocommit this fires immediately; inside a txn
                // the wake is buffered and replayed on COMMIT (rollback
                // discards it so rolled-back enqueues do not deliver).
                self.record_queue_wake(&queue_wait_scope(), queue);
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
                    bookmark: None,
                })
            }
            QueueCommand::Pop { queue, side, count } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let (lifecycle, _ps, txn) = runtime_lifecycle(self, queue);
                let popped = lifecycle
                    .pop(queue, ast_side_to_lc(*side), *count, &txn)
                    .map_err(map_qse)?;

                let mut result =
                    UnifiedResult::with_columns(vec!["message_id".into(), "payload".into()]);
                for (message_id, payload) in &popped {
                    let mut record = UnifiedRecord::new();
                    record.set(
                        "message_id",
                        Value::text(message_id_string(EntityId::new(*message_id))),
                    );
                    record.set("payload", payload.clone());
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
                    bookmark: None,
                })
            }
            QueueCommand::Peek { queue, count } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let (lifecycle, _ps, txn) = runtime_lifecycle(self, queue);
                let messages = lifecycle.peek(queue, *count, &txn);

                let mut result =
                    UnifiedResult::with_columns(vec!["message_id".into(), "payload".into()]);
                for message in messages {
                    let mut record = UnifiedRecord::new();
                    record.set(
                        "message_id",
                        Value::text(message_id_string(EntityId::new(message.message_id))),
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
                    bookmark: None,
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
                    bookmark: None,
                })
            }
            QueueCommand::Purge { queue } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let (lifecycle, _ps, txn) = runtime_lifecycle(self, queue);
                let count = lifecycle.purge(queue, &txn).map_err(map_qse)?;
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
                wait_ms,
            } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                // Slice B of PRD #718: reject `WAIT` issued inside an
                // explicit transaction, and reject `WAIT > cap` before
                // any waiter is registered. Both checks fire before the
                // lifecycle is touched so a refused statement leaves
                // no side effects (no group auto-create, no parking).
                if let Some(ms) = *wait_ms {
                    if self.current_xid().is_some() {
                        return Err(RedDBError::Query(
                            "QUEUE READ … WAIT is autocommit-only: refusing to park inside an explicit transaction (BEGIN/COMMIT)"
                                .to_string(),
                        ));
                    }
                    let cap =
                        self.config_u64(QUEUE_MAX_WAIT_MS_CONFIG_KEY, QUEUE_MAX_WAIT_MS_DEFAULT);
                    if ms > cap {
                        return Err(RedDBError::Query(format!(
                            "QUEUE READ … WAIT {ms}ms exceeds server cap {QUEUE_MAX_WAIT_MS_CONFIG_KEY} = {cap}ms"
                        )));
                    }
                }
                // Resolve the consumer group up-front so the lifecycle
                // sees the same auto-created `_work_default` / fanout
                // group the legacy `read_messages` would have minted.
                let config = load_queue_config(store.as_ref(), queue);
                let group_owned =
                    resolve_read_group(store.as_ref(), queue, group.as_deref(), consumer, &config)?;
                let group_ref = group_owned.as_str();
                let delivered = self
                    .group_read_with_optional_wait(queue, group_ref, consumer, *count, *wait_ms)?;

                // Issue #742 — record consumer presence on every read,
                // including empty returns. Heartbeat-driven aliveness
                // is the contract; pending deliveries don't define it.
                {
                    let lease_count = u32::try_from(delivered.len()).unwrap_or(u32::MAX);
                    let now_ns = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos() as u64)
                        .unwrap_or(0);
                    self.queue_presence().heartbeat(
                        queue,
                        group_ref,
                        consumer,
                        lease_count,
                        now_ns,
                    );
                }

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
                        Value::text(message_id_string(EntityId::new(message.message_id))),
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
                    bookmark: None,
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
                    bookmark: None,
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
                let (lifecycle, _ps, txn) = runtime_lifecycle(self, queue);
                let delivered = lifecycle
                    .claim_delivering(queue, consumer, *min_idle_ms, &txn)
                    .map_err(map_qse)?;

                let mut result = UnifiedResult::with_columns(vec![
                    "message_id".into(),
                    "delivery_id".into(),
                    "payload".into(),
                    "consumer".into(),
                    "delivery_count".into(),
                ]);

                for message in delivered {
                    let mut record = UnifiedRecord::new();
                    record.set(
                        "message_id",
                        Value::text(message_id_string(EntityId::new(message.message_id))),
                    );
                    record.set("delivery_id", Value::text(message.delivery_id));
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
                    bookmark: None,
                })
            }
            QueueCommand::Ack {
                queue,
                group,
                message_id,
                delivery_id,
            } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let (group_owned, message_entity) = resolve_ack_nack_handle(
                    store.as_ref(),
                    queue,
                    group,
                    message_id,
                    delivery_id.as_deref(),
                )?;
                let group_ref = group_owned.as_str();
                require_queue_group(store.as_ref(), queue, group_ref)?;
                let (lifecycle, ps, txn) = runtime_lifecycle(self, queue);
                let did = match delivery_id.as_deref() {
                    Some(d) => d.to_string(),
                    None => ps
                        .find_pending_by_key(queue, message_entity.raw(), group_ref)
                        .ok_or_else(|| {
                            RedDBError::NotFound(format!(
                                "no pending delivery for message '{}' on queue '{}' (group '{}')",
                                message_entity.raw(),
                                queue,
                                group_ref
                            ))
                        })?,
                };
                lifecycle.ack(&txn, &did).map_err(map_qse)?;
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
                delivery_id,
                delay_ms,
            } => {
                let store = self.inner.db.store();
                ensure_queue_exists(store.as_ref(), queue)?;
                let config = load_queue_config(store.as_ref(), queue);
                // Issue #723: a per-failure DELAY override is a write
                // operation that re-shapes retry behavior; readers must
                // not be able to silently re-schedule another worker's
                // work. Embedded callers (no auth identity attached)
                // are trusted and bypass the check.
                if delay_ms.is_some() {
                    if let Some((_, role)) = current_auth_identity() {
                        if !role.can_write() {
                            return Err(RedDBError::InvalidOperation(format!(
                                "role '{role}' is not authorized to override NACK retry delay on queue '{queue}'"
                            )));
                        }
                    }
                }
                let (group_owned, message_entity) = resolve_ack_nack_handle(
                    store.as_ref(),
                    queue,
                    group,
                    message_id,
                    delivery_id.as_deref(),
                )?;
                let group_ref = group_owned.as_str();
                require_queue_group(store.as_ref(), queue, group_ref)?;
                let (lifecycle, ps, txn) = runtime_lifecycle(self, queue);
                let did = match delivery_id.as_deref() {
                    Some(d) => d.to_string(),
                    None => ps
                        .find_pending_by_key(queue, message_entity.raw(), group_ref)
                        .ok_or_else(|| {
                            RedDBError::NotFound(format!(
                                "no pending delivery for message '{}' on queue '{}' (group '{}')",
                                message_entity.raw(),
                                queue,
                                group_ref
                            ))
                        })?,
                };
                // Resolve the effective retry delay: per-failure
                // override wins, then queue default, then zero
                // (immediate requeue — pre-#723 behavior).
                let effective_delay_ms = delay_ms.or(config.retry_delay_ms).unwrap_or(0);
                let pending_attempt = ps.read_pending_attempt(&did).map_err(map_qse)?;
                let nack_attempts = pending_attempt.attempts.saturating_add(1);
                let retry_available_at_ns = if effective_delay_ms > 0 {
                    Some(now_ns().saturating_add(effective_delay_ms.saturating_mul(1_000_000)))
                } else {
                    None
                };
                let retry_deadline = if effective_delay_ms > 0 {
                    Some(
                        std::time::Instant::now()
                            + std::time::Duration::from_millis(effective_delay_ms),
                    )
                } else {
                    None
                };
                let outcome = lifecycle
                    .nack_with_retry_deadline(&txn, &did, retry_deadline)
                    .map_err(map_qse)?;
                // Apply delay only when the message was actually
                // requeued — DLQ promotion / drop terminate the
                // retry cycle and a delay would be meaningless.
                if matches!(outcome, RetirementOutcome::Requeued) && effective_delay_ms > 0 {
                    set_message_available_at_ns(
                        store.as_ref(),
                        queue,
                        message_entity,
                        retry_available_at_ns,
                        config.ttl_ms,
                    )?;
                }
                // Issue #723: routine retries do not flood audit
                // channels (telemetry already covers them via
                // `queue_nacked_total{outcome=...}`). Significant
                // overrides — large delays, destination changes,
                // drops — are audited so operators see the events
                // that re-shape operational risk.
                self.maybe_emit_nack_audit(
                    queue,
                    group_ref,
                    &did,
                    *delay_ms,
                    config.retry_delay_ms,
                    &outcome,
                );
                let outcome_label = match &outcome {
                    RetirementOutcome::Requeued => NackOutcomeLabel::Retry,
                    RetirementOutcome::MovedToDlq(_) => NackOutcomeLabel::Dlq,
                    RetirementOutcome::Dropped => NackOutcomeLabel::Drop,
                };
                self.queue_telemetry().record_nacked(
                    queue,
                    group_ref,
                    config.mode.as_str(),
                    outcome_label,
                );
                if let RetirementOutcome::MovedToDlq(dlq) = &outcome {
                    OperatorEvent::QueueDlqPromoted {
                        queue: queue.to_string(),
                        group: group_ref.to_string(),
                        dlq: dlq.clone(),
                        message_id: pending_attempt.message_id,
                        attempts: nack_attempts,
                        reason: format!("lifecycle_nack:{did}"),
                    }
                    .emit(self.audit_log());
                }
                let message = match outcome {
                    RetirementOutcome::Requeued => {
                        if effective_delay_ms > 0 {
                            format!("message requeued (delay={effective_delay_ms}ms)")
                        } else {
                            "message requeued".to_string()
                        }
                    }
                    RetirementOutcome::MovedToDlq(dlq) => {
                        format!("message moved to dead-letter queue '{}'", dlq)
                    }
                    RetirementOutcome::Dropped => "message dropped after max attempts".to_string(),
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
            bookmark: None,
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

        let (move_lifecycle, _move_ps, move_txn) = runtime_lifecycle(self, source);
        for message in &selected {
            move_lifecycle
                .delete_with_state(source, message.id.raw(), &move_txn)
                .map_err(map_qse)?;
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
            bookmark: None,
        })
    }

    /// Issue #723: routine retries are observable through metrics
    /// (`queue_nacked_total{outcome=...}`) so this is the audit
    /// shoulder for the *non-routine* cases: explicit NACK delay
    /// overrides whose magnitude or destination-changing impact would
    /// be invisible in metrics alone. Specifically:
    ///
    /// - Explicit override ≥ 60s (a worker decided to defer well past
    ///   the queue's default cadence — operators care).
    /// - Override that lands on a DLQ promotion or drop (destination
    ///   changed; the override may have influenced retire-vs-requeue
    ///   accounting on the caller's side and the audit trail needs to
    ///   show who asked).
    ///
    /// Calls with no override are intentionally silent here.
    fn maybe_emit_nack_audit(
        &self,
        queue: &str,
        group: &str,
        delivery_id: &str,
        override_ms: Option<u64>,
        default_ms: Option<u64>,
        outcome: &RetirementOutcome,
    ) {
        let Some(override_ms) = override_ms else {
            return;
        };
        let outcome_label = match outcome {
            RetirementOutcome::Requeued => "requeued",
            RetirementOutcome::MovedToDlq(_) => "dlq",
            RetirementOutcome::Dropped => "dropped",
        };
        const SIGNIFICANT_DELAY_MS: u64 = 60_000;
        let destination_changed = !matches!(outcome, RetirementOutcome::Requeued);
        if override_ms < SIGNIFICANT_DELAY_MS && !destination_changed {
            return;
        }
        self.audit_log().record_event(
            AuditEvent::builder("queue/nack/override")
                .source(AuditAuthSource::System)
                .outcome(Outcome::Success)
                .resource(format!("queue:{queue}"))
                .fields([
                    AuditFieldEscaper::field("queue", queue),
                    AuditFieldEscaper::field("group", group),
                    AuditFieldEscaper::field("delivery_id", delivery_id),
                    AuditFieldEscaper::field("override_delay_ms", override_ms),
                    AuditFieldEscaper::field("default_delay_ms", default_ms.unwrap_or(0)),
                    AuditFieldEscaper::field("outcome", outcome_label),
                ])
                .build(),
        );
    }

    /// Whether `collection` is declared as a queue model. Used to route a
    /// `CLAIM` through the [`QueueLifecycle`] seam instead of the raw
    /// row-update path (#1609).
    pub(super) fn is_queue_collection(&self, collection: &str) -> bool {
        self.db()
            .collection_contract_arc(collection)
            .map(|contract| contract.declared_model == crate::catalog::CollectionModel::Queue)
            .unwrap_or(false)
    }

    /// Route a queue-collection `CLAIM` through the [`QueueLifecycle`]
    /// delivery seam (ADR 0020, #1609).
    ///
    /// A `CLAIM` on a queue collection is a delivery *acquisition* — select
    /// and lock pending messages — not a raw UPDATE of the underlying queue
    /// rows. Dispatching here keeps the `QueueLifecycle` state machine
    /// (ACK/NACK, retry, DLQ, pending delivery, replica replay) the sole
    /// authority for delivery state, instead of `execute_update_inner_tracked`
    /// mutating queue storage directly.
    ///
    /// Shapes the delivery seam cannot express are rejected up front with a
    /// clear [`RedDBError::InvalidOperation`] rather than silently falling
    /// back to raw storage mutation (see [`validate_queue_claim_shape`]).
    pub(super) fn execute_queue_shaped_claim(
        &self,
        raw_query: &str,
        query: &UpdateQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        validate_queue_claim_shape(query)?;

        let queue = query.table.as_str();
        let store = self.inner.db.store();
        ensure_queue_exists(store.as_ref(), queue)?;

        // A queue-shaped CLAIM maps onto WORK-mode delivery: each message is
        // reserved for exactly one consumer. FANOUT queues fan every message
        // to every group, which a single-target CLAIM cannot express — those
        // must be consumed through GROUP READ.
        let config = load_queue_config(store.as_ref(), queue);
        if config.mode != QueueMode::Work {
            return Err(RedDBError::InvalidOperation(format!(
                "CLAIM on queue '{queue}' cannot be expressed: FANOUT delivery \
                 must be consumed through GROUP READ, not a queue-shaped CLAIM"
            )));
        }

        // Attribute the delivery to the WORK default consumer group (the same
        // group an unqualified `QUEUE READ` uses), so the pending locks the
        // lifecycle records resolve for a later ACK/NACK by delivery id.
        let group = resolve_read_group(store.as_ref(), queue, None, "", &config)?;
        let count = query.claim_limit.unwrap_or(0) as usize;
        let (lifecycle, _ps, txn) = runtime_lifecycle(self, queue);
        let delivered = lifecycle
            .deliver(&txn, queue, &group, count)
            .map_err(map_qse)?;

        let mut result = UnifiedResult::with_columns(vec!["delivery_id".into(), "payload".into()]);
        for message in &delivered {
            let mut record = UnifiedRecord::new();
            record.set("delivery_id", Value::text(message.delivery_id.clone()));
            record.set("payload", message.payload.clone());
            result.push(record);
        }
        let affected_rows = delivered.len() as u64;
        if affected_rows > 0 {
            self.invalidate_result_cache();
        }

        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: QueryMode::Sql,
            statement: "queue_claim",
            engine: "runtime-queue",
            result,
            affected_rows,
            statement_type: "update",
            bookmark: None,
        })
    }
}

/// Reject `CLAIM` shapes that [`QueueLifecycle`] delivery cannot express
/// (#1609). Queue delivery is strictly FIFO (oldest-available first) and
/// acquires *up to* N available messages:
///
/// - a descending `ORDER BY` contradicts FIFO delivery order, and
/// - `CLAIM EXACT` demands an all-or-nothing batch the delivery seam does
///   not offer.
///
/// Both surface a clear [`RedDBError::InvalidOperation`] instead of a
/// silent raw storage mutation.
fn validate_queue_claim_shape(query: &UpdateQuery) -> RedDBResult<()> {
    if query.order_by.iter().any(|clause| !clause.ascending) {
        return Err(RedDBError::InvalidOperation(format!(
            "CLAIM on queue '{}' cannot be expressed: a descending ORDER BY \
             conflicts with FIFO queue delivery order",
            query.table
        )));
    }
    if query.claim_exact {
        return Err(RedDBError::InvalidOperation(format!(
            "CLAIM EXACT on queue '{}' cannot be expressed: queue delivery \
             acquires up to N available messages, not an exact-or-nothing batch",
            query.table
        )));
    }
    Ok(())
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
        retry_delay_ms: None,
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
                retry_delay_ms: row_u64(row, "retry_delay_ms").filter(|v| *v > 0),
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
    fields.insert(
        "retry_delay_ms".to_string(),
        config
            .retry_delay_ms
            .map(Value::UnsignedInteger)
            .unwrap_or(Value::Null),
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
    let lock_deadline_ns = load_queue_config(store, queue)
        .lock_deadline_ms
        .saturating_mul(1_000_000);
    let attempts_by_key: HashMap<(String, String, u64), u64> = manager
        .query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                row_text(row, "kind").as_deref() == Some("queue_attempts_lc")
                    && row_text(row, "queue").as_deref() == Some(queue)
            })
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
    Ok(manager
        .query_all(|entity| {
            entity.data.as_row().is_some_and(|row| {
                matches!(
                    row_text(row, "kind").as_deref(),
                    Some("queue_pending") | Some("queue_pending_lc")
                ) && row_text(row, "queue").as_deref() == Some(queue)
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
            let group = row_text(row, "group")?;
            let message_id = row_u64(row, "message_id")?;
            let kind = row_text(row, "kind")?;
            let delivered_at_ns = if kind == "queue_pending_lc" {
                row_u64(row, "lock_deadline_ns")
                    .unwrap_or(0)
                    .saturating_sub(lock_deadline_ns)
            } else {
                row_u64(row, "delivered_at_ns")?
            };
            let delivery_count = if kind == "queue_pending_lc" {
                attempts_by_key
                    .get(&(queue.to_string(), group.clone(), message_id))
                    .copied()
                    .unwrap_or(1)
            } else {
                row_u64(row, "delivery_count").unwrap_or(1)
            };
            Some(QueuePendingEntry {
                entity_id: entity.id,
                group,
                message_id: EntityId::new(message_id),
                consumer: row_text(row, "consumer").unwrap_or_default(),
                delivered_at_ns,
                delivery_count: delivery_count as u32,
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
        .map(|mut view| {
            view.available_at_ns = read_message_available_at_ns(store, queue, view.id);
            view.ordering_key = read_message_ordering_key(store, queue, view.id);
            view
        })
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
        available_at_ns: None,
        ordering_key: None,
    })
}

/// Insert a moved payload onto `queue` using only the payload value —
/// priority / attempts / TTL fall back to the destination queue's
/// catalog config (mirrors a fresh enqueue rather than carrying source
/// metadata over). Used by `PrimaryQueueStore::move_to_queue`, the
/// `QueueLifecycle::move_between_queues` adapter that owns only
/// `(message_id, payload)` after `pop_messages` retires the source row.
pub(super) fn insert_moved_queue_message_payload(
    store: &UnifiedStore,
    queue: &str,
    payload: &Value,
) -> RedDBResult<EntityId> {
    let config = load_queue_config(store, queue);
    let position = next_queue_position(store, queue, QueueSide::Right)?;
    let enqueued_at_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let entity = UnifiedEntity::new(
        EntityId::new(0),
        EntityKind::QueueMessage {
            queue: queue.to_string(),
            position,
        },
        EntityData::QueueMessage(QueueMessageData {
            payload: payload.clone(),
            priority: None,
            enqueued_at_ns,
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
    Ok(id)
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
        "key",
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
        "available_at" => Some(Value::UnsignedInteger(
            message.available_at_ns.unwrap_or(message.enqueued_at_ns),
        )),
        "key" => Some(
            message
                .ordering_key
                .as_ref()
                .map(|key| Value::text(key.clone()))
                .unwrap_or(Value::Null),
        ),
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
        .and_then(queue_message_view_from_entity)
        .map(|mut view| {
            view.available_at_ns = read_message_available_at_ns(store, queue, view.id);
            view
        }))
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

/// ADR 0026: resolve the ACK/NACK handle. When `delivery_id` is supplied,
/// it wins unconditionally — strict failure if the handle does not resolve
/// to a live pending delivery on `queue`. When only the legacy tuple is
/// supplied, emit a rate-limited deprecation log line and use the tuple.
/// At least one handle must be present.
pub(super) fn resolve_ack_nack_handle(
    store: &UnifiedStore,
    queue: &str,
    group_hint: &str,
    message_id_hint: &str,
    delivery_id: Option<&str>,
) -> RedDBResult<(String, EntityId)> {
    if let Some(did) = delivery_id {
        return resolve_delivery_id(store, queue, did);
    }
    if group_hint.is_empty() || message_id_hint.is_empty() {
        return Err(RedDBError::Query(
            "ACK/NACK requires either GROUP <group> '<message_id>' or WITH delivery_id = '<id>'"
                .to_string(),
        ));
    }
    log_tuple_deprecation(queue);
    let entity = parse_message_id(message_id_hint)?;
    Ok((group_hint.to_string(), entity))
}

fn resolve_delivery_id(
    store: &UnifiedStore,
    queue: &str,
    delivery_id: &str,
) -> RedDBResult<(String, EntityId)> {
    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return Err(RedDBError::Query(format!(
            "delivery_id '{}' does not resolve to a live pending delivery",
            delivery_id
        )));
    };
    for entity in manager.query_all(|entity| {
        entity.data.as_row().is_some_and(|row| {
            row_text(row, "kind").as_deref() == Some("queue_pending_lc")
                && row_text(row, "delivery_id").as_deref() == Some(delivery_id)
        })
    }) {
        if let Some(row) = entity.data.as_row() {
            let row_queue = row_text(row, "queue").unwrap_or_default();
            let row_group = row_text(row, "group").unwrap_or_default();
            let row_message = row_u64(row, "message_id").unwrap_or(0);
            if row_queue != queue {
                return Err(RedDBError::Query(format!(
                    "delivery_id '{}' belongs to queue '{}', not '{}'",
                    delivery_id, row_queue, queue
                )));
            }
            return Ok((row_group, EntityId::new(row_message)));
        }
    }
    Err(RedDBError::Query(format!(
        "delivery_id '{}' does not resolve to a live pending delivery",
        delivery_id
    )))
}

/// Per-(connection, queue) rate-limited "tuple ACK is deprecated" log line.
/// One emission per minute matches ADR 0026's operational guidance.
fn log_tuple_deprecation(queue: &str) {
    use std::sync::atomic::Ordering;
    use std::sync::{Mutex, OnceLock};
    use std::time::Instant;

    static LAST_EMIT: OnceLock<Mutex<HashMap<(u64, String), Instant>>> = OnceLock::new();
    const COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

    let map = LAST_EMIT.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (super::impl_core::current_connection_id(), queue.to_string());
    let now = Instant::now();
    let mut guard = match map.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let should_emit =
        !matches!(guard.get(&key), Some(prev) if now.duration_since(*prev) < COOLDOWN);
    if should_emit {
        guard.insert(key.clone(), now);
        drop(guard);
        TUPLE_DEPRECATION_EMITS.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            target: "reddb::queue_lifecycle",
            queue = queue,
            connection_id = key.0,
            "ACK/NACK by (queue, group, message_id) tuple is deprecated; \
             switch to the server-issued delivery_id (ADR 0026). \
             The tuple path will be removed one minor release after introduction.",
        );
    }
}

/// Total count of tuple-deprecation log emissions since process start.
/// Intentionally process-wide and `pub` so the transport-bridge
/// integration tests can observe that the legacy tuple path emitted a
/// deprecation while the `delivery_id` path stayed silent, without
/// having to plumb a `tracing::Subscriber` through every test.
pub static TUPLE_DEPRECATION_EMITS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn message_id_string(message_id: EntityId) -> String {
    message_id.raw().to_string()
}

/// Issue #917 — render a delivered queue message as the JSON object the
/// RedWire `QueueEventPush` frame carries. Mirrors the column shape the
/// SQL `QUEUE READ` projection emits (`message_id` / `payload` /
/// `consumer` / `delivery_count`) so the wire push and the pull path
/// stay client-compatible.
fn delivered_message_json(
    message: crate::runtime::queue_lifecycle::DeliveredMessage,
) -> crate::serde_json::Value {
    use crate::serde_json::{Map, Value as JsonValue};
    let mut obj = Map::new();
    obj.insert(
        "message_id".to_string(),
        JsonValue::String(message_id_string(EntityId::new(message.message_id))),
    );
    obj.insert(
        "payload".to_string(),
        crate::presentation::entity_json::storage_value_to_json(&message.payload),
    );
    obj.insert("consumer".to_string(), JsonValue::String(message.consumer));
    obj.insert(
        "delivery_count".to_string(),
        JsonValue::Number(message.delivery_count as f64),
    );
    JsonValue::Object(obj)
}

/// Slice 10 of issue #527 — render-time scan of pending entries
/// per (queue, group) for `queue_pending_gauge` exposition. Walks
/// `red_queue_meta` live so the gauge cannot drift from the source
/// of truth.
pub(crate) fn pending_counts_by_group(
    store: &UnifiedStore,
) -> std::collections::BTreeMap<(String, String), u64> {
    let mut counts: std::collections::BTreeMap<(String, String), u64> =
        std::collections::BTreeMap::new();
    let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
        return counts;
    };
    for entity in manager.query_all(|entity| {
        entity
            .data
            .as_row()
            .is_some_and(|row| row_text(row, "kind").as_deref() == Some("queue_pending"))
    }) {
        if let Some(row) = entity.data.as_row() {
            let queue = row_text(row, "queue");
            let group = row_text(row, "group");
            if let (Some(q), Some(g)) = (queue, group) {
                *counts.entry((q, g)).or_insert(0) += 1;
            }
        }
    }
    counts
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
        analytics_config: Vec::new(),
        session_key: None,
        session_gap_ms: None,
        retention_duration_ms: None,
        analytical_storage: None,

        ai_policy: None,
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
    queue_message_metadata(Some(ttl_ms), None, None)
}

/// Build the per-message metadata row attached to a queue message. Both
/// fields are optional — `_ttl_ms` carries the per-message TTL (existing
/// behaviour) and `_available_at_ns` carries the first-delivery instant
/// for delayed messages (issue #722). When both are present they share
/// the same row, since `UnifiedStore::set_metadata` replaces the entry.
pub(super) fn queue_message_metadata(
    ttl_ms: Option<u64>,
    available_at_ns: Option<u64>,
    ordering_key: Option<&str>,
) -> Metadata {
    let mut fields = HashMap::new();
    if let Some(ttl_ms) = ttl_ms {
        fields.insert(
            "_ttl_ms".to_string(),
            if ttl_ms <= i64::MAX as u64 {
                MetadataValue::Int(ttl_ms as i64)
            } else {
                MetadataValue::Timestamp(ttl_ms)
            },
        );
    }
    if let Some(at_ns) = available_at_ns {
        fields.insert(
            "_available_at_ns".to_string(),
            MetadataValue::Timestamp(at_ns),
        );
    }
    if let Some(key) = ordering_key {
        fields.insert(
            "_ordering_key".to_string(),
            MetadataValue::String(key.to_string()),
        );
    }
    Metadata::with_fields(fields)
}

/// Smallest future `available_at_ns` among messages currently sitting
/// on `queue`. Used by `QUEUE READ … WAIT` (issue #722) to cap the
/// condvar park horizon: without this, a waiter on a quiet queue with
/// only delayed messages would never wake when one became due, since
/// the wait registry is only notified by producer commits. Returns
/// `None` when no future-dated message exists (the common case — the
/// caller falls back to the user-supplied wait budget).
pub(super) fn earliest_future_available_at(store: &UnifiedStore, queue: &str) -> Option<u64> {
    let now_ns = now_ns();
    let views = load_queue_message_views_with_runtime(None, store, queue).ok()?;
    views
        .iter()
        .filter_map(|v| v.available_at_ns)
        .filter(|at| *at > now_ns)
        .min()
}

/// Update the `_available_at_ns` metadata for a queue message in place
/// without dropping any `_ttl_ms` already present (issue #723 — used by
/// NACK retry delay). `available_at_ns = None` clears the field. The
/// `fallback_ttl_ms` argument is consulted only when the existing
/// metadata row carries no `_ttl_ms` — keeps the per-message TTL in
/// sync with the queue default in the common case where no per-message
/// TTL was set explicitly. Tolerant of missing collections / entities:
/// returns `Ok(())` rather than failing the surrounding NACK if the
/// underlying message has gone away between resolution and metadata
/// update (a benign race; the next delivery cycle reflects truth).
pub(super) fn set_message_available_at_ns(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
    available_at_ns: Option<u64>,
    fallback_ttl_ms: Option<u64>,
) -> RedDBResult<()> {
    let existing_ttl_ms = store
        .get_metadata(queue, message_id)
        .and_then(|md| match md.get("_ttl_ms")? {
            MetadataValue::Int(i) if *i >= 0 => Some(*i as u64),
            MetadataValue::Timestamp(t) => Some(*t),
            _ => None,
        })
        .or(fallback_ttl_ms);
    let existing_ordering_key = read_message_ordering_key(store, queue, message_id);
    let metadata = queue_message_metadata(
        existing_ttl_ms,
        available_at_ns,
        existing_ordering_key.as_deref(),
    );
    match store.set_metadata(queue, message_id, metadata) {
        Ok(()) => Ok(()),
        Err(crate::storage::StoreError::CollectionNotFound(_)) => Ok(()),
        Err(err) => Err(RedDBError::Internal(err.to_string())),
    }
}

/// Read the `_ordering_key` metadata for a queue message. Returns `None`
/// for keyless messages or when the metadata row is absent.
pub(super) fn read_message_ordering_key(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> Option<String> {
    let md = store.get_metadata(queue, message_id)?;
    match md.get("_ordering_key")? {
        MetadataValue::String(value) => Some(value.clone()),
        _ => None,
    }
}

/// Read the `_available_at_ns` metadata for a queue message. Returns
/// `None` for messages with no delay (the common case) or when the
/// metadata row is missing entirely.
pub(super) fn read_message_available_at_ns(
    store: &UnifiedStore,
    queue: &str,
    message_id: EntityId,
) -> Option<u64> {
    let md = store.get_metadata(queue, message_id)?;
    match md.get("_available_at_ns")? {
        MetadataValue::Timestamp(t) => Some(*t),
        MetadataValue::Int(i) if *i >= 0 => Some(*i as u64),
        _ => None,
    }
}

/// Rough payload byte estimate for outbox watermark tracking.
fn estimate_payload_bytes(payload: &Value) -> u64 {
    match payload {
        Value::Json(v) => v.len() as u64,
        Value::Text(s) => s.len() as u64,
        _ => 64,
    }
}

#[cfg(test)]
mod presence_integration_tests {
    use super::*;
    use crate::storage::queue::presence::{PresenceState, DEFAULT_PRESENCE_TTL_MS};
    use crate::{RedDBOptions, RedDBRuntime};

    /// Issue #742 acceptance: `QUEUE READ` must register and refresh
    /// consumer presence. The snapshot exposes the same `(queue, group,
    /// consumer)` triple the read named, with `state = Active`.
    #[test]
    fn queue_read_emits_consumer_presence_heartbeat() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE QUEUE tasks").unwrap();
        rt.execute_query("QUEUE GROUP CREATE tasks workers")
            .unwrap();
        rt.execute_query("QUEUE PUSH tasks {'job':'a'}").unwrap();
        rt.execute_query("QUEUE READ tasks GROUP workers CONSUMER w1")
            .unwrap();

        let snap = rt.queue_consumer_presence_snapshot(DEFAULT_PRESENCE_TTL_MS);
        assert_eq!(snap.len(), 1, "exactly one heartbeat recorded");
        let row = &snap[0];
        assert_eq!(row.queue, "tasks");
        assert_eq!(row.group, "workers");
        assert_eq!(row.consumer, "w1");
        assert_eq!(row.state, PresenceState::Active);

        let counts = rt.queue_active_consumer_counts(DEFAULT_PRESENCE_TTL_MS);
        assert_eq!(
            counts[&("tasks".to_string(), "workers".to_string())],
            1,
            "active count reflects the live consumer"
        );
    }

    /// Issue #742 acceptance: a read that returns no messages must
    /// still heartbeat — aliveness is independent of pending
    /// deliveries.
    #[test]
    fn empty_queue_read_still_heartbeats() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("CREATE QUEUE empty_q").unwrap();
        rt.execute_query("QUEUE GROUP CREATE empty_q workers")
            .unwrap();
        // No PUSH — the queue is empty.
        rt.execute_query("QUEUE READ empty_q GROUP workers CONSUMER w1")
            .unwrap();

        let snap = rt.queue_consumer_presence_snapshot(DEFAULT_PRESENCE_TTL_MS);
        assert_eq!(
            snap.len(),
            1,
            "empty read still registers consumer presence"
        );
        assert_eq!(snap[0].state, PresenceState::Active);
        assert_eq!(
            snap[0].lease_count, 0,
            "no messages delivered → zero leases"
        );
    }
}
