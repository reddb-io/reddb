//! RedWire output-stream dispatch (issue #762, PRD #759 S3).
//!
//! Carries the wire-side lifecycle envelopes for an output stream:
//!   - `OpenStream`  (client→server)  — request to start streaming
//!     a `SELECT`'s rows back over a multiplexed `stream_id`.
//!   - `OpenAck`     (server→client)  — server accepted; carries
//!     the lease handle + the snapshot LSN the stream pinned.
//!   - `StreamChunk` (server→client)  — one or more rows as JSON.
//!     Multiple chunks per stream; `terminal: true` may be set on
//!     the last one when the producer wishes to coalesce with
//!     `StreamEnd`. The standalone `StreamEnd` envelope is the
//!     canonical close-of-stream marker.
//!   - `StreamError` (server→client)  — protocol violation or
//!     execution error for a specific `stream_id`. Non-fatal at
//!     the connection level (AC #6: server must not crash).
//!   - `StreamEnd`   (server→client)  — close-of-stream marker
//!     carrying summary stats (row_count, lease_id, snapshot_lsn).
//!   - `StreamCancel`(client→server)  — client asks to terminate
//!     a specific stream; other streams on the connection are
//!     unaffected (AC #3).
//!
//! Reuses [`crate::server::output_stream`] for the lease + config
//! primitives (S1 / issue #760) so HTTP and RedWire agree on TTL
//! and chunk semantics.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{oneshot, Mutex};

use crate::runtime::RedDBRuntime;
use crate::serde_json::{self, Value as JsonValue};
use crate::server::output_stream::{
    self as outs, Clock, OpenStreamError, StreamConfig, SystemClock,
};
pub use reddb_wire::redwire::stream::{
    OpenStreamParseError, OpenStreamRequest, StreamCancelRequest,
};
use reddb_wire::redwire::{encode_frame, Frame};

pub fn parse_open_stream(payload: &[u8]) -> Result<OpenStreamRequest, OpenStreamParseError> {
    reddb_wire::redwire::stream::parse_open_stream(payload)
}

pub fn parse_stream_cancel(payload: &[u8]) -> StreamCancelRequest {
    reddb_wire::redwire::stream::parse_stream_cancel(payload)
}

pub fn build_open_ack_frame(
    correlation_id: u64,
    stream_id: u16,
    lease_id: u64,
    snapshot_lsn: u64,
    resumable: bool,
) -> Result<Frame, reddb_wire::BuildError> {
    reddb_wire::redwire::stream::build_open_ack_frame(
        correlation_id,
        stream_id,
        lease_id,
        snapshot_lsn,
        resumable,
    )
}

/// Per-connection registry of in-flight output streams. Keyed by
/// `stream_id` — the wire-spec multiplex tag — so a `StreamCancel`
/// can target one stream without disturbing the rest of the
/// connection (AC #3).
#[derive(Default)]
pub struct StreamRegistry {
    inner: Mutex<HashMap<u16, oneshot::Sender<()>>>,
}

impl StreamRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new stream. Returns the receiver half the worker
    /// task selects on for cancellation, or `Err(InUse)` if the
    /// `stream_id` is already active on this connection.
    pub async fn register(&self, stream_id: u16) -> Result<oneshot::Receiver<()>, RegisterError> {
        if stream_id == 0 {
            return Err(RegisterError::ReservedStreamId);
        }
        let mut guard = self.inner.lock().await;
        if guard.contains_key(&stream_id) {
            return Err(RegisterError::StreamInUse);
        }
        let (tx, rx) = oneshot::channel();
        guard.insert(stream_id, tx);
        Ok(rx)
    }

    /// Signal the named stream to cancel. Returns `false` if the
    /// `stream_id` is unknown — caller should emit `StreamError`
    /// with `unknown_stream`.
    pub async fn cancel(&self, stream_id: u16) -> bool {
        let mut guard = self.inner.lock().await;
        match guard.remove(&stream_id) {
            Some(tx) => {
                let _ = tx.send(());
                true
            }
            None => false,
        }
    }

    /// Remove the stream from the registry once the worker task
    /// has finished (normally or via cancel). Idempotent.
    pub async fn unregister(&self, stream_id: u16) {
        let mut guard = self.inner.lock().await;
        guard.remove(&stream_id);
    }

    pub async fn active_count(&self) -> usize {
        self.inner.lock().await.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegisterError {
    ReservedStreamId,
    StreamInUse,
}

impl RegisterError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ReservedStreamId => "open_stream_reserved_id",
            Self::StreamInUse => "open_stream_id_in_use",
        }
    }
    pub fn message(&self) -> &'static str {
        match self {
            Self::ReservedStreamId => {
                "OpenStream cannot use stream_id 0 (reserved for unsolicited)"
            }
            Self::StreamInUse => "OpenStream stream_id already has an active stream",
        }
    }
}

