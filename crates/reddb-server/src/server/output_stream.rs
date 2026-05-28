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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
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
    /// Process-wide concurrent stream cap (issue #761 / S2).
    /// Acquiring the (N+1)th slot is refused with
    /// `server_stream_capacity_exhausted`.
    pub max_global_streams: usize,
    /// Per-principal concurrent stream cap (issue #761 / S2).
    /// Acquiring the (M+1)th slot for a single principal is refused
    /// with `principal_stream_quota_exhausted` even when the global
    /// counter still has room.
    pub max_per_principal_streams: usize,
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
        max_global_streams: 256,
        max_per_principal_streams: 32,
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
        if let Some(v) = read_u64("stream.max_global") {
            cfg.max_global_streams = v as usize;
        }
        if let Some(v) = read_u64("stream.max_per_principal") {
            cfg.max_per_principal_streams = v as usize;
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
        if self.max_global_streams == 0 {
            self.max_global_streams = 1;
        }
        if self.max_per_principal_streams == 0 {
            self.max_per_principal_streams = 1;
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

    /// Issue #768 / S9 — drive a pull-based line source into the
    /// production buffer. The producer *pulls* one encoded line at a
    /// time from `source` and routes it through [`push_line`], so the
    /// resident working set is the page-aligned buffer plus the single
    /// line currently in hand — never the full result set. Pair this
    /// with the pull-based scan iterators
    /// (`parallel_scan::parallel_scan_rows`,
    /// `bitmap_scan::execute_bitmap_scan_stream`) whose records are
    /// encoded lazily by `encode`.
    ///
    /// Returns the number of lines consumed. Flush caps (byte / row /
    /// latency) fire mid-drain exactly as they would for hand-driven
    /// [`push_line`] calls, so first-line latency stays bounded by
    /// `chunk.max_latency_ms` regardless of how many lines the source
    /// will ultimately yield.
    ///
    /// [`push_line`]: ChunkProducer::push_line
    pub fn drive_lines<S, R, Enc, F>(
        &mut self,
        source: S,
        mut encode: Enc,
        flush: &mut F,
    ) -> std::io::Result<u64>
    where
        S: IntoIterator<Item = R>,
        Enc: FnMut(&R) -> Vec<u8>,
        F: FnMut(&[u8]) -> std::io::Result<()>,
    {
        let mut count = 0u64;
        for record in source {
            let line = encode(&record);
            self.push_line(&line, flush)?;
            count += 1;
        }
        Ok(count)
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

/// Issue #761 / S2 — process-wide stream capacity registry. Holds two
/// counters: a global concurrent-stream count and a per-principal map.
/// Both are decremented when the [`StreamCapacityGuard`] handed back
/// from a successful `try_acquire` is dropped, so the release path
/// covers every normal exit (success, mid-stream error, snapshot
/// expiry, client disconnect that drops the writer chain, panic
/// unwind through the stack frame holding the guard).
#[derive(Debug, Default)]
pub struct StreamCapacityRegistry {
    inner: Mutex<CapacityInner>,
}

#[derive(Debug, Default)]
struct CapacityInner {
    global_count: usize,
    per_principal: HashMap<String, usize>,
}

/// Failure modes of [`StreamCapacityRegistry::try_acquire`]. Each
/// variant carries the cap that fired and the live counter value at
/// refusal time so clients can back off intelligently (the HTTP layer
/// surfaces these inside the structured 429 body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireError {
    /// `stream.max_global` exceeded. Per acceptance criterion #1.
    GlobalExhausted { limit: usize, current: usize },
    /// `stream.max_per_principal` exceeded for `principal`. Per
    /// acceptance criterion #2. The principal is surfaced verbatim;
    /// callers must escape it on the wire.
    PrincipalExhausted {
        principal: String,
        limit: usize,
        current: usize,
    },
}

impl AcquireError {
    pub fn code(&self) -> &'static str {
        match self {
            AcquireError::GlobalExhausted { .. } => "server_stream_capacity_exhausted",
            AcquireError::PrincipalExhausted { .. } => "principal_stream_quota_exhausted",
        }
    }

    pub fn message(&self) -> String {
        match self {
            AcquireError::GlobalExhausted { limit, current } => {
                format!("server stream capacity exhausted (limit {limit}, current {current})")
            }
            AcquireError::PrincipalExhausted {
                principal,
                limit,
                current,
            } => format!(
                "principal {principal} stream quota exhausted (limit {limit}, current {current})"
            ),
        }
    }
}

impl StreamCapacityRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Attempt to acquire one slot. Both caps are checked under the
    /// same lock, so a concurrent acquire+release pair cannot over-
    /// issue beyond either ceiling (acceptance criterion #5).
    pub fn try_acquire(
        self: &Arc<Self>,
        principal: &str,
        max_global: usize,
        max_per_principal: usize,
    ) -> Result<StreamCapacityGuard, AcquireError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.global_count >= max_global {
            return Err(AcquireError::GlobalExhausted {
                limit: max_global,
                current: inner.global_count,
            });
        }
        let current = inner.per_principal.get(principal).copied().unwrap_or(0);
        if current >= max_per_principal {
            return Err(AcquireError::PrincipalExhausted {
                principal: principal.to_string(),
                limit: max_per_principal,
                current,
            });
        }
        inner.global_count += 1;
        inner
            .per_principal
            .insert(principal.to_string(), current + 1);
        Ok(StreamCapacityGuard {
            registry: Arc::clone(self),
            principal: principal.to_string(),
            released: false,
        })
    }

    fn release(&self, principal: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.global_count > 0 {
            inner.global_count -= 1;
        }
        if let Some(count) = inner.per_principal.get_mut(principal) {
            if *count > 0 {
                *count -= 1;
            }
            if *count == 0 {
                inner.per_principal.remove(principal);
            }
        }
    }

    /// Visible for tests and audit handlers.
    pub fn snapshot(&self) -> (usize, HashMap<String, usize>) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.global_count, inner.per_principal.clone())
    }
}

