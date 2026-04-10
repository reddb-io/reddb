//! Core Queue / Deque implementation
//!
//! Uses a BTreeMap keyed by monotonically increasing sequence numbers
//! for FIFO ordering. For priority mode, keys are (Reverse(priority), sequence)
//! to ensure highest-priority messages are dequeued first.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::storage::schema::Value;

/// Which end of the queue to push/pop from
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueSide {
    Left,  // front (head)
    Right, // back (tail) — default for push
}

/// A message in the queue
#[derive(Debug, Clone)]
pub struct QueueMessage {
    /// Monotonic sequence number
    pub seq: u64,
    /// Message payload
    pub payload: Value,
    /// Optional priority (higher = more urgent)
    pub priority: Option<i32>,
    /// Enqueue timestamp (nanoseconds)
    pub enqueued_at_ns: u64,
    /// Delivery attempts
    pub attempts: u32,
}

/// Core queue data structure supporting FIFO, LIFO, and priority modes.
pub struct QueueStore {
    /// Messages keyed by sequence number
    messages: BTreeMap<u64, QueueMessage>,
    /// Next sequence number
    next_seq: AtomicU64,
    /// Maximum queue size (0 = unlimited)
    max_size: usize,
    /// Whether this is a priority queue
    priority_mode: bool,
    /// Priority index: (Reverse(priority), seq) for priority ordering
    priority_index: Option<BTreeMap<(std::cmp::Reverse<i32>, u64), u64>>,
}

impl QueueStore {
    /// Create a new FIFO queue
    pub fn new(max_size: usize) -> Self {
        Self {
            messages: BTreeMap::new(),
            next_seq: AtomicU64::new(1),
            max_size,
            priority_mode: false,
            priority_index: None,
        }
    }

    /// Create a priority queue
    pub fn new_priority(max_size: usize) -> Self {
        Self {
            messages: BTreeMap::new(),
            next_seq: AtomicU64::new(1),
            max_size,
            priority_mode: true,
            priority_index: Some(BTreeMap::new()),
        }
    }

    /// Push a message to the back (RPUSH). Returns the sequence number.
    pub fn push_back(&mut self, payload: Value, priority: Option<i32>) -> Result<u64, QueueError> {
        if self.max_size > 0 && self.messages.len() >= self.max_size {
            return Err(QueueError::Full);
        }
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let msg = QueueMessage {
            seq,
            payload,
            priority,
            enqueued_at_ns: now_ns,
            attempts: 0,
        };
        if let Some(ref mut idx) = self.priority_index {
            idx.insert((std::cmp::Reverse(priority.unwrap_or(0)), seq), seq);
        }
        self.messages.insert(seq, msg);
        Ok(seq)
    }

    /// Push a message to the front (LPUSH). Returns the sequence number.
    pub fn push_front(&mut self, payload: Value, priority: Option<i32>) -> Result<u64, QueueError> {
        // LPUSH uses seq=0 trick — we assign a sequence below the current minimum
        if self.max_size > 0 && self.messages.len() >= self.max_size {
            return Err(QueueError::Full);
        }
        let seq = self
            .messages
            .keys()
            .next()
            .copied()
            .unwrap_or(1)
            .saturating_sub(1);
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        let msg = QueueMessage {
            seq,
            payload,
            priority,
            enqueued_at_ns: now_ns,
            attempts: 0,
        };
        if let Some(ref mut idx) = self.priority_index {
            idx.insert((std::cmp::Reverse(priority.unwrap_or(0)), seq), seq);
        }
        self.messages.insert(seq, msg);
        Ok(seq)
    }

    /// Pop from the front (LPOP) — FIFO dequeue. For priority queues, pops highest priority.
    pub fn pop_front(&mut self) -> Option<QueueMessage> {
        if self.priority_mode {
            if let Some(ref mut idx) = self.priority_index {
                let key = idx.keys().next().copied()?;
                let seq = idx.remove(&key)?;
                return self.messages.remove(&seq);
            }
        }
        let seq = *self.messages.keys().next()?;
        let msg = self.messages.remove(&seq)?;
        if let Some(ref mut idx) = self.priority_index {
            idx.remove(&(std::cmp::Reverse(msg.priority.unwrap_or(0)), seq));
        }
        Some(msg)
    }

    /// Pop from the back (RPOP) — LIFO dequeue
    pub fn pop_back(&mut self) -> Option<QueueMessage> {
        let seq = *self.messages.keys().next_back()?;
        let msg = self.messages.remove(&seq)?;
        if let Some(ref mut idx) = self.priority_index {
            idx.remove(&(std::cmp::Reverse(msg.priority.unwrap_or(0)), seq));
        }
        Some(msg)
    }

    /// Peek at the front message without removing it
    pub fn peek_front(&self, count: usize) -> Vec<&QueueMessage> {
        if self.priority_mode {
            if let Some(ref idx) = self.priority_index {
                return idx
                    .values()
                    .take(count)
                    .filter_map(|seq| self.messages.get(seq))
                    .collect();
            }
        }
        self.messages.values().take(count).collect()
    }

    /// Number of messages in the queue
    pub fn len(&self) -> usize {
        self.messages.len()
    }

    /// Whether the queue is empty
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Whether the queue is full
    pub fn is_full(&self) -> bool {
        self.max_size > 0 && self.messages.len() >= self.max_size
    }

    /// Remove all messages
    pub fn purge(&mut self) -> usize {
        let count = self.messages.len();
        self.messages.clear();
        if let Some(ref mut idx) = self.priority_index {
            idx.clear();
        }
        count
    }

