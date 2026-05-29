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

/// Default chunk high-water mark (rows). Bounds the resident record set
/// of a [`RowStream`] regardless of how many rows the source yields.
pub(crate) const DEFAULT_HIGH_WATER_MARK: usize = 1024;

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
        }
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
        let mut records: Vec<UnifiedRecord> = Vec::new();
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
            // empty; ensure a terminal is set and stop.
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
            records.extend(chunk.records);
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
            records.extend(chunk.records);
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
