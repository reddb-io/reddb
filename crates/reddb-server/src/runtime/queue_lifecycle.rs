//! `QueueLifecycle` Module — slice 2 of PRD #527.
//!
//! WORK-mode `deliver` + `ack` happy path against a `QueueStore`. The
//! Module owns transition policy (which available messages get reserved,
//! how the lock deadline is computed); storage owns persistence. No
//! `&Engine` dependency — only the `QueueStore` trait — so lifecycle
//! transitions can be exercised in unit tests against the in-memory fake
//! without booting the engine.
//!
//! Scope of this slice (per issue #529):
//! - WORK mode only (no FANOUT)
//! - happy path only (no NACK, no DLQ routing, no lock expiry)
//! - no caller in `impl_queue.rs` yet (module is `pub(crate)`)
//!
//! Retry/lock policy parameters arrive via `LifecycleConfig` rather than
//! a catalog lookup — the catalog wiring lands in a later slice.
//!
//! `EffectiveScope` / auth checks stay with the Statement frame at the
//! caller; this Module trusts that callers have already authorised.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::runtime::queue_telemetry::{NackOutcomeLabel, QueueTelemetryCounters};
use crate::storage::queue::lifecycle::{
    DeliveryId, DlqTarget, MessageId, PendingDeliveryView, QueueSide, QueueStore, QueueStoreError,
    QueueTxn, Result,
};
use crate::storage::queue::mode::QueueMode;
use crate::storage::schema::Value;
use crate::telemetry::operator_event::OperatorEvent;

/// Monotonic clock abstraction. The Module reads "now" through this so
/// unit tests can drive lock-expiry transitions without `std::thread::sleep`.
/// Production wiring uses [`SystemClock`].
pub(crate) trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// Production clock — thin wrapper over [`Instant::now`].
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Lock duration + retry policy. Constructor takes this so the catalog
/// wiring slice can swap the source without touching the Module surface.
///
/// `max_attempts` is *not* a config field — the retry budget lives on
/// each message and is read at decision time via
/// [`QueueStore::read_max_attempts`], so a single queue can carry
/// per-message overrides instead of being capped at one global value.
#[derive(Debug, Clone)]
pub(crate) struct LifecycleConfig {
    pub(crate) lock_duration: Duration,
    /// Destination queue for retired messages. `None` → drop on retire.
    pub(crate) dlq_target: Option<DlqTarget>,
    /// Delivery semantics. WORK reserves each message for exactly one
    /// consumer; FANOUT delivers every message to every group
    /// independently. Caller supplies — sourced from the queue's
    /// `CollectionDescriptor` in the eventual production wiring.
    pub(crate) mode: QueueMode,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            lock_duration: Duration::from_secs(30),
            dlq_target: None,
            mode: QueueMode::Work,
        }
    }
}

/// Internal outcome of a retirement decision. Kept private — the public
/// `nack` surface returns `Result<()>`. Exposed for tests via a tap so
/// slice 10 (observability wiring) can replace the tap with the real
/// Prometheus / AuditLogger emitters without changing this Module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RetirementOutcome {
    Requeued,
    MovedToDlq(DlqTarget),
    Dropped,
}

/// What `deliver` returns to the caller — opaque handle plus payload.
#[derive(Debug, Clone)]
pub(crate) struct Delivery {
    pub(crate) delivery_id: DeliveryId,
    pub(crate) payload: Value,
}

/// Non-mutating view of a queue message, returned by [`QueueLifecycle::peek`].
/// Carries the per-message retry budget (sourced via
/// [`QueueStore::read_max_attempts`]) so callers don't need a second lookup
/// to mirror the legacy `queue_delivery::peek_messages` + max-attempts pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueueMessageView {
    pub(crate) message_id: MessageId,
    pub(crate) payload: Value,
    pub(crate) max_attempts: u32,
}

/// WORK-mode queue lifecycle.
pub(crate) struct QueueLifecycle<S: QueueStore> {
    store: S,
    config: LifecycleConfig,
    clock: Arc<dyn Clock>,
    outcomes: Mutex<Vec<RetirementOutcome>>,
    /// Optional handle to the runtime-shared Prometheus counters.
    /// `None` in lifecycle unit tests so the Module stays free of
    /// runtime-state dependencies; production wiring (slice 12)
    /// will pass `Some(runtime.queue_telemetry_arc())`.
    telemetry: Option<Arc<QueueTelemetryCounters>>,
}

impl<S: QueueStore> QueueLifecycle<S> {
    pub(crate) fn new(store: S, config: LifecycleConfig) -> Self {
        Self::with_clock(store, config, Arc::new(SystemClock))
    }