    /// Get a message by sequence number (for ack/nack)
    pub fn get(&self, seq: u64) -> Option<&QueueMessage> {
        self.messages.get(&seq)
    }

    /// Remove a specific message by sequence (for ack)
    pub fn remove(&mut self, seq: u64) -> Option<QueueMessage> {
        let msg = self.messages.remove(&seq)?;
        if let Some(ref mut idx) = self.priority_index {
            idx.remove(&(std::cmp::Reverse(msg.priority.unwrap_or(0)), seq));
        }
        Some(msg)
    }

    /// Increment attempt count for a message
    pub fn increment_attempts(&mut self, seq: u64) -> Option<u32> {
        if let Some(msg) = self.messages.get_mut(&seq) {
            msg.attempts += 1;
            Some(msg.attempts)
        } else {
            None
        }
    }

    /// Whether this is a priority queue
    pub fn is_priority(&self) -> bool {
        self.priority_mode
    }

    /// Approximate memory usage
    pub fn memory_bytes(&self) -> usize {
        let mut size = std::mem::size_of::<Self>();
        size += self.messages.len() * (std::mem::size_of::<QueueMessage>() + 48);
        if let Some(ref idx) = self.priority_index {
            size += idx.len() * 32;
        }
        size
    }
}

/// Queue errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueError {
    /// Queue is at maximum capacity
    Full,
    /// Message not found
    NotFound(u64),
}

impl std::fmt::Display for QueueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full => write!(f, "queue is full"),
            Self::NotFound(seq) => write!(f, "message {} not found", seq),
        }
    }
}

impl std::error::Error for QueueError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_queue_fifo() {
        let mut q = QueueStore::new(0);
        q.push_back(Value::Text("first".into()), None).unwrap();
        q.push_back(Value::Text("second".into()), None).unwrap();
        q.push_back(Value::Text("third".into()), None).unwrap();

        assert_eq!(q.len(), 3);
        let msg = q.pop_front().unwrap();
        assert_eq!(msg.payload, Value::Text("first".into()));
        let msg = q.pop_front().unwrap();
        assert_eq!(msg.payload, Value::Text("second".into()));
    }

    #[test]
    fn test_queue_lifo() {
        let mut q = QueueStore::new(0);
        q.push_back(Value::Text("first".into()), None).unwrap();
        q.push_back(Value::Text("second".into()), None).unwrap();

        let msg = q.pop_back().unwrap();
        assert_eq!(msg.payload, Value::Text("second".into()));
    }

    #[test]
    fn test_queue_lpush() {
        let mut q = QueueStore::new(0);
        q.push_back(Value::Text("middle".into()), None).unwrap();
        q.push_front(Value::Text("front".into()), None).unwrap();

        let msg = q.pop_front().unwrap();
        assert_eq!(msg.payload, Value::Text("front".into()));
    }

    #[test]
    fn test_queue_max_size() {
        let mut q = QueueStore::new(2);
        assert!(q.push_back(Value::Integer(1), None).is_ok());
        assert!(q.push_back(Value::Integer(2), None).is_ok());
        assert_eq!(q.push_back(Value::Integer(3), None), Err(QueueError::Full));
        assert!(q.is_full());
    }

    #[test]
    fn test_queue_priority() {
        let mut q = QueueStore::new_priority(0);
        q.push_back(Value::Text("low".into()), Some(1)).unwrap();
        q.push_back(Value::Text("high".into()), Some(10)).unwrap();
        q.push_back(Value::Text("medium".into()), Some(5)).unwrap();

        // Highest priority should come first
        let msg = q.pop_front().unwrap();
        assert_eq!(msg.payload, Value::Text("high".into()));
        let msg = q.pop_front().unwrap();
        assert_eq!(msg.payload, Value::Text("medium".into()));
        let msg = q.pop_front().unwrap();
        assert_eq!(msg.payload, Value::Text("low".into()));
    }

    #[test]
    fn test_queue_peek() {
        let mut q = QueueStore::new(0);
        q.push_back(Value::Text("a".into()), None).unwrap();
        q.push_back(Value::Text("b".into()), None).unwrap();
        q.push_back(Value::Text("c".into()), None).unwrap();

        let peeked = q.peek_front(2);
        assert_eq!(peeked.len(), 2);
        assert_eq!(q.len(), 3); // not removed
    }

    #[test]
    fn test_queue_purge() {
        let mut q = QueueStore::new(0);
        q.push_back(Value::Integer(1), None).unwrap();
        q.push_back(Value::Integer(2), None).unwrap();

        let purged = q.purge();
        assert_eq!(purged, 2);
        assert!(q.is_empty());
    }

    #[test]
    fn test_queue_remove_by_seq() {
        let mut q = QueueStore::new(0);
        let seq1 = q.push_back(Value::Integer(1), None).unwrap();
        let seq2 = q.push_back(Value::Integer(2), None).unwrap();

        let removed = q.remove(seq1).unwrap();
        assert_eq!(removed.payload, Value::Integer(1));
        assert_eq!(q.len(), 1);
        assert!(q.get(seq2).is_some());
    }

    #[test]
    fn test_queue_attempts() {
        let mut q = QueueStore::new(0);
        let seq = q.push_back(Value::Text("msg".into()), None).unwrap();

        assert_eq!(q.get(seq).unwrap().attempts, 0);
        q.increment_attempts(seq);
        assert_eq!(q.get(seq).unwrap().attempts, 1);
        q.increment_attempts(seq);
        assert_eq!(q.get(seq).unwrap().attempts, 2);
    }
}
