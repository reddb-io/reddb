//! Bounded-memory streaming output channel for the query executor (#806).
//!
//! Second slice of #750. The executor historically materialised the
//! entire result set into a `Vec<UnifiedRecord>` before the caller saw
//! a single row. This module introduces a chunked streaming channel —
//! [`RowStream`] — that pulls rows one at a time from a fallible source
//! iterator and groups them into [`RowChunk`]s no larger than a
//! high-water mark, so the resident working set is **one chunk, not the
//! whole result**.
//!
//! Two construction modes share the same downstream surface:
//!
//! * [`RowStream::from_lazy`] wraps a pull iterator that produces rows
//!   on demand (e.g. an unfiltered table scan that converts cheap entity
//!   handles to records lazily). Peak resident records is `O(chunk)` —
//!   see [`RowStream::peak_buffered`] and the bounded-memory unit test.
//! * [`RowStream::from_unified`] wraps an already-materialised
//!   [`UnifiedResult`]. Ordering-/grouping-dependent shapes (ORDER BY,
//!   GROUP BY / aggregate, join) are inherently `O(N)` to compute, so
//!   they materialise as before and are then re-chunked for output —
//!   preserving their ordering and snapshot guarantees while still
//!   exposing the streaming surface. [`RowStream::collect_unified`]
//!   round-trips such a stream back to a byte-identical `UnifiedResult`,
//!   which is how the existing `/query` route consumes the new path
//!   (collecting chunks internally) without any observable change.
//!
//! A stream always closes with a [`StreamTerminal`] frame: a clean end
//! carries the row count; a source error surfaces as a documented
//! [`StreamTerminal::Error`] frame rather than truncating the stream
//! silently. This mirrors the #805 `/query/stream` transport's terminal
//! `{"end": …}` / `{"error": …}` frames at the executor level.

use super::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Default chunk high-water mark (rows). Bounds the resident record set
/// of a [`RowStream`] regardless of how many rows the source yields.
pub(crate) const DEFAULT_HIGH_WATER_MARK: usize = 1024;

/// Per-owner buffer arena for query-result row chunks (#885).
///
/// A [`RowStream`] historically allocated a fresh `Vec<UnifiedRecord>`
/// for every chunk it assembled in [`RowStream::next_chunk`]. For a
/// result that spans many chunks that is one heap allocation per
/// chunk-fetch on the row-streaming path. This arena keeps a small
/// free-list of emptied buffers so the chunk Vec is reused across the
/// chunk-fetches of a single statement instead of reallocated.
///
/// Ownership model (the safety argument from the issue): the arena is
/// owned by the `StatementFrame` that already owns the query lifecycle
/// and lent to the stream it spawns via an `Rc`. It is **not** a
/// `thread_local!` scratch — under tokio's multi-threaded work-stealing
/// runtime a task may resume on a different worker after `.await`, which
/// would make thread-local scratch unsound; tying the buffer lifetime to
/// the frame sidesteps that entirely. The arena is single-owner and
/// never shared across threads.
///
/// Reuse is leak-free by construction: a buffer is cleared the moment it
/// is recycled (and again when leased), so no record from a prior chunk
/// can bleed into a reused buffer — only the backing allocation is
/// retained. Caps bound how much memory the free-list can pin so a single
/// oversized chunk does not hoard capacity for the rest of the frame.
#[derive(Debug, Default)]
pub(crate) struct RowBufferArena {
    /// Emptied buffers available for reuse. Each is `len == 0`; only the
    /// backing capacity is retained.
    free: Vec<Vec<UnifiedRecord>>,
}

impl RowBufferArena {
    /// Maximum number of buffers the free-list retains. Only one chunk
    /// buffer is ever in flight per stream (lease → consume → recycle),
    /// so a small cap is plenty; the extra slots absorb the rare case of
    /// nested streams sharing one frame arena.
    const MAX_BUFFERS: usize = 4;
    /// Drop (rather than retain) any recycled buffer whose capacity grew
    /// past this many records, so a one-off huge chunk cannot pin a large
    /// allocation for the remainder of the frame.
    const MAX_BUFFER_CAPACITY: usize = DEFAULT_HIGH_WATER_MARK * 4;

