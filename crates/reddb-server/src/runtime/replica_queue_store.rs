//! Replica `QueueStore` adapter (slice 11 of PRD #527 / issue #538).
//!
//! Companion to [`PrimaryQueueStore`](super::primary_queue_store) — the
//! second adapter that proves the `QueueStore` trait is the real seam
//! between the lifecycle decision Module and storage. The primary
//! adapter mutates and decides; the replica adapter applies outcomes.
//!
//! Determinism contract (ADR-0020 / PRD #527):
//!
//! - **Primary** runs `QueueLifecycle` against `PrimaryQueueStore`. Each
//!   transition (deliver/ack/nack/dlq) writes meta-row state into
//!   `red_queue_meta` and queue messages into the queue collection. The
//!   underlying `UnifiedStore::insert_auto` / `delete` calls emit CDC
//!   change records that propagate to replicas.
//! - **Replica** never instantiates `QueueLifecycle`. The logical-WAL
//!   apply path (`LogicalChangeApplier`) replays the CDC change records
//!   verbatim into the replica's local `UnifiedStore`. The pending
//!   rows, attempt counters, and DLQ contents are reproduced bit-for-
//!   bit because the same `delivery_id` strings, lock deadlines, and
//!   message payloads are carried in the change record's entity bytes.
//! - `ReplicaQueueStore` exposes the same read surface as
//!   `PrimaryQueueStore` so callers can observe queue state through
//!   the `QueueStore` trait. Mutation methods fail closed with
//!   [`QueueStoreError::ReplicaImmutable`] — reaching one of them means
//!   `QueueLifecycle` was wired on a replica, which breaks the
//!   determinism contract.
//!
//! Read methods deliberately mirror the meta-row scheme used by
//! `PrimaryQueueStore` (`queue_pending_lc`, `queue_acked_lc`,
//! `queue_attempts_lc`) so the same state shape works on both sides
//! of the seam.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::storage::queue::lifecycle::{
    DeliveryId, MessageId, QueueSide, QueueStore, QueueStoreError, Result,
};
use crate::storage::schema::Value;
use crate::storage::unified::entity::RowData;
use crate::storage::{EntityData, EntityId, EntityKind, UnifiedStore};

use super::RedDBRuntime;

const QUEUE_META_COLLECTION: &str = "red_queue_meta";
const KIND_PENDING_LC: &str = "queue_pending_lc";
const KIND_ACKED_LC: &str = "queue_acked_lc";
const KIND_ATTEMPTS_LC: &str = "queue_attempts_lc";

/// `QueueStore` implementation backed by the replica-side `UnifiedStore`.
/// Constructed against the same runtime the replica's logical-WAL apply
/// path writes into — reads observe whatever state the applier has
/// reproduced from the primary's CDC stream.
pub(crate) struct ReplicaQueueStore {
    runtime: RedDBRuntime,
}

impl ReplicaQueueStore {
    pub(crate) fn new(runtime: RedDBRuntime) -> Self {
        Self { runtime }
    }

    fn store(&self) -> Arc<UnifiedStore> {
        self.runtime.db().store()
    }

    fn meta_rows<F>(&self, predicate: F) -> Vec<RowData>
    where
        F: Fn(&RowData) -> bool + Sync,
    {
        let store = self.store();
        let Some(manager) = store.get_collection(QUEUE_META_COLLECTION) else {
            return Vec::new();
        };
        manager
            .query_all(|entity| entity.data.as_row().is_some_and(&predicate))
            .into_iter()
            .filter_map(|entity| entity.data.as_row().cloned())
            .collect()
    }

