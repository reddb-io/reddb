//! Output streaming primitives for issue #760 (PRD #759 / ADR 0029) — the
//! "shim slice" of bidirectional streaming.
//!
//! Scope of this module:
//!   - [`StreamConfig`]: capture the `stream.*` namespace from `red_config`
//!     at lease open and freeze it for the lease's lifetime (acceptance
//!     criterion: KV mutations mid-stream do not retroactively change
//!     behaviour).
//!   - [`StreamLease`]: an internal, unforwarded handle bound to a
//!     snapshot LSN and a frozen config. No external surface yet (S2 will
//!     add quotas + per-principal accounting).
//!   - [`open_stream`]: refuses with `stream_in_transaction_unsupported`
//!     when the caller already has an active `BEGIN` on the session.
//!   - [`ChunkProducer`]: page-aligned (N × 16 KiB) production buffer
//!     that flushes on the first of byte / row / latency cap.
//!   - [`Clock`]: trait-injected time source so TTL expiry is testable.
//!
//! The HTTP NDJSON wire framing built on top of these primitives lives in
//! `handlers_query::handle_query_ndjson_stream` and is dispatched from
//! `routing::try_route_streaming`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::runtime::RedDBRuntime;
use crate::storage::schema::types::Value;

const RED_CONFIG_COLLECTION: &str = "red_config";

/// Engine page size — the production buffer is always a multiple of this.
/// Matches `storage::engine::PAGE_SIZE`.
pub const PAGE_SIZE: usize = 16 * 1024;

/// Injectable time source. Production code uses [`SystemClock`]; tests
/// drive TTL expiry with [`FakeClock`] so they don't depend on wall time.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[derive(Debug)]
pub struct FakeClock {
    now_ms: AtomicU64,
}

impl FakeClock {
    pub fn new(now_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(now_ms),
        }
    }

    pub fn advance(&self, ms: u64) {
        self.now_ms.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.now_ms.load(Ordering::SeqCst)
    }
}

