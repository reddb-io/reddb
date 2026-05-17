//! Queue lifecycle trait + in-memory fake.
//!
//! Narrow `QueueStore` interface the future `QueueLifecycle` Module will
//! depend on. The fake (`InMemoryQueueStore`) is reused across
//! `QueueLifecycle` unit tests so transitions can be exercised without
//! booting the engine. This is tracer-bullet scope (PRD #527, issue #528):
//! the trait compiles, the fake passes its own contract tests, and no
//! production code consumes it yet.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use crate::storage::schema::Value;

/// Opaque queue identifier (user-facing queue name).
pub(crate) type QueueId = String;

/// Monotonic message sequence inside a queue — matches `QueueMessage::seq`.
pub(crate) type MessageId = u64;

/// Consumer group name.
pub(crate) type ConsumerGroupId = String;

/// Server-issued opaque delivery handle (base32, no embedded structure).
pub(crate) type DeliveryId = String;

/// DLQ routing target — for now just the destination queue name.
pub(crate) type DlqTarget = String;

/// Which end of a queue to scan.
pub(crate) use super::deque::QueueSide;

/// Errors surfaced by `QueueStore`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueueStoreError {
    UnknownDelivery(DeliveryId),
    UnknownQueue(QueueId),
}

impl std::fmt::Display for QueueStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownDelivery(id) => write!(f, "unknown delivery {id}"),
            Self::UnknownQueue(q) => write!(f, "unknown queue {q}"),
        }
    }
}

impl std::error::Error for QueueStoreError {}

pub(crate) type Result<T> = std::result::Result<T, QueueStoreError>;

/// Narrow storage surface the `QueueLifecycle` Module depends on.
///
/// Methods are intentionally minimal — the lifecycle owns transition
/// policy; the store owns persistence semantics.
pub(crate) trait QueueStore {
    /// Available (not yet pending) message ids on `queue`, scanning from `side`.
    fn available_messages(&self, queue: &str, side: QueueSide) -> Vec<MessageId>;

    /// Reserve `message_id` for `group` with a pending deadline. Idempotent
    /// on the `(queue, message_id, group)` key — repeated calls with the
    /// same key return the same `DeliveryId` and refresh the deadline.
    fn mark_pending(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
        deadline: Instant,
    ) -> Result<DeliveryId>;

    /// Release a pending delivery back to the available pool. No-op if
    /// `delivery_id` is unknown (already released or never existed).
    fn release_pending(&self, delivery_id: &str) -> Result<()>;

    /// Permanently retire a pending delivery — removes the pending entry
    /// AND the underlying message from the available pool. Used for ACK.
    /// Returns `UnknownDelivery` if `delivery_id` is not currently held.
    fn ack_pending(&self, delivery_id: &str) -> Result<()>;

    /// Increment attempt count for `delivery_id`. Returns the new count.
    fn bump_attempt(&self, delivery_id: &str) -> Result<u32>;

    /// Move `original` onto the DLQ at `dlq_target`.
    fn enqueue_dlq(&self, dlq_target: &str, original: Value) -> Result<()>;

    /// Pending deadline for `delivery_id`, if it is currently held.
    fn read_lock_deadline(&self, delivery_id: &str) -> Option<Instant>;

    /// Read the stored payload for `message_id` on `queue`, if known.
    fn read_message(&self, queue: &str, message_id: MessageId) -> Option<Value>;
}

#[derive(Debug, Clone)]
struct PendingDelivery {
    queue: QueueId,
    message_id: MessageId,
    group: ConsumerGroupId,
    deadline: Instant,
    attempts: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct DlqRecord {
    pub target: DlqTarget,
    pub original: Value,
}

#[derive(Default)]
struct State {
    /// All known messages per queue, ordered by insertion (acts as the
    /// "available" set minus anything currently pending).
    queues: HashMap<QueueId, Vec<MessageId>>,
    pending: HashMap<DeliveryId, PendingDelivery>,
    /// Reverse index for idempotent `mark_pending`.
    by_key: HashMap<(QueueId, MessageId, ConsumerGroupId), DeliveryId>,
    /// Stored payloads keyed by `(queue, message_id)`. Seeded by tests via
    /// `seed_payload`; real storage hydrates from segment files.
    payloads: HashMap<(QueueId, MessageId), Value>,
    dlq: Vec<DlqRecord>,
}

/// In-memory fake for unit tests. Thread-safe via a single Mutex — the
/// real implementation will live elsewhere; this only needs to be correct,
/// not fast.
pub(crate) struct InMemoryQueueStore {
    state: Mutex<State>,
    counter: AtomicU64,
}

impl InMemoryQueueStore {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            counter: AtomicU64::new(0),
        }
    }

    /// Seed a queue with message ids — test helper. Real storage hydrates
    /// from the segment files; the fake takes a vector.
    pub(crate) fn seed_queue(&self, queue: &str, messages: Vec<MessageId>) {
        let mut state = self.state.lock().expect("state poisoned");
        state.queues.insert(queue.to_string(), messages);
    }

    /// Snapshot of the DLQ — test helper.
    pub(crate) fn dlq_snapshot(&self) -> Vec<DlqRecord> {
        self.state.lock().expect("state poisoned").dlq.clone()
    }

    /// Associate `payload` with `(queue, message_id)` — test helper used
    /// by lifecycle tests that need `read_message` to return data.
    pub(crate) fn seed_payload(&self, queue: &str, message_id: MessageId, payload: Value) {
        let mut state = self.state.lock().expect("state poisoned");
        state
            .payloads
            .insert((queue.to_string(), message_id), payload);
    }

    fn next_delivery_id(&self) -> DeliveryId {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let mut bytes = [0u8; 16];
        bytes[..8].copy_from_slice(&n.to_le_bytes());
        bytes[8..].copy_from_slice(&nanos.to_le_bytes());
        let hash = blake3::hash(&bytes);
        base32_lower(&hash.as_bytes()[..15])
    }
}

