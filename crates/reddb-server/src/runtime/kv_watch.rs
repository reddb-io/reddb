use crate::replication::cdc::{CdcBuffer, KvWatchEvent};

/// CDC-backed WATCH cursor for one normal KV key.
pub struct KvWatchStream<'a> {
    cdc: &'a CdcBuffer,
    collection: String,
    key: String,
    cursor_lsn: u64,
}

impl<'a> KvWatchStream<'a> {
    pub fn subscribe(
        cdc: &'a CdcBuffer,
        collection: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            cursor_lsn: cdc.current_lsn(),
            cdc,
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
}