    /// Same as [`new`], but with a caller-supplied clock. Used by unit
    /// tests exercising lazy lock-expiry reclaim so deadlines can be
    /// crossed without sleeping.
    pub(crate) fn with_clock(
        store: S,
        config: LifecycleConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            config,
            clock,
            outcomes: Mutex::new(Vec::new()),
            telemetry: None,
        }
    }

    /// Attach the runtime-shared Prometheus counters. Called by the
    /// production wiring (slice 12 of issue #527); lifecycle unit
    /// tests omit it.
    #[allow(dead_code)]
    pub(crate) fn with_telemetry(mut self, telemetry: Arc<QueueTelemetryCounters>) -> Self {
        self.telemetry = Some(telemetry);
        self
    }

    /// Reserve up to `count` available messages from `queue` for `group`
    /// and return their opaque delivery handles + payloads. WORK
    /// semantics: each message is reserved for exactly one consumer
    /// (subsequent `deliver` calls skip messages already pending).
    pub(crate) fn deliver(
        &self,
        txn: &QueueTxn,
        queue: &str,
        group: &str,
        count: usize,
    ) -> Result<Vec<Delivery>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let now = self.clock.now();
        // Lazy reclaim: release any pending row whose lock deadline has
        // already passed before we scan for available messages. No
        // sweeper, no timer — every deliver pays its own reclaim cost.
        // Developer signal only — high-volume routine event, not
        // paging-grade (slice 10 of issue #527).
        tracing::debug!(
            target: "reddb::queue_lifecycle",
            queue = queue,
            group = group,
            "queue lock reclaim sweep"
        );
        self.store.reclaim_expired(txn, queue, now)?;
        let available = match self.config.mode {
            QueueMode::Work => self.store.available_messages(queue, QueueSide::Left),
            QueueMode::Fanout => self
                .store
                .available_messages_for_group(queue, group, QueueSide::Left),
        };
        let mut out = Vec::with_capacity(count.min(available.len()));
        for message_id in available.into_iter().take(count) {
            let deadline = now + self.config.lock_duration;
            let delivery_id = self.store.mark_pending(txn, queue, message_id, group, deadline)?;
            let payload = self
                .store
                .read_message(queue, message_id)
                .ok_or_else(|| QueueStoreError::UnknownQueue(queue.to_string()))?;
            out.push(Delivery {
                delivery_id,
                payload,
            });
        }
        if !out.is_empty() {
            if let Some(telemetry) = self.telemetry.as_ref() {
                telemetry.record_delivered(
                    queue,
                    group,
                    self.config.mode.as_str(),
                    out.len() as u64,
                );
            }
        }
        Ok(out)
    }

    /// Retire `delivery_id` — the message is consumed and will not be
    /// redelivered. Unknown delivery ids surface `UnknownDelivery`.
    pub(crate) fn ack(&self, txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        self.retire(txn, delivery_id)
    }

    /// Apply mode-appropriate retirement: WORK consumes the message
    /// (removes from queue + payload); FANOUT retires only the calling
    /// group's pending row and marks the (msg, group) as acked.
    fn retire(&self, txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        match self.config.mode {
            QueueMode::Work => self.store.ack_pending(txn, delivery_id),
            QueueMode::Fanout => self.store.retire_for_group(txn, delivery_id),
        }
    }

    /// Negative-acknowledge `delivery_id`. The Module picks one of
    /// `Requeued | MovedToDlq | Dropped` based on the attempt counter
    /// against `LifecycleConfig::max_attempts` + `dlq_target` and applies
    /// it. Callers never see the decision — observe outcomes via the test
    /// tap.
    pub(crate) fn nack(&self, txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        let bumped = self.store.bump_attempt(txn, delivery_id)?;
        let max_attempts = self
            .store
            .read_max_attempts(&bumped.queue, bumped.message_id);
        let attempts = bumped.attempts;
        if attempts >= max_attempts {
            match &self.config.dlq_target {
                Some(target) => {
                    let payload = self
                        .store
                        .read_pending_payload(delivery_id)
                        .ok_or_else(|| {
                            QueueStoreError::UnknownDelivery(delivery_id.to_string())
                        })?;
                    self.retire(txn, delivery_id)?;
                    self.store.enqueue_dlq(txn, target, payload)?;
                    self.record(RetirementOutcome::MovedToDlq(target.clone()));
                    // DLQ promotion is forensic — emit through the
                    // process-wide audit sink (which also lays a
                    // tracing::warn breadcrumb under
                    // target=reddb::operator). The lifecycle surface
                    // doesn't yet carry queue/group on the delivery
                    // handle (slice 12 of issue #527); the production
                    // path emits the populated event today via
                    // `queue_delivery::move_message_to_dlq_or_drop`.
                    OperatorEvent::QueueDlqPromoted {
                        queue: String::new(),
                        group: String::new(),
                        dlq: target.clone(),
                        message_id: 0,
                        attempts,
                        reason: format!("lifecycle_nack:{delivery_id}"),
                    }
                    .emit_global();
                }
                None => {
                    self.retire(txn, delivery_id)?;
                    self.record(RetirementOutcome::Dropped);
                }
            }
        } else {
            self.store.release_pending(txn, delivery_id)?;
            self.record(RetirementOutcome::Requeued);
        }
        // Counter bumping when the telemetry handle is attached.
        // Per-(queue, group, mode) labels stay accurate even though
        // the lifecycle surface lacks queue/group on the nack handle
        // today — the production wiring (slice 12) builds the
        // lifecycle with the full descriptor and constructs the
        // labels at that layer. Until then the lifecycle-only path
        // bumps with placeholder labels so unit tests don't depend
        // on absent context.
        if let Some(telemetry) = self.telemetry.as_ref() {
            let outcome = match self.outcomes.lock().unwrap_or_else(|p| p.into_inner()).last() {
                Some(RetirementOutcome::MovedToDlq(_)) => NackOutcomeLabel::Dlq,
                Some(RetirementOutcome::Dropped) => NackOutcomeLabel::Drop,
                Some(RetirementOutcome::Requeued) | None => NackOutcomeLabel::Retry,
            };
            telemetry.record_nacked("", "", self.config.mode.as_str(), outcome);
        }
        Ok(())
    }

    /// Non-mutating peek — returns up to `count` available messages on
    /// `queue` from the left, mirroring `queue_delivery::peek_messages`.
    /// Does **not** mark anything pending, does **not** record tombstones,
    /// does **not** bump attempt counters. The `&QueueTxn` parameter is
    /// kept for shape-consistency with the mutating ops (so the bridge
    /// in PRD #598 doesn't need a different call site convention) but
    /// is intentionally unused here.
    pub(crate) fn peek(
        &self,
        queue: &str,
        count: usize,
        _txn: &QueueTxn,
    ) -> Vec<QueueMessageView> {
        if count == 0 {
            return Vec::new();
        }
        let available = self.store.available_messages(queue, QueueSide::Left);
        available
            .into_iter()
            .take(count)
            .filter_map(|message_id| {
                self.store
                    .read_message(queue, message_id)
                    .map(|payload| QueueMessageView {
                        message_id,
                        payload,
                        max_attempts: self.store.read_max_attempts(queue, message_id),
                    })
            })
            .collect()
    }

    /// Non-mutating read — returns the payload backing `delivery_id` if it
    /// is currently held pending. Mirrors the read-shaped portion of
    /// `queue_delivery::read_messages` without touching pending state or
    /// the attempts counter. Returns `None` for unknown / already-retired
    /// delivery ids.
    pub(crate) fn read(&self, delivery_id: &str, _txn: &QueueTxn) -> Option<Value> {
        self.store.read_pending_payload(delivery_id)
    }

    /// Claim (steal) every pending delivery on `queue` whose lock has
    /// expired beyond `min_idle_ms`. Mirrors
    /// `queue_delivery::claim_messages` semantics:
    ///
    /// - selection criterion is `(now - lock_deadline) >= min_idle_ms`;
    ///   non-expired locks are skipped silently
    /// - each claimed delivery has its lock refreshed (idempotent
    ///   `mark_pending` on the same `(queue, message_id, group)` key) and
    ///   its attempt counter bumped — claim counts as a delivery attempt
    /// - if the bump pushes attempts past `read_max_attempts`, the
    ///   delivery is routed through nack-style retirement (DLQ when
    ///   `LifecycleConfig::dlq_target` is set, else dropped) and is NOT
    ///   returned to the caller
    ///
    /// `new_consumer` is accepted for shape-consistency with the legacy
    /// `queue_delivery::claim_messages` signature; the lifecycle layer
    /// does not track per-consumer identity (the pending row is keyed by
    /// consumer group, not consumer), so the parameter is intentionally
    /// not persisted here.
    pub(crate) fn claim(
        &self,
        queue: &str,
        _new_consumer: &str,
        min_idle_ms: u64,
        txn: &QueueTxn,
    ) -> Result<Vec<DeliveryId>> {
        let now = self.clock.now();
        let threshold = Duration::from_millis(min_idle_ms);
        // "Expired beyond `min_idle_ms`" means `now >= deadline + threshold`.
        // Phrasing it as an addition on the deadline (rather than a
        // subtraction at `now`) avoids the `saturating_duration_since`
        // pitfall where a deadline strictly in the future returns a
        // zero gap and a `threshold == 0` would accept it.
        let mut candidates: Vec<PendingDeliveryView> = self
            .store
            .pending_deliveries_for_queue(queue)
            .into_iter()
            .filter(|p| p.deadline + threshold <= now)
            .collect();
        // Stable ordering by deadline so the oldest-stuck-first invariant
        // from `queue_delivery::claim_messages` carries through.
        candidates.sort_by_key(|p| p.deadline);

        let mut claimed = Vec::with_capacity(candidates.len());
        for entry in candidates {
            let bumped = self.store.bump_attempt(txn, &entry.delivery_id)?;
            let max_attempts = self
                .store
                .read_max_attempts(&bumped.queue, bumped.message_id);
            if bumped.attempts > max_attempts {
                match &self.config.dlq_target {
                    Some(target) => {
                        let payload = self
                            .store
                            .read_pending_payload(&entry.delivery_id)
                            .ok_or_else(|| {
                                QueueStoreError::UnknownDelivery(entry.delivery_id.clone())
                            })?;
                        self.retire(txn, &entry.delivery_id)?;
                        self.store.enqueue_dlq(txn, target, payload)?;
                        self.record(RetirementOutcome::MovedToDlq(target.clone()));
                    }
                    None => {
                        self.retire(txn, &entry.delivery_id)?;
                        self.record(RetirementOutcome::Dropped);
                    }
                }
                continue;
            }
            // Refresh the lock on the same (queue, message_id, group) key.
            // `mark_pending` is idempotent on that key — it returns the
            // existing delivery_id and refreshes the deadline.
            let new_deadline = now + self.config.lock_duration;
            self.store
                .mark_pending(txn, &entry.queue, entry.message_id, &entry.group, new_deadline)?;
            claimed.push(entry.delivery_id);
        }
        Ok(claimed)
    }

    /// Atomically remove every pending and available message from
    /// `queue`. Records one tombstone per removed message id via
    /// `txn.record_pending_tombstone(...)`, in the same shape
    /// `ack_pending` uses. Returns the count of message ids purged.
    /// Mirrors `queue_delivery::purge_messages` semantics; failure modes
    /// propagate and no partial purge is observable.
    pub(crate) fn purge(&self, queue: &str, txn: &QueueTxn) -> Result<usize> {
        self.store.purge_queue(txn, queue)
    }

    fn record(&self, outcome: RetirementOutcome) {
        self.outcomes
            .lock()
            .expect("outcomes poisoned")
            .push(outcome);
    }

    #[cfg(test)]
    pub(crate) fn recorded_outcomes(&self) -> Vec<RetirementOutcome> {
        self.outcomes.lock().expect("outcomes poisoned").clone()
    }

    #[cfg(test)]
    pub(crate) fn store_ref(&self) -> &S {
        &self.store
    }
}

