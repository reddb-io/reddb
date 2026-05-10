use crate::replication::cdc::{CdcBuffer, KvWatchEvent};
use crate::runtime::KvStatsCounters;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const WATCH_BUFFER_CAPACITY: usize = 1024;
const WATCH_POLL_BATCH: usize = WATCH_BUFFER_CAPACITY * 4;

enum KvWatchMatch {
    Key(String),
    Prefix(String),
}

/// CDC-backed WATCH cursor for one normal KV key or key prefix.
pub struct KvWatchStream<'a> {
    cdc: &'a CdcBuffer,
    stats: &'a KvStatsCounters,
    collection: String,
    matcher: KvWatchMatch,
    cursor_lsn: u64,
    buffer: VecDeque<KvWatchEvent>,
    dropped_event_count: u64,
    idle_timeout: Duration,
    last_activity: Instant,
    active: bool,
}

impl<'a> KvWatchStream<'a> {
    pub(crate) fn subscribe(
        cdc: &'a CdcBuffer,
        stats: &'a KvStatsCounters,
        collection: impl Into<String>,
        key: impl Into<String>,
        from_lsn: Option<u64>,
        idle_timeout_ms: u64,
    ) -> Self {
        Self::new(
            cdc,
            stats,
            collection,
            KvWatchMatch::Key(key.into()),
            from_lsn,
            idle_timeout_ms,
        )
    }

    pub(crate) fn subscribe_prefix(
        cdc: &'a CdcBuffer,
        stats: &'a KvStatsCounters,
        collection: impl Into<String>,
        prefix: impl Into<String>,
        from_lsn: Option<u64>,
        idle_timeout_ms: u64,
    ) -> Self {
        Self::new(
            cdc,
            stats,
            collection,
            KvWatchMatch::Prefix(prefix.into()),
            from_lsn,
            idle_timeout_ms,
        )
    }

    fn new(
        cdc: &'a CdcBuffer,
        stats: &'a KvStatsCounters,
        collection: impl Into<String>,
        matcher: KvWatchMatch,
        from_lsn: Option<u64>,
        idle_timeout_ms: u64,
    ) -> Self {
        stats.incr_watch_streams_active();
        Self {
            cursor_lsn: from_lsn.unwrap_or_else(|| cdc.current_lsn()),
            cdc,
            stats,
            collection: collection.into(),
            matcher,
            buffer: VecDeque::with_capacity(WATCH_BUFFER_CAPACITY),
            dropped_event_count: 0,
            idle_timeout: Duration::from_millis(idle_timeout_ms.max(1)),
            last_activity: Instant::now(),
            active: true,
        }
    }

    pub fn poll_next(&mut self) -> Option<KvWatchEvent> {
        if !self.active {
            return None;
        }
        if self.last_activity.elapsed() >= self.idle_timeout {
            self.close();
            return None;
        }
        self.last_activity = Instant::now();

        if self.buffer.is_empty() {
            self.fill_buffer();
        }

        self.buffer.pop_front().map(|mut event| {
            event.dropped_event_count = self.dropped_event_count;
            event
        })
    }

    pub fn dropped_event_count(&self) -> u64 {
        self.dropped_event_count
    }

    pub fn record_drop_count(&mut self, count: u64) {
        self.dropped_event_count = self.dropped_event_count.saturating_add(count);
        self.stats.add_watch_drops(count);
    }

    fn fill_buffer(&mut self) {
        if let Some(oldest_lsn) = self.cdc.oldest_lsn() {
            if self.cursor_lsn + 1 < oldest_lsn {
                self.record_drop_count(oldest_lsn - self.cursor_lsn - 1);
                self.cursor_lsn = oldest_lsn - 1;
            }
        }

        for event in self.cdc.poll(self.cursor_lsn, WATCH_POLL_BATCH) {
            self.cursor_lsn = event.lsn;
            let Some(kv) = event.kv else {
                continue;
            };
            if !self.matches(&kv) {
                continue;
            }
            if self.buffer.len() >= WATCH_BUFFER_CAPACITY {
                self.buffer.pop_front();
                self.record_drop_count(1);
            }
            self.buffer.push_back(kv);
        }
    }

    fn matches(&self, event: &KvWatchEvent) -> bool {
        if event.collection != self.collection {
            return false;
        }
        match &self.matcher {
            KvWatchMatch::Key(key) => event.key == *key,
            KvWatchMatch::Prefix(prefix) => event.key.starts_with(prefix),
        }
    }

    fn close(&mut self) {
        if self.active {
            self.active = false;
            self.stats.decr_watch_streams_active();
        }
    }
}

impl Drop for KvWatchStream<'_> {
    fn drop(&mut self) {
        self.close();
    }
}
