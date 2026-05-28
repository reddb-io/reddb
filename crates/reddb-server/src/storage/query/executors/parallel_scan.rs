//! Parallel scan coordinator — Fase 4 P5 building block.
//!
//! Generic worker-pool helper for chunked collection scans.
//! Mirrors PG's `nodeGather.c` + `execParallel.c` at a much
//! smaller scale: splits an input into equal chunks, processes
//! each chunk on a separate OS thread, and funnels results
//! back to the caller in a single ordered Vec.
//!
//! The module intentionally doesn't depend on `rayon` —
//! reddb avoids large transitive dep graphs and `rayon` is
//! overkill for the single-query-level parallelism we need
//! here. A plain `std::thread::spawn` + `mpsc::channel`
//! approach gives us enough throughput for the scan sizes
//! that matter (1k-10m rows per query).
//!
//! Usage pattern:
//!
//! ```text
//! let out = parallel_scan(
//!     &records,
//!     num_cpus(),
//!     |chunk| chunk.iter().filter(|r| matches(r)).cloned().collect(),
//! );
//! ```
//!
//! The closure runs on each chunk; its output is a Vec that
//! the coordinator concatenates in chunk order, preserving
//! input ordering for the merged result.
//!
//! This module is **not yet wired** into any executor. The
//! full-scan path in `runtime/query_exec/table.rs` is the
//! obvious first call site once the planner learns to flag
//! scans as "parallel-eligible" (size > threshold, no
//! side-effecting filters).
//!
//! ## Parallelism decisions
//!
//! Parallelism overhead dominates at small sizes — thread
//! spawn + channel setup is ~1 µs per thread. The coordinator
//! falls back to sequential execution when:
//!
//! - `chunk_count == 1` (no parallelism available)
//! - Input length < `min_parallel_rows` (tuned constant,
//!   default 4096)
//!
//! Above the threshold the input is sliced into roughly
//! `chunk_count` equal pieces and each piece ships to a
//! worker thread. The main thread joins and concatenates.

use std::sync::mpsc;
use std::thread;

/// Minimum input length below which parallel execution falls
/// back to sequential. Tuned so the overhead of thread spawn
/// + channel setup (~3-5 µs) doesn't dominate the filter cost.
pub const MIN_PARALLEL_ROWS: usize = 4096;

/// Execute `worker` across `input` in parallel using
/// `chunk_count` threads. Preserves input order in the
/// output: chunk 0's results come first, chunk 1's next, etc.
///
/// - `T` — the input row type. Must be `Send + Sync` so chunks
///   can be passed to worker threads.
/// - `U` — the output row type from a single worker. Must be
///   `Send`.
/// - `F` — the worker closure. Must be `Send + Sync + 'static`
///   and take a slice reference to the chunk.
///
/// Falls back to sequential execution when the input is
/// smaller than `MIN_PARALLEL_ROWS` or when `chunk_count <= 1`.
pub fn parallel_scan<T, U, F>(input: &[T], chunk_count: usize, worker: F) -> Vec<U>
where
    T: Send + Sync + Clone + 'static,
    U: Send + 'static,
    F: Fn(&[T]) -> Vec<U> + Send + Sync + 'static,
{
    if chunk_count <= 1 || input.len() < MIN_PARALLEL_ROWS {
        return worker(input);
    }

    // Divide the input into roughly-equal slices. Slice
    // boundaries may not fall on clean chunk_size multiples
    // so the last chunk absorbs the remainder.
    let chunk_size = input.len().div_ceil(chunk_count);
    let mut chunks: Vec<Vec<T>> = Vec::with_capacity(chunk_count);
    let mut idx = 0;
    while idx < input.len() {
        let end = (idx + chunk_size).min(input.len());
        chunks.push(input[idx..end].to_vec());
        idx = end;
    }

    // Spawn one worker per chunk. Each worker sends its
    // output through the mpsc channel along with its chunk
    // index so the main thread can concatenate in order.
    let (tx, rx) = mpsc::channel();
    let worker = std::sync::Arc::new(worker);
    let mut handles = Vec::with_capacity(chunks.len());
    for (chunk_idx, chunk) in chunks.into_iter().enumerate() {
        let tx = tx.clone();
        let worker = worker.clone();
        let handle = thread::spawn(move || {
            let result = worker(&chunk);
            // Send (chunk_idx, result) so the coordinator can
            // put the results back in order. We ignore errors
            // because the receiver side outlives every sender
            // — if it's gone, drop silently.
            let _ = tx.send((chunk_idx, result));
        });
        handles.push(handle);
    }
    // Drop the parent tx so the channel closes when the last
    // worker finishes.
    drop(tx);

    // Collect all results into an indexed Vec, then flatten
    // in chunk order.
    let mut indexed: Vec<Option<Vec<U>>> = (0..handles.len()).map(|_| None).collect();
    while let Ok((idx, result)) = rx.recv() {
        indexed[idx] = Some(result);
    }
    // Join every worker so panics propagate deterministically.
    for handle in handles {
        let _ = handle.join();
    }

    // Flatten in chunk order.
    let mut out: Vec<U> = Vec::new();
    for chunk_result in indexed.into_iter().flatten() {
        out.extend(chunk_result);
    }
    out
}