impl QueueStore for InMemoryQueueStore {
    fn available_messages(&self, queue: &str, side: QueueSide) -> Vec<MessageId> {
        let state = self.state.lock().expect("state poisoned");
        let Some(msgs) = state.queues.get(queue) else {
            return Vec::new();
        };
        let pending: std::collections::HashSet<MessageId> = state
            .pending
            .values()
            .filter(|p| p.queue == queue)
            .map(|p| p.message_id)
            .collect();
        let mut out: Vec<MessageId> = msgs
            .iter()
            .copied()
            .filter(|m| !pending.contains(m))
            .collect();
        if matches!(side, QueueSide::Right) {
            out.reverse();
        }
        out
    }

    fn mark_pending(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
        deadline: Instant,
    ) -> Result<DeliveryId> {
        let key = (queue.to_string(), message_id, group.to_string());
        {
            let mut state = self.state.lock().expect("state poisoned");
            if !state.queues.contains_key(queue) {
                return Err(QueueStoreError::UnknownQueue(queue.to_string()));
            }
            if let Some(existing) = state.by_key.get(&key).cloned() {
                if let Some(entry) = state.pending.get_mut(&existing) {
                    entry.deadline = deadline;
                }
                return Ok(existing);
            }
        }
        let delivery_id = self.next_delivery_id();
        let mut state = self.state.lock().expect("state poisoned");
        state.pending.insert(
            delivery_id.clone(),
            PendingDelivery {
                queue: queue.to_string(),
                message_id,
                group: group.to_string(),
                deadline,
                attempts: 0,
            },
        );
        state.by_key.insert(key, delivery_id.clone());
        Ok(delivery_id)
    }

    fn release_pending(&self, delivery_id: &str) -> Result<()> {
        let mut state = self.state.lock().expect("state poisoned");
        if let Some(entry) = state.pending.remove(delivery_id) {
            let key = (entry.queue, entry.message_id, entry.group);
            state.by_key.remove(&key);
        }
        Ok(())
    }

    fn ack_pending(&self, delivery_id: &str) -> Result<()> {
        let mut state = self.state.lock().expect("state poisoned");
        let entry = state
            .pending
            .remove(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        let key = (entry.queue.clone(), entry.message_id, entry.group);
        state.by_key.remove(&key);
        if let Some(msgs) = state.queues.get_mut(&entry.queue) {
            msgs.retain(|m| *m != entry.message_id);
        }
        state.payloads.remove(&(entry.queue, entry.message_id));
        Ok(())
    }

    fn bump_attempt(&self, delivery_id: &str) -> Result<u32> {
        let mut state = self.state.lock().expect("state poisoned");
        let entry = state
            .pending
            .get_mut(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        entry.attempts += 1;
        Ok(entry.attempts)
    }

    fn enqueue_dlq(&self, dlq_target: &str, original: Value) -> Result<()> {
        let mut state = self.state.lock().expect("state poisoned");
        state.dlq.push(DlqRecord {
            target: dlq_target.to_string(),
            original,
        });
        Ok(())
    }

    fn read_lock_deadline(&self, delivery_id: &str) -> Option<Instant> {
        let state = self.state.lock().expect("state poisoned");
        state.pending.get(delivery_id).map(|p| p.deadline)
    }

    fn read_message(&self, queue: &str, message_id: MessageId) -> Option<Value> {
        let state = self.state.lock().expect("state poisoned");
        state
            .payloads
            .get(&(queue.to_string(), message_id))
            .cloned()
    }
}

/// RFC 4648 base32 (lowercase, no padding). Hand-rolled — no `base32`
/// crate in workspace deps, and the alphabet is trivial.
fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity((bytes.len() * 8 + 4) / 5);
    let mut buf: u32 = 0;
    let mut bits: u32 = 0;
    for &b in bytes {
        buf = (buf << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let idx = ((buf >> bits) & 0x1f) as usize;
            out.push(ALPHABET[idx] as char);
        }
    }
    if bits > 0 {
        let idx = ((buf << (5 - bits)) & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn deadline_in(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn delivery_id_is_opaque_base32() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let id = store
            .mark_pending("q", 1, "g", deadline_in(1000))
            .expect("mark");
        assert!(!id.is_empty(), "delivery_id is empty");
        assert!(
            id.chars()
                .all(|c| matches!(c, 'a'..='z' | '2'..='7')),
            "delivery_id {id} not base32-lower"
        );
    }

    #[test]
    fn delivery_ids_are_unique() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1, 2]);
        let a = store.mark_pending("q", 1, "g", deadline_in(1000)).unwrap();
        let b = store.mark_pending("q", 2, "g", deadline_in(1000)).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn mark_pending_is_idempotent_on_same_key() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let a = store.mark_pending("q", 1, "g", deadline_in(1000)).unwrap();
        let b = store.mark_pending("q", 1, "g", deadline_in(2000)).unwrap();
        assert_eq!(a, b, "same (queue, msg, group) should return same delivery_id");
    }

