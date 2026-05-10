use crate::replication::cdc::{CdcBuffer, KvWatchEvent};
use crate::runtime::KvStatsCounters;

/// CDC-backed WATCH cursor for one normal KV key.
pub struct KvWatchStream<'a> {
    cdc: &'a CdcBuffer,
    stats: &'a KvStatsCounters,
    collection: String,
    key: String,
    cursor_lsn: u64,
}

impl<'a> KvWatchStream<'a> {
    pub(crate) fn subscribe(
        cdc: &'a CdcBuffer,
        stats: &'a KvStatsCounters,
        collection: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        stats.incr_watch_streams_active();
        Self {
            cursor_lsn: cdc.current_lsn(),
            cdc,
            stats,
            collection: collection.into(),
            key: key.into(),
        }
    }

    pub fn poll_next(&mut self) -> Option<KvWatchEvent> {
        for event in self.cdc.poll(self.cursor_lsn, 256) {
            self.cursor_lsn = event.lsn;
            let Some(kv) = event.kv else {
                continue;
            };
            if kv.collection == self.collection && kv.key == self.key {
                return Some(kv);
            }
        }
        None
    }

    pub fn record_drop_count(&self, count: u64) {
        self.stats.add_watch_drops(count);
    }
}

impl Drop for KvWatchStream<'_> {
    fn drop(&mut self) {
        self.stats.decr_watch_streams_active();
    }
}