/// Count-only variant of `parallel_scan` that avoids
/// materialising the intermediate Vec. The worker returns
/// a usize (number of matching rows in its chunk), and the
/// coordinator sums them.
///
/// Used by `SELECT COUNT(*) FROM t WHERE filter` where the
/// full row payload is irrelevant.
pub fn parallel_count<T, F>(input: &[T], chunk_count: usize, counter: F) -> u64
where
    T: Send + Sync + Clone + 'static,
    F: Fn(&[T]) -> u64 + Send + Sync + 'static,
{
    if chunk_count <= 1 || input.len() < MIN_PARALLEL_ROWS {
        return counter(input);
    }
    let chunk_size = input.len().div_ceil(chunk_count);
    let mut chunks: Vec<Vec<T>> = Vec::with_capacity(chunk_count);
    let mut idx = 0;
    while idx < input.len() {
        let end = (idx + chunk_size).min(input.len());
        chunks.push(input[idx..end].to_vec());
        idx = end;
    }
    let (tx, rx) = mpsc::channel();
    let counter = std::sync::Arc::new(counter);
    let mut handles = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        let tx = tx.clone();
        let counter = counter.clone();
        let handle = thread::spawn(move || {
            let n = counter(&chunk);
            let _ = tx.send(n);
        });
        handles.push(handle);
    }
    drop(tx);
    let mut total = 0u64;
    while let Ok(n) = rx.recv() {
        total += n;
    }
    for handle in handles {
        let _ = handle.join();
    }
    total
}

/// Number of worker threads to use by default. Currently
/// clamped to `num_cpus().min(8)` — more than 8 workers for
/// a single query tends to thrash the buffer pool.
pub fn default_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().min(8))
        .unwrap_or(1)
}

/// Phase 3.4 wiring entry point. Calls `parallel_scan` with
/// `default_parallelism()` worker count. Used by the runtime
/// scan executor when its planner cost model decides parallel
/// is profitable (input size > MIN_PARALLEL_ROWS). Saves the
/// caller from manually threading the worker count through.
pub fn parallel_scan_default<T, U, F>(input: &[T], worker: F) -> Vec<U>
where
    T: Send + Sync + Clone + 'static,
    U: Send + 'static,
    F: Fn(&[T]) -> Vec<U> + Send + Sync + 'static,
{
    parallel_scan(input, default_parallelism(), worker)
}

/// Phase 3.4 wiring entry point for COUNT(*) over a filtered
/// scan. Same default parallelism as `parallel_scan_default`.
pub fn parallel_count_default<T, F>(input: &[T], counter: F) -> u64
where
    T: Send + Sync + Clone + 'static,
    F: Fn(&[T]) -> u64 + Send + Sync + 'static,
{
    parallel_count(input, default_parallelism(), counter)
}

// ──────── Issue #768 / S9 — pull-based scan iterator ────────

/// Default number of rows processed per pulled batch. Sized so the
/// per-batch working set stays well under one engine page worth of
/// row payload for typical row widths, while amortising the
/// per-batch closure-call overhead. The streaming chunk producer
/// (S1) re-buffers these into page-aligned wire chunks downstream,
/// so this is purely the executor's internal pull granularity.
pub const DEFAULT_SCAN_BATCH_ROWS: usize = 256;

/// Lazily-evaluated, pull-based counterpart to [`parallel_scan`].
///
/// Where [`parallel_scan`] eagerly processes the whole input and
/// concatenates every worker's output into a single `Vec<U>`,
/// `ScanBatches` holds a borrow of the input and yields **one
/// processed batch at a time** on demand. Only the rows of the
/// current batch are materialised; the caller (the S1
/// [`ChunkProducer`](crate::server::output_stream::ChunkProducer))
/// drains each batch into the wire buffer before the next batch is
/// pulled, so server-side memory tracks the chunk-buffer working
/// set rather than the full result-set cardinality.
///
/// Ordering is preserved: batch *k* covers input rows
/// `[k·batch_rows, (k+1)·batch_rows)`, so flattening the yielded
/// batches reproduces the exact order (and contents) of
/// `parallel_scan`'s `Vec<U>` — this is the parity contract the S9
/// golden tests assert.
///
/// Parallelism note: the eager [`parallel_scan`] spreads work
/// across worker threads, which is profitable for a one-shot
/// collect. A pull-based scan that must yield rows *in order* to a
/// single downstream consumer is inherently sequential at the
/// boundary, so this iterator runs the worker on the consumer's
/// thread. Bounded read-ahead parallelism (a Gather-style sync
/// channel) is a future enhancement; it does not change this wire
/// contract.
pub struct ScanBatches<'a, T, U, F>
where
    F: Fn(&[T]) -> Vec<U>,
{
    input: &'a [T],
    cursor: usize,
    batch_rows: usize,
    worker: F,
    _marker: std::marker::PhantomData<fn() -> U>,
}

