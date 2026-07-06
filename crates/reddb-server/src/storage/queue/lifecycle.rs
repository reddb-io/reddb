//! Queue lifecycle trait + in-memory fake.
//!
//! Narrow `QueueStore` interface the future `QueueLifecycle` Module will
//! depend on. The fake (`InMemoryQueueStore`) is reused across
//! `QueueLifecycle` unit tests so transitions can be exercised without
//! booting the engine. This is tracer-bullet scope (PRD #527, issue #528):
//! the trait compiles, the fake passes its own contract tests, and no
//! production code consumes it yet.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
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
    /// Mutation attempted against a replica-side `QueueStore`. Replicas
    /// receive queue state via the logical-WAL apply path; calling a
    /// mutation method on the replica adapter signals that
    /// `QueueLifecycle` was wired on a replica, which violates the
    /// determinism contract (primary owns decisions; replica replays).
    ReplicaImmutable,
}

impl std::fmt::Display for QueueStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownDelivery(id) => write!(f, "unknown delivery {id}"),
            Self::UnknownQueue(q) => write!(f, "unknown queue {q}"),
            Self::ReplicaImmutable => write!(
                f,
                "replica QueueStore is immutable — decisions live on the primary"
            ),
        }
    }
}

impl std::error::Error for QueueStoreError {}

pub(crate) type Result<T> = std::result::Result<T, QueueStoreError>;

/// Tombstone record observed through [`QueueTxn::record_pending_tombstone`].
///
/// Used by tests against [`InMemoryQueueStore`] to assert that the
/// ack-and-delete flow records the expected tombstone calls. Production
/// adapters route the same call through the runtime's pending-tombstones
/// map (see `RedDBRuntime::record_pending_tombstone`); the field shape is
/// kept narrow on purpose — only the public surface ADR-0020 calls out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TombstoneRecord {
    pub(crate) queue: QueueId,
    pub(crate) message_id: MessageId,
}

/// Bridge from a [`QueueTxn`] to the runtime's MVCC delete path.
///
/// The lifecycle layer depends only on this trait, so it stays free of
/// `&Engine` / runtime types. The production runtime supplies an
/// implementation (`runtime::primary_queue_store::RuntimeQueueBridge`)
/// that retires the underlying message through
/// `current_xid → set_xmax → record_pending_tombstone`, making lifecycle
/// ack/purge **rollback-safe**: a retirement inside a statement that
/// later rolls back leaves the message visible again, and on commit the
/// message is gone — parity with the pre-lifecycle queue retirement path.
pub(crate) trait QueueTxnContext: Send + Sync {
    /// Retire `(queue, message_id)` through the runtime MVCC path. Inside
    /// an open transaction this stamps `xmax` and records a pending
    /// tombstone (revived on rollback); under autocommit it falls back to
    /// a hard delete. No new transaction is opened — the call participates
    /// in the caller's Statement frame.
    fn retire_message(&self, queue: &str, message_id: MessageId);
}

/// Transaction-context handle threaded through every mutating
/// [`QueueStore`] method. Carries two things: the observable tombstone
/// log (asserted by in-memory tests) and — in production — the live
/// connection context via an attached [`QueueTxnContext`].
///
/// In-memory tests construct a fresh `QueueTxn::new()` (no context) per
/// call and assert `recorded_tombstones()` post-hoc. Production callers
/// build one bound to the running `RedDBRuntime` connection (see
/// `PrimaryQueueStore::new_txn`) so `record_pending_tombstone` actually
/// drives the MVCC retire against the caller's transaction instead of
/// discarding the record.
pub(crate) struct QueueTxn {
    tombstones: Mutex<Vec<TombstoneRecord>>,
    /// Live runtime connection context. `None` for the stub used by
    /// in-memory unit tests; `Some` once a production caller binds the
    /// running transaction. When present, `record_pending_tombstone`
    /// routes the underlying message retirement through the real MVCC
    /// path.
    context: Option<Arc<dyn QueueTxnContext>>,
}

impl QueueTxn {
    pub(crate) fn new() -> Self {
        Self {
            tombstones: Mutex::new(Vec::new()),
            context: None,
        }
    }

    /// Bind a live runtime connection context (production path). The
    /// resulting txn drives `record_pending_tombstone` through the
    /// runtime MVCC retire in addition to logging the tombstone.
    pub(crate) fn with_context(context: Arc<dyn QueueTxnContext>) -> Self {
        Self {
            tombstones: Mutex::new(Vec::new()),
            context: Some(context),
        }
    }

    /// Whether a live runtime context is attached. The primary adapter
    /// consults this to decide whether the retire is already being driven
    /// by the txn (production) or must be performed against its own
    /// runtime handle (no-context unit-test path).
    pub(crate) fn has_context(&self) -> bool {
        self.context.is_some()
    }

    /// Record that the running transaction has marked `(queue, message_id)`
    /// for deletion. Mirrors the runtime-side `record_pending_tombstone`
    /// contract — the message stays addressable until the transaction
    /// commits; rollback revives it. When a live [`QueueTxnContext`] is
    /// attached, the underlying message is retired through the real MVCC
    /// path here; otherwise this only appends to the observable log.
    pub(crate) fn record_pending_tombstone(&self, queue: &str, message_id: MessageId) {
        self.tombstones
            .lock()
            .expect("queue txn poisoned")
            .push(TombstoneRecord {
                queue: queue.to_string(),
                message_id,
            });
        if let Some(context) = &self.context {
            context.retire_message(queue, message_id);
        }
    }

