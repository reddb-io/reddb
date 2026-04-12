//! Timestamp-based monotonic IDs for log entries.
//!
//! Layout (64 bits):
//! ┌──────────────────────────────────┬──────────┐
//! │ timestamp_us (52 bits)           │ seq (12) │
//! │ ~142 years of range from epoch   │ 4096/µs  │
//! └──────────────────────────────────┴──────────┘
//!
//! Properties:
//! - Monotonically increasing (natural time ordering)
//! - No collisions up to 4,095 entries per microsecond (~4B/sec theoretical)
//! - Sortable: ORDER BY id = ORDER BY time
//! - Extractable: timestamp_us = id >> 12, timestamp_ms = id >> 12 / 1000

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const SEQ_BITS: u64 = 12;
const SEQ_MASK: u64 = (1 << SEQ_BITS) - 1; // 0xFFF

/// Generator for timestamp-based log IDs.
pub struct LogIdGenerator {
    last_ts: AtomicU64,
    seq: AtomicU64,
}

impl LogIdGenerator {
    pub fn new() -> Self {
        Self {
            last_ts: AtomicU64::new(0),
            seq: AtomicU64::new(0),
        }
    }

    /// Generate the next log ID. Monotonically increasing, thread-safe.
    pub fn next(&self) -> LogId {
        let now_ns = now_micros();

        let prev_ts = self.last_ts.load(Ordering::SeqCst);

        if now_ns > prev_ts {
            self.last_ts.store(now_ns, Ordering::SeqCst);
            self.seq.store(0, Ordering::SeqCst);
            LogId((now_ns << SEQ_BITS) | 0)
        } else {
            let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
            if seq > SEQ_MASK {
                // Overflow: advance timestamp by 1 to guarantee monotonicity
                let advanced = prev_ts + 1;
                self.last_ts.store(advanced, Ordering::SeqCst);
                self.seq.store(0, Ordering::SeqCst);
                LogId((advanced << SEQ_BITS) | 0)
            } else {
                LogId((prev_ts << SEQ_BITS) | seq)
            }
        }
    }

    /// Restore generator state from the highest existing ID (for reload).
    pub fn restore(&self, max_id: u64) {
        let ts = max_id >> SEQ_BITS;
        let seq = max_id & SEQ_MASK;
        let mut current = self.last_ts.load(Ordering::SeqCst);
        while ts >= current {
            match self
                .last_ts
                .compare_exchange(current, ts, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => {
                    self.seq.store(seq + 1, Ordering::SeqCst);
                    break;
                }
                Err(updated) => current = updated,
            }
        }
    }
}

/// A timestamp-based log entry ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogId(pub u64);

impl LogId {
    pub fn raw(self) -> u64 {
        self.0
    }

    /// Extract the timestamp component in microseconds.
    pub fn timestamp_us(self) -> u64 {
        self.0 >> SEQ_BITS
    }

    /// Extract the timestamp in milliseconds.
    pub fn timestamp_ms(self) -> u64 {
        self.timestamp_us() / 1_000
    }

    /// Extract the sequence within the same microsecond.
    pub fn sequence(self) -> u16 {
        (self.0 & SEQ_MASK) as u16
    }

    /// Create a LogId from a timestamp in milliseconds (for range queries).
    pub fn from_ms(ms: u64) -> Self {
        Self((ms * 1_000) << SEQ_BITS)
    }

    /// Create a LogId from a timestamp in microseconds.
    pub fn from_us(us: u64) -> Self {
        Self(us << SEQ_BITS)
    }
}

impl std::fmt::Display for LogId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monotonic() {
        let gen = LogIdGenerator::new();
        let a = gen.next();
        let b = gen.next();
        let c = gen.next();
        assert!(b.raw() > a.raw(), "b > a");
        assert!(c.raw() > b.raw(), "c > b");
    }

    #[test]
    fn test_timestamp_extraction() {
        let gen = LogIdGenerator::new();
        let id = gen.next();
        let ts_us = id.timestamp_us();
        let now = now_micros();
        assert!((now - ts_us) < 1_000_000, "within 1 second");
    }

    #[test]
    fn test_sequence_within_same_ns() {
        let gen = LogIdGenerator::new();
        let a = gen.next();
        let b = gen.next();
        // Both should have same or adjacent timestamp, different seq
        assert_ne!(a.raw(), b.raw());
    }

    #[test]
    fn test_from_ms() {
        let id = LogId::from_ms(1712880000000);
        assert_eq!(id.timestamp_ms(), 1712880000000);
        assert_eq!(id.sequence(), 0);
    }

    #[test]
    fn test_restore() {
        let gen = LogIdGenerator::new();
        let first = gen.next();
        gen.restore(first.raw() + 1000);
        let after = gen.next();
        assert!(after.raw() > first.raw() + 1000);
    }

    #[test]
    fn test_high_throughput_no_collision() {
        let gen = LogIdGenerator::new();
        let mut ids = Vec::with_capacity(10000);
        for _ in 0..10000 {
            ids.push(gen.next().raw());
        }
        // All unique
        let mut deduped = ids.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(ids.len(), deduped.len(), "no collisions in 10K IDs");
        // Monotonically increasing
        for i in 1..ids.len() {
            assert!(ids[i] > ids[i - 1], "monotonic at index {}", i);
        }
    }
}