/// Build a stand-alone `StreamError` frame addressed to a given
/// `stream_id`. The correlation id echoes the request frame so a
/// client can pair the error with the offending request.
pub fn build_stream_error_frame(
    correlation_id: u64,
    stream_id: u16,
    code: &str,
    message: &str,
) -> std::io::Result<Frame> {
    reddb_wire::redwire::stream::build_stream_error_frame(
        correlation_id,
        stream_id,
        None,
        code,
        message,
    )
    .map_err(|e| std::io::Error::other(format!("build StreamError: {e}")))
}

/// Run an output stream end-to-end. Emits OpenAck → StreamChunk*
/// → StreamEnd via the supplied `send` closure, observing the
/// `cancel_rx` between rows to honour `StreamCancel` (AC #3).
///
/// The function materialises `execute_query`'s result first
/// (matching the S1 HTTP behaviour — pull-based scan executors
/// are PRD #759 phase 3) and then dribbles rows out as
/// `StreamChunk` envelopes via the same byte/row/latency
/// page-aligned producer the HTTP path uses.
pub async fn run_output_stream(
    runtime: Arc<RedDBRuntime>,
    correlation_id: u64,
    stream_id: u16,
    request: OpenStreamRequest,
    in_transaction: bool,
    mut cancel_rx: oneshot::Receiver<()>,
    send: FrameTx,
) {
    let clock = SystemClock;
    let config = StreamConfig::load(&runtime);
    let snapshot_lsn = runtime.cdc_current_lsn();

    let lease = match outs::open_stream(config, snapshot_lsn, in_transaction, &clock) {
        Ok(l) => l,
        Err(OpenStreamError::TransactionActive) => {
            let err = OpenStreamError::TransactionActive;
            let frame = match build_stream_error_frame(
                correlation_id,
                stream_id,
                err.code(),
                err.message(),
            ) {
                Ok(f) => f,
                Err(_) => return,
            };
            send.send_frame(frame);
            return;
        }
    };

    // OpenAck — always first.
    let ack = match reddb_wire::redwire::stream::build_open_ack_frame(
        correlation_id,
        stream_id,
        lease.id,
        lease.snapshot_lsn,
        false,
    ) {
        Ok(f) => f,
        Err(_) => return,
    };
    send.send_frame(ack);

    // Materialise.
    let result = runtime.execute_query(&request.sql);

    // Stream rows out as StreamChunk envelopes.
    let mut seq: u64 = 0;
    let mut row_count: u64 = 0;
    let mut cancelled = false;
    let mut had_error: Option<(String, String)> = None;

    match result {
        Ok(qr) => {
            let columns = qr.result.columns.clone();
            let rows: Vec<JsonValue> = qr
                .result
                .records
                .iter()
                .map(|r| crate::presentation::query_result_json::unified_record_json(r, &columns))
                .collect();

            // One `StreamChunk` envelope per row. The page-aligned
            // batcher used by the HTTP NDJSON path (S1) is byte-
            // oriented; on the wire path each row already ships as
            // its own framed envelope, so TCP / framing already
            // handles the batching for us. Keeping one row per
            // envelope keeps `StreamCancel` latency bounded to
            // "between two adjacent rows".
            for row in rows {
                // Check cancel between rows (AC #3).
                if let Ok(()) = cancel_rx.try_recv() {
                    cancelled = true;
                    break;
                }
                if lease.snapshot_expired(clock.now_ms()) {
                    had_error = Some((
                        "snapshot_expired".to_string(),
                        "stream snapshot pin TTL elapsed".to_string(),
                    ));
                    break;
                }
                let row_bytes = serde_json::to_vec(&row).unwrap_or_default();
                let frame =
                    match reddb_wire::redwire::stream::build_stream_chunk_frame_from_json_bytes(
                        correlation_id,
                        stream_id,
                        seq,
                        vec![row_bytes],
                        false,
                    ) {
                        Ok(f) => f,
                        Err(_) => break,
                    };
                send.send_frame(frame);
                seq += 1;
                row_count += 1;
            }
            // `config` is kept observed even when the batcher is
            // bypassed so the frozen-config invariant from S1 still
            // applies (no mid-stream KV-driven behaviour change).
            let _ = config;
        }
        Err(err) => {
            had_error = Some(("query_failed".to_string(), err.to_string()));
        }
    }

    if let Some((code, message)) = had_error {
        if let Ok(frame) = reddb_wire::redwire::stream::build_stream_error_frame(
            correlation_id,
            stream_id,
            Some(seq),
            &code,
            &message,
        ) {
            send.send_frame(frame);
        }
    }

    // StreamEnd is always emitted — including after error or
    // cancel — so the client can drop bookkeeping on `StreamEnd`
    // rather than on connection EOF.
    if let Ok(frame) = reddb_wire::redwire::stream::build_stream_end_frame(
        correlation_id,
        stream_id,
        row_count,
        lease.id,
        lease.snapshot_lsn,
        cancelled,
    ) {
        send.send_frame(frame);
    }
}

