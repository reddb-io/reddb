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
    let chunk_size = (input.len() + chunk_count - 1) / chunk_count;
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
    let chunk_size = (input.len() + chunk_count - 1) / chunk_count;
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