    /// Snapshot of every tombstone recorded against this txn — used by
    /// in-memory tests asserting on the ack-and-delete flow.
    pub(crate) fn recorded_tombstones(&self) -> Vec<TombstoneRecord> {
        self.tombstones.lock().expect("queue txn poisoned").clone()
    }
}

impl Default for QueueTxn {
    fn default() -> Self {
        Self::new()
    }
}

/// Narrow storage surface the `QueueLifecycle` Module depends on.
///
/// Methods are intentionally minimal — the lifecycle owns transition
/// policy; the store owns persistence semantics.
pub(crate) trait QueueStore {
    /// Available (not yet pending) message ids on `queue`, scanning from `side`.
    fn available_messages(&self, queue: &str, side: QueueSide) -> Vec<MessageId>;

    /// Look up the `DeliveryId` currently held for the
    /// `(queue, message_id, group)` tuple, if any. Returns `None` when
    /// no pending row matches — including the case where the tuple was
    /// retired (acked / moved to DLQ) or never marked pending.
    ///
    /// Prereq seam for the wire compat bridge (PRD #598): exposes the
    /// idempotency key `mark_pending` already consults internally so
    /// the bridge can resolve a re-delivery to the same `DeliveryId`
    /// without round-tripping through `mark_pending` itself.
    fn find_pending_by_key(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
    ) -> Option<DeliveryId>;

    /// Reserve `message_id` for `group` with a pending deadline. Idempotent
    /// on the `(queue, message_id, group)` key — repeated calls with the
    /// same key return the same `DeliveryId` and refresh the deadline.
    fn mark_pending(
        &self,
        txn: &QueueTxn,
        queue: &str,
        message_id: MessageId,
        group: &str,
        consumer: &str,
        deadline: Instant,
    ) -> Result<DeliveryId>;

    /// Release a pending delivery back to the available pool. No-op if
    /// `delivery_id` is unknown (already released or never existed).
    fn release_pending(&self, txn: &QueueTxn, delivery_id: &str) -> Result<()>;

    /// Permanently retire a pending delivery — removes the pending entry
    /// AND the underlying message from the available pool. Used for ACK
    /// in WORK mode. Returns `UnknownDelivery` if `delivery_id` is not
    /// currently held.
    fn ack_pending(&self, txn: &QueueTxn, delivery_id: &str) -> Result<()>;

    /// Retire a pending delivery for one consumer group only. Used for
    /// ACK / terminal NACK in FANOUT mode: the pending row goes away and
    /// the (queue, msg, group) tuple is recorded as "acked" so the same
    /// group will not see the message again, but the message remains in
    /// the queue and the payload stays addressable for other groups that
    /// have not yet retired it.
    fn retire_for_group(&self, txn: &QueueTxn, delivery_id: &str) -> Result<()>;

    /// Available message ids on `queue` from the perspective of a single
    /// consumer group: filters out messages that this group has already
    /// retired or currently holds pending, but ignores other groups'
    /// state. Used by `QueueLifecycle::deliver` in FANOUT mode.
    fn available_messages_for_group(
        &self,
        queue: &str,
        group: &str,
        side: QueueSide,
    ) -> Vec<MessageId>;

    /// Increment attempt count for `delivery_id`. Returns the new count
    /// alongside the `(queue, message_id)` the delivery resolves to —
    /// callers (notably `QueueLifecycle::nack`) need the pair to consult
    /// per-message policy via [`QueueStore::read_max_attempts`] without a
    /// second pending-row lookup. Pending row is left in place; only the
    /// attempts counter mutates.
    fn bump_attempt(&self, txn: &QueueTxn, delivery_id: &str) -> Result<BumpedAttempt>;

    /// Read the current attempt count for `delivery_id` without
    /// mutating pending state.
    fn read_pending_attempt(&self, delivery_id: &str) -> Result<BumpedAttempt>;

    /// Per-message retry budget — the value the lifecycle compares the
    /// `bump_attempt` return against to decide retire-vs-requeue. Sourced
    /// from the row data on `Primary`/`Replica` (the same field
    /// `impl_queue::queue_message_max_attempts` reads) and from the test
    /// seed map on `InMemoryQueueStore`. Returns a sensible default if no
    /// per-message override is set so the trait surface is total.
    fn read_max_attempts(&self, queue: &str, message_id: MessageId) -> u32;

    /// Move `original` onto the DLQ at `dlq_target`.
    fn enqueue_dlq(&self, txn: &QueueTxn, dlq_target: &str, original: Value) -> Result<()>;

    /// Pending deadline for `delivery_id`, if it is currently held.
    fn read_lock_deadline(&self, delivery_id: &str) -> Option<Instant>;

    /// Read the stored payload for `message_id` on `queue`, if known.
    fn read_message(&self, queue: &str, message_id: MessageId) -> Option<Value>;

    /// Read the optional ordering key stored on `message_id`.
    fn read_ordering_key(&self, queue: &str, message_id: MessageId) -> Option<String>;

    /// Whether `group` already has a pending delivery with `ordering_key`.
    fn ordering_key_in_flight(&self, queue: &str, group: &str, ordering_key: &str) -> bool;

