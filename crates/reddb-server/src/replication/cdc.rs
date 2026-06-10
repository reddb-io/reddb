//! Change Data Capture (CDC) — stream of database change events.
//!
//! Exposes entity mutations (insert, update, delete) as a pollable event stream.
//! Consumers poll with a cursor (LSN) to receive new events since their last position.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json::{Map, Value as JsonValue};

pub use reddb_wire::replication::{public_item_kind, ChangeOperation, ChangeRecord};
// Issue #991 — range-authority fence types travel with the ChangeRecord
// contract; re-exported separately to keep the pinned contract line above
// byte-for-byte (see `protocol_authority` reddb-wire test).
pub use reddb_wire::replication::{RangeAdmitError, RangeAuthority};
// Issue #992 — range-indexed WAL streaming / per-range catch-up primitives.
// The filtering and per-range progress contract lives in reddb-wire; the
// server drives them over the single physical WAL's derived stream.
pub use reddb_wire::replication::{
    classify_range_record, plan_range_catchup, RangeCatchupPlan, RangeProgressTracker,
    RangeStreamDecision, RangeStreamPosition, RangeStreamProgress, RangeStreamReject,
};

/// A single change event.
#[derive(Debug, Clone)]
pub struct ChangeEvent {
    /// Monotonically increasing sequence number
    pub lsn: u64,
    /// When the change occurred (unix ms)
    pub timestamp: u64,
    /// Type of operation
    pub operation: ChangeOperation,
    /// Collection name
    pub collection: String,
    /// Entity ID affected
    pub entity_id: u64,
    /// Entity kind (table, graph_node, graph_edge, vector, etc.)
    pub entity_kind: String,
    /// For `Update` events, the list of column names whose values
    /// changed (including added/removed columns). `None` when the
    /// emitter didn't compute a damage vector (inserts, deletes, and
    /// coarse-grained update paths that haven't been rewired yet).
    /// Downstream CDC consumers can use this to skip replaying
    /// updates that touched columns they don't care about.
    pub changed_columns: Option<Vec<String>>,
    /// Optional KV-specific payload for WATCH subscribers. Existing CDC
    /// consumers can ignore this field and continue using the generic shape.
    pub kv: Option<KvWatchEvent>,
}

impl ChangeEvent {
    pub fn rid(&self) -> u64 {
        self.entity_id
    }

    pub fn kind(&self) -> &'static str {
        public_item_kind(&self.entity_kind)
    }
}

/// A committed single-key KV change surfaced by WATCH.
#[derive(Debug, Clone, PartialEq)]
pub struct KvWatchEvent {
    pub collection: String,
    pub key: String,
    pub op: ChangeOperation,
    pub before: Option<JsonValue>,
    pub after: Option<JsonValue>,
    pub lsn: u64,
    pub committed_at: u64,
    pub dropped_event_count: u64,
}

impl KvWatchEvent {
    pub fn to_json_value(&self) -> JsonValue {
        let mut object = Map::new();
        object.insert("key".to_string(), JsonValue::String(self.key.clone()));
        object.insert(
            "op".to_string(),
            JsonValue::String(self.op.as_str().to_string()),
        );
        object.insert(
            "before".to_string(),
            self.before.clone().unwrap_or(JsonValue::Null),
        );
        object.insert(
            "after".to_string(),
            self.after.clone().unwrap_or(JsonValue::Null),
        );
        object.insert("lsn".to_string(), JsonValue::Number(self.lsn as f64));
        object.insert(
            "committed_at".to_string(),
            JsonValue::Number(self.committed_at as f64),
        );
        object.insert(
            "dropped_event_count".to_string(),
            JsonValue::Number(self.dropped_event_count as f64),
        );
        JsonValue::Object(object)
    }
}

pub fn change_record_from_entity(
    lsn: u64,
    timestamp: u64,
    operation: ChangeOperation,
    collection: impl Into<String>,
    entity_kind: impl Into<String>,
    entity: &crate::storage::UnifiedEntity,
    format_version: u32,
    metadata: Option<JsonValue>,
) -> ChangeRecord {
    let entity_bytes = match operation {
        ChangeOperation::Delete | ChangeOperation::Refresh => None,
        ChangeOperation::Insert | ChangeOperation::Update => Some(
            crate::storage::UnifiedStore::serialize_entity(entity, format_version),
        ),
    };

    ChangeRecord {
        term: crate::replication::DEFAULT_REPLICATION_TERM,
        lsn,
        timestamp,
        operation,
        collection: collection.into(),
        entity_id: entity.id.raw(),
        entity_kind: entity_kind.into(),
        entity_bytes,
        metadata: metadata.map(server_json_to_wire_json),
        refresh_records: None,
        // Issue #991 — range authority is stamped by callers that route the
        // change through the ownership catalog via `with_range_authority`;
        // the base builder leaves it unset so non-range paths are unaffected.
        range_id: None,
        ownership_epoch: None,
    }
}