    fn pending_for_delivery(&self, delivery_id: &str) -> Option<PendingRow> {
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_PENDING_LC)
                && row_text(row, "delivery_id").as_deref() == Some(delivery_id)
        })
        .into_iter()
        .next()
        .and_then(|row| PendingRow::from_row(&row))
    }

    fn pending_message_ids(&self, queue: &str, group: Option<&str>) -> Vec<MessageId> {
        let queue_owned = queue.to_string();
        let group_owned = group.map(|g| g.to_string());
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_PENDING_LC)
                && row_text(row, "queue").as_deref() == Some(&queue_owned)
                && group_owned
                    .as_ref()
                    .map(|g| row_text(row, "group").as_deref() == Some(g))
                    .unwrap_or(true)
        })
        .into_iter()
        .filter_map(|row| row_u64(&row, "message_id"))
        .collect()
    }

    fn acked_message_ids(&self, queue: &str, group: &str) -> Vec<MessageId> {
        let queue_owned = queue.to_string();
        let group_owned = group.to_string();
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_ACKED_LC)
                && row_text(row, "queue").as_deref() == Some(&queue_owned)
                && row_text(row, "group").as_deref() == Some(&group_owned)
        })
        .into_iter()
        .filter_map(|row| row_u64(&row, "message_id"))
        .collect()
    }

    fn read_attempts(&self, queue: &str, message_id: MessageId, group: &str) -> u32 {
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_ATTEMPTS_LC)
                && row_text(row, "queue").as_deref() == Some(queue)
                && row_text(row, "group").as_deref() == Some(group)
                && row_u64(row, "message_id") == Some(message_id)
        })
        .into_iter()
        .next()
        .and_then(|row| row_u64(&row, "attempts").map(|v| v as u32))
        .unwrap_or(0)
    }

    fn live_message_ids(&self, queue: &str) -> Vec<MessageId> {
        let store = self.store();
        let Some(manager) = store.get_collection(queue) else {
            return Vec::new();
        };
        let mut out: Vec<(u64, MessageId)> = manager
            .query_all(|entity| {
                matches!(entity.kind, EntityKind::QueueMessage { .. })
                    && matches!(entity.data, EntityData::QueueMessage(_))
            })
            .into_iter()
            .filter_map(|entity| {
                let position = match &entity.kind {
                    EntityKind::QueueMessage { position, .. } => *position,
                    _ => return None,
                };
                let acked = match &entity.data {
                    EntityData::QueueMessage(d) => d.acked,
                    _ => return None,
                };
                if acked {
                    return None;
                }
                Some((position, entity.id.raw()))
            })
            .collect();
        out.sort_by_key(|(pos, _)| *pos);
        out.into_iter().map(|(_, id)| id).collect()
    }

    /// Test helper — list the DLQ entries for `target` queue. Mirrors
    /// `PrimaryQueueStore::list_queue_messages` access pattern but
    /// returns just payloads in insertion order.
    #[cfg(test)]
    pub(crate) fn dlq_payloads(&self, target: &str) -> Vec<Value> {
        let store = self.store();
        let Some(manager) = store.get_collection(target) else {
            return Vec::new();
        };
        let mut out: Vec<(u64, Value)> = manager
            .query_all(|entity| {
                matches!(entity.kind, EntityKind::QueueMessage { .. })
                    && matches!(entity.data, EntityData::QueueMessage(_))
            })
            .into_iter()
            .filter_map(|entity| {
                let position = match &entity.kind {
                    EntityKind::QueueMessage { position, .. } => *position,
                    _ => return None,
                };
                let payload = match &entity.data {
                    EntityData::QueueMessage(d) => d.payload.clone(),
                    _ => return None,
                };
                Some((position, payload))
            })
            .collect();
        out.sort_by_key(|(pos, _)| *pos);
        out.into_iter().map(|(_, p)| p).collect()
    }
}

#[derive(Debug, Clone)]
struct PendingRow {
    queue: String,
    message_id: MessageId,
    lock_deadline_ns: u64,
}

impl PendingRow {
    fn from_row(row: &RowData) -> Option<Self> {
        Some(Self {
            queue: row_text(row, "queue")?,
            message_id: row_u64(row, "message_id")?,
            lock_deadline_ns: row_u64(row, "lock_deadline_ns")?,
        })
    }
}