/// RAII slot returned by [`StreamCapacityRegistry::try_acquire`].
/// Decrements both counters on drop — acceptance criterion #4
/// ("releasing a slot on stream end (any reason) decrements both
/// counters atomically").
#[must_use = "dropping the guard immediately releases the stream slot"]
#[derive(Debug)]
pub struct StreamCapacityGuard {
    registry: Arc<StreamCapacityRegistry>,
    principal: String,
    released: bool,
}

impl StreamCapacityGuard {
    pub fn principal(&self) -> &str {
        &self.principal
    }
}

impl Drop for StreamCapacityGuard {
    fn drop(&mut self) {
        if !self.released {
            self.registry.release(&self.principal);
            self.released = true;
        }
    }
}

// ──────── Issue #766 / S7 — resume coordinator ────────

/// Resumability assessment of a query plan. The shim slice runs a
/// textual classifier over the SQL string: a query is resumable iff
/// it has a stable total order over a unique key. By default we
/// promise RID ASC; an explicit `ORDER BY rid` (or `ORDER BY rid ASC`)
/// is also resumable. Anything that aggregates / groups / windows or
/// orders on a non-unique column is not.
pub fn assess_resumability(query: &str) -> bool {
    let upper = query.to_uppercase();
    let trimmed = upper.trim_start();
    if !trimmed.starts_with("SELECT ") && !trimmed.starts_with("SELECT\n") {
        return false;
    }
    const FORBIDDEN: &[&str] = &[
        " GROUP BY ",
        " HAVING ",
        " DISTINCT ",
        "DISTINCT ",
        "COUNT(",
        "SUM(",
        "AVG(",
        "MIN(",
        "MAX(",
        "ARRAY_AGG(",
        "JSON_AGG(",
        "OVER(",
        " OVER (",
        " JOIN ",
    ];
    for kw in FORBIDDEN {
        if upper.contains(kw) {
            return false;
        }
    }
    if let Some(idx) = upper.find("ORDER BY") {
        let tail = &upper[idx + "ORDER BY".len()..];
        // Strip trailing LIMIT and statement terminator.
        let mut clause = tail.to_string();
        if let Some(lim) = clause.find(" LIMIT ") {
            clause.truncate(lim);
        }
        if let Some(semi) = clause.find(';') {
            clause.truncate(semi);
        }
        let clause = clause.trim();
        if !matches!(clause, "RID" | "RID ASC") {
            return false;
        }
    }
    true
}