/// Frozen snapshot of the `stream.*` namespace at lease open. Acceptance
/// criterion: a `red_config` mutation while a stream is running does not
/// retroactively change the running stream's behaviour — that is, the
/// per-lease config is value-typed and not a back-reference.
#[derive(Debug, Clone, Copy)]
pub struct StreamConfig {
    pub snapshot_ttl_ms: u64,
    pub chunk_default_pages: usize,
    pub chunk_min_pages: usize,
    pub chunk_max_pages: usize,
    pub chunk_max_rows: usize,
    pub chunk_max_latency_ms: u64,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl StreamConfig {
    /// Defaults match ADR 0029. `snapshot_ttl_ms` is the only key
    /// observable mid-stream; the chunk caps only affect framing.
    pub const DEFAULT: StreamConfig = StreamConfig {
        snapshot_ttl_ms: 60_000,
        chunk_default_pages: 4,
        chunk_min_pages: 1,
        chunk_max_pages: 64,
        chunk_max_rows: 1000,
        chunk_max_latency_ms: 50,
    };

    /// Read every `stream.*` key from `red_config` over the runtime's KV
    /// store. Missing keys fall back to [`Self::DEFAULT`]. Unparseable or
    /// out-of-range values fall back silently — bad config never gets to
    /// terminate a request that would otherwise succeed.
    pub fn load(runtime: &RedDBRuntime) -> Self {
        let db = runtime.db();
        let read_u64 = |key: &str| -> Option<u64> {
            match db.get_kv(RED_CONFIG_COLLECTION, key) {
                Some((Value::Integer(v), _)) if v >= 0 => Some(v as u64),
                Some((Value::UnsignedInteger(v), _)) => Some(v),
                Some((Value::Text(text), _)) => text.parse().ok(),
                _ => None,
            }
        };

        let mut cfg = Self::DEFAULT;
        if let Some(v) = read_u64("stream.snapshot.ttl_ms") {
            cfg.snapshot_ttl_ms = v;
        }
        if let Some(v) = read_u64("stream.chunk.default_pages") {
            cfg.chunk_default_pages = v as usize;
        }
        if let Some(v) = read_u64("stream.chunk.min_pages") {
            cfg.chunk_min_pages = v as usize;
        }
        if let Some(v) = read_u64("stream.chunk.max_pages") {
            cfg.chunk_max_pages = v as usize;
        }
        if let Some(v) = read_u64("stream.chunk.max_rows") {
            cfg.chunk_max_rows = v as usize;
        }
        if let Some(v) = read_u64("stream.chunk.max_latency_ms") {
            cfg.chunk_max_latency_ms = v;
        }
        cfg.normalize();
        cfg
    }

    /// Clamp interrelated fields to a self-consistent state. Floors come
    /// from ADR 0029 ("hard floor 1 page"); ceilings prevent zero-row /
    /// zero-byte caps that would prevent the producer from ever flushing.
    fn normalize(&mut self) {
        if self.chunk_min_pages == 0 {
            self.chunk_min_pages = 1;
        }
        if self.chunk_max_pages < self.chunk_min_pages {
            self.chunk_max_pages = self.chunk_min_pages;
        }
        if self.chunk_default_pages < self.chunk_min_pages {
            self.chunk_default_pages = self.chunk_min_pages;
        }
        if self.chunk_default_pages > self.chunk_max_pages {
            self.chunk_default_pages = self.chunk_max_pages;
        }
        if self.chunk_max_rows == 0 {
            self.chunk_max_rows = 1;
        }
    }

    /// Page-aligned production buffer size in bytes. Acceptance criterion:
    /// "production buffer is always N × 16 KiB".
    pub fn production_buffer_bytes(&self) -> usize {
        self.chunk_default_pages.saturating_mul(PAGE_SIZE)
    }
}

/// Monotonic, process-local lease id. The id is internal — the bearer
/// token still authenticates the open, the lease only identifies the
/// stream for audit and termination (ADR 0029 "Authorization").
static NEXT_LEASE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct StreamLease {
    pub id: u64,
    pub snapshot_lsn: u64,
    pub opened_at_ms: u64,
    pub config: StreamConfig,
}

impl StreamLease {
    /// `true` once `snapshot_ttl_ms` has elapsed since the lease was opened.
    /// The shim slice materialises the result first, so expiry in practice
    /// only fires for streams that take longer than `ttl_ms` to drain to
    /// the client. The check is wired in so the wire envelope can carry
    /// `snapshot_expired` exactly the way later slices (pull-based
    /// executors) will need it.
    pub fn snapshot_expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.opened_at_ms) >= self.config.snapshot_ttl_ms
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum OpenStreamError {
    /// Acceptance criterion #4: `OpenStream` against a session with an
    /// active `BEGIN` is refused with this code. The wire shape carries
    /// it back as `{"error": {"code": "stream_in_transaction_unsupported"}}`.
    TransactionActive,
}

impl OpenStreamError {
    pub fn code(&self) -> &'static str {
        match self {
            OpenStreamError::TransactionActive => "stream_in_transaction_unsupported",
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            OpenStreamError::TransactionActive => {
                "cannot open output stream while a transaction is active on this session"
            }
        }
    }
}

/// Issue a lease. The caller is responsible for binding `snapshot_lsn`
/// to the same MVCC view the underlying executor will read from; in the
/// shim slice this is `runtime.cdc_current_lsn()` captured before
/// `execute_query`.
pub fn open_stream(
    config: StreamConfig,
    snapshot_lsn: u64,
    in_transaction: bool,
    clock: &dyn Clock,
) -> Result<StreamLease, OpenStreamError> {
    if in_transaction {
        return Err(OpenStreamError::TransactionActive);
    }
    Ok(StreamLease {
        id: NEXT_LEASE_ID.fetch_add(1, Ordering::SeqCst),
        snapshot_lsn,
        opened_at_ms: clock.now_ms(),
        config,
    })
}

/// Page-aligned chunk producer. The producer accumulates byte-encoded
/// rows in an N × 16 KiB buffer; on the first of byte / row / latency
/// cap it forwards the buffer to the supplied flush closure, which the
/// transport layer turns into a chunked-encoding frame.
///
/// The struct does not know about HTTP, NDJSON, or chunked transfer —
/// it is wire-agnostic so the gRPC and RedWire paths can reuse it.
pub struct ChunkProducer<'a> {
    buf: Vec<u8>,
    rows: usize,
    window_started_ms: u64,
    cap_bytes: usize,
    cap_rows: usize,
    cap_latency_ms: u64,
    clock: &'a dyn Clock,
    total_flushes: u64,
    total_bytes: u64,
    total_rows: u64,
    last_flush_reason: Option<FlushReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushReason {
    Byte,
    Row,
    Latency,
    Terminal,
}

