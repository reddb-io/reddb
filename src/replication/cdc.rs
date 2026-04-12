//! Change Data Capture (CDC) — stream of database change events.
//!
//! Exposes entity mutations (insert, update, delete) as a pollable event stream.
//! Consumers poll with a cursor (LSN) to receive new events since their last position.

use std::collections::VecDeque;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Type of change operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeOperation {
    Insert,
    Update,
    Delete,
}

impl ChangeOperation {
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
}

/// Internal state protected by a single lock (prevents lock-ordering deadlocks).
struct CdcState {
    current_lsn: u64,
    events: VecDeque<ChangeEvent>,
}

/// CDC event buffer — circular buffer of change events.
pub struct CdcBuffer {
    state: RwLock<CdcState>,
    max_size: usize,
}

impl CdcBuffer {
    /// Create a new CDC buffer with maximum capacity.
    pub fn new(max_size: usize) -> Self {
        Self {
            state: RwLock::new(CdcState {
                current_lsn: 0,
                events: VecDeque::with_capacity(max_size.min(10_000)),
            }),
            max_size,
        }
    }

    /// Emit a change event into the buffer.
    pub fn emit(
        &self,
        operation: ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
    ) -> u64 {
        let mut state = self.state.write().unwrap_or_else(|e| e.into_inner());
        state.current_lsn += 1;
        let event_lsn = state.current_lsn;

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
        };

        if state.events.len() >= self.max_size {
            state.events.pop_front();
        }
        state.events.push_back(event);

        event_lsn
    }

    /// Poll for events since a given LSN.
    pub fn poll(&self, since_lsn: u64, max_count: usize) -> Vec<ChangeEvent> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state
            .events
            .iter()
            .filter(|e| e.lsn > since_lsn)
            .take(max_count)
            .cloned()
            .collect()
    }

    /// Get the current (latest) LSN.
    pub fn current_lsn(&self) -> u64 {
        self.state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .current_lsn
    }

    /// Get the oldest available LSN (or None if empty).
    pub fn oldest_lsn(&self) -> Option<u64> {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        state.events.front().map(|e| e.lsn)
    }

    /// Get buffer stats (single lock acquisition — no deadlock risk).
    pub fn stats(&self) -> CdcStats {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        CdcStats {
            buffered_events: state.events.len(),
            current_lsn: state.current_lsn,
            oldest_lsn: state.events.front().map(|e| e.lsn),
            newest_lsn: state.events.back().map(|e| e.lsn),
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
}
