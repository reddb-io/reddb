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

use std::time::{Duration, Instant};

use crate::storage::queue::lifecycle::{
    DeliveryId, MessageId, QueueSide, QueueStore, QueueStoreError, Result,
};
use crate::storage::schema::Value;

/// Lock duration + future retry policy knobs. Constructor takes this so
/// the catalog wiring slice can swap the source without touching the
/// Module surface.
#[derive(Debug, Clone)]
pub(crate) struct LifecycleConfig {
    pub(crate) lock_duration: Duration,
}

impl Default for LifecycleConfig {
    fn default() -> Self {
        Self {
            lock_duration: Duration::from_secs(30),
        }
    }
}

/// What `deliver` returns to the caller — opaque handle plus payload.
#[derive(Debug, Clone)]
pub(crate) struct Delivery {
    pub(crate) delivery_id: DeliveryId,
    pub(crate) payload: Value,
}

/// WORK-mode queue lifecycle.
pub(crate) struct QueueLifecycle<S: QueueStore> {
    store: S,
    config: LifecycleConfig,
}

impl<S: QueueStore> QueueLifecycle<S> {
    pub(crate) fn new(store: S, config: LifecycleConfig) -> Self {
        Self { store, config }
    }

    /// Reserve up to `count` available messages from `queue` for `group`
    /// and return their opaque delivery handles + payloads. WORK
    /// semantics: each message is reserved for exactly one consumer
    /// (subsequent `deliver` calls skip messages already pending).
    pub(crate) fn deliver(
        &self,
        queue: &str,
        group: &str,
        count: usize,
    ) -> Result<Vec<Delivery>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let available = self.store.available_messages(queue, QueueSide::Left);
        let mut out = Vec::with_capacity(count.min(available.len()));
        for message_id in available.into_iter().take(count) {
            let deadline = Instant::now() + self.config.lock_duration;
            let delivery_id = self.store.mark_pending(queue, message_id, group, deadline)?;
            let payload = self
                .store
                .read_message(queue, message_id)
                .ok_or_else(|| QueueStoreError::UnknownQueue(queue.to_string()))?;
            out.push(Delivery {
                delivery_id,
                payload,
            });
        }
        Ok(out)
    }

    /// Retire `delivery_id` — the message is consumed and will not be
    /// redelivered. Unknown delivery ids surface `UnknownDelivery`.
    pub(crate) fn ack(&self, delivery_id: &str) -> Result<()> {
        self.store.ack_pending(delivery_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::queue::lifecycle::InMemoryQueueStore;

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
        let deliveries = lc.deliver("q", "workers", 1).expect("deliver");
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].payload, Value::text("first"));
        assert!(!deliveries[0].delivery_id.is_empty());
    }

    #[test]
    fn work_second_consumer_gets_a_different_message() {
        let lc = lifecycle(store_with(&[(1, "first"), (2, "second")]));
        let a = lc.deliver("q", "workers", 1).expect("deliver a");
        let b = lc.deliver("q", "workers", 1).expect("deliver b");
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_ne!(a[0].delivery_id, b[0].delivery_id);
        assert_ne!(a[0].payload, b[0].payload);
        assert_eq!(b[0].payload, Value::text("second"));
    }

    #[test]
    fn work_exhausted_queue_returns_empty() {
        let lc = lifecycle(store_with(&[(1, "only")]));
        let first = lc.deliver("q", "workers", 1).expect("first");
        assert_eq!(first.len(), 1);
        let empty = lc.deliver("q", "workers", 1).expect("empty");
        assert!(empty.is_empty(), "exhausted queue should yield no deliveries");
    }

    #[test]
    fn deliver_with_count_zero_is_noop() {
        let lc = lifecycle(store_with(&[(1, "first")]));
        let got = lc.deliver("q", "workers", 0).expect("zero count");
        assert!(got.is_empty());
    }

    #[test]
    fn ack_retires_message_no_longer_redeliverable() {
        let lc = lifecycle(store_with(&[(1, "first")]));
        let delivered = lc.deliver("q", "workers", 1).expect("deliver");
        let delivery_id = delivered[0].delivery_id.clone();
        lc.ack(&delivery_id).expect("ack");

        // Same group should not see it again.
        let again = lc.deliver("q", "workers", 1).expect("redeliver attempt");
        assert!(again.is_empty(), "acked message must not redeliver");
    }

    #[test]
    fn ack_unknown_delivery_id_errors() {
        let store = InMemoryQueueStore::new();
        let lc = QueueLifecycle::new(store, LifecycleConfig::default());
        let err = lc.ack("does-not-exist").unwrap_err();
        assert!(matches!(err, QueueStoreError::UnknownDelivery(_)));
    }

    #[test]
    fn deliver_count_larger_than_available_returns_all_available() {
        let lc = lifecycle(store_with(&[(1, "a"), (2, "b")]));
        let got = lc.deliver("q", "workers", 10).expect("deliver");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].payload, Value::text("a"));
        assert_eq!(got[1].payload, Value::text("b"));
        assert_ne!(got[0].delivery_id, got[1].delivery_id);
    }

    #[test]
    fn deliver_on_unknown_queue_returns_empty() {
        let store = InMemoryQueueStore::new();
        let lc = QueueLifecycle::new(store, LifecycleConfig::default());
        let got = lc.deliver("missing", "workers", 5).expect("deliver");
        assert!(got.is_empty());
    }
}