impl<'a> ChunkProducer<'a> {
    pub fn new(config: &StreamConfig, clock: &'a dyn Clock) -> Self {
        let cap_bytes = config.production_buffer_bytes();
        Self {
            buf: Vec::with_capacity(cap_bytes),
            rows: 0,
            window_started_ms: clock.now_ms(),
            cap_bytes,
            cap_rows: config.chunk_max_rows,
            cap_latency_ms: config.chunk_max_latency_ms,
            clock,
            total_flushes: 0,
            total_bytes: 0,
            total_rows: 0,
            last_flush_reason: None,
        }
    }

    /// Append one already-encoded line (NDJSON: bytes + `\n`). Returns
    /// `true` if the append triggered a flush.
    pub fn push_line<F>(&mut self, line: &[u8], flush: &mut F) -> std::io::Result<bool>
    where
        F: FnMut(&[u8]) -> std::io::Result<()>,
    {
        self.buf.extend_from_slice(line);
        self.rows += 1;
        self.total_rows += 1;

        if self.buf.len() >= self.cap_bytes {
            self.flush(flush, FlushReason::Byte)?;
            return Ok(true);
        }
        if self.rows >= self.cap_rows {
            self.flush(flush, FlushReason::Row)?;
            return Ok(true);
        }
        let elapsed = self.clock.now_ms().saturating_sub(self.window_started_ms);
        if elapsed >= self.cap_latency_ms {
            self.flush(flush, FlushReason::Latency)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Force-flush any buffered bytes — used after the final NDJSON line
    /// (`{"end": …}`) to push the tail of the buffer before closing the
    /// connection.
    pub fn finish<F>(&mut self, flush: &mut F) -> std::io::Result<()>
    where
        F: FnMut(&[u8]) -> std::io::Result<()>,
    {
        if !self.buf.is_empty() {
            self.flush(flush, FlushReason::Terminal)?;
        }
        Ok(())
    }

    fn flush<F>(&mut self, flush: &mut F, reason: FlushReason) -> std::io::Result<()>
    where
        F: FnMut(&[u8]) -> std::io::Result<()>,
    {
        flush(&self.buf)?;
        self.total_bytes += self.buf.len() as u64;
        self.total_flushes += 1;
        self.last_flush_reason = Some(reason);
        self.buf.clear();
        self.rows = 0;
        self.window_started_ms = self.clock.now_ms();
        Ok(())
    }

    pub fn total_flushes(&self) -> u64 {
        self.total_flushes
    }
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
    pub fn total_rows(&self) -> u64 {
        self.total_rows
    }
    pub fn last_flush_reason(&self) -> Option<FlushReason> {
        self.last_flush_reason
    }
}

/// HTTP chunked transfer encoding helpers. We do not pull in `hyper` for
/// the streaming path; the existing SSE handler also hand-rolls its own
/// HTTP framing, and matching that style keeps the diff narrow.
pub fn write_chunked_response_header<W: std::io::Write>(
    writer: &mut W,
    status: u16,
    content_type: &str,
) -> std::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        status,
        crate::server::transport::status_text(status),
        content_type,
    );
    writer.write_all(header.as_bytes())?;
    writer.flush()
}

/// Emit one HTTP chunk (`<hex-size>\r\n<bytes>\r\n`). A zero-length
/// payload is silently dropped — the terminator chunk lives in
/// [`write_chunked_terminator`].
pub fn write_chunk<W: std::io::Write>(writer: &mut W, bytes: &[u8]) -> std::io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let size = format!("{:x}\r\n", bytes.len());
    writer.write_all(size.as_bytes())?;
    writer.write_all(bytes)?;
    writer.write_all(b"\r\n")?;
    writer.flush()
}

/// Final `0\r\n\r\n` chunk that terminates a chunked body.
pub fn write_chunked_terminator<W: std::io::Write>(writer: &mut W) -> std::io::Result<()> {
    writer.write_all(b"0\r\n\r\n")?;
    writer.flush()
}