#[cfg(test)]
pub(crate) struct TestClock {
    inner: Mutex<Instant>,
}

#[cfg(test)]
impl TestClock {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Instant::now()),
        }
    }

    pub(crate) fn advance(&self, by: Duration) {
        let mut now = self.inner.lock().expect("clock poisoned");
        *now += by;
    }
}

#[cfg(test)]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self.inner.lock().expect("clock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queue::lifecycle::{InMemoryQueueStore, TombstoneRecord};

    fn store_with(messages: &[(MessageId, &str)]) -> InMemoryQueueStore {
        let store = InMemoryQueueStore::new();
        let ids: Vec<MessageId> = messages.iter().map(|(id, _)| *id).collect();
        store.seed_queue("q", ids);
        for (id, payload) in messages {
            store.seed_payload("q", *id, Value::text(*payload));
        }
        store
    }

    fn lifecycle(store: InMemoryQueueStore) -> QueueLifecycle<InMemoryQueueStore> {
        QueueLifecycle::new(store, LifecycleConfig::default())
    }

    #[test]
    fn work_deliver_returns_one_message_to_one_consumer() {
        let lc = lifecycle(store_with(&[(1, "first"), (2, "second")]));
        let deliveries = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver");
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].payload, Value::text("first"));
        assert!(!deliveries[0].delivery_id.is_empty());
    }

    #[test]
    fn work_second_consumer_gets_a_different_message() {
        let lc = lifecycle(store_with(&[(1, "first"), (2, "second")]));
        let a = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver a");
        let b = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver b");
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_ne!(a[0].delivery_id, b[0].delivery_id);
        assert_ne!(a[0].payload, b[0].payload);
        assert_eq!(b[0].payload, Value::text("second"));
    }

    #[test]
    fn work_exhausted_queue_returns_empty() {
        let lc = lifecycle(store_with(&[(1, "only")]));
        let first = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("first");
        assert_eq!(first.len(), 1);
        let empty = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("empty");
        assert!(empty.is_empty(), "exhausted queue should yield no deliveries");
    }

    #[test]
    fn deliver_with_count_zero_is_noop() {
        let lc = lifecycle(store_with(&[(1, "first")]));
        let got = lc.deliver(&QueueTxn::new(),"q", "workers", 0).expect("zero count");
        assert!(got.is_empty());
    }

    #[test]
    fn ack_retires_message_no_longer_redeliverable() {
        let lc = lifecycle(store_with(&[(1, "first")]));
        let delivered = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver");
        let delivery_id = delivered[0].delivery_id.clone();
        lc.ack(&QueueTxn::new(),&delivery_id).expect("ack");

        // Same group should not see it again.
        let again = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("redeliver attempt");
        assert!(again.is_empty(), "acked message must not redeliver");
    }

    #[test]
    fn ack_unknown_delivery_id_errors() {
        let store = InMemoryQueueStore::new();
        let lc = QueueLifecycle::new(store, LifecycleConfig::default());
        let err = lc.ack(&QueueTxn::new(),"does-not-exist").unwrap_err();
        assert!(matches!(err, QueueStoreError::UnknownDelivery(_)));
    }

    #[test]
    fn deliver_count_larger_than_available_returns_all_available() {
        let lc = lifecycle(store_with(&[(1, "a"), (2, "b")]));
        let got = lc.deliver(&QueueTxn::new(),"q", "workers", 10).expect("deliver");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].payload, Value::text("a"));
        assert_eq!(got[1].payload, Value::text("b"));
        assert_ne!(got[0].delivery_id, got[1].delivery_id);
    }

    #[test]
    fn deliver_on_unknown_queue_returns_empty() {
        let store = InMemoryQueueStore::new();
        let lc = QueueLifecycle::new(store, LifecycleConfig::default());
        let got = lc.deliver(&QueueTxn::new(),"missing", "workers", 5).expect("deliver");
        assert!(got.is_empty());
    }

    /// Build a [`LifecycleConfig`] for the WORK-mode nack tests and seed
    /// the per-message retry budget on the store under
    /// `(queue="q", message_id=1)` — every WORK test in this module
    /// uses message id `1` as the sole seeded message. Per-message
    /// `max_attempts` is no longer a config field; the lifecycle reads
    /// it via `QueueStore::read_max_attempts` at nack time.
    fn config_with(
        store: &InMemoryQueueStore,
        max_attempts: u32,
        dlq: Option<&str>,
    ) -> LifecycleConfig {
        store.seed_max_attempts("q", 1, max_attempts);
        LifecycleConfig {
            dlq_target: dlq.map(|s| s.to_string()),
            ..LifecycleConfig::default()
        }
    }

    #[test]
    fn nack_below_max_requeues_same_message() {
        let store = store_with(&[(1, "payload")]);
        let cfg = config_with(&store, 3, None);
        let lc = QueueLifecycle::new(store, cfg);

        let first = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver-1");
        assert_eq!(first[0].payload, Value::text("payload"));
        lc.nack(&QueueTxn::new(),&first[0].delivery_id).expect("nack-1");

        let second = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver-2");
        assert_eq!(second.len(), 1, "requeued message must redeliver");
        assert_eq!(second[0].payload, Value::text("payload"));
        assert_ne!(second[0].delivery_id, first[0].delivery_id);

        assert_eq!(lc.recorded_outcomes(), vec![RetirementOutcome::Requeued]);
    }

    #[test]
    fn three_nacks_at_max_three_yield_two_requeues_then_retire() {
        let store = store_with(&[(1, "payload")]);
        let cfg = config_with(&store, 3, None);
        let lc = QueueLifecycle::new(store, cfg);

        for _ in 0..2 {
            let d = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver");
            lc.nack(&QueueTxn::new(),&d[0].delivery_id).expect("nack");
        }
        let third = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver-3");
        lc.nack(&QueueTxn::new(),&third[0].delivery_id).expect("nack-3");

        assert_eq!(
            lc.recorded_outcomes(),
            vec![
                RetirementOutcome::Requeued,
                RetirementOutcome::Requeued,
                RetirementOutcome::Dropped,
            ]
        );
        assert!(lc.deliver(&QueueTxn::new(),"q", "workers", 1).unwrap().is_empty());
    }

    #[test]
    fn nack_at_max_with_dlq_promotes_to_dlq_target() {
        let store = store_with(&[(1, "orders/42")]);
        let cfg = config_with(&store, 2, Some("orders.dlq"));
        let lc = QueueLifecycle::new(store, cfg);

        // First nack → Requeued
        let a = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver-a");
        lc.nack(&QueueTxn::new(),&a[0].delivery_id).expect("nack-a");
        // Second nack → MovedToDlq
        let b = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver-b");
        lc.nack(&QueueTxn::new(),&b[0].delivery_id).expect("nack-b");

        assert_eq!(
            lc.recorded_outcomes(),
            vec![
                RetirementOutcome::Requeued,
                RetirementOutcome::MovedToDlq("orders.dlq".to_string()),
            ]
        );

        let dlq = lc.store.dlq_snapshot();
        assert_eq!(dlq.len(), 1, "exactly one DLQ enqueue");
        assert_eq!(dlq[0].target, "orders.dlq");
        assert_eq!(dlq[0].original, Value::text("orders/42"));

        // Original is retired — not redeliverable on the source queue.
        assert!(lc.deliver(&QueueTxn::new(),"q", "workers", 1).unwrap().is_empty());
    }

    #[test]
    fn nack_at_max_without_dlq_drops_silently() {
        let store = store_with(&[(1, "ephemeral")]);
        let cfg = config_with(&store, 1, None);
        let lc = QueueLifecycle::new(store, cfg);

        let d = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver");
        lc.nack(&QueueTxn::new(),&d[0].delivery_id).expect("nack");

        assert_eq!(lc.recorded_outcomes(), vec![RetirementOutcome::Dropped]);
        assert!(lc.store.dlq_snapshot().is_empty(), "no DLQ enqueue when target unset");
        assert!(lc.deliver(&QueueTxn::new(),"q", "workers", 1).unwrap().is_empty());
    }

    #[test]
    fn nack_unknown_delivery_id_errors() {
        let store = InMemoryQueueStore::new();
        let cfg = config_with(&store, 3, None);
        let lc = QueueLifecycle::new(store, cfg);
        let err = lc.nack(&QueueTxn::new(),"nope").unwrap_err();
        assert!(matches!(err, QueueStoreError::UnknownDelivery(_)));
        assert!(lc.recorded_outcomes().is_empty());
    }

    fn fanout_config() -> LifecycleConfig {
        LifecycleConfig {
            mode: QueueMode::Fanout,
            ..LifecycleConfig::default()
        }
    }

    fn fanout_config_with(
        store: &InMemoryQueueStore,
        max_attempts: u32,
        dlq: Option<&str>,
    ) -> LifecycleConfig {
        store.seed_max_attempts("q", 1, max_attempts);
        LifecycleConfig {
            mode: QueueMode::Fanout,
            dlq_target: dlq.map(|s| s.to_string()),
            ..LifecycleConfig::default()
        }
    }

    #[test]
    fn fanout_two_groups_both_receive_same_message() {
        let lc = QueueLifecycle::new(store_with(&[(1, "shared")]), fanout_config());

        let a = lc.deliver(&QueueTxn::new(),"q", "subs.a", 1).expect("deliver a");
        let b = lc.deliver(&QueueTxn::new(),"q", "subs.b", 1).expect("deliver b");

        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].payload, Value::text("shared"));
        assert_eq!(b[0].payload, Value::text("shared"));
        assert_ne!(a[0].delivery_id, b[0].delivery_id);
    }

    #[test]
    fn fanout_ack_by_one_group_leaves_other_pending_intact() {
        let lc = QueueLifecycle::new(store_with(&[(1, "shared")]), fanout_config());

        let a = lc.deliver(&QueueTxn::new(),"q", "subs.a", 1).expect("deliver a");
        let b = lc.deliver(&QueueTxn::new(),"q", "subs.b", 1).expect("deliver b");

        lc.ack(&QueueTxn::new(),&a[0].delivery_id).expect("ack a");

        // A must not see the message again.
        assert!(lc.deliver(&QueueTxn::new(),"q", "subs.a", 1).unwrap().is_empty());

        // B's delivery is still ackable — pending row untouched.
        lc.ack(&QueueTxn::new(),&b[0].delivery_id).expect("ack b still valid");
    }

    #[test]
    fn fanout_nack_by_one_group_does_not_touch_other() {
        // Two groups deliver; group A nacks (below max) and requeues
        // *for A only*. Group B's attempt counter must stay at 0 and
        // its pending row must remain intact.
        let store = store_with(&[(1, "shared")]);
        let cfg = fanout_config_with(&store, 3, None);
        let lc = QueueLifecycle::new(store, cfg);

        let a = lc.deliver(&QueueTxn::new(),"q", "subs.a", 1).expect("deliver a");
        let b = lc.deliver(&QueueTxn::new(),"q", "subs.b", 1).expect("deliver b");
        let b_delivery = b[0].delivery_id.clone();

        lc.nack(&QueueTxn::new(),&a[0].delivery_id).expect("nack a");
        assert_eq!(lc.recorded_outcomes(), vec![RetirementOutcome::Requeued]);

        // A redelivery on group A should hand back the same message,
        // because the message wasn't removed and A's pending was released.
        let a2 = lc.deliver(&QueueTxn::new(),"q", "subs.a", 1).expect("a redeliver");
        assert_eq!(a2.len(), 1);
        assert_eq!(a2[0].payload, Value::text("shared"));

        // B's original delivery is still valid and acks cleanly — its
        // attempt counter never moved, its pending row never released.
        lc.ack(&QueueTxn::new(),&b_delivery).expect("ack b's original delivery_id");
    }

    #[test]
    fn fanout_terminal_nack_with_dlq_only_retires_caller_group() {
        // max=1 + DLQ target. Group A nacks → MovedToDlq. Group B has
        // not yet delivered: it must still be able to deliver the
        // original message (FANOUT keeps the payload for other groups).
        let store = store_with(&[(1, "orders/42")]);
        let cfg = fanout_config_with(&store, 1, Some("orders.dlq"));
        let lc = QueueLifecycle::new(store, cfg);

        let a = lc.deliver(&QueueTxn::new(),"q", "subs.a", 1).expect("deliver a");
        lc.nack(&QueueTxn::new(),&a[0].delivery_id).expect("nack a");

        assert_eq!(
            lc.recorded_outcomes(),
            vec![RetirementOutcome::MovedToDlq("orders.dlq".to_string())]
        );
        let dlq = lc.store.dlq_snapshot();
        assert_eq!(dlq.len(), 1);
        assert_eq!(dlq[0].original, Value::text("orders/42"));

        // A is done; B has never delivered — message must still be available to B.
        assert!(lc.deliver(&QueueTxn::new(),"q", "subs.a", 1).unwrap().is_empty());
        let b = lc.deliver(&QueueTxn::new(),"q", "subs.b", 1).expect("deliver b");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].payload, Value::text("orders/42"));
    }

    #[test]
    fn fanout_terminal_nack_no_dlq_drops_for_caller_group_only() {
        let store = store_with(&[(1, "p")]);
        let cfg = fanout_config_with(&store, 1, None);
        let lc = QueueLifecycle::new(store, cfg);

        let a = lc.deliver(&QueueTxn::new(),"q", "subs.a", 1).expect("deliver a");
        lc.nack(&QueueTxn::new(),&a[0].delivery_id).expect("nack a");

        assert_eq!(lc.recorded_outcomes(), vec![RetirementOutcome::Dropped]);
        assert!(lc.store.dlq_snapshot().is_empty());
        assert!(lc.deliver(&QueueTxn::new(),"q", "subs.a", 1).unwrap().is_empty());

        // B still sees it.
        let b = lc.deliver(&QueueTxn::new(),"q", "subs.b", 1).expect("deliver b");
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].payload, Value::text("p"));
    }

    #[test]
    fn fanout_does_not_share_pending_across_groups_with_work_semantics() {
        // Regression for the WORK code path leaking into FANOUT: in WORK
        // a single deliver call blocks the message from a second group;
        // in FANOUT it must not.
        let lc = QueueLifecycle::new(store_with(&[(1, "x"), (2, "y")]), fanout_config());

        let a = lc.deliver(&QueueTxn::new(),"q", "subs.a", 2).expect("deliver a");
        let b = lc.deliver(&QueueTxn::new(),"q", "subs.b", 2).expect("deliver b");
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 2);
        let a_payloads: Vec<_> = a.iter().map(|d| d.payload.clone()).collect();
        let b_payloads: Vec<_> = b.iter().map(|d| d.payload.clone()).collect();
        assert_eq!(a_payloads, b_payloads);
    }

    #[test]
    fn nack_requeue_preserves_attempt_count_across_new_delivery_id() {
        // With max=3, a NACK→requeue→NACK→requeue→NACK should retire,
        // even though the second and third nacks operate on fresh
        // delivery_ids (the redelivery path).
        let store = store_with(&[(1, "p")]);
        let cfg = config_with(&store, 3, Some("dlq"));
        let lc = QueueLifecycle::new(store, cfg);

        let mut ids = Vec::new();
        for _ in 0..3 {
            let d = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver");
            assert_eq!(d.len(), 1, "should always redeliver until retired");
            ids.push(d[0].delivery_id.clone());
            lc.nack(&QueueTxn::new(),&d[0].delivery_id).expect("nack");
        }

        // All three delivery_ids must differ — each nack-then-deliver
        // forges a new handle, but attempts persist via (queue,msg,group).
        assert_ne!(ids[0], ids[1]);
        assert_ne!(ids[1], ids[2]);

        assert_eq!(
            lc.recorded_outcomes(),
            vec![
                RetirementOutcome::Requeued,
                RetirementOutcome::Requeued,
                RetirementOutcome::MovedToDlq("dlq".to_string()),
            ]
        );
        assert_eq!(lc.store.dlq_snapshot().len(), 1);
    }

    fn config_with_lock(lock: Duration) -> LifecycleConfig {
        LifecycleConfig {
            lock_duration: lock,
            ..LifecycleConfig::default()
        }
    }

    #[test]
    fn expired_pending_is_reclaimed_lazily_on_next_deliver() {
        // Acceptance: deliver, advance test clock past deadline,
        // deliver again — the same message must come back.
        let clock = Arc::new(TestClock::new());
        let lc = QueueLifecycle::with_clock(
            store_with(&[(1, "only")]),
            config_with_lock(Duration::from_millis(100)),
            clock.clone() as Arc<dyn Clock>,
        );

        let first = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("first");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].payload, Value::text("only"));

        // Before deadline: no redelivery (slot held).
        clock.advance(Duration::from_millis(50));
        let blocked = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("still locked");
        assert!(blocked.is_empty(), "lock still held — must not redeliver");

        // Cross the deadline: next deliver reclaims and hands the
        // message back. A fresh delivery_id is forged for the new lease.
        clock.advance(Duration::from_millis(60));
        let second = lc.deliver(&QueueTxn::new(),"q", "workers", 1).expect("after expiry");
        assert_eq!(second.len(), 1, "expired lock must release for redelivery");
        assert_eq!(second[0].payload, Value::text("only"));
        assert_ne!(second[0].delivery_id, first[0].delivery_id);
    }

    #[test]
    fn partial_advance_then_recreate_module_keeps_lock_held() {
        // Acceptance: deliver, advance clock partially (still within
        // deadline), recreate Module against same store — message
        // stays locked. Models a primary restart: in-flight pending
        // rows are not flushed; the original deadline is honored.
        let clock = Arc::new(TestClock::new());
        let store = store_with(&[(1, "in-flight")]);

        let lc1 = QueueLifecycle::with_clock(
            store.clone(),
            config_with_lock(Duration::from_secs(30)),
            clock.clone() as Arc<dyn Clock>,
        );
        let first = lc1.deliver(&QueueTxn::new(),"q", "workers", 1).expect("deliver");
        assert_eq!(first.len(), 1);
        let original_delivery_id = first[0].delivery_id.clone();

        // Partial advance — still well within the 30s deadline.
        clock.advance(Duration::from_secs(5));

        // "Restart": new Module instance pointing at the same store
        // (and same clock — the lock deadline is anchored in wall time,
        // not in process identity).
        let lc2 = QueueLifecycle::with_clock(
            store.clone(),
            config_with_lock(Duration::from_secs(30)),
            clock.clone() as Arc<dyn Clock>,
        );
        let again = lc2.deliver(&QueueTxn::new(),"q", "workers", 1).expect("post-restart deliver");
        assert!(
            again.is_empty(),
            "pending row must survive Module recreation while deadline holds"
        );

        // The original delivery handle remains ackable through the new
        // Module — confirming the pending row was not silently dropped.
        lc2.ack(&QueueTxn::new(),&original_delivery_id).expect("original delivery still ackable");
    }

    // Issue #602 — peek + read (non-mutating inspection).

    #[test]
    fn peek_returns_available_slice_without_mutating_state() {
        let store = store_with(&[(1, "a"), (2, "b"), (3, "c")]);
        let lc = lifecycle(store);
        let t = QueueTxn::new();

        let view = lc.peek("q", 2, &t);
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].message_id, 1);
        assert_eq!(view[0].payload, Value::text("a"));
        assert_eq!(view[1].message_id, 2);
        assert_eq!(view[1].payload, Value::text("b"));

        // peek must not mark anything pending: the same deliver should
        // still hand back message 1 from the front.
        let delivered = lc.deliver(&QueueTxn::new(), "q", "workers", 1).expect("deliver");
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].payload, Value::text("a"));

        // peek must not have recorded a tombstone either.
        assert!(t.recorded_tombstones().is_empty(), "peek must not tombstone");
    }

    #[test]
    fn peek_count_zero_returns_empty() {
        let lc = lifecycle(store_with(&[(1, "a")]));
        let got = lc.peek("q", 0, &QueueTxn::new());
        assert!(got.is_empty());
    }

    #[test]
    fn peek_unknown_queue_returns_empty() {
        let store = InMemoryQueueStore::new();
        let lc = QueueLifecycle::new(store, LifecycleConfig::default());
        let got = lc.peek("missing", 5, &QueueTxn::new());
        assert!(got.is_empty());
    }

    #[test]
    fn peek_count_larger_than_available_returns_all() {
        let lc = lifecycle(store_with(&[(1, "a"), (2, "b")]));
        let view = lc.peek("q", 10, &QueueTxn::new());
        assert_eq!(view.len(), 2);
    }

    #[test]
    fn peek_skips_pending_messages() {
        let lc = lifecycle(store_with(&[(1, "a"), (2, "b"), (3, "c")]));
        // Reserve message 1 — peek must skip it.
        let _ = lc.deliver(&QueueTxn::new(), "q", "workers", 1).expect("deliver");
        let view = lc.peek("q", 5, &QueueTxn::new());
        let ids: Vec<_> = view.iter().map(|v| v.message_id).collect();
        assert_eq!(ids, vec![2, 3]);
    }

    #[test]
    fn peek_carries_per_message_max_attempts() {
        let store = store_with(&[(1, "a"), (2, "b")]);
        store.seed_max_attempts("q", 1, 7);
        // 2 unseeded — falls back to crate default (3).
        let lc = lifecycle(store);
        let view = lc.peek("q", 2, &QueueTxn::new());
        assert_eq!(view[0].message_id, 1);
        assert_eq!(view[0].max_attempts, 7);
        assert_eq!(view[1].message_id, 2);
        assert_eq!(view[1].max_attempts, 3);
    }

    #[test]
    fn peek_works_on_fanout_queue_and_does_not_mutate() {
        // FANOUT acceptance: peek behaves like WORK — the store-level
        // `available_messages` filter (pending across any group) is the
        // same surface both modes use. Acked-per-group state is not
        // peek's concern (peek mirrors `queue_delivery::peek_messages`
        // which is mode-agnostic).
        let store = store_with(&[(1, "x"), (2, "y")]);
        let lc = QueueLifecycle::new(store, fanout_config());

        let view = lc.peek("q", 5, &QueueTxn::new());
        assert_eq!(view.len(), 2);
        assert_eq!(view[0].payload, Value::text("x"));
        assert_eq!(view[1].payload, Value::text("y"));

        // Two FANOUT groups should still both see both messages — peek
        // didn't touch any pending row.
        let a = lc.deliver(&QueueTxn::new(), "q", "subs.a", 2).expect("deliver a");
        let b = lc.deliver(&QueueTxn::new(), "q", "subs.b", 2).expect("deliver b");
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn read_returns_payload_for_pending_delivery_without_locking() {
        let lc = lifecycle(store_with(&[(1, "payload-1")]));
        let t = QueueTxn::new();
        let delivered = lc.deliver(&t, "q", "workers", 1).expect("deliver");
        let id = delivered[0].delivery_id.clone();
        let deadline_before = lc.store_ref().read_lock_deadline(&id);

        let got = lc.read(&id, &t);
        assert_eq!(got, Some(Value::text("payload-1")));

        // read must not touch the lock deadline — same instant before and after.
        assert_eq!(lc.store_ref().read_lock_deadline(&id), deadline_before);
        // read must not record a tombstone.
        assert!(t.recorded_tombstones().is_empty(), "read must not tombstone");
        // The delivery handle stays valid and ackable.
        lc.ack(&QueueTxn::new(), &id).expect("delivery still ackable after read");
    }

    #[test]
    fn read_returns_none_for_unknown_delivery() {
        let lc = lifecycle(store_with(&[(1, "p")]));
        assert!(lc.read("does-not-exist", &QueueTxn::new()).is_none());
    }

    // Issue #604 — purge (atomic remove-all, txn-context tombstones).

    #[test]
    fn purge_on_empty_queue_returns_zero_and_records_no_tombstones() {
        let store = InMemoryQueueStore::new();
        let lc = QueueLifecycle::new(store, LifecycleConfig::default());
        let t = QueueTxn::new();
        let n = lc.purge("missing", &t).expect("purge unknown");
        assert_eq!(n, 0);
        assert!(t.recorded_tombstones().is_empty());
    }

    #[test]
    fn purge_on_seeded_queue_with_no_pending_removes_all_and_returns_count() {
        let lc = lifecycle(store_with(&[(1, "a"), (2, "b"), (3, "c")]));
        let t = QueueTxn::new();

        let n = lc.purge("q", &t).expect("purge");
        assert_eq!(n, 3);

        // Available pool is empty afterwards.
        assert!(
            lc.store_ref().available_messages("q", QueueSide::Left).is_empty(),
            "no available messages after purge",
        );
        // Payloads are gone too — read_message returns None.
        for id in [1u64, 2, 3] {
            assert!(
                lc.store_ref().read_message("q", id).is_none(),
                "message {id} payload should be purged",
            );
        }

        // One tombstone per removed message id, ordered by message id.
        assert_eq!(
            t.recorded_tombstones(),
            vec![
                TombstoneRecord { queue: "q".to_string(), message_id: 1 },
                TombstoneRecord { queue: "q".to_string(), message_id: 2 },
                TombstoneRecord { queue: "q".to_string(), message_id: 3 },
            ],
        );
    }

    #[test]
    fn purge_removes_pending_rows_and_records_tombstone_per_message() {
        // Two messages, one of them currently pending under a WORK
        // delivery — purge must clean up the pending row alongside the
        // available row, and tombstone both.
        let lc = lifecycle(store_with(&[(1, "first"), (2, "second")]));
        let delivered = lc.deliver(&QueueTxn::new(), "q", "workers", 1).expect("deliver");
        assert_eq!(delivered.len(), 1);
        let pending_id = delivered[0].delivery_id.clone();

        let t = QueueTxn::new();
        let n = lc.purge("q", &t).expect("purge");
        assert_eq!(n, 2);

        // No pending row survives (lock deadline lookup returns None).
        assert!(
            lc.store_ref().read_lock_deadline(&pending_id).is_none(),
            "pending row should be gone after purge",
        );
        // Re-purge is idempotent — empty queue + no new tombstones.
        let t2 = QueueTxn::new();
        assert_eq!(lc.purge("q", &t2).expect("re-purge"), 0);
        assert!(t2.recorded_tombstones().is_empty());

        assert_eq!(
            t.recorded_tombstones(),
            vec![
                TombstoneRecord { queue: "q".to_string(), message_id: 1 },
                TombstoneRecord { queue: "q".to_string(), message_id: 2 },
            ],
        );
    }

    #[test]
    fn purge_on_fanout_queue_tombstones_each_message_once() {
        // FANOUT acceptance: two groups each holding a pending row for
        // the same message must produce exactly one tombstone per
        // message id after purge, not one per pending row.
        let lc = QueueLifecycle::new(store_with(&[(1, "shared"), (2, "other")]), fanout_config());
        let _a = lc.deliver(&QueueTxn::new(), "q", "subs.a", 2).expect("deliver a");
        let _b = lc.deliver(&QueueTxn::new(), "q", "subs.b", 2).expect("deliver b");

        let t = QueueTxn::new();
        let n = lc.purge("q", &t).expect("purge");
        assert_eq!(n, 2, "two unique message ids — not four pending rows");
        assert_eq!(
            t.recorded_tombstones(),
            vec![
                TombstoneRecord { queue: "q".to_string(), message_id: 1 },
                TombstoneRecord { queue: "q".to_string(), message_id: 2 },
            ],
        );

        // Neither group can deliver anything afterwards.
        assert!(lc.deliver(&QueueTxn::new(), "q", "subs.a", 5).unwrap().is_empty());
        assert!(lc.deliver(&QueueTxn::new(), "q", "subs.b", 5).unwrap().is_empty());
    }

    // Issue #603 — claim (steal-after-min-idle, bumps attempt).

    fn lifecycle_with_clock(
        store: InMemoryQueueStore,
        cfg: LifecycleConfig,
        clock: Arc<TestClock>,
    ) -> QueueLifecycle<InMemoryQueueStore> {
        QueueLifecycle::with_clock(store, cfg, clock as Arc<dyn Clock>)
    }

    #[test]
    fn claim_on_queue_with_no_pending_returns_empty() {
        let lc = lifecycle(store_with(&[(1, "a")]));
        let claimed = lc.claim("q", "consumer-x", 0, &QueueTxn::new()).expect("claim");
        assert!(claimed.is_empty());
        // Unknown queue is also empty.
        let claimed = lc.claim("missing", "c", 0, &QueueTxn::new()).expect("claim");
        assert!(claimed.is_empty());
    }

    #[test]
    fn claim_on_idle_but_not_expired_returns_empty() {
        // Deliver a message, advance the clock slightly (well within the
        // lock_duration), and request `min_idle_ms=0`. Even with a zero
        // threshold the lock is *not yet* expired (deadline is in the
        // future), so no candidate is selected.
        let clock = Arc::new(TestClock::new());
        let store = store_with(&[(1, "payload")]);
        let lc = lifecycle_with_clock(
            store,
            LifecycleConfig {
                lock_duration: Duration::from_secs(30),
                ..LifecycleConfig::default()
            },
            clock.clone(),
        );

        let d = lc.deliver(&QueueTxn::new(), "q", "workers", 1).expect("deliver");
        let original_deadline = lc.store_ref().read_lock_deadline(&d[0].delivery_id);

        clock.advance(Duration::from_secs(5));
        let claimed = lc.claim("q", "consumer-x", 0, &QueueTxn::new()).expect("claim");
        assert!(claimed.is_empty(), "lock still held; nothing to claim");

        // Deadline unchanged, no tombstone written.
        assert_eq!(lc.store_ref().read_lock_deadline(&d[0].delivery_id), original_deadline);
    }

    #[test]
    fn claim_on_expired_delivery_reassigns_and_bumps_attempt() {
        let clock = Arc::new(TestClock::new());
        let store = store_with(&[(1, "payload")]);
        store.seed_max_attempts("q", 1, 10);
        let lc = lifecycle_with_clock(
            store,
            LifecycleConfig {
                lock_duration: Duration::from_millis(100),
                ..LifecycleConfig::default()
            },
            clock.clone(),
        );

        let d = lc.deliver(&QueueTxn::new(), "q", "workers", 1).expect("deliver");
        let id = d[0].delivery_id.clone();
        let pre_deadline = lc.store_ref().read_lock_deadline(&id).expect("pre-deadline");

        // Cross the lock deadline plus the requested idle threshold.
        clock.advance(Duration::from_millis(500));
        let t = QueueTxn::new();
        let claimed = lc.claim("q", "consumer-x", 100, &t).expect("claim");
        assert_eq!(claimed, vec![id.clone()], "expired delivery must be claimed");

        // Deadline refreshed forward.
        let post_deadline = lc.store_ref().read_lock_deadline(&id).expect("post-deadline");
        assert!(post_deadline > pre_deadline, "deadline must move forward on claim");

        // Attempt counter bumped to 1 — observable by tripping
        // `read_max_attempts` later. Easiest check: another claim with
        // max_attempts=10 still succeeds; with max_attempts=1 (re-seed
        // and re-expire) the second claim would route through retirement
        // (covered in the next test).

        // claim does not write a tombstone on the happy path.
        assert!(t.recorded_tombstones().is_empty(), "happy-path claim must not tombstone");
    }

    #[test]
    fn claim_that_exhausts_attempts_retires_delivery() {
        // max_attempts=1: the first claim's bump_attempt pushes attempts
        // to 1, which is NOT > max — so the claim succeeds. The second
        // claim's bump pushes attempts to 2, which IS > max — so it
        // routes through nack-style retirement (drop, since no DLQ).
        let clock = Arc::new(TestClock::new());
        let store = store_with(&[(1, "payload")]);
        store.seed_max_attempts("q", 1, 1);
        let lc = lifecycle_with_clock(
            store,
            LifecycleConfig {
                lock_duration: Duration::from_millis(100),
                ..LifecycleConfig::default()
            },
            clock.clone(),
        );

        let d = lc.deliver(&QueueTxn::new(), "q", "workers", 1).expect("deliver");
        let id = d[0].delivery_id.clone();

        // First claim — within budget, reassigns.
        clock.advance(Duration::from_millis(500));
        let c1 = lc.claim("q", "consumer-x", 100, &QueueTxn::new()).expect("claim-1");
        assert_eq!(c1, vec![id.clone()]);

        // Second claim — over budget, retired (dropped).
        clock.advance(Duration::from_millis(500));
        let t = QueueTxn::new();
        let c2 = lc.claim("q", "consumer-y", 100, &t).expect("claim-2");
        assert!(c2.is_empty(), "exhausted delivery must not be returned");

        // WORK-mode retirement tombstones the underlying message.
        assert!(
            lc.store_ref().read_lock_deadline(&id).is_none(),
            "pending row must be gone after retirement",
        );
        assert_eq!(
            lc.recorded_outcomes(),
            vec![RetirementOutcome::Dropped],
        );
        // ack_pending records one tombstone per retired message.
        assert_eq!(t.recorded_tombstones().len(), 1);
    }

    #[test]
    fn claim_that_exhausts_attempts_with_dlq_target_promotes_to_dlq() {
        let clock = Arc::new(TestClock::new());
        let store = store_with(&[(1, "payload")]);
        store.seed_max_attempts("q", 1, 1);
        let lc = lifecycle_with_clock(
            store,
            LifecycleConfig {
                lock_duration: Duration::from_millis(100),
                dlq_target: Some("q.dlq".to_string()),
                ..LifecycleConfig::default()
            },
            clock.clone(),
        );

        let d = lc.deliver(&QueueTxn::new(), "q", "workers", 1).expect("deliver");
        let _id = d[0].delivery_id.clone();

        clock.advance(Duration::from_millis(500));
        let _ = lc.claim("q", "consumer-x", 100, &QueueTxn::new()).expect("claim-1");

        clock.advance(Duration::from_millis(500));
        let c2 = lc.claim("q", "consumer-y", 100, &QueueTxn::new()).expect("claim-2");
        assert!(c2.is_empty());

        assert_eq!(
            lc.recorded_outcomes(),
            vec![RetirementOutcome::MovedToDlq("q.dlq".to_string())],
        );
        let dlq = lc.store_ref().dlq_snapshot();
        assert_eq!(dlq.len(), 1);
        assert_eq!(dlq[0].target, "q.dlq");
        assert_eq!(dlq[0].original, Value::text("payload"));
    }

    #[test]
    fn claim_respects_min_idle_threshold_per_delivery() {
        // Two messages delivered at the same instant. After expiry, advance
        // by an amount that satisfies min_idle_ms=0 but not min_idle_ms=200.
        let clock = Arc::new(TestClock::new());
        let store = store_with(&[(1, "a"), (2, "b")]);
        let lc = lifecycle_with_clock(
            store,
            LifecycleConfig {
                lock_duration: Duration::from_millis(100),
                ..LifecycleConfig::default()
            },
            clock.clone(),
        );

        let d = lc.deliver(&QueueTxn::new(), "q", "workers", 2).expect("deliver");
        assert_eq!(d.len(), 2);

        // Cross the deadline by 50ms — locks are expired, but not by
        // 200ms beyond expiry.
        clock.advance(Duration::from_millis(150));
        let none = lc.claim("q", "consumer-x", 200, &QueueTxn::new()).expect("strict");
        assert!(none.is_empty(), "min_idle 200ms not yet satisfied past deadline");

        // Loosen the threshold — both deliveries claim.
        let both = lc.claim("q", "consumer-x", 0, &QueueTxn::new()).expect("loose");
        assert_eq!(both.len(), 2);
    }

    #[test]
    fn read_returns_none_after_ack() {
        let lc = lifecycle(store_with(&[(1, "p")]));
        let t = QueueTxn::new();
        let delivered = lc.deliver(&t, "q", "workers", 1).expect("deliver");
        let id = delivered[0].delivery_id.clone();
        lc.ack(&t, &id).expect("ack");
        assert!(
            lc.read(&id, &QueueTxn::new()).is_none(),
            "retired delivery must not be readable",
        );
    }
}