    pub(crate) fn new() -> Self {
        Self { free: Vec::new() }
    }

    /// Hand out a cleared buffer for the next chunk. Reuses a recycled
    /// allocation when one is available, otherwise allocates fresh. The
    /// returned buffer is always empty, so a reused buffer never carries
    /// rows from a prior chunk.
    pub(crate) fn lease(&mut self) -> Vec<UnifiedRecord> {
        match self.free.pop() {
            Some(mut buf) => {
                buf.clear();
                buf
            }
            None => Vec::new(),
        }
    }

    /// Return a drained buffer to the free-list for reuse. Clears it first
    /// so no record can bleed across reuses, and refuses to retain it when
    /// the free-list is full or the buffer's capacity is oversized.
    pub(crate) fn recycle(&mut self, mut buf: Vec<UnifiedRecord>) {
        if self.free.len() >= Self::MAX_BUFFERS || buf.capacity() > Self::MAX_BUFFER_CAPACITY {
            return;
        }
        buf.clear();
        self.free.push(buf);
    }

    /// Reclaim the arena to a clean state at frame end — drops every
    /// retained buffer so nothing is pinned past the frame that owns it.
    pub(crate) fn reset(&mut self) {
        self.free.clear();
    }

    /// Number of buffers currently held for reuse. Observability surface
    /// for the reuse / no-bleed unit tests.
    #[cfg(test)]
    pub(crate) fn pooled(&self) -> usize {
        self.free.len()
    }
}

/// One bounded batch of rows emitted by a [`RowStream`]. Its length never
/// exceeds the stream's high-water mark.
#[derive(Debug, Default)]
pub(crate) struct RowChunk {
    pub(crate) records: Vec<UnifiedRecord>,
}

/// Terminal frame closing a [`RowStream`].
///
/// `End` is emitted once the source drains cleanly. `Error` is emitted
/// when the source yields an `Err` mid-stream: the rows already produced
/// are delivered, then the stream closes with the documented error frame
/// — it is never silently truncated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StreamTerminal {
    End { row_count: u64 },
    Error { code: String, message: String },
}

/// Map a [`RedDBError`] to the same stable machine token the #805
/// `/query/stream` transport uses for its terminal error frame, so the
/// executor-level terminal and the wire-level terminal agree.
fn terminal_error_code(err: &RedDBError) -> &'static str {
    match err {
        RedDBError::NotFound(_) => "not_found",
        RedDBError::Query(_) => "query_error",
        RedDBError::MaterializationLimitExceeded { .. } => "materialization_limit_exceeded",
        _ => "internal_error",
    }
}

/// Bounded-memory chunked row stream.
///
/// Pulls rows from `source` and groups them into [`RowChunk`]s of at most
/// `high_water_mark` rows. The only records resident at once are the
/// chunk currently being assembled, so peak memory is `O(high_water_mark)`
/// rather than `O(N)`.
pub(crate) struct RowStream {
    /// Projected column names, fixed for the lifetime of the stream.
    columns: Vec<String>,
    /// Carried-through query statistics (from the materialised source, if any).
    stats: crate::storage::query::unified::QueryStats,
    /// Fast-path pre-serialized JSON carried through from a materialised
    /// source so `collect_unified` round-trips it verbatim.
    pre_serialized_json: Option<String>,
    /// Maximum rows per emitted chunk.
    high_water_mark: usize,
    /// Fallible row source. `next()` returning `Some(Err(_))` closes the
    /// stream with a terminal error frame.
    source: Box<dyn Iterator<Item = RedDBResult<UnifiedRecord>>>,
    /// Total rows emitted so far.
    row_count: u64,
    /// Largest number of records ever resident in a single in-flight chunk.
    /// Observability surface for the bounded-memory tests; the value is
    /// written on the hot path but only read under `cfg(test)`.
    #[cfg_attr(not(test), allow(dead_code))]
    peak_buffered: usize,
    /// Set once the source drains or errors.
    terminal: Option<StreamTerminal>,
    /// Optional per-owner buffer arena (#885). When present, chunk
    /// buffers are leased from / recycled to this arena instead of
    /// allocated fresh per chunk. `None` preserves the original
    /// allocate-per-chunk behaviour exactly (used by every consumer that
    /// is not driven by a `StatementFrame`-owned arena).
    arena: Option<Rc<RefCell<RowBufferArena>>>,
}