impl ReplicaQueueStore {
    fn pending_delivery_for_key(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
    ) -> Option<DeliveryId> {
        self.meta_rows(|row| {
            row_text(row, "kind").as_deref() == Some(KIND_PENDING_LC)
                && row_text(row, "queue").as_deref() == Some(queue)
                && row_text(row, "group").as_deref() == Some(group)
                && row_u64(row, "message_id") == Some(message_id)
        })
        .into_iter()
        .next()
        .and_then(|row| row_text(&row, "delivery_id"))
    }
}

impl QueueStore for ReplicaQueueStore {
    fn available_messages(&self, queue: &str, side: QueueSide) -> Vec<MessageId> {
        let pending: HashSet<MessageId> =
            self.pending_message_ids(queue, None).into_iter().collect();
        let mut out: Vec<MessageId> = self
            .live_message_ids(queue)
            .into_iter()
            .filter(|id| !pending.contains(id))
            .collect();
        if matches!(side, QueueSide::Right) {
            out.reverse();
        }
        out
    }

    fn available_messages_for_group(
        &self,
        queue: &str,
        group: &str,
        side: QueueSide,
    ) -> Vec<MessageId> {
        let pending: HashSet<MessageId> = self
            .pending_message_ids(queue, Some(group))
            .into_iter()
            .collect();
        let acked: HashSet<MessageId> =
            self.acked_message_ids(queue, group).into_iter().collect();
        let mut out: Vec<MessageId> = self
            .live_message_ids(queue)
            .into_iter()
            .filter(|id| !pending.contains(id) && !acked.contains(id))
            .collect();
        if matches!(side, QueueSide::Right) {
            out.reverse();
        }
        out
    }

    fn find_pending_by_key(
        &self,
        queue: &str,
        message_id: MessageId,
        group: &str,
    ) -> Option<DeliveryId> {
        self.pending_delivery_for_key(queue, message_id, group)
    }

    fn mark_pending(
        &self,
        _queue: &str,
        _message_id: MessageId,
        _group: &str,
        _deadline: Instant,
    ) -> Result<DeliveryId> {
        Err(QueueStoreError::ReplicaImmutable)
    }

    fn release_pending(&self, _delivery_id: &str) -> Result<()> {
        Err(QueueStoreError::ReplicaImmutable)
    }

    fn ack_pending(&self, _delivery_id: &str) -> Result<()> {
        Err(QueueStoreError::ReplicaImmutable)
    }

    fn retire_for_group(&self, _delivery_id: &str) -> Result<()> {
        Err(QueueStoreError::ReplicaImmutable)
    }

    fn bump_attempt(&self, _delivery_id: &str) -> Result<u32> {
        Err(QueueStoreError::ReplicaImmutable)
    }

    fn enqueue_dlq(&self, _dlq_target: &str, _original: Value) -> Result<()> {
        Err(QueueStoreError::ReplicaImmutable)
    }

    fn read_lock_deadline(&self, delivery_id: &str) -> Option<Instant> {
        let row = self.pending_for_delivery(delivery_id)?;
        let now_ns = now_unix_ns();
        let remaining_ns = row.lock_deadline_ns.saturating_sub(now_ns);
        Some(Instant::now() + Duration::from_nanos(remaining_ns))
    }

    fn read_message(&self, queue: &str, message_id: MessageId) -> Option<Value> {
        let store = self.store();
        let manager = store.get_collection(queue)?;
        let entity = manager.get(EntityId::new(message_id))?;
        match entity.data {
            EntityData::QueueMessage(data) if !data.acked => Some(data.payload),
            _ => None,
        }
    }

    fn read_pending_payload(&self, delivery_id: &str) -> Option<Value> {
        let row = self.pending_for_delivery(delivery_id)?;
        self.read_message(&row.queue, row.message_id)
    }

    fn reclaim_expired(&self, _queue: &str, _now: Instant) -> Result<()> {
        // Replica reclaim happens by replaying the primary's release
        // change records — never invoked directly. Fail closed for the
        // same reason as the other mutation methods.
        Err(QueueStoreError::ReplicaImmutable)
    }
}