/// Borrow the lease registry's clock by default — the routing handler
/// uses this, tests inject their own.
pub fn system_clock() -> Arc<dyn Clock> {
    static INSTANCE: std::sync::OnceLock<Arc<dyn Clock>> = std::sync::OnceLock::new();
    Arc::clone(INSTANCE.get_or_init(|| Arc::new(SystemClock)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_stream_refuses_when_session_has_active_transaction() {
        let clock = FakeClock::new(0);
        let err = open_stream(StreamConfig::DEFAULT, 42, true, &clock).unwrap_err();
        assert_eq!(err, OpenStreamError::TransactionActive);
        assert_eq!(err.code(), "stream_in_transaction_unsupported");
    }

    #[test]
    fn open_stream_succeeds_when_session_is_autocommit() {
        let clock = FakeClock::new(1_700_000_000_000);
        let lease = open_stream(StreamConfig::DEFAULT, 123, false, &clock).unwrap();
        assert_eq!(lease.snapshot_lsn, 123);
        assert_eq!(lease.opened_at_ms, 1_700_000_000_000);
        assert!(lease.id >= 1);
    }

    #[test]
    fn lease_ids_are_unique_and_monotonic() {
        let clock = FakeClock::new(0);
        let a = open_stream(StreamConfig::DEFAULT, 1, false, &clock).unwrap();
        let b = open_stream(StreamConfig::DEFAULT, 1, false, &clock).unwrap();
        assert!(b.id > a.id);
    }

    #[test]
    fn snapshot_expired_uses_injected_clock_and_ttl() {
        // TTL fake-clock test: advance past ttl_ms and snapshot_expired
        // flips. Acceptance criterion #5.
        let clock = FakeClock::new(0);
        let mut config = StreamConfig::DEFAULT;
        config.snapshot_ttl_ms = 5_000;
        let lease = open_stream(config, 0, false, &clock).unwrap();

        assert!(!lease.snapshot_expired(clock.now_ms()));
        clock.advance(4_999);
        assert!(!lease.snapshot_expired(clock.now_ms()));
        clock.advance(1);
        assert!(lease.snapshot_expired(clock.now_ms()));
    }

    #[test]
    fn stream_config_loads_defaults_when_kv_is_empty() {
        // Without runtime, just sanity-check the defaults match ADR 0029.
        let cfg = StreamConfig::DEFAULT;
        assert_eq!(cfg.snapshot_ttl_ms, 60_000);
        assert_eq!(cfg.chunk_default_pages, 4);
        assert_eq!(cfg.chunk_min_pages, 1);
        assert_eq!(cfg.chunk_max_pages, 64);
        assert_eq!(cfg.chunk_max_rows, 1000);
        assert_eq!(cfg.chunk_max_latency_ms, 50);
        assert_eq!(cfg.production_buffer_bytes(), 64 * 1024);
    }

    #[test]
    fn stream_config_normalize_clamps_inconsistent_inputs() {
        let mut cfg = StreamConfig {
            snapshot_ttl_ms: 1,
            chunk_default_pages: 100,
            chunk_min_pages: 0,
            chunk_max_pages: 8,
            chunk_max_rows: 0,
            chunk_max_latency_ms: 1,
        };
        cfg.normalize();
        assert_eq!(cfg.chunk_min_pages, 1);
        assert_eq!(cfg.chunk_max_pages, 8);
        assert_eq!(cfg.chunk_default_pages, 8); // clamped down to max
        assert!(cfg.chunk_max_rows >= 1);
    }

    /// Test sink that accumulates flushed chunks into an interior-mutable
    /// `Vec`. Avoids the closure-capture-mutable-then-borrow-immutable
    /// dance that the borrow checker rejects when assertions are
    /// interleaved with `push_line` calls.
    struct CapturingSink {
        chunks: std::cell::RefCell<Vec<Vec<u8>>>,
    }
    impl CapturingSink {
        fn new() -> Self {
            Self {
                chunks: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn len(&self) -> usize {
            self.chunks.borrow().len()
        }
        fn last_len(&self) -> Option<usize> {
            self.chunks.borrow().last().map(|c| c.len())
        }
    }

    fn capture<'a>(sink: &'a CapturingSink) -> impl FnMut(&[u8]) -> std::io::Result<()> + 'a {
        move |bytes: &[u8]| {
            sink.chunks.borrow_mut().push(bytes.to_vec());
            Ok(())
        }
    }

    #[test]
    fn chunk_producer_flushes_on_byte_cap() {
        let clock = FakeClock::new(0);
        let cfg = StreamConfig {
            chunk_default_pages: 1, // 16 KiB
            chunk_min_pages: 1,
            chunk_max_pages: 1,
            chunk_max_rows: 1_000_000,
            chunk_max_latency_ms: 1_000_000,
            ..StreamConfig::DEFAULT
        };
        let sink = CapturingSink::new();
        let mut producer = ChunkProducer::new(&cfg, &clock);
        let mut flush = capture(&sink);

        producer
            .push_line(&vec![b'x'; 8 * 1024], &mut flush)
            .unwrap();
        assert_eq!(sink.len(), 0);

        let triggered = producer
            .push_line(&vec![b'y'; 8 * 1024], &mut flush)
            .unwrap();
        assert!(triggered);
        assert_eq!(sink.len(), 1);
        assert_eq!(sink.last_len(), Some(16 * 1024));
        assert_eq!(producer.last_flush_reason(), Some(FlushReason::Byte));
    }

    #[test]
    fn chunk_producer_flushes_on_row_cap() {
        let clock = FakeClock::new(0);
        let cfg = StreamConfig {
            chunk_default_pages: 4, // 64 KiB — well above any test row size
            chunk_min_pages: 1,
            chunk_max_pages: 64,
            chunk_max_rows: 3,
            chunk_max_latency_ms: 1_000_000,
            ..StreamConfig::DEFAULT
        };
        let sink = CapturingSink::new();
        let mut producer = ChunkProducer::new(&cfg, &clock);
        let mut flush = capture(&sink);
        let row = b"{\"row\":{}}\n";
        producer.push_line(row, &mut flush).unwrap();
        producer.push_line(row, &mut flush).unwrap();
        assert_eq!(sink.len(), 0);
        let triggered = producer.push_line(row, &mut flush).unwrap();
        assert!(triggered);
        assert_eq!(sink.len(), 1);
        assert_eq!(producer.last_flush_reason(), Some(FlushReason::Row));
    }

    #[test]
    fn chunk_producer_flushes_on_latency_cap() {
        let clock = FakeClock::new(0);
        let cfg = StreamConfig {
            chunk_default_pages: 4,
            chunk_min_pages: 1,
            chunk_max_pages: 64,
            chunk_max_rows: 1_000_000,
            chunk_max_latency_ms: 50,
            ..StreamConfig::DEFAULT
        };
        let sink = CapturingSink::new();
        let mut producer = ChunkProducer::new(&cfg, &clock);
        let mut flush = capture(&sink);
        producer.push_line(b"{\"row\":{}}\n", &mut flush).unwrap();
        assert_eq!(sink.len(), 0);
        clock.advance(60);
        let triggered = producer.push_line(b"{\"row\":{}}\n", &mut flush).unwrap();
        assert!(triggered);
        assert_eq!(producer.last_flush_reason(), Some(FlushReason::Latency));
    }

    #[test]
    fn chunk_producer_finish_emits_terminal_flush() {
        let clock = FakeClock::new(0);
        let cfg = StreamConfig::DEFAULT;
        let sink = CapturingSink::new();
        let mut producer = ChunkProducer::new(&cfg, &clock);
        let mut flush = capture(&sink);
        producer.push_line(b"{\"row\":{}}\n", &mut flush).unwrap();
        producer.finish(&mut flush).unwrap();
        assert_eq!(sink.len(), 1);
        assert_eq!(producer.last_flush_reason(), Some(FlushReason::Terminal));
    }

    #[test]
    fn write_chunked_helpers_produce_well_formed_chunks() {
        let mut buf: Vec<u8> = Vec::new();
        write_chunked_response_header(&mut buf, 200, "application/x-ndjson").unwrap();
        write_chunk(&mut buf, b"{\"row\":{}}\n").unwrap();
        write_chunked_terminator(&mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Transfer-Encoding: chunked\r\n"));
        // 11 bytes in `{"row":{}}\n` → hex 'b'.
        assert!(text.contains("\r\nb\r\n{\"row\":{}}\n\r\n"));
        assert!(text.ends_with("0\r\n\r\n"));
    }
}