impl RowStream {
    /// Wrap a lazy, fallible row source. Column names and stats are
    /// supplied by the caller (the source produces only rows).
    pub(crate) fn from_lazy(
        columns: Vec<String>,
        stats: crate::storage::query::unified::QueryStats,
        high_water_mark: usize,
        source: Box<dyn Iterator<Item = RedDBResult<UnifiedRecord>>>,
    ) -> Self {
        Self {
            columns,
            stats,
            pre_serialized_json: None,
            high_water_mark: high_water_mark.max(1),
            source,
            row_count: 0,
            peak_buffered: 0,
            terminal: None,
            arena: None,
        }
    }

    /// Wrap an already-materialised [`UnifiedResult`]. The records are
    /// re-chunked for output; `collect_unified` reverses this exactly.
    ///
    /// Used for ordering-/grouping-dependent shapes (ORDER BY, aggregate,
    /// join) whose computation is inherently `O(N)` — they keep their
    /// existing semantics and simply gain the streaming surface.
    pub(crate) fn from_unified(result: UnifiedResult, high_water_mark: usize) -> Self {
        let UnifiedResult {
            columns,
            records,
            stats,
            pre_serialized_json,
        } = result;
        Self {
            columns,
            stats,
            pre_serialized_json,
            high_water_mark: high_water_mark.max(1),
            source: Box::new(records.into_iter().map(Ok)),
            row_count: 0,
            peak_buffered: 0,
            terminal: None,
            arena: None,
        }
    }

    /// Bind a per-owner buffer arena (#885) to this stream. Chunk buffers
    /// will be leased from / recycled to it instead of allocated fresh per
    /// chunk. Builder-style so existing constructors keep their original
    /// signatures and the arena stays opt-in.
    pub(crate) fn with_arena(mut self, arena: Rc<RefCell<RowBufferArena>>) -> Self {
        self.arena = Some(arena);
        self
    }

    /// Largest record count ever resident in one in-flight chunk. Bounded
    /// by the high-water mark by construction — the bounded-memory proof.
    #[cfg(test)]
    pub(crate) fn peak_buffered(&self) -> usize {
        self.peak_buffered
    }

    /// Terminal frame, available only after the stream is fully drained.
    #[cfg(test)]
    pub(crate) fn terminal(&self) -> Option<&StreamTerminal> {
        self.terminal.as_ref()
    }

    /// Pull the next bounded chunk. Returns `None` once the stream has
    /// closed (drained or errored); inspect [`RowStream::terminal`] for
    /// the reason. A returned chunk is always non-empty.
    pub(crate) fn next_chunk(&mut self) -> Option<RowChunk> {
        if self.terminal.is_some() {
            return None;
        }
        // Lease a chunk buffer from the per-owner arena when one is wired
        // (#885); otherwise allocate fresh, preserving the original
        // per-chunk allocation behaviour byte-for-byte.
        let mut records: Vec<UnifiedRecord> = match &self.arena {
            Some(arena) => arena.borrow_mut().lease(),
            None => Vec::new(),
        };
        while records.len() < self.high_water_mark {
            match self.source.next() {
                Some(Ok(record)) => records.push(record),
                Some(Err(err)) => {
                    // Deliver rows gathered before the failure, then close
                    // with the documented error frame — never truncate
                    // silently.
                    self.terminal = Some(StreamTerminal::Error {
                        code: terminal_error_code(&err).to_string(),
                        message: err.to_string(),
                    });
                    break;
                }
                None => {
                    self.terminal = Some(StreamTerminal::End {
                        row_count: self.row_count + records.len() as u64,
                    });
                    break;
                }
            }
        }
        self.peak_buffered = self.peak_buffered.max(records.len());
        self.row_count += records.len() as u64;
        if records.is_empty() {
            // Either we errored with nothing buffered, or the source was
            // empty; ensure a terminal is set and stop. Recycle the
            // (empty) leased buffer rather than dropping it, so a stream
            // whose row count is an exact multiple of the high-water mark
            // does not silently discard the chunk allocation (#885).
            if let Some(arena) = &self.arena {
                arena.borrow_mut().recycle(records);
            }
            if self.terminal.is_none() {
                self.terminal = Some(StreamTerminal::End {
                    row_count: self.row_count,
                });
            }
            return None;
        }
        Some(RowChunk { records })
    }