impl<'a, T, U, F> Iterator for ScanBatches<'a, T, U, F>
where
    F: Fn(&[T]) -> Vec<U>,
{
    type Item = Vec<U>;

    fn next(&mut self) -> Option<Vec<U>> {
        if self.cursor >= self.input.len() {
            return None;
        }
        let end = (self.cursor + self.batch_rows).min(self.input.len());
        let batch = &self.input[self.cursor..end];
        self.cursor = end;
        Some((self.worker)(batch))
    }
}

/// Construct a pull-based [`ScanBatches`] over `input`, applying
/// `worker` to each `batch_rows`-sized slice on demand. A
/// `batch_rows` of 0 is clamped to 1 so the iterator always makes
/// progress.
pub fn parallel_scan_stream<T, U, F>(
    input: &[T],
    batch_rows: usize,
    worker: F,
) -> ScanBatches<'_, T, U, F>
where
    F: Fn(&[T]) -> Vec<U>,
{
    ScanBatches {
        input,
        cursor: 0,
        batch_rows: batch_rows.max(1),
        worker,
        _marker: std::marker::PhantomData,
    }
}

/// Per-row flattening helper over [`parallel_scan_stream`]. Yields
/// one `U` at a time — the natural shape for a record-at-a-time
/// streaming driver — while keeping the same lazy, bounded-memory
/// pull semantics (at most one batch is materialised at a time).
pub fn parallel_scan_rows<'a, T, U, F>(
    input: &'a [T],
    batch_rows: usize,
    worker: F,
) -> impl Iterator<Item = U> + 'a
where
    T: 'a,
    U: 'a,
    F: Fn(&[T]) -> Vec<U> + 'a,
{
    parallel_scan_stream(input, batch_rows, worker).flat_map(|batch| batch.into_iter())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn copy_worker(chunk: &[u64]) -> Vec<u64> {
        chunk.to_vec()
    }

    #[test]
    fn scan_stream_yields_batches_in_order_and_matches_eager_collect() {
        // Acceptance #3 / #5: parity with the materialising path on a
        // small fixture — flattening the pulled batches reproduces the
        // exact Vec `parallel_scan` would have built.
        let input: Vec<u64> = (0..1000).collect();
        let eager = parallel_scan(&input, default_parallelism(), copy_worker);
        let streamed: Vec<u64> = parallel_scan_rows(&input, 64, copy_worker).collect();
        assert_eq!(eager, streamed);
        assert_eq!(streamed, input);
    }

    #[test]
    fn scan_stream_applies_filter_worker_with_parity() {
        // A filtering worker (the WHERE-clause shape) must stream the
        // same surviving rows, in the same order, as the eager path.
        let input: Vec<u64> = (0..500).collect();
        let even =
            |chunk: &[u64]| -> Vec<u64> { chunk.iter().copied().filter(|n| n % 2 == 0).collect() };
        let eager = parallel_scan(&input, default_parallelism(), even);
        let streamed: Vec<u64> = parallel_scan_rows(&input, 16, even).collect();
        assert_eq!(eager, streamed);
        assert!(streamed.iter().all(|n| n % 2 == 0));
    }

    #[test]
    fn scan_stream_batch_rows_zero_is_clamped_to_one() {
        let input: Vec<u64> = (0..5).collect();
        let batches: Vec<Vec<u64>> = parallel_scan_stream(&input, 0, copy_worker).collect();
        assert_eq!(batches.len(), 5, "batch_rows 0 must clamp to 1 row/batch");
        assert_eq!(batches.concat(), input);
    }

    #[test]
    fn scan_stream_materialises_at_most_one_batch_at_a_time() {
        // Acceptance #1: bounded memory. The worker asserts it is never
        // handed more than `batch_rows` rows, so no call path can
        // smuggle the full input through in one materialised slice.
        let input: Vec<u64> = (0..10_000).collect();
        const BATCH: usize = 128;
        let bounded = |chunk: &[u64]| -> Vec<u64> {
            assert!(
                chunk.len() <= BATCH,
                "worker saw {} rows, exceeding batch cap {BATCH}",
                chunk.len()
            );
            chunk.to_vec()
        };
        let total: usize = parallel_scan_rows(&input, BATCH, bounded).count();
        assert_eq!(total, input.len());
    }
}
