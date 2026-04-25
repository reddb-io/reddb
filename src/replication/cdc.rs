//! Change Data Capture (CDC) — stream of database change events.
//!
//! Exposes entity mutations (insert, update, delete) as a pollable event stream.
//! Consumers poll with a cursor (LSN) to receive new events since their last position.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json::{Map, Value as JsonValue};

/// Type of change operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeOperation {
    Insert,
    Update,
    Delete,
}

impl ChangeOperation {
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "insert" => Some(Self::Insert),
            "update" => Some(Self::Update),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

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
}

/// Structured logical WAL record serialized into the replication buffer and
/// archived segments.
#[derive(Debug, Clone)]
pub struct ChangeRecord {
    pub lsn: u64,
    pub timestamp: u64,
    pub operation: ChangeOperation,
    pub collection: String,
    pub entity_id: u64,
    pub entity_kind: String,
    pub entity_bytes: Option<Vec<u8>>,
    pub metadata: Option<JsonValue>,
}

impl ChangeRecord {
    pub fn from_entity(
        lsn: u64,
        timestamp: u64,
        operation: ChangeOperation,
        collection: impl Into<String>,
        entity_kind: impl Into<String>,
        entity: &crate::storage::UnifiedEntity,
        format_version: u32,
        metadata: Option<JsonValue>,
    ) -> Self {
        let entity_bytes = match operation {
            ChangeOperation::Delete => None,
            ChangeOperation::Insert | ChangeOperation::Update => Some(
                crate::storage::UnifiedStore::serialize_entity(entity, format_version),
            ),
        };

        Self {
            lsn,
            timestamp,
            operation,
            collection: collection.into(),
            entity_id: entity.id.raw(),
            entity_kind: entity_kind.into(),
            entity_bytes,
            metadata,
        }
    }

    pub fn to_json_value(&self) -> JsonValue {
        let mut object = Map::new();
        object.insert("lsn".to_string(), JsonValue::Number(self.lsn as f64));
        object.insert(
            "timestamp".to_string(),
            JsonValue::Number(self.timestamp as f64),
        );
        object.insert(
            "operation".to_string(),
            JsonValue::String(self.operation.as_str().to_string()),
        );
        object.insert(
            "collection".to_string(),
            JsonValue::String(self.collection.clone()),
        );
        object.insert(
            "entity_id".to_string(),
            JsonValue::Number(self.entity_id as f64),
        );
        object.insert(
            "entity_kind".to_string(),
            JsonValue::String(self.entity_kind.clone()),
        );
        if let Some(bytes) = &self.entity_bytes {
            object.insert(
                "entity_bytes_hex".to_string(),
                JsonValue::String(hex::encode(bytes)),
            );
        }
        if let Some(metadata) = &self.metadata {
            object.insert("metadata".to_string(), metadata.clone());
        }
        JsonValue::Object(object)
    }

    pub fn encode(&self) -> Vec<u8> {
        crate::json::to_string(&self.to_json_value())
            .unwrap_or_else(|_| "{}".to_string())
            .into_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|err| err.to_string())?;
        let value = crate::json::from_str::<JsonValue>(text).map_err(|err| err.to_string())?;
        let operation = value
            .get("operation")
            .and_then(JsonValue::as_str)
            .and_then(ChangeOperation::from_str)
            .ok_or_else(|| "invalid replication operation".to_string())?;
        let entity_bytes = value
            .get("entity_bytes_hex")
            .and_then(JsonValue::as_str)
            .map(hex::decode)
            .transpose()
            .map_err(|err| err.to_string())?;

        Ok(Self {
            lsn: value.get("lsn").and_then(JsonValue::as_u64).unwrap_or(0),
            timestamp: value
                .get("timestamp")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            operation,
            collection: value
                .get("collection")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string(),
            entity_id: value
                .get("entity_id")
                .and_then(JsonValue::as_u64)
                .unwrap_or(0),
            entity_kind: value
                .get("entity_kind")
                .and_then(JsonValue::as_str)
                .unwrap_or("entity")
                .to_string(),
            entity_bytes,
            metadata: value.get("metadata").cloned(),
        })
    }
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
    ) where
        I: IntoIterator<Item = u64>,
        I::IntoIter: ExactSizeIterator,
    {
        let iter = entity_ids.into_iter();
        let count = iter.len();
        if count == 0 {
            return;
        }

        let first_lsn = self.next_lsn.fetch_add(count as u64, Ordering::AcqRel) + 1;
        if self.max_size == 0 {
            return;
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
            });
        }
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
            lsn: 7,
            timestamp: 1234,
            operation: ChangeOperation::Update,
            collection: "users".to_string(),
            entity_id: 42,
            entity_kind: "row".to_string(),
            entity_bytes: Some(vec![1, 2, 3]),
            metadata: Some(crate::json!({"role": "admin"})),
        };

        let decoded = ChangeRecord::decode(&record.encode()).expect("decode");
        assert_eq!(decoded.lsn, record.lsn);
        assert_eq!(decoded.collection, record.collection);
        assert_eq!(decoded.entity_id, record.entity_id);
        assert_eq!(decoded.entity_bytes, record.entity_bytes);
    }
}
