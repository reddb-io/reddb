//! Consumer Groups for Queue
//!
//! Multiple consumers can read from the same queue. Each consumer group
//! tracks which messages have been delivered and acknowledged.

use std::collections::{BTreeMap, HashMap};

/// A pending message awaiting acknowledgment
#[derive(Debug, Clone)]
pub struct PendingEntry {
    /// Message sequence number
    pub seq: u64,
    /// Consumer that received it
    pub consumer: String,
    /// When it was delivered (nanoseconds)
    pub delivered_at_ns: u64,
    /// How many times this message has been delivered
    pub delivery_count: u32,
}

/// Consumer state within a group
#[derive(Debug, Clone)]
pub struct ConsumerState {
    /// Consumer name
    pub name: String,
    /// Number of pending (unacked) messages
    pub pending_count: usize,
}

/// A consumer group manages message delivery and acknowledgment
/// for a set of consumers reading from the same queue.
pub struct ConsumerGroup {
    /// Group name
    pub name: String,
    /// Pending messages: seq → PendingEntry
    pending: BTreeMap<u64, PendingEntry>,
    /// Consumer states
    consumers: HashMap<String, ConsumerState>,
    /// Last delivered sequence number
    last_delivered_seq: u64,
}

impl ConsumerGroup {
    /// Create a new consumer group
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            pending: BTreeMap::new(),
            consumers: HashMap::new(),
            last_delivered_seq: 0,
        }
    }

    /// Register a consumer (or return existing)
    pub fn add_consumer(&mut self, name: &str) {
        self.consumers
            .entry(name.to_string())
            .or_insert(ConsumerState {
                name: name.to_string(),
                pending_count: 0,
            });
    }

    /// Record that a message was delivered to a consumer
    pub fn deliver(&mut self, seq: u64, consumer: &str) {
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;

        let delivery_count = self
            .pending
            .get(&seq)
            .map(|p| p.delivery_count + 1)
            .unwrap_or(1);

        self.pending.insert(
            seq,
            PendingEntry {
                seq,
                consumer: consumer.to_string(),
                delivered_at_ns: now_ns,
                delivery_count,
            },
        );

        if let Some(state) = self.consumers.get_mut(consumer) {
            state.pending_count += 1;
        }

        if seq > self.last_delivered_seq {
            self.last_delivered_seq = seq;
        }
    }

    /// Acknowledge a message (remove from pending)
    pub fn ack(&mut self, seq: u64) -> bool {
        if let Some(entry) = self.pending.remove(&seq) {
            if let Some(state) = self.consumers.get_mut(&entry.consumer) {
                state.pending_count = state.pending_count.saturating_sub(1);
            }
            true
        } else {
            false
        }
    }

    /// Negative acknowledge: re-make the message available
    pub fn nack(&mut self, seq: u64) -> bool {
        self.pending.remove(&seq).is_some()
    }

    /// Get all pending entries for a consumer
    pub fn pending_for_consumer(&self, consumer: &str) -> Vec<&PendingEntry> {
        self.pending
            .values()
            .filter(|p| p.consumer == consumer)
            .collect()
    }

    /// Get all pending entries
    pub fn all_pending(&self) -> Vec<&PendingEntry> {
        self.pending.values().collect()
    }

    /// Number of pending messages
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// List consumers
    pub fn consumers(&self) -> Vec<&ConsumerState> {
        self.consumers.values().collect()
    }

    /// Check if a message is pending
    pub fn is_pending(&self, seq: u64) -> bool {
        self.pending.contains_key(&seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_consumer_group_basic() {
        let mut group = ConsumerGroup::new("workers");
        group.add_consumer("worker1");
        group.add_consumer("worker2");

        group.deliver(1, "worker1");
        group.deliver(2, "worker2");

        assert_eq!(group.pending_count(), 2);
        assert!(group.is_pending(1));
        assert!(group.is_pending(2));
    }

    #[test]
    fn test_consumer_group_ack() {
        let mut group = ConsumerGroup::new("workers");
        group.add_consumer("worker1");

        group.deliver(1, "worker1");
        group.deliver(2, "worker1");

        assert!(group.ack(1));
        assert!(!group.is_pending(1));
        assert!(group.is_pending(2));
        assert_eq!(group.pending_count(), 1);
    }

    #[test]
    fn test_consumer_group_nack() {
        let mut group = ConsumerGroup::new("workers");
        group.add_consumer("worker1");

        group.deliver(1, "worker1");
        assert!(group.nack(1));
        assert!(!group.is_pending(1));
    }

    #[test]
    fn test_consumer_group_pending_for_consumer() {
        let mut group = ConsumerGroup::new("workers");
        group.add_consumer("w1");
        group.add_consumer("w2");

        group.deliver(1, "w1");
        group.deliver(2, "w2");
        group.deliver(3, "w1");

        let w1_pending = group.pending_for_consumer("w1");
        assert_eq!(w1_pending.len(), 2);
    }
}
