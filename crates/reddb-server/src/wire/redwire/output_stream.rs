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

use crate::application::OperationContext;
use crate::auth::Role;
use crate::runtime::query_request::{PreparedRegistry, QueryRequest, QueryRequestExecutor};
use crate::runtime::RedDBRuntime;
use crate::server::output_stream::{
    self as outs, Clock, OpenStreamError, StreamConfig, SystemClock,
};
pub use reddb_wire::redwire::stream::{
    OpenStreamParseError, OpenStreamRequest, StreamCancelRequest,
};
use reddb_wire::redwire::Frame;

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
/// The function materialises an admitted request result first
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
    operation_context: OperationContext,
    role: Role,
) {
    let clock = SystemClock;
    let config = match StreamConfig::load(&runtime) {
        Ok(config) => config,
        Err(error) => {
            if let Ok(frame) = build_stream_error_frame(
                correlation_id,
                stream_id,
                "stream_config_unavailable",
                &error.to_string(),
            ) {
                if send.send_frame(frame).await.is_err() {
                    return;
                }
            }
            return;
        }
    };
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
            let _ = send.send_frame(frame).await;
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
    if send.send_frame(ack).await.is_err() {
        return;
    }

    // Keep the completed result's shared admission until the stream finishes
    // or its worker is cancelled, including while a slow peer blocks sending.
    let result = super::session::execute_with_redwire_context(&operation_context, role, || {
        QueryRequestExecutor::new(&runtime, &PreparedRegistry::new())
            .execute(QueryRequest::sql(request.sql, Vec::new()))
    });

    // Stream rows out as StreamChunk envelopes.
    let mut seq: u64 = 0;
    let mut row_count: u64 = 0;
    let mut cancelled = false;
    let mut had_error: Option<(String, String)> = None;

    match result {
        Ok(qr) => {
            // One `StreamChunk` envelope per row. The page-aligned
            // batcher used by the HTTP NDJSON path (S1) is byte-
            // oriented; on the wire path each row already ships as
            // its own framed envelope, so TCP / framing already
            // handles the batching for us. Keeping one row per
            // envelope keeps `StreamCancel` latency bounded to
            // "between two adjacent rows".
            for record in &qr.result.records {
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
                // Render only the next row. Retaining a second complete JSON
                // result beside the admitted records doubles stream residency.
                let row_bytes = crate::presentation::query_result_json::unified_record_json(
                    record,
                    &qr.result.columns,
                )
                .to_string_compact()
                .into_bytes();
                let frame =
                    match reddb_wire::redwire::stream::build_stream_chunk_frame_from_json_bytes(
                        correlation_id,
                        stream_id,
                        seq,
                        vec![row_bytes],
                        false,
                    ) {
                        Ok(f) => f,
                        Err(error) => {
                            had_error = Some((
                                "stream_chunk_encoding_failed".to_string(),
                                error.to_string(),
                            ));
                            break;
                        }
                    };
                if send.send_frame(frame).await.is_err() {
                    return;
                }
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
            let _ = send.send_frame(frame).await;
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
        let _ = send.send_frame(frame).await;
    }
}

/// Encoded-frame transmit handle handed to stream workers. The
/// session loop owns the matching receiver and drains it into the
/// socket's write half — so multiple concurrent workers can
/// interleave their output without contending on a writer mutex
/// (AC #2 — interleaved chunks for two streams on one connection).
#[derive(Clone)]
pub struct FrameTx {
    tx: tokio::sync::mpsc::Sender<bytes::Bytes>,
    runtime: Arc<RedDBRuntime>,
    failure: tokio::sync::watch::Sender<bool>,
}

impl FrameTx {
    pub fn new(tx: tokio::sync::mpsc::Sender<bytes::Bytes>, runtime: Arc<RedDBRuntime>) -> Self {
        Self {
            tx,
            runtime,
            failure: tokio::sync::watch::channel(false).0,
        }
    }

    pub(crate) fn spawn_writer<W>(
        &self,
        mut receiver: tokio::sync::mpsc::Receiver<bytes::Bytes>,
        mut writer: W,
    ) -> tokio::task::JoinHandle<()>
    where
        W: tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        use tokio::io::AsyncWriteExt;
        let failure = self.failure.clone();
        tokio::spawn(async move {
            let mut failed = failure.subscribe();
            // Do not retain a frame sender here: clean Bye/half-close must
            // drain to EOF. Failure cancels even a blocked socket write and
            // drops both its current buffer and all queued buffers.
            tokio::select! {
                biased;
                _ = failed.wait_for(|value| *value) => {},
                _ = async {
                    while let Some(bytes) = receiver.recv().await {
                        if writer.write_all(&bytes).await.is_err() {
                            failure.send_replace(true);
                            break;
                        }
                    }
                } => {},
            }
        })
    }

    pub(crate) async fn failed(&self) {
        let _ = self.failure.subscribe().wait_for(|value| *value).await;
    }

    /// Reserve both the payload and the encoded allocation before copying.
    /// Exhaustion fails this connection; never deliver a truncated success or
    /// let a stream worker silently discard a frame and continue with StreamEnd.
    pub async fn send_frame(&self, frame: Frame) -> std::io::Result<()> {
        let result = self.send_frame_inner(frame).await;
        if result.is_err() {
            self.failure.send_replace(true);
        }
        result
    }

    async fn send_frame_inner(&self, mut frame: Frame) -> std::io::Result<()> {
        use reddb_wire::redwire::{Flags, FRAME_HEADER_SIZE, MAX_FRAME_SIZE};
        if *self.failure.borrow() {
            return Err(std::io::Error::other("redwire: write channel failed"));
        }
        // Session builders emit uncompressed replies. A producer requesting
        // compression gets the protocol's existing plaintext fallback until
        // the compression workspace has its own admission. Preserve all other
        // flags and avoid allocating an unaccounted compressor workspace.
        frame.flags = Flags::from_bits(frame.flags.bits() & !Flags::COMPRESSED.bits());
        let capacity = frame
            .payload
            .len()
            .checked_add(FRAME_HEADER_SIZE)
            .filter(|size| *size <= MAX_FRAME_SIZE as usize)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "redwire: response exceeds frame limit",
                )
            })?;
        let bytes = self
            .runtime
            .serialize_query_buffer(capacity, frame.payload.capacity(), move |buffer| {
                reddb_wire::redwire::codec::encode_uncompressed_frame_into(&frame, buffer);
                drop(frame);
            })
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::OutOfMemory,
                    "redwire: response exceeds shared memory budget",
                )
            })?;
        tokio::select! {
            biased;
            _ = self.failed() => Err(std::io::Error::other("redwire: write channel failed")),
            result = self.tx.send(bytes) => result.map_err(|_| std::io::Error::other("redwire: write channel closed")),
        }
    }
}