/// Encoded-frame transmit handle handed to stream workers. The
/// session loop owns the matching receiver and drains it into the
/// socket's write half — so multiple concurrent workers can
/// interleave their output without contending on a writer mutex
/// (AC #2 — interleaved chunks for two streams on one connection).
#[derive(Clone)]
pub struct FrameTx {
    tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
}

impl FrameTx {
    pub fn new(tx: tokio::sync::mpsc::UnboundedSender<Vec<u8>>) -> Self {
        Self { tx }
    }

    /// Encode + enqueue. Drops silently if the receiver has been
    /// dropped (connection torn down); the worker's next iteration
    /// will hit cancellation / EOF and exit naturally.
    pub fn send_frame(&self, frame: Frame) {
        let bytes = encode_frame(&frame);
        let _ = self.tx.send(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_wire::redwire::MessageKind;

    #[test]
    fn parse_open_stream_accepts_minimal_payload() {
        let req = parse_open_stream(br#"{"sql":"SELECT 1"}"#).unwrap();
        assert_eq!(req.sql, "SELECT 1");
        assert!(req.opts_raw.is_empty());
    }

    #[test]
    fn parse_open_stream_captures_opts_opaque() {
        let req =
            parse_open_stream(br#"{"sql":"SELECT 1","opts":{"resume_after_rid":42}}"#).unwrap();
        assert_eq!(req.sql, "SELECT 1");
        assert!(!req.opts_raw.is_empty());
    }

    #[test]
    fn parse_open_stream_rejects_non_object() {
        assert!(matches!(
            parse_open_stream(b"\"sql\""),
            Err(OpenStreamParseError::NotObject)
        ));
    }

    #[test]
    fn parse_open_stream_rejects_missing_sql() {
        assert!(matches!(
            parse_open_stream(b"{}"),
            Err(OpenStreamParseError::MissingSql)
        ));
    }

    #[test]
    fn parse_open_stream_rejects_empty_sql() {
        assert!(matches!(
            parse_open_stream(br#"{"sql":""}"#),
            Err(OpenStreamParseError::EmptySql)
        ));
    }

    #[test]
    fn parse_open_stream_rejects_invalid_json() {
        assert!(matches!(
            parse_open_stream(b"{not json"),
            Err(OpenStreamParseError::NotJson)
        ));
    }

    #[test]
    fn parse_stream_cancel_with_reason() {
        let r = parse_stream_cancel(br#"{"reason":"client-abort"}"#);
        assert_eq!(r.reason.as_deref(), Some("client-abort"));
    }

    #[test]
    fn parse_stream_cancel_empty_payload_is_default() {
        assert_eq!(parse_stream_cancel(b""), StreamCancelRequest::default());
        assert_eq!(parse_stream_cancel(b"{}"), StreamCancelRequest::default());
    }

    #[tokio::test]
    async fn registry_rejects_reserved_id_and_duplicates() {
        let r = StreamRegistry::new();
        assert!(matches!(
            r.register(0).await,
            Err(RegisterError::ReservedStreamId)
        ));
        let _rx = r.register(1).await.unwrap();
        assert!(matches!(
            r.register(1).await,
            Err(RegisterError::StreamInUse)
        ));
        assert_eq!(r.active_count().await, 1);
    }

    #[tokio::test]
    async fn registry_cancel_signals_named_stream_only() {
        // AC #3 — cancelling stream X must not disturb stream Y.
        let r = StreamRegistry::new();
        let rx1 = r.register(1).await.unwrap();
        let mut rx2 = r.register(2).await.unwrap();
        assert!(r.cancel(1).await);
        // Stream 1's rx fires.
        assert!(rx1.await.is_ok());
        // Stream 2's rx remains pending (try_recv would yield Empty).
        match rx2.try_recv() {
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            other => panic!("stream 2 should not be cancelled: {other:?}"),
        }
        assert_eq!(r.active_count().await, 1);
    }

    #[tokio::test]
    async fn registry_cancel_unknown_returns_false() {
        let r = StreamRegistry::new();
        assert!(!r.cancel(99).await);
    }

    #[tokio::test]
    async fn registry_unregister_is_idempotent() {
        let r = StreamRegistry::new();
        let _rx = r.register(1).await.unwrap();
        r.unregister(1).await;
        r.unregister(1).await;
        assert_eq!(r.active_count().await, 0);
    }

    #[test]
    fn build_stream_error_frame_carries_stream_id_and_correlation() {
        let frame = build_stream_error_frame(99, 7, "unknown_stream", "no such stream").unwrap();
        assert_eq!(frame.kind, MessageKind::StreamError);
        assert_eq!(frame.stream_id, 7);
        assert_eq!(frame.correlation_id, 99);
    }
}
