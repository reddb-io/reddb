use std::pin::Pin;
use std::task::{Context, Poll};

use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::Stream;

use crate::replication::cdc::{CdcBuffer, KvWatchEvent};

/// CDC-backed stream of WATCH events for one KV key.
pub struct KvWatchStream {
    collection: String,
    key: String,
    inner: BroadcastStream<KvWatchEvent>,
}

impl KvWatchStream {
    pub fn subscribe(
        cdc: &CdcBuffer,
        collection: impl Into<String>,
        key: impl Into<String>,
    ) -> Self {
        Self {
            collection: collection.into(),
            key: key.into(),
            inner: BroadcastStream::new(cdc.subscribe_kv()),
        }
    }
}

impl Stream for KvWatchStream {
    type Item = KvWatchEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.inner).poll_next(cx) {
                Poll::Ready(Some(Ok(event)))
                    if event.collection == self.collection && event.key == self.key =>
                {
                    return Poll::Ready(Some(event));
                }
                Poll::Ready(Some(Ok(_))) => continue,
                Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(_)))) => continue,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

/// Blocking adapter for the current std::net HTTP server's SSE endpoint.
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