pub fn server_json_to_wire_json(
    value: JsonValue,
) -> reddb_wire::replication::ChangeRecordJsonValue {
    reddb_wire::replication::parse_change_record_json_value(&value.to_string_compact())
        .unwrap_or(reddb_wire::replication::ChangeRecordJsonValue::Null)
}

pub fn wire_json_to_server_json(
    value: &reddb_wire::replication::ChangeRecordJsonValue,
) -> JsonValue {
    crate::json::from_str(&reddb_wire::replication::change_record_json_value_to_string(value))
        .unwrap_or(JsonValue::Null)
}

/// CDC event buffer — circular buffer of change events.
///
/// Splits the "next LSN" counter (write-contended on every emit)
/// from the event ring (short-hold push/pop) so that concurrent
/// emitters don't serialise on a single RwLock. The previous
/// design used one `RwLock<CdcState>` that turned every insert
/// into a write-lock acquire, capping 16-way concurrent writes
/// at ~1000 ops/s (each writer paid ~1ms queueing for the same
/// mutex even though the work it guarded was a one-line VecDeque
/// push).
///
/// New layout:
///   - LSN is an `AtomicU64`, assigned with `fetch_add(1)`.
///     Zero contention.
///   - Events are guarded by a `parking_lot::Mutex<VecDeque>`.
///     The critical section is `pop_front (if full) + push_back`
///     — microseconds at most, parking-free at low contention.
///
/// Readers (`poll`, `current_lsn`, `stats`) take the same mutex
/// briefly; they're cold paths compared to the write hot path.
pub struct CdcBuffer {
    next_lsn: AtomicU64,
    events: parking_lot::Mutex<VecDeque<ChangeEvent>>,
    max_size: usize,
}

impl CdcBuffer {
    /// Create a new CDC buffer with maximum capacity.
    pub fn new(max_size: usize) -> Self {
        Self {
            next_lsn: AtomicU64::new(0),
            events: parking_lot::Mutex::new(VecDeque::with_capacity(max_size.min(10_000))),
            max_size,
        }
    }

    /// Emit a change event into the buffer. `changed_columns`
    /// defaults to `None` for backwards compatibility; call sites
    /// that have a damage vector available should use
    /// [`Self::emit_with_columns`] instead.
    pub fn emit(
        &self,
        operation: ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
    ) -> u64 {
        self.emit_with_columns(operation, collection, entity_id, entity_kind, None)
    }

    /// Emit a change event with an optional list of column names
    /// that were affected. Use from update paths that have already
    /// computed a [`RowDamageVector`](crate::application::entity::RowDamageVector)
    /// so CDC consumers can filter by touched column without re-diffing.
    pub fn emit_with_columns(
        &self,
        operation: ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
        changed_columns: Option<Vec<String>>,
    ) -> u64 {
        // LSN assignment is lock-free — multiple emitters each get a
        // unique monotonic LSN without waiting on any other emitter.
        let event_lsn = self.next_lsn.fetch_add(1, Ordering::AcqRel) + 1;

        let event = ChangeEvent {
            lsn: event_lsn,
            timestamp: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            operation,
            collection: collection.to_string(),
            entity_id,
            entity_kind: entity_kind.to_string(),
            changed_columns,
            kv: None,
        };

        // Short-hold ring push. Under heavy contention parking_lot
        // spins a few times before parking, so typical hold time is
        // a couple hundred nanoseconds.
        let mut events = self.events.lock();
        if events.len() >= self.max_size {
            events.pop_front();
        }
        events.push_back(event);

        event_lsn
    }

    /// Emit many same-collection events with one LSN reservation and one
    /// ring-buffer lock. This is used by bulk insert paths that do not need
    /// per-row logical-WAL records.
    pub fn emit_batch_same_collection<I>(
        &self,
        operation: ChangeOperation,
        collection: &str,
        entity_kind: &str,
        entity_ids: I,
    ) -> Vec<u64>
    where
        I: IntoIterator<Item = u64>,
        I::IntoIter: ExactSizeIterator,
    {
        let iter = entity_ids.into_iter();
        let count = iter.len();
        if count == 0 {
            return Vec::new();
        }

        let first_lsn = self.next_lsn.fetch_add(count as u64, Ordering::AcqRel) + 1;
        let lsns = (0..count)
            .map(|idx| first_lsn + idx as u64)
            .collect::<Vec<_>>();
        if self.max_size == 0 {
            return lsns;
        }

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let collection = collection.to_string();
        let entity_kind = entity_kind.to_string();

        let skip = count.saturating_sub(self.max_size);
        let kept = count - skip;
        let mut events = self.events.lock();
        let overflow = events
            .len()
            .saturating_add(kept)
            .saturating_sub(self.max_size);
        for _ in 0..overflow {
            events.pop_front();
        }

        for (idx, entity_id) in iter.enumerate().skip(skip) {
            events.push_back(ChangeEvent {
                lsn: first_lsn + idx as u64,
                timestamp,
                operation,
                collection: collection.clone(),
                entity_id,
                entity_kind: entity_kind.clone(),
                changed_columns: None,
                kv: None,
            });
        }
        lsns
    }