    /// Read the stored payload for the message backing `delivery_id`, if it
    /// is currently held. Used by `QueueLifecycle::nack` to capture the
    /// payload before `ack_pending` retires the underlying message.
    fn read_pending_payload(&self, delivery_id: &str) -> Option<Value>;

    /// Release every pending row on `queue` whose `lock_deadline` is at or
    /// before `now`. The reclaimed messages become eligible for delivery
    /// again. Attempt counts are preserved (release, not retire), so a
    /// reclaim looks the same to NACK accounting as an explicit release.
    /// Implementations must be idempotent — calling twice with the same
    /// `now` is a no-op on the second call.
    fn reclaim_expired(&self, txn: &QueueTxn, queue: &str, now: Instant) -> Result<()>;

    /// Atomically remove every message on `queue` — both available rows
    /// and currently-pending ones. Records a tombstone per removed
    /// `(queue, message_id)` through `txn.record_pending_tombstone(...)`,
    /// in the same shape as `ack_pending`. Returns the count of message
    /// ids purged. Mirrors legacy `QUEUE PURGE` semantics:
    /// every pending row, every available row, and the underlying
    /// payloads all go away. Failure modes propagate; no partial purge
    /// is observable to readers after a successful return.
    fn purge_queue(&self, txn: &QueueTxn, queue: &str) -> Result<usize>;

    /// Enumerate every pending delivery currently held on `queue`,
    /// regardless of consumer group. Used by `QueueLifecycle::claim` to
    /// find candidates whose lock has expired beyond the caller-supplied
    /// `min_idle_ms` threshold. Read-only — no transaction context.
    fn pending_deliveries_for_queue(&self, queue: &str) -> Vec<PendingDeliveryView>;

    /// Destructive read: pop up to `count` messages from `queue` scanning
    /// from `side`, retiring each underlying message through the same
    /// MVCC tombstone path as `ack_pending`. Returns the popped
    /// `(message_id, payload)` pairs in scan order. Skips messages that
    /// are currently pending (held by another delivery) or whose lock
    /// cannot be acquired non-blockingly. Group-less — caller does not
    /// hold a delivery handle.
    ///
    /// Backing store operation for `QueueLifecycle::pop`.
    fn pop_available(
        &self,
        txn: &QueueTxn,
        queue: &str,
        side: QueueSide,
        count: usize,
    ) -> Result<Vec<(MessageId, Value)>>;

    /// Retire `(queue, message_id)` through the same MVCC tombstone path
    /// `ack_pending` uses, without going through a pending row. Used by
    /// the auto-DLQ-exceed branch on read, the cross-queue MOVE delete
    /// side, and any future caller that already holds the (queue,
    /// message_id) pair directly. Idempotent — calling on a
    /// already-retired message is a no-op.
    ///
    /// Backing store operation for `QueueLifecycle::delete_with_state`.
    fn delete_with_state(&self, txn: &QueueTxn, queue: &str, message_id: MessageId) -> Result<()>;

    /// Atomically move up to `count` messages from `source` onto `dest`,
    /// scanning `source` from `side`. Each moved message is inserted on
    /// `dest` (gaining a fresh `message_id`) and then retired on
    /// `source` via the same MVCC tombstone path as `delete_with_state`.
    /// Returns the count actually moved (≤ `count`; can be 0 when
    /// `source` is empty or every candidate is locked).
    ///
    /// Prereq for `QueueLifecycle::move_between_queues`, the
    /// lifecycle-side replacement for the inline MOVE logic in
    /// `impl_queue.rs`.
    fn move_to_queue(
        &self,
        txn: &QueueTxn,
        source: &str,
        dest: &str,
        side: QueueSide,
        count: usize,
    ) -> Result<usize>;
}

/// Read-only view of a pending delivery, returned by
/// [`QueueStore::pending_deliveries_for_queue`]. Carries the same fields
/// `mark_pending` needs to refresh a lock (queue, message_id, group) plus
/// the delivery handle and current lock deadline.
#[derive(Debug, Clone)]
pub(crate) struct PendingDeliveryView {
    pub(crate) delivery_id: DeliveryId,
    pub(crate) queue: QueueId,
    pub(crate) message_id: MessageId,
    pub(crate) group: ConsumerGroupId,
    pub(crate) deadline: Instant,
}

#[derive(Debug, Clone)]
struct PendingDelivery {
    queue: QueueId,
    message_id: MessageId,
    group: ConsumerGroupId,
    consumer: String,
    deadline: Instant,
    attempts: u32,
}

/// Return value of [`QueueStore::bump_attempt`] — the new attempt count
/// plus the pending key the delivery resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BumpedAttempt {
    pub(crate) attempts: u32,
    pub(crate) queue: QueueId,
    pub(crate) message_id: MessageId,
}

/// Crate-wide fallback when a `read_max_attempts` caller hits a tuple
/// without a configured override. Mirrors the user-facing
/// `DEFAULT_QUEUE_MAX_ATTEMPTS` in `storage::query` — duplicated here so
/// `InMemoryQueueStore` can resolve a default without a runtime/query
/// dep.
pub(crate) const DEFAULT_READ_MAX_ATTEMPTS: u32 = 3;

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
    /// Attempt counts keyed by `(queue, message_id, group)`. Survives
    /// release/redeliver cycles so NACK→requeue→redeliver preserves the
    /// retry budget; cleared on `ack_pending`.
    attempts: HashMap<(QueueId, MessageId, ConsumerGroupId), u32>,
    /// Groups that have retired a message under FANOUT semantics — the
    /// message remains in the queue for other groups, but `available_for_group`
    /// must filter it out for any group present in this set.
    acked: std::collections::HashSet<(QueueId, MessageId, ConsumerGroupId)>,
    /// Per-message retry budget overrides for the in-memory fake. Seeded
    /// by tests via `seed_max_attempts`; absence falls back to
    /// `DEFAULT_READ_MAX_ATTEMPTS`.
    max_attempts: HashMap<(QueueId, MessageId), u32>,
    /// Optional ordering keys keyed by `(queue, message_id)`.
    ordering_keys: HashMap<(QueueId, MessageId), String>,
    dlq: Vec<DlqRecord>,
}