#[cfg(test)]
mod memory_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_wire::redwire::MessageKind;

    #[tokio::test]
    async fn outbound_memory_survives_queue_receive_and_response_clones() {
        use crate::storage::memory_pools::MemoryPool;
        let runtime = RedDBRuntime::with_options(
            crate::RedDBOptions::in_memory().with_memory_budget(1024 * 1024),
        )
        .expect("bounded runtime");
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let runtime = Arc::new(runtime);
        let sender = FrameTx::new(tx, Arc::clone(&runtime));
        sender
            .send_frame(
                reddb_wire::redwire::build_reply_frame(7, MessageKind::Result, vec![1; 128 * 1024])
                    .expect("reply fixture"),
            )
            .await
            .expect("admitted response");
        assert!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers)
                >= 128 * 1024,
            "queued encoded responses must remain charged to the runtime budget"
        );
        let bytes = rx.recv().await.expect("queued response");
        let copy = bytes.clone();
        drop(bytes);
        assert!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers)
                >= copy.len() as u64,
            "socket ownership and clones must retain admission after dequeue"
        );
        let (frame, consumed) = reddb_wire::redwire::decode_frame(&copy).expect("wire bytes");
        assert_eq!(consumed, copy.len());
        assert_eq!(frame.correlation_id, 7);
        assert_eq!(frame.payload, vec![1; 128 * 1024]);
        drop(copy);
        assert_eq!(
            runtime
                .memory_accounting()
                .used_bytes(MemoryPool::QueryBuffers),
            0
        );
    }

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