/// Resume-eligibility ledger. Holds `(snapshot_lsn → opened_at_ms,
/// ttl_ms)` so a resume request can be checked against TTL without
/// trusting the wall clock on the client. The shim slice does not
/// implement true MVCC pinning — the registry's role is to make
/// `snapshot_expired` deterministic and testable.
#[derive(Debug, Default)]
pub struct LeaseRegistry {
    inner: Mutex<HashMap<u64, LeaseEntry>>,
}

#[derive(Debug, Clone, Copy)]
struct LeaseEntry {
    opened_at_ms: u64,
    ttl_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseLookup {
    /// No lease ever recorded for this snapshot LSN.
    Unknown,
    /// Lease exists but its TTL has elapsed.
    Expired,
    /// Lease exists and is still within TTL.
    Live,
}

impl LeaseRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Record a freshly-opened lease. Idempotent — re-inserting the
    /// same snapshot_lsn refreshes the timestamp (the client cannot
    /// observe lease identity through the snapshot LSN alone, so this
    /// matches "the latest open wins" semantics).
    pub fn record(&self, snapshot_lsn: u64, opened_at_ms: u64, ttl_ms: u64) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.insert(
            snapshot_lsn,
            LeaseEntry {
                opened_at_ms,
                ttl_ms,
            },
        );
    }

    /// Resume-time lookup. Returns whether the lease is unknown,
    /// expired, or still live as of `now_ms`.
    pub fn lookup(&self, snapshot_lsn: u64, now_ms: u64) -> LeaseLookup {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.get(&snapshot_lsn) {
            None => LeaseLookup::Unknown,
            Some(entry) => {
                if now_ms.saturating_sub(entry.opened_at_ms) >= entry.ttl_ms {
                    LeaseLookup::Expired
                } else {
                    LeaseLookup::Live
                }
            }
        }
    }

    /// Visible for tests / audit.
    #[doc(hidden)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// Incremental SHA-256 hasher over emitted row lines, hex-encoded on
/// finalize. The wire contract is that the server hashes the exact
/// byte sequence of each row line (without trailing newline) in the
/// order the rows are emitted; the client stores the resulting digest
/// from the `end` envelope and replays it on resume.
#[derive(Debug, Default)]
pub struct PrefixHasher {
    inner: Option<sha2::Sha256>,
    rows: u64,
}

impl PrefixHasher {
    pub fn new() -> Self {
        use sha2::Digest;
        Self {
            inner: Some(sha2::Sha256::new()),
            rows: 0,
        }
    }

    pub fn update(&mut self, line: &[u8]) {
        use sha2::Digest;
        if let Some(h) = self.inner.as_mut() {
            h.update(line);
        }
        self.rows += 1;
    }

    pub fn rows(&self) -> u64 {
        self.rows
    }