/// In-memory fake for unit tests. Thread-safe via a single Mutex — the
/// real implementation will live elsewhere; this only needs to be correct,
/// not fast. Cloneable so the same backing state can be handed to two
/// `QueueLifecycle` instances (used by the crash-safety test that
/// recreates the Module against an existing store).
#[derive(Clone)]
pub(crate) struct InMemoryQueueStore {
    state: Arc<Mutex<State>>,
    counter: Arc<AtomicU64>,
}

impl InMemoryQueueStore {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            counter: Arc::new(AtomicU64::new(0)),
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

    /// Seed a per-message retry budget — test helper. Real adapters
    /// source the value from the `QueueMessageData` row on push; the
    /// fake takes the value directly so unit tests can drive the
    /// retire-vs-requeue decision without booting the engine.
    pub(crate) fn seed_max_attempts(&self, queue: &str, message_id: MessageId, max_attempts: u32) {
        let mut state = self.state.lock().expect("state poisoned");
        state
            .max_attempts
            .insert((queue.to_string(), message_id), max_attempts);
    }

    /// Associate `payload` with `(queue, message_id)` — test helper used
    /// by lifecycle tests that need `read_message` to return data.
    pub(crate) fn seed_payload(&self, queue: &str, message_id: MessageId, payload: Value) {
        let mut state = self.state.lock().expect("state poisoned");
        state
            .payloads
            .insert((queue.to_string(), message_id), payload);
    }