/// Read attempts as an externally observable counter — used by tests
/// to compare replica state against the primary. Exposed via a free
/// helper rather than a trait method because the `QueueStore` trait
/// only surfaces attempt count as the return of `bump_attempt`.
#[cfg(test)]
pub(crate) fn observed_attempts(
    store: &ReplicaQueueStore,
    queue: &str,
    message_id: MessageId,
    group: &str,
) -> u32 {
    store.read_attempts(queue, message_id, group)
}

fn row_text(row: &RowData, field: &str) -> Option<String> {
    match row.get_field(field)? {
        Value::Text(value) => Some(value.to_string()),
        _ => None,
    }
}

fn row_u64(row: &RowData, field: &str) -> Option<u64> {
    match row.get_field(field)? {
        Value::UnsignedInteger(v) => Some(*v),
        Value::Integer(v) if *v >= 0 => Some(*v as u64),
        _ => None,
    }
}

fn now_unix_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{RedDBOptions, REDDB_FORMAT_VERSION};
    use crate::replication::cdc::{ChangeOperation, ChangeRecord};
    use crate::replication::logical::{ApplyMode, ApplyOutcome, LogicalChangeApplier};
    use crate::runtime::primary_queue_store::PrimaryQueueStore;
    use crate::runtime::queue_lifecycle::{QueueLifecycle, RetirementOutcome};
    use crate::storage::queue::lifecycle::QueueStoreError;
    use crate::RedDBRuntime;

    fn boot() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime boots")
    }

    fn push(rt: &RedDBRuntime, queue: &str, payload: &str) {
        rt.execute_query(&format!("QUEUE PUSH {queue} '{payload}'"))
            .expect("push");
    }

    /// Synthesize a CDC change record from every entity in `collection`
    /// on `source`, assigning monotonic LSNs starting at `next_lsn`.
    /// Returns the records in (collection, entity_id) iteration order
    /// plus the next free LSN.
    ///
    /// This is the test seam that stands in for the wire-level
    /// archived-WAL replay: the records carry the serialized entities
    /// the primary wrote, and the `LogicalChangeApplier` materializes
    /// them on the replica byte-for-byte.
    fn snapshot_as_change_records(
        source: &Arc<UnifiedStore>,
        collection: &str,
        next_lsn: &mut u64,
    ) -> Vec<ChangeRecord> {
        let format_version = source.format_version();
        let Some(manager) = source.get_collection(collection) else {
            return Vec::new();
        };
        let mut entities = manager.query_all(|_| true);
        // Stable ordering so LSNs are deterministic across runs.
        entities.sort_by_key(|e| e.id.raw());
        entities
            .into_iter()
            .map(|entity| {
                let lsn = *next_lsn;
                *next_lsn += 1;
                let kind_label = match &entity.kind {
                    EntityKind::TableRow { .. } => "row",
                    EntityKind::QueueMessage { .. } => "queue_message",
                    _ => "entity",
                };
                ChangeRecord::from_entity(
                    lsn,
                    lsn,
                    ChangeOperation::Insert,
                    collection,
                    kind_label,
                    &entity,
                    format_version,
                    None,
                )
            })
            .collect()
    }

    fn replay_collections(
        primary: &Arc<UnifiedStore>,
        replica: &crate::storage::RedDB,
        collections: &[&str],
    ) {
        let applier = LogicalChangeApplier::new(0);
        let mut next_lsn: u64 = 1;
        for collection in collections {
            // Create the collection on the replica before replaying so
            // `insert_auto` has somewhere to land — apply_record only
            // looks the collection up when it finds an existing one.
            let _ = replica.store().get_or_create_collection(*collection);
            for record in snapshot_as_change_records(primary, collection, &mut next_lsn) {
                let outcome = applier
                    .apply(replica, &record, ApplyMode::Replica)
                    .expect("apply replica record");
                assert!(matches!(outcome, ApplyOutcome::Applied));
            }
        }
    }

    #[test]
    fn replica_store_mutation_methods_fail_closed() {
        let rt = boot();
        let replica = ReplicaQueueStore::new(rt);

        let err = replica
            .mark_pending("q", 1, "g", Instant::now() + Duration::from_secs(1))
            .unwrap_err();
        assert!(matches!(err, QueueStoreError::ReplicaImmutable));
        assert!(matches!(
            replica.release_pending("any").unwrap_err(),
            QueueStoreError::ReplicaImmutable
        ));
        assert!(matches!(
            replica.ack_pending("any").unwrap_err(),
            QueueStoreError::ReplicaImmutable
        ));
        assert!(matches!(
            replica.retire_for_group("any").unwrap_err(),
            QueueStoreError::ReplicaImmutable
        ));
        assert!(matches!(
            replica.bump_attempt("any").unwrap_err(),
            QueueStoreError::ReplicaImmutable
        ));
        assert!(matches!(
            replica.enqueue_dlq("dlq", Value::text("x")).unwrap_err(),
            QueueStoreError::ReplicaImmutable
        ));
        assert!(matches!(
            replica.reclaim_expired("q", Instant::now()).unwrap_err(),
            QueueStoreError::ReplicaImmutable
        ));
    }

    #[test]
    fn deliver_ack_nack_dlq_on_primary_then_replay_matches_on_replica() {
        // Primary: run a deliver → nack (Requeued) → deliver → ack
        // → push fresh → deliver → nack → nack (MovedToDlq) sequence.
        // Then snapshot the primary's queue collections + meta and
        // replay them on a fresh replica DB. Read the replica's
        // observable state via `ReplicaQueueStore` and assert it
        // matches the primary's via `PrimaryQueueStore`.

        let primary_rt = boot();
        primary_rt
            .execute_query("CREATE QUEUE qrep MAX_ATTEMPTS 2 WITH DLQ qrep_dlq")
            .expect("create");
        push(&primary_rt, "qrep", "alpha");
        push(&primary_rt, "qrep", "beta");

        let primary_store = PrimaryQueueStore::new(primary_rt.clone());
        let cfg = primary_store.lifecycle_config("qrep");
        let lc = QueueLifecycle::new(primary_store, cfg);

        // alpha: deliver → nack (Requeued, below max) → deliver → ack.
        let a1 = lc.deliver("qrep", "workers", 1).expect("a1");
        assert_eq!(a1[0].payload, Value::text("alpha"));
        lc.nack(&a1[0].delivery_id).expect("a1 nack");

        let a2 = lc.deliver("qrep", "workers", 1).expect("a2");
        assert_eq!(a2[0].payload, Value::text("alpha"), "alpha redelivered");
        lc.ack(&a2[0].delivery_id).expect("a2 ack");

        // beta: deliver → nack → deliver → nack → moves to DLQ on
        // the second nack because max_attempts=2.
        let b1 = lc.deliver("qrep", "workers", 1).expect("b1");
        assert_eq!(b1[0].payload, Value::text("beta"));
        lc.nack(&b1[0].delivery_id).expect("b1 nack");
        let b2 = lc.deliver("qrep", "workers", 1).expect("b2");
        assert_eq!(b2[0].payload, Value::text("beta"));
        lc.nack(&b2[0].delivery_id).expect("b2 nack");

        assert_eq!(
            lc.recorded_outcomes(),
            vec![
                RetirementOutcome::Requeued,
                RetirementOutcome::Requeued,
                RetirementOutcome::MovedToDlq("qrep_dlq".to_string()),
            ]
        );

        // Push a fresh message that stays in flight at replay time
        // (deliver but no ack/nack) — covers the pending-row case.
        push(&primary_rt, "qrep", "gamma");
        let g1 = lc.deliver("qrep", "workers", 1).expect("g1");
        assert_eq!(g1[0].payload, Value::text("gamma"));

        // --- Replay on a fresh replica DB ---
        let replica_rt = boot();
        // Pre-create the queue + dlq collections on the replica so
        // the CDC replay lands the queue messages correctly.
        replica_rt
            .execute_query("CREATE QUEUE qrep MAX_ATTEMPTS 2 WITH DLQ qrep_dlq")
            .expect("replica create");

        replay_collections(
            &primary_rt.db().store(),
            &replica_rt.db(),
            &["qrep", "qrep_dlq", QUEUE_META_COLLECTION],
        );

        let replica_store = ReplicaQueueStore::new(replica_rt.clone());

        // 1. DLQ contents match — `beta` retired into qrep_dlq.
        let dlq = replica_store.dlq_payloads("qrep_dlq");
        assert_eq!(dlq, vec![Value::text("beta")], "DLQ contents diverged");

        // 2. Pending row for gamma surfaces with the same delivery_id
        //    the primary minted, and the lock deadline is in the future.
        let gamma_delivery = &g1[0].delivery_id;
        assert!(
            replica_store.read_lock_deadline(gamma_delivery).is_some(),
            "replica missing pending row for delivery {gamma_delivery}",
        );
        assert_eq!(
            replica_store.read_pending_payload(gamma_delivery),
            Some(Value::text("gamma")),
            "pending payload diverged"
        );

        // 3. Available messages on the replica match the primary's
        //    view: only gamma is in the queue (alpha was acked, beta
        //    moved to DLQ) and it's pending → available list is empty.
        let primary_store_for_read = PrimaryQueueStore::new(primary_rt.clone());
        let primary_available =
            primary_store_for_read.available_messages("qrep", QueueSide::Left);
        let replica_available = replica_store.available_messages("qrep", QueueSide::Left);
        assert_eq!(
            primary_available, replica_available,
            "available_messages diverged"
        );
        assert!(
            replica_available.is_empty(),
            "gamma is pending; nothing should be available"
        );

        // 4. Attempt counter survives replay: gamma's attempts == 0
        //    (no nack on the active delivery); beta's attempts row was
        //    cleared on retire so it's also 0; the persisted counter
        //    matches what `PrimaryQueueStore` sees.
        let gamma_msg_id = primary_rt
            .db()
            .store()
            .get_collection("qrep")
            .and_then(|mgr| {
                mgr.query_all(|e| {
                    matches!(e.kind, EntityKind::QueueMessage { .. })
                        && matches!(&e.data, EntityData::QueueMessage(d) if d.payload == Value::text("gamma"))
                })
                .into_iter()
                .next()
                .map(|e| e.id.raw())
            })
            .expect("gamma message id");
        assert_eq!(
            observed_attempts(&replica_store, "qrep", gamma_msg_id, "workers"),
            0,
            "gamma should have zero attempts post-deliver",
        );

        // 5. Mutation surface on the replica still fails closed —
        //    confirms QueueLifecycle was *not* invoked on the replica.
        assert!(matches!(
            replica_store.ack_pending(gamma_delivery).unwrap_err(),
            QueueStoreError::ReplicaImmutable
        ));
    }

    #[test]
    fn find_pending_by_key_conforms_across_primary_replica_inmemory() {
        // Trait conformance for `QueueStore::find_pending_by_key` (issue
        // #599 prereq A): all three adapters must answer the same shape
        // of question — Some(delivery_id) for a seeded
        // `(queue, message_id, group)` tuple, None for a tuple that
        // was never marked pending. Delivery-id strings differ across
        // adapters (each store mints its own), so each call is
        // cross-checked against the id that adapter's `mark_pending`
        // returned, not against the other adapters' ids.

        use crate::storage::queue::lifecycle::InMemoryQueueStore;

        let deadline = Instant::now() + Duration::from_secs(60);

        // 1. InMemory: direct mark_pending.
        let mem = InMemoryQueueStore::new();
        mem.seed_queue("q", vec![1, 2]);
        let mem_id = mem.mark_pending("q", 1, "g", deadline).expect("mem mark");
        assert_eq!(
            mem.find_pending_by_key("q", 1, "g"),
            Some(mem_id),
            "in-memory: seeded tuple must resolve to its delivery_id",
        );
        assert_eq!(
            mem.find_pending_by_key("q", 1, "other"),
            None,
            "in-memory: different group must miss",
        );
        assert_eq!(
            mem.find_pending_by_key("q", 2, "g"),
            None,
            "in-memory: un-marked message must miss",
        );

        // 2. Primary: real adapter, mark_pending against the runtime.
        let primary_rt = boot();
        primary_rt
            .execute_query("CREATE QUEUE qpk")
            .expect("create");
        push(&primary_rt, "qpk", "payload");
        let primary_store = PrimaryQueueStore::new(primary_rt.clone());
        let msg_id = primary_rt
            .db()
            .store()
            .get_collection("qpk")
            .and_then(|mgr| {
                mgr.query_all(|e| matches!(e.kind, EntityKind::QueueMessage { .. }))
                    .into_iter()
                    .next()
                    .map(|e| e.id.raw())
            })
            .expect("seeded message");
        let primary_id = primary_store
            .mark_pending("qpk", msg_id, "workers", deadline)
            .expect("primary mark");
        assert_eq!(
            primary_store.find_pending_by_key("qpk", msg_id, "workers"),
            Some(primary_id.clone()),
            "primary: seeded tuple must resolve to its delivery_id",
        );
        assert_eq!(
            primary_store.find_pending_by_key("qpk", msg_id, "other"),
            None,
            "primary: different group must miss",
        );
        assert_eq!(
            primary_store.find_pending_by_key("qpk", msg_id + 999, "workers"),
            None,
            "primary: unknown message must miss",
        );

        // 3. Replica: replay the primary's meta + queue collections onto
        //    a fresh runtime; same key must resolve to the same
        //    delivery_id the primary minted (the CDC stream carries it
        //    byte-for-byte).
        let replica_rt = boot();
        replica_rt
            .execute_query("CREATE QUEUE qpk")
            .expect("replica create");
        replay_collections(
            &primary_rt.db().store(),
            &replica_rt.db(),
            &["qpk", QUEUE_META_COLLECTION],
        );
        let replica_store = ReplicaQueueStore::new(replica_rt);
        assert_eq!(
            replica_store.find_pending_by_key("qpk", msg_id, "workers"),
            Some(primary_id),
            "replica: seeded tuple must resolve to the primary's delivery_id",
        );
        assert_eq!(
            replica_store.find_pending_by_key("qpk", msg_id, "other"),
            None,
            "replica: different group must miss",
        );
        assert_eq!(
            replica_store.find_pending_by_key("qpk", msg_id + 999, "workers"),
            None,
            "replica: unknown message must miss",
        );
    }

    #[test]
    fn replica_reads_track_post_replay_state_with_no_background_work() {
        // Acceptance: no background work on the replica side — strictly
        // WAL-driven. This test boots a replica runtime, replays a
        // single deliver outcome, asserts the pending row is visible,
        // does *not* advance any timer or trigger any sweeper, and
        // confirms a second `available_messages` call returns the
        // same result. Nothing on the replica should have moved on
        // its own between the two calls.

        let primary_rt = boot();
        primary_rt
            .execute_query("CREATE QUEUE qpassive")
            .expect("create");
        push(&primary_rt, "qpassive", "stay-in-flight");

        let primary_store = PrimaryQueueStore::new(primary_rt.clone());
        let cfg = primary_store.lifecycle_config("qpassive");
        let lc = QueueLifecycle::new(primary_store, cfg);
        let d = lc.deliver("qpassive", "w", 1).expect("deliver");

        let replica_rt = boot();
        replica_rt
            .execute_query("CREATE QUEUE qpassive")
            .expect("replica create");
        replay_collections(
            &primary_rt.db().store(),
            &replica_rt.db(),
            &["qpassive", QUEUE_META_COLLECTION],
        );
        let replica_store = ReplicaQueueStore::new(replica_rt);

        // First observation: nothing available (one pending row).
        let before = replica_store.available_messages("qpassive", QueueSide::Left);
        assert!(before.is_empty(), "pending row blocks availability");
        assert!(replica_store
            .read_lock_deadline(&d[0].delivery_id)
            .is_some());

        // Second observation, no additional replay, no time advance:
        // identical result. (Confirms no sweeper / no background
        // mutation on the replica side.)
        let after = replica_store.available_messages("qpassive", QueueSide::Left);
        assert_eq!(before, after, "replica drifted with no WAL input");
    }
}
