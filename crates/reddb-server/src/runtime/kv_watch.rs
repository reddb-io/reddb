use crate::replication::cdc::{CdcBuffer, KvWatchEvent};

/// Blocking CDC-backed stream of WATCH events for one KV key.
pub struct BlockingKvWatchStream {
    collection: String,
    key: String,
    receiver: tokio::sync::broadcast::Receiver<KvWatchEvent>,
}

impl BlockingKvWatchStream {
    pub fn subscribe(
        cdc: &CdcBuffer,
        collection: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            collection: collection.into(),
            key: key.into(),
            receiver: cdc.subscribe_kv(),
        }
    }

    pub fn next_event(&mut self) -> Option<KvWatchEvent> {
        loop {
            match self.receiver.blocking_recv() {
                Ok(event) if event.collection == self.collection && event.key == self.key => {
                    return Some(event);
                }
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}