    /// Associate an ordering key with `(queue, message_id)` — test helper.
    pub(crate) fn seed_ordering_key(&self, queue: &str, message_id: MessageId, key: &str) {
        let mut state = self.state.lock().expect("state poisoned");
        state
            .ordering_keys
            .insert((queue.to_string(), message_id), key.to_string());
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
        _txn: &QueueTxn,
        queue: &str,
        message_id: MessageId,
        group: &str,
        consumer: &str,
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
                    entry.consumer = consumer.to_string();
                    entry.deadline = deadline;
                }
                return Ok(existing);
            }
        }
        let delivery_id = self.next_delivery_id();
        let mut state = self.state.lock().expect("state poisoned");
        let attempts = state.attempts.get(&key).copied().unwrap_or(0);
        state.pending.insert(
            delivery_id.clone(),
            PendingDelivery {
                queue: queue.to_string(),
                message_id,
                group: group.to_string(),
                consumer: consumer.to_string(),
                deadline,
                attempts,
            },
        );
        state.by_key.insert(key, delivery_id.clone());
        Ok(delivery_id)
    }

    fn find_pending_by_key(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
    ) -> Option<DeliveryId> {
        let state = self.state.lock().expect("state poisoned");
        state
            .by_key
            .get(&(queue.to_string(), message_id, group.to_string()))
            .cloned()
    }

    fn release_pending(&self, _txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        let mut state = self.state.lock().expect("state poisoned");
        if let Some(entry) = state.pending.remove(delivery_id) {
            let key = (entry.queue, entry.message_id, entry.group);
            state.by_key.remove(&key);
        }
        Ok(())
    }

    fn ack_pending(&self, txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        let mut state = self.state.lock().expect("state poisoned");
        let entry = state
            .pending
            .remove(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        let key = (entry.queue.clone(), entry.message_id, entry.group);
        state.by_key.remove(&key);
        state.attempts.remove(&key);
        if let Some(msgs) = state.queues.get_mut(&entry.queue) {
            msgs.retain(|m| *m != entry.message_id);
        }
        state
            .payloads
            .remove(&(entry.queue.clone(), entry.message_id));
        state
            .ordering_keys
            .remove(&(entry.queue.clone(), entry.message_id));
        // ack-and-delete on a WORK-mode queue tombstones the underlying
        // message — mirror the runtime-side `record_pending_tombstone`
        // call so tests observe the would-be MVCC tombstone. Drop the
        // state lock first so a future txn implementation that needs to
        // re-enter the store can.
        drop(state);
        txn.record_pending_tombstone(&entry.queue, entry.message_id);
        Ok(())
    }

    fn retire_for_group(&self, _txn: &QueueTxn, delivery_id: &str) -> Result<()> {
        let mut state = self.state.lock().expect("state poisoned");
        let entry = state
            .pending
            .remove(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        let key = (entry.queue, entry.message_id, entry.group);
        state.by_key.remove(&key);
        state.attempts.remove(&key);
        state.acked.insert(key);
        Ok(())
    }

    fn available_messages_for_group(
        &self,
        queue: &str,
        group: &str,
        side: QueueSide,
    ) -> Vec<MessageId> {
        let state = self.state.lock().expect("state poisoned");
        let Some(msgs) = state.queues.get(queue) else {
            return Vec::new();
        };
        let pending: std::collections::HashSet<MessageId> = state
            .pending
            .values()
            .filter(|p| p.queue == queue && p.group == group)
            .map(|p| p.message_id)
            .collect();
        let mut out: Vec<MessageId> = msgs
            .iter()
            .copied()
            .filter(|m| !pending.contains(m))
            .filter(|m| {
                !state
                    .acked
                    .contains(&(queue.to_string(), *m, group.to_string()))
            })
            .collect();
        if matches!(side, QueueSide::Right) {
            out.reverse();
        }
        out
    }

    fn bump_attempt(&self, _txn: &QueueTxn, delivery_id: &str) -> Result<BumpedAttempt> {
        let mut state = self.state.lock().expect("state poisoned");
        let entry = state
            .pending
            .get_mut(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        entry.attempts += 1;
        let count = entry.attempts;
        let queue = entry.queue.clone();
        let message_id = entry.message_id;
        let key = (queue.clone(), message_id, entry.group.clone());
        state.attempts.insert(key, count);
        Ok(BumpedAttempt {
            attempts: count,
            queue,
            message_id,
        })
    }

    fn read_pending_attempt(&self, delivery_id: &str) -> Result<BumpedAttempt> {
        let state = self.state.lock().expect("state poisoned");
        let entry = state
            .pending
            .get(delivery_id)
            .ok_or_else(|| QueueStoreError::UnknownDelivery(delivery_id.to_string()))?;
        Ok(BumpedAttempt {
            attempts: entry.attempts,
            queue: entry.queue.clone(),
            message_id: entry.message_id,
        })
    }

    fn read_max_attempts(&self, queue: &str, message_id: MessageId) -> u32 {
        let state = self.state.lock().expect("state poisoned");
        state
            .max_attempts
            .get(&(queue.to_string(), message_id))
            .copied()
            .unwrap_or(DEFAULT_READ_MAX_ATTEMPTS)
    }

    fn enqueue_dlq(&self, _txn: &QueueTxn, dlq_target: &str, original: Value) -> Result<()> {
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

    fn read_ordering_key(&self, queue: &str, message_id: MessageId) -> Option<String> {
        let state = self.state.lock().expect("state poisoned");
        state
            .ordering_keys
            .get(&(queue.to_string(), message_id))
            .cloned()
    }

    fn ordering_key_in_flight(&self, queue: &str, group: &str, ordering_key: &str) -> bool {
        let state = self.state.lock().expect("state poisoned");
        state.pending.values().any(|pending| {
            pending.queue == queue
                && pending.group == group
                && state
                    .ordering_keys
                    .get(&(pending.queue.clone(), pending.message_id))
                    .is_some_and(|key| key == ordering_key)
        })
    }

    fn read_pending_payload(&self, delivery_id: &str) -> Option<Value> {
        let state = self.state.lock().expect("state poisoned");
        let entry = state.pending.get(delivery_id)?;
        state
            .payloads
            .get(&(entry.queue.clone(), entry.message_id))
            .cloned()
    }

    fn reclaim_expired(&self, _txn: &QueueTxn, queue: &str, now: Instant) -> Result<()> {
        let mut state = self.state.lock().expect("state poisoned");
        let expired: Vec<DeliveryId> = state
            .pending
            .iter()
            .filter(|(_, p)| p.queue == queue && p.deadline <= now)
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(entry) = state.pending.remove(&id) {
                let key = (entry.queue, entry.message_id, entry.group);
                state.by_key.remove(&key);
            }
        }
        Ok(())
    }

    fn purge_queue(&self, txn: &QueueTxn, queue: &str) -> Result<usize> {
        // Collect the set of message ids first so the tombstones we
        // record line up with the rows we actually remove. The unique
        // set is the union of `queues[queue]` (everything ever pushed
        // and not yet ack-removed) and any currently-pending row that
        // references this queue — that second source catches the edge
        // case where a pending row outlived its `queues` entry (which
        // the in-memory fake never produces today, but the trait
        // surface is total against either ordering).
        let mut message_ids: Vec<MessageId> = {
            let state = self.state.lock().expect("state poisoned");
            let mut ids: Vec<MessageId> = state.queues.get(queue).cloned().unwrap_or_default();
            for pending in state.pending.values() {
                if pending.queue == queue && !ids.contains(&pending.message_id) {
                    ids.push(pending.message_id);
                }
            }
            ids
        };
        message_ids.sort_unstable();
        message_ids.dedup();

        {
            let mut state = self.state.lock().expect("state poisoned");
            // Drop every pending row referencing this queue, plus its
            // attempts/by_key/acked sibling entries.
            let pending_to_remove: Vec<DeliveryId> = state
                .pending
                .iter()
                .filter(|(_, p)| p.queue == queue)
                .map(|(id, _)| id.clone())
                .collect();
            for id in pending_to_remove {
                if let Some(entry) = state.pending.remove(&id) {
                    let key = (entry.queue, entry.message_id, entry.group);
                    state.by_key.remove(&key);
                    state.attempts.remove(&key);
                }
            }
            state.acked.retain(|(q, _, _)| q != queue);
            // Drop the available rows and their payloads.
            state.queues.remove(queue);
            state.payloads.retain(|(q, _), _| q != queue);
            state.ordering_keys.retain(|(q, _), _| q != queue);
        }

        // Tombstones recorded after the state mutation, mirroring the
        // ack_pending shape (one tombstone per removed message_id).
        for message_id in &message_ids {
            txn.record_pending_tombstone(queue, *message_id);
        }
        Ok(message_ids.len())
    }

    fn pending_deliveries_for_queue(&self, queue: &str) -> Vec<PendingDeliveryView> {
        let state = self.state.lock().expect("state poisoned");
        state
            .pending
            .iter()
            .filter(|(_, p)| p.queue == queue)
            .map(|(id, p)| PendingDeliveryView {
                delivery_id: id.clone(),
                queue: p.queue.clone(),
                message_id: p.message_id,
                group: p.group.clone(),
                deadline: p.deadline,
            })
            .collect()
    }

    fn pop_available(
        &self,
        txn: &QueueTxn,
        queue: &str,
        side: QueueSide,
        count: usize,
    ) -> Result<Vec<(MessageId, Value)>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut popped: Vec<(MessageId, Value)> = Vec::new();
        {
            let mut state = self.state.lock().expect("state poisoned");
            // `pending` snapshot keyed by message_id — pop must skip any
            // available row that already has a pending delivery against it.
            let pending_ids: std::collections::HashSet<MessageId> = state
                .pending
                .values()
                .filter(|p| p.queue == queue)
                .map(|p| p.message_id)
                .collect();
            let Some(ids) = state.queues.get(queue).cloned() else {
                return Ok(Vec::new());
            };
            let iter: Box<dyn Iterator<Item = MessageId>> = match side {
                QueueSide::Left => Box::new(ids.into_iter()),
                QueueSide::Right => Box::new(ids.into_iter().rev()),
            };
            let mut to_remove: Vec<MessageId> = Vec::new();
            for mid in iter {
                if popped.len() >= count {
                    break;
                }
                if pending_ids.contains(&mid) {
                    continue;
                }
                let Some(payload) = state.payloads.get(&(queue.to_string(), mid)).cloned() else {
                    continue;
                };
                popped.push((mid, payload));
                to_remove.push(mid);
            }
            if !to_remove.is_empty() {
                if let Some(queue_ids) = state.queues.get_mut(queue) {
                    queue_ids.retain(|id| !to_remove.contains(id));
                }
                for mid in &to_remove {
                    state.payloads.remove(&(queue.to_string(), *mid));
                    state.max_attempts.remove(&(queue.to_string(), *mid));
                }
            }
        }
        for (mid, _) in &popped {
            txn.record_pending_tombstone(queue, *mid);
        }
        Ok(popped)
    }

    fn delete_with_state(&self, txn: &QueueTxn, queue: &str, message_id: MessageId) -> Result<()> {
        let removed = {
            let mut state = self.state.lock().expect("state poisoned");
            let key = (queue.to_string(), message_id);
            let was_present = state
                .queues
                .get(queue)
                .map(|ids| ids.contains(&message_id))
                .unwrap_or(false)
                || state.payloads.contains_key(&key);
            if let Some(queue_ids) = state.queues.get_mut(queue) {
                queue_ids.retain(|id| *id != message_id);
            }
            state.payloads.remove(&key);
            state.max_attempts.remove(&key);
            was_present
        };
        if removed {
            txn.record_pending_tombstone(queue, message_id);
        }
        Ok(())
    }

    fn move_to_queue(
        &self,
        txn: &QueueTxn,
        source: &str,
        dest: &str,
        side: QueueSide,
        count: usize,
    ) -> Result<usize> {
        if count == 0 {
            return Ok(0);
        }
        // Allocate fresh message ids on `dest` from the same counter that
        // `seed_queue`/`mark_pending` use, so test assertions on the
        // destination side can match.
        let popped = self.pop_available(txn, source, side, count)?;
        if popped.is_empty() {
            return Ok(0);
        }
        let moved = popped.len();
        {
            let mut state = self.state.lock().expect("state poisoned");
            // Allocate new ids first, then drop the `state` borrow before
            // re-entering it for the payload writes. Keeps the borrow
            // checker happy and the lock contiguous.
            let assignments: Vec<(MessageId, Value)> = popped
                .into_iter()
                .map(|(_, payload)| (self.counter.fetch_add(1, Ordering::Relaxed), payload))
                .collect();
            let dest_ids = state.queues.entry(dest.to_string()).or_default();
            for (id, _) in &assignments {
                dest_ids.push(*id);
            }
            for (id, payload) in assignments {
                state.payloads.insert((dest.to_string(), id), payload);
            }
        }
        Ok(moved)
    }
}

/// RFC 4648 base32 (lowercase, no padding). Hand-rolled — no `base32`
/// crate in workspace deps, and the alphabet is trivial.
fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity((bytes.len() * 8).div_ceil(5));
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

    fn txn() -> QueueTxn {
        QueueTxn::new()
    }

    #[test]
    fn pop_available_drains_and_records_tombstones() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![10, 20, 30]);
        for &id in &[10u64, 20, 30] {
            store.seed_payload("q", id, Value::Null);
        }
        let t = txn();
        let popped = store.pop_available(&t, "q", QueueSide::Left, 2).unwrap();
        assert_eq!(
            popped.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![10, 20]
        );
        // Remaining available is the un-popped id.
        assert_eq!(store.available_messages("q", QueueSide::Left), vec![30]);
        // Tombstones recorded in pop order.
        let stones = t.recorded_tombstones();
        assert_eq!(stones.len(), 2);
        assert_eq!(stones[0].message_id, 10);
        assert_eq!(stones[1].message_id, 20);
    }

    #[test]
    fn pop_available_skips_pending_messages() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![10, 20]);
        for &id in &[10u64, 20] {
            store.seed_payload("q", id, Value::Null);
        }
        let t = txn();
        // Lock id=10 as pending — pop must skip it.
        store
            .mark_pending(&t, "q", 10, "g", "c", deadline_in(1000))
            .unwrap();
        let popped = store.pop_available(&t, "q", QueueSide::Left, 10).unwrap();
        assert_eq!(
            popped.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![20]
        );
    }

    #[test]
    fn delete_with_state_retires_and_records_tombstone() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![10, 20]);
        store.seed_payload("q", 10, Value::Null);
        store.seed_payload("q", 20, Value::Null);
        let t = txn();
        store.delete_with_state(&t, "q", 10).unwrap();
        assert_eq!(store.available_messages("q", QueueSide::Left), vec![20]);
        let stones = t.recorded_tombstones();
        assert_eq!(stones.len(), 1);
        assert_eq!(stones[0].message_id, 10);
        // Idempotent: delete of an already-retired id is a silent no-op
        // (no extra tombstone recorded).
        store.delete_with_state(&t, "q", 10).unwrap();
        assert_eq!(t.recorded_tombstones().len(), 1);
    }

    #[test]
    fn move_to_queue_transfers_payloads_and_retires_source() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("src", vec![100, 200]);
        store.seed_payload("src", 100, Value::Boolean(true));
        store.seed_payload("src", 200, Value::Boolean(false));
        let t = txn();
        let moved = store
            .move_to_queue(&t, "src", "dst", QueueSide::Left, 5)
            .unwrap();
        assert_eq!(moved, 2);
        // Source drained, dest carries two new ids (counter-allocated).
        assert!(store.available_messages("src", QueueSide::Left).is_empty());
        let dst_ids = store.available_messages("dst", QueueSide::Left);
        assert_eq!(dst_ids.len(), 2);
        // Payloads survived the move in order.
        assert_eq!(
            store.read_message("dst", dst_ids[0]),
            Some(Value::Boolean(true))
        );
        assert_eq!(
            store.read_message("dst", dst_ids[1]),
            Some(Value::Boolean(false))
        );
        // Tombstone recorded for each source id.
        let stones = t.recorded_tombstones();
        assert_eq!(stones.len(), 2);
    }

    #[test]
    fn delivery_id_is_opaque_base32() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let t = txn();
        let id = store
            .mark_pending(&t, "q", 1, "g", "c", deadline_in(1000))
            .expect("mark");
        assert!(!id.is_empty(), "delivery_id is empty");
        assert!(
            id.chars().all(|c| matches!(c, 'a'..='z' | '2'..='7')),
            "delivery_id {id} not base32-lower"
        );
    }

    #[test]
    fn delivery_ids_are_unique() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1, 2]);
        let t = txn();
        let a = store
            .mark_pending(&t, "q", 1, "g", "c", deadline_in(1000))
            .unwrap();
        let b = store
            .mark_pending(&t, "q", 2, "g", "c", deadline_in(1000))
            .unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn mark_pending_is_idempotent_on_same_key() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let t = txn();
        let a = store
            .mark_pending(&t, "q", 1, "g", "c", deadline_in(1000))
            .unwrap();
        let b = store
            .mark_pending(&t, "q", 1, "g", "c", deadline_in(2000))
            .unwrap();
        assert_eq!(
            a, b,
            "same (queue, msg, group) should return same delivery_id"
        );
    }

    #[test]
    fn release_pending_is_noop_on_unknown_id() {
        let store = InMemoryQueueStore::new();
        let t = txn();
        assert!(store.release_pending(&t, "does-not-exist").is_ok());
    }

    #[test]
    fn bump_attempt_returns_new_count() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let t = txn();
        let id = store
            .mark_pending(&t, "q", 1, "g", "c", deadline_in(1000))
            .unwrap();
        let first = store.bump_attempt(&t, &id).unwrap();
        assert_eq!(first.attempts, 1);
        assert_eq!(first.queue, "q");
        assert_eq!(first.message_id, 1);
        assert_eq!(store.bump_attempt(&t, &id).unwrap().attempts, 2);
        assert_eq!(store.bump_attempt(&t, &id).unwrap().attempts, 3);
    }

    #[test]
    fn ack_and_delete_records_one_pending_tombstone_per_call() {
        // Acceptance criterion (issue #601): the ack-and-delete flow on
        // `InMemoryQueueStore` must observe a `record_pending_tombstone`
        // call for the message it tombstoned. Two seeded messages, two
        // mark_pending + ack_pending pairs → exactly two tombstones in
        // the order acked. Mutations that are *not* delete-shaped
        // (release_pending, retire_for_group, mark_pending,
        // bump_attempt, reclaim_expired) must record nothing.
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1, 2, 3]);
        store.seed_payload("q", 1, Value::text("p1"));
        store.seed_payload("q", 2, Value::text("p2"));
        store.seed_payload("q", 3, Value::text("p3"));
        let t = txn();

        // mark_pending alone does not tombstone.
        let d1 = store
            .mark_pending(&t, "q", 1, "g", "c", deadline_in(1000))
            .unwrap();
        let d2 = store
            .mark_pending(&t, "q", 2, "g", "c", deadline_in(1000))
            .unwrap();
        let d3 = store
            .mark_pending(&t, "q", 3, "g", "c", deadline_in(1000))
            .unwrap();
        assert!(
            t.recorded_tombstones().is_empty(),
            "mark_pending must not record tombstones"
        );

        // ack_pending tombstones the underlying message.
        store.ack_pending(&t, &d1).unwrap();
        store.ack_pending(&t, &d2).unwrap();
        assert_eq!(
            t.recorded_tombstones(),
            vec![
                TombstoneRecord {
                    queue: "q".to_string(),
                    message_id: 1
                },
                TombstoneRecord {
                    queue: "q".to_string(),
                    message_id: 2
                },
            ],
            "each ack_pending must record exactly one tombstone, in order",
        );

        // release_pending, bump_attempt, retire_for_group, reclaim_expired
        // are not delete-shaped — none of them must add tombstones.
        store.release_pending(&t, &d3).unwrap();
        assert_eq!(
            t.recorded_tombstones().len(),
            2,
            "release_pending must not record"
        );
        let d3 = store
            .mark_pending(&t, "q", 3, "g", "c", deadline_in(1000))
            .unwrap();
        store.bump_attempt(&t, &d3).unwrap();
        assert_eq!(
            t.recorded_tombstones().len(),
            2,
            "bump_attempt must not record"
        );
        store.retire_for_group(&t, &d3).unwrap();
        assert_eq!(
            t.recorded_tombstones().len(),
            2,
            "retire_for_group must not record"
        );
        store
            .reclaim_expired(&t, "q", Instant::now() + Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            t.recorded_tombstones().len(),
            2,
            "reclaim_expired must not record"
        );
    }

    #[test]
    fn read_max_attempts_defaults_to_three_when_not_seeded() {
        let store = InMemoryQueueStore::new();
        assert_eq!(
            store.read_max_attempts("q", 1),
            DEFAULT_READ_MAX_ATTEMPTS,
            "unseeded message must return the crate-wide default",
        );
        assert_eq!(DEFAULT_READ_MAX_ATTEMPTS, 3);
    }

    #[test]
    fn seed_max_attempts_overrides_default_per_message() {
        let store = InMemoryQueueStore::new();
        store.seed_max_attempts("q", 1, 7);
        store.seed_max_attempts("q", 2, 1);
        assert_eq!(store.read_max_attempts("q", 1), 7);
        assert_eq!(store.read_max_attempts("q", 2), 1);
        // Different queue, same id — not affected by the seed above.
        assert_eq!(
            store.read_max_attempts("other", 1),
            DEFAULT_READ_MAX_ATTEMPTS
        );
    }

    #[test]
    fn bump_attempt_unknown_id_errors() {
        let store = InMemoryQueueStore::new();
        let t = txn();
        let err = store.bump_attempt(&t, "nope").unwrap_err();
        assert!(matches!(err, QueueStoreError::UnknownDelivery(_)));
    }

    #[test]
    fn enqueue_dlq_records_original() {
        let store = InMemoryQueueStore::new();
        let t = txn();
        store
            .enqueue_dlq(&t, "orders.dlq", Value::text("payload-1"))
            .unwrap();
        store
            .enqueue_dlq(&t, "orders.dlq", Value::Integer(42))
            .unwrap();
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
        let t = txn();
        let _ = store
            .mark_pending(&t, "q", 2, "g", "c", deadline_in(1000))
            .unwrap();
        let avail = store.available_messages("q", QueueSide::Left);
        assert_eq!(avail, vec![1, 3]);
        let avail_right = store.available_messages("q", QueueSide::Right);
        assert_eq!(avail_right, vec![3, 1]);
    }

    #[test]
    fn release_returns_message_to_available() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let t = txn();
        let id = store
            .mark_pending(&t, "q", 1, "g", "c", deadline_in(1000))
            .unwrap();
        assert!(store.available_messages("q", QueueSide::Left).is_empty());
        store.release_pending(&t, &id).unwrap();
        assert_eq!(store.available_messages("q", QueueSide::Left), vec![1]);
    }

    #[test]
    fn read_lock_deadline_reflects_pending_state() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let t = txn();
        let dl = deadline_in(1000);
        let id = store.mark_pending(&t, "q", 1, "g", "c", dl).unwrap();
        assert_eq!(store.read_lock_deadline(&id), Some(dl));
        store.release_pending(&t, &id).unwrap();
        assert_eq!(store.read_lock_deadline(&id), None);
    }

    #[test]
    fn mark_pending_refreshes_deadline_on_repeat() {
        let store = InMemoryQueueStore::new();
        store.seed_queue("q", vec![1]);
        let t = txn();
        let d1 = deadline_in(1000);
        let d2 = deadline_in(5000);
        let id = store.mark_pending(&t, "q", 1, "g", "c", d1).unwrap();
        let id2 = store.mark_pending(&t, "q", 1, "g", "c", d2).unwrap();
        assert_eq!(id, id2);
        assert_eq!(store.read_lock_deadline(&id), Some(d2));
    }

    #[test]
    fn mark_pending_unknown_queue_errors() {
        let store = InMemoryQueueStore::new();
        let t = txn();
        let err = store
            .mark_pending(&t, "missing", 1, "g", "c", deadline_in(1000))
            .unwrap_err();
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