    /// Drain the stream into a [`UnifiedResult`], collecting chunks
    /// internally. Columns / stats / pre-serialized JSON are carried
    /// through verbatim, so a `from_unified(r).collect_unified()`
    /// round-trip reproduces `r` exactly. A source error surfaces as the
    /// corresponding `Err`, never as a short read.
    pub(crate) fn collect_unified(mut self) -> RedDBResult<UnifiedResult> {
        let mut records: Vec<UnifiedRecord> = Vec::new();
        while let Some(chunk) = self.next_chunk() {
            // `append` moves the chunk's rows into the accumulator in
            // order (identical to the old `extend`) and leaves the chunk
            // buffer empty-but-allocated, so it can be recycled to the
            // arena for the next chunk-fetch (#885). Without an arena the
            // buffer simply drops here, exactly as before.
            let mut buf = chunk.records;
            records.append(&mut buf);
            if let Some(arena) = &self.arena {
                arena.borrow_mut().recycle(buf);
            }
        }
        if let Some(StreamTerminal::Error { message, .. }) = self.terminal {
            return Err(RedDBError::Query(message));
        }
        Ok(UnifiedResult {
            columns: self.columns,
            records,
            stats: self.stats,
            pre_serialized_json: self.pre_serialized_json,
        })
    }