    #[test]
    fn release_pending_is_noop_on_unknown_id() {
        let store = InMemoryQueueStore::new();
        assert!(store.release_pending("does-not-exist").is_ok());
    }

    #[test]
    fn bump_attempt_returns_new_count() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let id = store.mark_pending("q", 1, "g", deadline_in(1000)).unwrap();
        assert_eq!(store.bump_attempt(&id).unwrap(), 1);
        assert_eq!(store.bump_attempt(&id).unwrap(), 2);
        assert_eq!(store.bump_attempt(&id).unwrap(), 3);
    }

    #[test]
    fn bump_attempt_unknown_id_errors() {
        let store = InMemoryQueueStore::new();
        let err = store.bump_attempt("nope").unwrap_err();
        assert!(matches!(err, QueueStoreError::UnknownDelivery(_)));
    }

    #[test]
    fn enqueue_dlq_records_original() {
        let store = InMemoryQueueStore::new();
        store.enqueue_dlq("orders.dlq", Value::text("payload-1")).unwrap();
        store.enqueue_dlq("orders.dlq", Value::Integer(42)).unwrap();
        let snap = store.dlq_snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].target, "orders.dlq");
        assert_eq!(snap[0].original, Value::text("payload-1"));
        assert_eq!(snap[1].original, Value::Integer(42));
    }

    #[test]
    fn available_messages_skips_pending() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1, 2, 3]);
        let _ = store.mark_pending("q", 2, "g", deadline_in(1000)).unwrap();
        let avail = store.available_messages("q", QueueSide::Left);
        assert_eq!(avail, vec![1, 3]);
        let avail_right = store.available_messages("q", QueueSide::Right);
        assert_eq!(avail_right, vec![3, 1]);
    }

    #[test]
    fn release_returns_message_to_available() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let id = store.mark_pending("q", 1, "g", deadline_in(1000)).unwrap();
        assert!(store.available_messages("q", QueueSide::Left).is_empty());
        store.release_pending(&id).unwrap();
        assert_eq!(store.available_messages("q", QueueSide::Left), vec![1]);
    }

    #[test]
    fn read_lock_deadline_reflects_pending_state() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let dl = deadline_in(1000);
        let id = store.mark_pending("q", 1, "g", dl).unwrap();
        assert_eq!(store.read_lock_deadline(&id), Some(dl));
        store.release_pending(&id).unwrap();
        assert_eq!(store.read_lock_deadline(&id), None);
    }

    #[test]
    fn mark_pending_refreshes_deadline_on_repeat() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let d1 = deadline_in(1000);
        let d2 = deadline_in(5000);
        let id = store.mark_pending("q", 1, "g", d1).unwrap();
        let id2 = store.mark_pending("q", 1, "g", d2).unwrap();
        assert_eq!(id, id2);
        assert_eq!(store.read_lock_deadline(&id), Some(d2));
    }

    #[test]
    fn mark_pending_unknown_queue_errors() {
        let store = InMemoryQueueStore::new();
        let err = store.mark_pending("missing", 1, "g", deadline_in(1000)).unwrap_err();
        assert!(matches!(err, QueueStoreError::UnknownQueue(_)));
    }

    #[test]
    fn base32_lower_roundtrip_shape() {
        // 15 bytes -> ceil(120/5) = 24 chars
        let s = base32_lower(&[0u8; 15]);
        assert_eq!(s.len(), 24);
        assert!(s.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7')));
    }
}