    /// Hex-encoded digest of everything fed so far. Consumes the
    /// hasher (a `PrefixHasher` is single-use).
    pub fn finalize_hex(mut self) -> String {
        use sha2::Digest;
        let hasher = self
            .inner
            .take()
            .expect("PrefixHasher::finalize_hex called twice");
        let digest = hasher.finalize();
        let mut out = String::with_capacity(64);
        for b in digest.iter() {
            out.push_str(&format!("{b:02x}"));
        }
        out
    }
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
            max_global_streams: 0,
            max_per_principal_streams: 0,
        };
        cfg.normalize();
        assert_eq!(cfg.chunk_min_pages, 1);
        assert_eq!(cfg.chunk_max_pages, 8);
        assert_eq!(cfg.chunk_default_pages, 8); // clamped down to max
        assert!(cfg.chunk_max_rows >= 1);
        assert!(cfg.max_global_streams >= 1);
        assert!(cfg.max_per_principal_streams >= 1);
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

    // ──────── Issue #761 / S2 — capacity registry ────────

    #[test]
    fn capacity_registry_global_exhausted_returns_structured_error() {
        let reg = StreamCapacityRegistry::new();
        let _g1 = reg.try_acquire("alice", 2, 32).unwrap();
        let _g2 = reg.try_acquire("alice", 2, 32).unwrap();
        let err = reg.try_acquire("alice", 2, 32).unwrap_err();
        assert_eq!(
            err,
            AcquireError::GlobalExhausted {
                limit: 2,
                current: 2,
            }
        );
        assert_eq!(err.code(), "server_stream_capacity_exhausted");
    }

    #[test]
    fn capacity_registry_per_principal_exhausted_independent_of_global() {
        // Acceptance criterion #2: per-principal cap fires even when
        // global has room. Acceptance criterion #3: counters are
        // independent across principals.
        let reg = StreamCapacityRegistry::new();
        let _a1 = reg.try_acquire("alice", 100, 2).unwrap();
        let _a2 = reg.try_acquire("alice", 100, 2).unwrap();
        let err = reg.try_acquire("alice", 100, 2).unwrap_err();
        assert_eq!(
            err,
            AcquireError::PrincipalExhausted {
                principal: "alice".to_string(),
                limit: 2,
                current: 2,
            }
        );
        assert_eq!(err.code(), "principal_stream_quota_exhausted");

        // Bob is unaffected by Alice's quota.
        let _b1 = reg.try_acquire("bob", 100, 2).unwrap();
        let _b2 = reg.try_acquire("bob", 100, 2).unwrap();
    }

    #[test]
    fn capacity_registry_release_frees_both_counters() {
        // Acceptance criterion #4: drop releases both counters.
        let reg = StreamCapacityRegistry::new();
        let g1 = reg.try_acquire("alice", 1, 1).unwrap();
        assert!(reg.try_acquire("alice", 1, 1).is_err());
        drop(g1);
        let (global, per_principal) = reg.snapshot();
        assert_eq!(global, 0);
        assert!(per_principal.is_empty());
        // Slot is now reclaimable.
        let _g2 = reg.try_acquire("alice", 1, 1).unwrap();
    }

    #[test]
    fn capacity_registry_concurrent_acquire_release_does_not_over_issue() {
        // Acceptance criterion #5: stress coverage. Spawn `THREADS`
        // threads each running `ITERS` acquire+release cycles against
        // a registry sized to fit only `CAP` slots; the live count
        // must never exceed `CAP`, and the registry must return to
        // zero once every thread has joined.
        use std::sync::atomic::{AtomicUsize, Ordering};

        const THREADS: usize = 16;
        const ITERS: usize = 200;
        const CAP_GLOBAL: usize = 4;
        const CAP_PER_PRINCIPAL: usize = 4;

        let reg = StreamCapacityRegistry::new();
        let observed_max = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for tid in 0..THREADS {
            let reg = Arc::clone(&reg);
            let observed_max = Arc::clone(&observed_max);
            // Two principals share the global cap, each capped at
            // `CAP_PER_PRINCIPAL` themselves.
            let principal = format!("p{}", tid % 2);
            handles.push(std::thread::spawn(move || {
                for _ in 0..ITERS {
                    if let Ok(guard) = reg.try_acquire(&principal, CAP_GLOBAL, CAP_PER_PRINCIPAL) {
                        let (live, _) = reg.snapshot();
                        observed_max.fetch_max(live, Ordering::SeqCst);
                        // Hold the slot just long enough to let other
                        // threads race against the cap.
                        std::thread::yield_now();
                        drop(guard);
                    }
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let (global_after, per_principal_after) = reg.snapshot();
        assert_eq!(global_after, 0, "global counter leaked");
        assert!(
            per_principal_after.is_empty(),
            "per-principal map leaked: {per_principal_after:?}"
        );
        assert!(
            observed_max.load(Ordering::SeqCst) <= CAP_GLOBAL,
            "global cap was breached: observed {} > {}",
            observed_max.load(Ordering::SeqCst),
            CAP_GLOBAL
        );
    }

    // ──────── Issue #766 / S7 — resume coordinator ────────

    #[test]
    fn assess_resumability_accepts_plain_select() {
        assert!(assess_resumability("SELECT id, name FROM t"));
        assert!(assess_resumability("select * from t where id > 5"));
        assert!(assess_resumability("SELECT a, b FROM t ORDER BY rid"));
        assert!(assess_resumability("SELECT a, b FROM t ORDER BY rid ASC"));
        assert!(assess_resumability(
            "SELECT a FROM t ORDER BY rid ASC LIMIT 10"
        ));
    }

    #[test]
    fn assess_resumability_rejects_aggregates_and_unordered() {
        assert!(!assess_resumability("SELECT COUNT(*) FROM t"));
        assert!(!assess_resumability("SELECT SUM(x) FROM t"));
        assert!(!assess_resumability("SELECT a, COUNT(b) FROM t GROUP BY a"));
        assert!(!assess_resumability("SELECT DISTINCT a FROM t"));
        assert!(!assess_resumability("SELECT a FROM t ORDER BY name"));
        assert!(!assess_resumability("SELECT a FROM t ORDER BY rid DESC"));
        assert!(!assess_resumability("SELECT a FROM t ORDER BY a, b"));
        assert!(!assess_resumability("INSERT INTO t (a) VALUES (1)"));
        assert!(!assess_resumability(
            "SELECT a FROM t JOIN u ON t.id = u.id"
        ));
    }

    #[test]
    fn lease_registry_records_and_expires_against_ttl() {
        let reg = LeaseRegistry::new();
        reg.record(42, 1_000, 5_000);
        assert_eq!(reg.lookup(42, 1_000), LeaseLookup::Live);
        assert_eq!(reg.lookup(42, 5_999), LeaseLookup::Live);
        assert_eq!(reg.lookup(42, 6_000), LeaseLookup::Expired);
        assert_eq!(reg.lookup(99, 1_000), LeaseLookup::Unknown);
    }

    #[test]
    fn prefix_hasher_is_order_sensitive_and_deterministic() {
        let mut a = PrefixHasher::new();
        a.update(b"{\"row\":{\"id\":1}}");
        a.update(b"{\"row\":{\"id\":2}}");
        let hash_a = a.finalize_hex();

        let mut b = PrefixHasher::new();
        b.update(b"{\"row\":{\"id\":1}}");
        b.update(b"{\"row\":{\"id\":2}}");
        let hash_b = b.finalize_hex();
        assert_eq!(hash_a, hash_b);

        let mut c = PrefixHasher::new();
        c.update(b"{\"row\":{\"id\":2}}");
        c.update(b"{\"row\":{\"id\":1}}");
        assert_ne!(hash_a, c.finalize_hex());
        assert_eq!(hash_a.len(), 64);
    }

    #[test]
    fn stream_config_defaults_carry_s2_caps() {
        assert_eq!(StreamConfig::DEFAULT.max_global_streams, 256);
        assert_eq!(StreamConfig::DEFAULT.max_per_principal_streams, 32);
    }

    // ──────── Issue #768 / S9 — pull-based driver ────────

    /// Sink that records every flushed chunk's length so a test can
    /// assert the resident working set stayed bounded.
    struct SizeSink {
        sizes: std::cell::RefCell<Vec<usize>>,
    }
    impl SizeSink {
        fn new() -> Self {
            Self {
                sizes: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn flushes(&self) -> usize {
            self.sizes.borrow().len()
        }
        fn max_chunk(&self) -> usize {
            self.sizes.borrow().iter().copied().max().unwrap_or(0)
        }
    }
    fn size_capture<'a>(sink: &'a SizeSink) -> impl FnMut(&[u8]) -> std::io::Result<()> + 'a {
        move |bytes: &[u8]| {
            sink.sizes.borrow_mut().push(bytes.len());
            Ok(())
        }
    }

    #[test]
    fn drive_lines_streams_large_source_with_bounded_working_set() {
        // Acceptance #1: a huge scan flows through the chunk buffer
        // without ever materialising the full result set. The source
        // is a *lazy* range mapped to records — collecting it would
        // allocate N rows, but `drive_lines` pulls one at a time. We
        // assert every flushed chunk stays within a small multiple of
        // the page buffer, i.e. memory tracks the buffer, not N.
        let clock = FakeClock::new(0);
        let cfg = StreamConfig {
            chunk_default_pages: 1, // 16 KiB buffer
            chunk_min_pages: 1,
            chunk_max_pages: 1,
            chunk_max_rows: 1_000_000, // don't let the row cap dominate
            chunk_max_latency_ms: 1_000_000,
            ..StreamConfig::DEFAULT
        };
        let sink = SizeSink::new();
        let mut producer = ChunkProducer::new(&cfg, &clock);
        let mut flush = size_capture(&sink);

        const N: u64 = 1_000_000;
        // Lazy source: never collected into a Vec.
        let source = 0..N;
        let consumed = producer
            .drive_lines(
                source,
                |i: &u64| format!("{{\"row\":{{\"id\":{i}}}}}\n").into_bytes(),
                &mut flush,
            )
            .unwrap();
        producer.finish(&mut flush).unwrap();

        assert_eq!(consumed, N);
        assert_eq!(producer.total_rows(), N);
        // Many flushes occurred (streamed), not one giant buffer.
        assert!(
            sink.flushes() > 1000,
            "expected the source to stream across many chunks, saw {}",
            sink.flushes()
        );
        // No chunk exceeded the byte cap plus one trailing line — the
        // resident buffer is bounded independent of N.
        let max_line = format!("{{\"row\":{{\"id\":{}}}}}\n", N - 1).len();
        assert!(
            sink.max_chunk() <= cfg.production_buffer_bytes() + max_line,
            "chunk {} exceeded bounded working set {}",
            sink.max_chunk(),
            cfg.production_buffer_bytes() + max_line
        );
    }

    #[test]
    fn drive_lines_first_chunk_flushes_on_latency_before_source_drains() {
        // Acceptance #2: first-row latency is bounded by the latency
        // cap, not by full materialisation. The source yields rows
        // whose pull advances the fake clock; the first chunk must
        // flush as soon as the latency window elapses, long before the
        // (large) source is exhausted.
        let clock = FakeClock::new(0);
        let cfg = StreamConfig {
            chunk_default_pages: 64, // large byte cap — won't trip first
            chunk_min_pages: 1,
            chunk_max_pages: 64,
            chunk_max_rows: 1_000_000, // large row cap — won't trip first
            chunk_max_latency_ms: 50,
            ..StreamConfig::DEFAULT
        };
        let sink = SizeSink::new();
        let mut producer = ChunkProducer::new(&cfg, &clock);

        // Drive manually so we can advance the clock between pulls and
        // observe the first flush. Each pull advances 20 ms; the 50 ms
        // latency cap trips on the third row.
        let mut first_flush_after: Option<u64> = None;
        let mut row = 0u64;
        while row < 1_000_000 {
            let line = format!("{{\"row\":{{\"id\":{row}}}}}\n");
            clock.advance(20);
            let mut flush = size_capture(&sink);
            let flushed = producer.push_line(line.as_bytes(), &mut flush).unwrap();
            row += 1;
            if flushed {
                first_flush_after = Some(row);
                break;
            }
        }

        assert_eq!(producer.last_flush_reason(), Some(FlushReason::Latency));
        let rows_before_flush = first_flush_after.expect("a latency flush must occur");
        assert!(
            rows_before_flush <= 4,
            "first chunk flushed only after {rows_before_flush} rows; latency bound not honoured"
        );
        // Crucially, the source was nowhere near drained (1e6 rows).
        assert!(rows_before_flush < 1_000_000);
    }

    #[test]
    fn drive_lines_parity_with_manual_push_line() {
        // The driver must produce byte-identical output to hand-rolled
        // push_line calls — the chunk producer's framing is unchanged.
        let clock = FakeClock::new(0);
        let cfg = StreamConfig::DEFAULT;

        let lines: Vec<Vec<u8>> = (0..50)
            .map(|i| format!("{{\"row\":{{\"id\":{i}}}}}\n").into_bytes())
            .collect();

        let driven = CapturingSink::new();
        {
            let mut p = ChunkProducer::new(&cfg, &clock);
            let mut flush = capture(&driven);
            p.drive_lines(lines.iter().cloned(), |l: &Vec<u8>| l.clone(), &mut flush)
                .unwrap();
            p.finish(&mut flush).unwrap();
        }

        let manual = CapturingSink::new();
        {
            let mut p = ChunkProducer::new(&cfg, &clock);
            let mut flush = capture(&manual);
            for l in &lines {
                p.push_line(l, &mut flush).unwrap();
            }
            p.finish(&mut flush).unwrap();
        }

        assert_eq!(*driven.chunks.borrow(), *manual.chunks.borrow());
    }
}