    /// Emit a committed logical KV event into the same CDC ring used by
    /// result-cache invalidation and `/changes` consumers.
    pub fn emit_kv(
        &self,
        operation: ChangeOperation,
        collection: &str,
        key: &str,
        entity_id: u64,
        before: Option<JsonValue>,
        after: Option<JsonValue>,
    ) -> u64 {
        let event_lsn = self.next_lsn.fetch_add(1, Ordering::AcqRel) + 1;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let kv = KvWatchEvent {
            collection: collection.to_string(),
            key: key.to_string(),
            op: operation,
            before,
            after,
            lsn: event_lsn,
            committed_at: timestamp,
            dropped_event_count: 0,
        };
        let event = ChangeEvent {
            lsn: event_lsn,
            timestamp,
            operation,
            collection: collection.to_string(),
            entity_id,
            entity_kind: "kv".to_string(),
            changed_columns: Some(vec!["value".to_string()]),
            kv: Some(kv),
        };

        let mut events = self.events.lock();
        if events.len() >= self.max_size {
            events.pop_front();
        }
        events.push_back(event);
        event_lsn
    }

    /// Poll for events since a given LSN.
    pub fn poll(&self, since_lsn: u64, max_count: usize) -> Vec<ChangeEvent> {
        let events = self.events.lock();
        events
            .iter()
            .filter(|e| e.lsn > since_lsn)
            .take(max_count)
            .cloned()
            .collect()
    }

    /// Get the current (latest) LSN.
    pub fn current_lsn(&self) -> u64 {
        self.next_lsn.load(Ordering::Acquire)
    }

    /// Restore the LSN cursor after process restart. Only advances;
    /// never rewinds. Under concurrent emit this is guarded by a
    /// compare-exchange loop.
    pub fn set_current_lsn(&self, lsn: u64) {
        let mut current = self.next_lsn.load(Ordering::Acquire);
        while lsn > current {
            match self
                .next_lsn
                .compare_exchange(current, lsn, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
    }

    /// Get the oldest available LSN (or None if empty).
    pub fn oldest_lsn(&self) -> Option<u64> {
        self.events.lock().front().map(|e| e.lsn)
    }

    /// Get buffer stats (single lock acquisition — no deadlock risk).
    pub fn stats(&self) -> CdcStats {
        let events = self.events.lock();
        CdcStats {
            buffered_events: events.len(),
            current_lsn: self.next_lsn.load(Ordering::Acquire),
            oldest_lsn: events.front().map(|e| e.lsn),
            newest_lsn: events.back().map(|e| e.lsn),
        }
    }
}

/// CDC buffer statistics.
#[derive(Debug, Clone)]
pub struct CdcStats {
    pub buffered_events: usize,
    pub current_lsn: u64,
    pub oldest_lsn: Option<u64>,
    pub newest_lsn: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emit_and_poll() {
        let buf = CdcBuffer::new(100);
        buf.emit(ChangeOperation::Insert, "users", 1, "table");
        buf.emit(ChangeOperation::Update, "users", 1, "table");
        buf.emit(ChangeOperation::Delete, "users", 1, "table");

        let events = buf.poll(0, 10);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].operation, ChangeOperation::Insert);
        assert_eq!(events[1].operation, ChangeOperation::Update);
        assert_eq!(events[2].operation, ChangeOperation::Delete);
        // Backwards-compat emit should leave changed_columns None.
        assert!(events[0].changed_columns.is_none());
        assert!(events[1].changed_columns.is_none());
    }