    /// Drain the stream into a flat `Vec<UnifiedRecord>`, collecting chunks
    /// internally. A source error surfaces as the corresponding `Err`
    /// rather than a short read.
    pub(crate) fn collect_records(mut self) -> RedDBResult<Vec<UnifiedRecord>> {
        let mut records: Vec<UnifiedRecord> = Vec::new();
        while let Some(chunk) = self.next_chunk() {
            // See `collect_unified`: append-then-recycle reuses the chunk
            // buffer via the arena (#885) while preserving row order.
            let mut buf = chunk.records;
            records.append(&mut buf);
            if let Some(arena) = &self.arena {
                arena.borrow_mut().recycle(buf);
            }
        }
        if let Some(StreamTerminal::Error { message, .. }) = self.terminal {
            return Err(RedDBError::Query(message));
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::Value;

    fn row(i: u64) -> UnifiedRecord {
        let mut r = UnifiedRecord::new();
        r.set("n", Value::UnsignedInteger(i));
        r
    }

    #[test]
    fn bounded_memory_peak_never_exceeds_high_water_mark() {
        // A large source (N = 10_000) drained through a small chunk cap
        // must never hold more than `hwm` records at once: the bounded-
        // memory guarantee (criterion: memory ∝ chunk size, not N).
        const N: u64 = 10_000;
        const HWM: usize = 128;
        let source = (0..N).map(|i| Ok(row(i)));
        let mut stream =
            RowStream::from_lazy(vec!["n".into()], Default::default(), HWM, Box::new(source));

        let mut total = 0u64;
        let mut chunks = 0u64;
        while let Some(chunk) = stream.next_chunk() {
            assert!(chunk.records.len() <= HWM, "chunk exceeded high-water mark");
            assert!(
                !chunk.records.is_empty(),
                "next_chunk yields only non-empty chunks"
            );
            total += chunk.records.len() as u64;
            chunks += 1;
        }
        assert_eq!(total, N);
        assert!(
            chunks >= N / HWM as u64,
            "source must be split into many chunks"
        );
        assert_eq!(
            stream.peak_buffered(),
            HWM,
            "peak resident set is exactly one chunk"
        );
        assert_eq!(
            stream.terminal(),
            Some(&StreamTerminal::End { row_count: N })
        );
    }

    #[test]
    fn mid_stream_error_surfaces_as_terminal_frame_after_delivering_rows() {
        // Three good rows, then a failure. The good rows are delivered,
        // and the stream closes with the documented error terminal — it
        // is not silently truncated.
        let mut yielded = 0;
        let source = std::iter::from_fn(move || {
            yielded += 1;
            match yielded {
                1..=3 => Some(Ok(row(yielded))),
                4 => Some(Err(RedDBError::Query("boom".into()))),
                _ => None,
            }
        });
        let mut stream =
            RowStream::from_lazy(vec!["n".into()], Default::default(), 2, Box::new(source));

        let mut delivered = 0u64;
        while let Some(chunk) = stream.next_chunk() {
            delivered += chunk.records.len() as u64;
        }
        assert_eq!(
            delivered, 3,
            "rows before the error are delivered, not dropped"
        );
        match stream.terminal() {
            Some(StreamTerminal::Error { code, message }) => {
                assert_eq!(code, "query_error");
                assert_eq!(message, "query error: boom");
            }
            other => panic!("expected error terminal, got {other:?}"),
        }
    }

    #[test]
    fn collect_unified_round_trips_a_materialised_result_verbatim() {
        let original = UnifiedResult {
            columns: vec!["a".into(), "b".into()],
            records: vec![row(1), row(2), row(3)],
            stats: Default::default(),
            pre_serialized_json: Some("{\"fast\":true}".into()),
        };
        let stream = RowStream::from_unified(original.clone(), 2);
        let collected = stream.collect_unified().expect("clean stream collects ok");
        assert_eq!(collected.columns, original.columns);
        assert_eq!(collected.records.len(), original.records.len());
        assert_eq!(collected.pre_serialized_json, original.pre_serialized_json);
    }

    #[test]
    fn collect_unified_propagates_a_source_error() {
        let source = std::iter::from_fn({
            let mut n = 0;
            move || {
                n += 1;
                match n {
                    1 => Some(Ok(row(1))),
                    2 => Some(Err(RedDBError::NotFound("t".into()))),
                    _ => None,
                }
            }
        });
        let stream =
            RowStream::from_lazy(vec!["n".into()], Default::default(), 8, Box::new(source));
        assert!(stream.collect_unified().is_err());
    }

    #[test]
    fn empty_source_closes_with_zero_row_end() {
        let source = std::iter::empty::<RedDBResult<UnifiedRecord>>();
        let mut stream = RowStream::from_lazy(Vec::new(), Default::default(), 16, Box::new(source));
        assert!(stream.next_chunk().is_none());
        assert_eq!(
            stream.terminal(),
            Some(&StreamTerminal::End { row_count: 0 })
        );
    }

    /// A leased buffer is always empty even when reused — recycling a
    /// buffer that held rows must not let those rows bleed into the next
    /// lease (#885 acceptance: "no data bleeds across requests when a
    /// buffer is reused").
    #[test]
    fn arena_lease_never_bleeds_prior_rows() {
        let mut arena = RowBufferArena::new();
        let mut buf = arena.lease();
        assert!(buf.is_empty(), "fresh lease is empty");
        buf.push(row(1));
        buf.push(row(2));
        arena.recycle(buf);
        assert_eq!(arena.pooled(), 1, "recycled buffer is retained for reuse");

        let reused = arena.lease();
        assert!(
            reused.is_empty(),
            "a reused buffer carries no rows from the prior chunk"
        );
        assert_eq!(arena.pooled(), 0, "leasing drains the free-list slot");
    }

    /// `reset()` reclaims the arena to a clean state — every retained
    /// buffer is dropped (#885 acceptance: "reset to a clean state at
    /// frame end").
    #[test]
    fn arena_reset_drops_retained_buffers() {
        let mut arena = RowBufferArena::new();
        let buf = arena.lease();
        arena.recycle(buf);
        let buf2 = arena.lease();
        arena.recycle(buf2);
        assert!(arena.pooled() >= 1);
        arena.reset();
        assert_eq!(arena.pooled(), 0, "reset clears the free-list");
    }

    /// Driving a multi-chunk stream through an arena recycles the single
    /// in-flight chunk buffer across chunk-fetches instead of allocating a
    /// fresh one each time, and the collected result is byte-identical to
    /// the arena-free baseline (#885 acceptance: byte-identical results,
    /// buffer reuse).
    #[test]
    fn arena_backed_stream_reuses_buffer_and_matches_baseline() {
        const N: u64 = 5_000;
        const HWM: usize = 256;

        let baseline = RowStream::from_lazy(
            vec!["n".into()],
            Default::default(),
            HWM,
            Box::new((0..N).map(|i| Ok(row(i)))),
        )
        .collect_records()
        .expect("baseline collects");

        let arena = Rc::new(RefCell::new(RowBufferArena::new()));
        let arena_backed = RowStream::from_lazy(
            vec!["n".into()],
            Default::default(),
            HWM,
            Box::new((0..N).map(|i| Ok(row(i)))),
        )
        .with_arena(Rc::clone(&arena))
        .collect_records()
        .expect("arena-backed collects");

        assert_eq!(
            arena_backed.len(),
            baseline.len(),
            "row count identical to the per-request-allocation baseline"
        );
        for (a, b) in arena_backed.iter().zip(baseline.iter()) {
            assert_eq!(
                a.get("n"),
                b.get("n"),
                "each row is byte-identical to the baseline"
            );
        }
        // After draining a many-chunk stream the arena holds exactly the
        // one recycled chunk buffer — proof the buffer was reused rather
        // than reallocated per chunk.
        assert_eq!(
            arena.borrow().pooled(),
            1,
            "one chunk buffer is recycled and reused across all chunk-fetches"
        );
    }

    /// `from_unified` round-trips byte-identically whether or not an arena
    /// is bound — the arena is a pure allocation optimisation (#885
    /// acceptance: byte-identical observable results).
    #[test]
    fn arena_backed_from_unified_round_trips_verbatim() {
        let original = UnifiedResult {
            columns: vec!["a".into(), "b".into()],
            records: vec![row(1), row(2), row(3)],
            stats: Default::default(),
            pre_serialized_json: Some("{\"fast\":true}".into()),
        };
        let arena = Rc::new(RefCell::new(RowBufferArena::new()));
        let collected = RowStream::from_unified(original.clone(), 2)
            .with_arena(arena)
            .collect_unified()
            .expect("arena-backed stream collects ok");
        assert_eq!(collected.columns, original.columns);
        assert_eq!(collected.records.len(), original.records.len());
        assert_eq!(collected.pre_serialized_json, original.pre_serialized_json);
    }

    /// Bounded-memory over a *real* table scan: a query producing N rows
    /// streamed through the executor's lazy scan source keeps at most one
    /// chunk resident — memory ∝ chunk size, not N (acceptance criterion).
    #[test]
    fn real_table_scan_streams_with_bounded_resident_set() {
        const N: usize = 500;
        const HWM: usize = 64;

        let rt = crate::RedDBRuntime::with_options(crate::RedDBOptions::in_memory())
            .expect("runtime boots");
        rt.execute_query("CREATE TABLE t (id INT, name TEXT)")
            .expect("create table");
        let values = (0..N)
            .map(|i| format!("({i}, 'row{i}')"))
            .collect::<Vec<_>>()
            .join(", ");
        rt.execute_query(&format!("INSERT INTO t (id, name) VALUES {values}"))
            .expect("insert rows");

        let db = rt.db();
        let mut stream =
            crate::runtime::record_search::stream_runtime_table_source_scan(db.as_ref(), "t", HWM)
                .expect("stream scan builds");

        let mut total = 0usize;
        while let Some(chunk) = stream.next_chunk() {
            assert!(chunk.records.len() <= HWM, "chunk exceeded high-water mark");
            total += chunk.records.len();
        }
        assert_eq!(total, N, "every visible row is streamed");
        assert!(
            stream.peak_buffered() <= HWM,
            "resident record set stayed bounded by the chunk size, not N"
        );
        assert_eq!(
            stream.terminal(),
            Some(&StreamTerminal::End {
                row_count: N as u64
            })
        );
    }
}