    #[test]
    fn test_emit_with_columns_propagates_damage_vector() {
        let buf = CdcBuffer::new(100);
        buf.emit_with_columns(
            ChangeOperation::Update,
            "users",
            7,
            "table",
            Some(vec!["email".to_string(), "age".to_string()]),
        );
        buf.emit(ChangeOperation::Update, "users", 8, "table");

        let events = buf.poll(0, 10);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].changed_columns.as_deref(),
            Some(vec!["email".to_string(), "age".to_string()].as_slice())
        );
        assert!(events[1].changed_columns.is_none());
    }

    #[test]
    fn test_poll_with_cursor() {
        let buf = CdcBuffer::new(100);
        buf.emit(ChangeOperation::Insert, "a", 1, "table");
        buf.emit(ChangeOperation::Insert, "b", 2, "table");
        buf.emit(ChangeOperation::Insert, "c", 3, "table");

        // Poll from lsn=1, should get events 2 and 3
        let events = buf.poll(1, 10);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].collection, "b");
        assert_eq!(events[1].collection, "c");
    }

    #[test]
    fn test_emit_batch_same_collection_assigns_contiguous_lsns() {
        let buf = CdcBuffer::new(100);
        buf.emit(ChangeOperation::Insert, "users", 10, "table");
        buf.emit_batch_same_collection(ChangeOperation::Insert, "users", "table", [11, 12, 13]);

        let events = buf.poll(0, 10);
        assert_eq!(events.len(), 4);
        assert_eq!(events[1].lsn, 2);
        assert_eq!(events[2].lsn, 3);
        assert_eq!(events[3].lsn, 4);
        assert_eq!(events[3].entity_id, 13);
        assert_eq!(buf.current_lsn(), 4);
    }

    #[test]
    fn test_emit_batch_same_collection_respects_ring_size() {
        let buf = CdcBuffer::new(3);
        buf.emit_batch_same_collection(ChangeOperation::Insert, "users", "table", [1, 2, 3, 4, 5]);

        let events = buf.poll(0, 10);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events
                .iter()
                .map(|event| event.entity_id)
                .collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        assert_eq!(events[0].lsn, 3);
        assert_eq!(events[2].lsn, 5);
        assert_eq!(buf.current_lsn(), 5);
    }

    #[test]
    fn test_circular_eviction() {
        let buf = CdcBuffer::new(3);
        buf.emit(ChangeOperation::Insert, "a", 1, "table");
        buf.emit(ChangeOperation::Insert, "b", 2, "table");
        buf.emit(ChangeOperation::Insert, "c", 3, "table");
        buf.emit(ChangeOperation::Insert, "d", 4, "table"); // evicts "a"

        let events = buf.poll(0, 10);
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].collection, "b"); // "a" was evicted
    }

    #[test]
    fn test_stats() {
        let buf = CdcBuffer::new(100);
        buf.emit(ChangeOperation::Insert, "x", 1, "table");
        buf.emit(ChangeOperation::Insert, "y", 2, "table");

        let stats = buf.stats();
        assert_eq!(stats.buffered_events, 2);
        assert_eq!(stats.current_lsn, 2);
        assert_eq!(stats.oldest_lsn, Some(1));
        assert_eq!(stats.newest_lsn, Some(2));
    }

    #[test]
    fn test_change_record_roundtrip() {
        let record = ChangeRecord {
            term: 3,
            lsn: 7,
            timestamp: 1234,
            operation: ChangeOperation::Update,
            collection: "users".to_string(),
            entity_id: 42,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1, 2, 3]),
            metadata: Some(server_json_to_wire_json(crate::json!({"role": "admin"}))),
            refresh_records: None,
            range_id: None,
            ownership_epoch: None,
        };

        let decoded = ChangeRecord::decode(&record.encode()).expect("decode");
        assert_eq!(decoded.term, record.term);
        assert_eq!(decoded.lsn, record.lsn);
        assert_eq!(decoded.collection, record.collection);
        assert_eq!(decoded.entity_id, record.entity_id);
        assert_eq!(decoded.entity_bytes, record.entity_bytes);
    }

    /// Issue #596 slice 9d — refresh records survive a JSON round-trip
    /// through the logical-WAL wire format the primary writes and the
    /// replica reads. Bit-for-bit equality on every payload byte is the
    /// contract — the replica calls `bulk_insert` with whatever bytes
    /// land in `refresh_records`, so a decode that silently drops or
    /// re-orders them would silently diverge replica state.
    #[test]
    fn test_change_record_refresh_roundtrip() {
        let records = vec![vec![0x10, 0x20, 0x30], vec![0xAA, 0xBB], Vec::new()];
        let record =
            ChangeRecord::for_refresh(11, 99, "mv_orders_summary", records.clone()).with_term(4);

        let decoded = ChangeRecord::decode(&record.encode()).expect("decode");
        assert_eq!(decoded.term, 4);
        assert_eq!(decoded.operation, ChangeOperation::Refresh);
        assert_eq!(decoded.collection, "mv_orders_summary");
        assert_eq!(decoded.refresh_records.as_deref(), Some(&records[..]));
    }

    #[test]
    fn test_change_record_legacy_payload_defaults_term() {
        let legacy = br#"{"lsn":9,"timestamp":1,"operation":"delete","collection":"users","rid":5,"kind":"row"}"#;
        let decoded = ChangeRecord::decode(legacy).expect("decode legacy record");
        assert_eq!(decoded.term, crate::replication::DEFAULT_REPLICATION_TERM);
        assert_eq!(decoded.lsn, 9);
    }
}
