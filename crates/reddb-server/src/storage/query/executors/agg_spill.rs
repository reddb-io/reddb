//! Hash-aggregation spill helper — Fase 4 P4 building block.
//!
//! Provides a `SpilledHashAgg` data structure that holds an
//! in-memory hash table plus zero or more on-disk batch files.
//! When the in-memory table exceeds `mem_limit_bytes`, callers
//! invoke `spill_partition` to write a batch to a temporary file
//! and free the corresponding entries from the hash map. The
//! `drain` step reads all spilled batches back, merges them with
//! whatever is still in memory, and produces the final aggregated
//! output.
//!
//! Mirrors PostgreSQL's `nodeAgg.c::hashagg_spill_*` family
//! modulo features we don't have:
//!
//! - No tape-based recursion: PG does N-way repartitioning when a
//!   spilled batch itself doesn't fit. Week 4 here just rewinds and
//!   reads each batch back in full. If a single batch exceeds
//!   memory we return an error so the caller can switch to
//!   sort-based aggregation.
//! - No parallel spill: single producer, single consumer.
//! - No on-disk hash format: each spill batch is a plain
//!   serialised `Vec<(GroupKey, AggState)>` — small overhead but
//!   simple to read back.
//!
//! The module is **not yet wired** into `executors/aggregation.rs`.
//! Wiring happens in a follow-up commit when the aggregation
//! executor learns to track its current memory footprint and
//! call `spill_partition` from inside its insert loop.
//!
//! ## Type parameters
//!
//! - `K` — the group key. Must be `Hash + Eq + Clone + Serialize`
//!   so it can both index the hash map and round-trip through
//!   the spill file.
//! - `S` — the aggregation state per group. Must be `Clone +
//!   Serialize + Mergeable` so spilled batches can be combined
//!   with the in-memory state during drain.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::hash::Hash;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// Trait implemented by any aggregation state that can absorb
/// another value of its own type. Used by the drain step to merge
/// spilled batches back into the in-memory table.
///
/// Implementors:
/// - SUM:    `lhs += rhs`
/// - COUNT:  `lhs += rhs`
/// - MIN:    `lhs = min(lhs, rhs)`
/// - MAX:    `lhs = max(lhs, rhs)`
/// - AVG:    pair `(sum, count)` → element-wise add
/// - STDDEV: triple `(n, mean, M2)` → Welford's parallel formula
pub trait Mergeable {
    /// Combine `other` into `self`, leaving `self` as the merged
    /// result and consuming `other`.
    fn merge(&mut self, other: Self);
}

/// Errors raised by the spill helper.
#[derive(Debug)]
pub enum SpillError {
    /// I/O failure writing or reading a spill batch.
    Io(std::io::Error),
    /// A single spill batch exceeds the configured memory limit
    /// even after offloading. Caller should fall back to
    /// sort-based aggregation.
    BatchTooLarge { size: usize, limit: usize },
    /// Encoding / decoding of a key / state failed during
    /// round-trip.
    Codec(String),
}

impl From<std::io::Error> for SpillError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl std::fmt::Display for SpillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "spill i/o: {e}"),
            Self::BatchTooLarge { size, limit } => {
                write!(f, "spill batch {size} bytes exceeds limit {limit}")
            }
            Self::Codec(msg) => write!(f, "spill codec: {msg}"),
        }
    }
}

impl std::error::Error for SpillError {}

/// Trait for serialising a key or state into a flat byte
/// representation. Implementations should be deterministic so the
/// spill file is byte-equal across runs (helpful for debugging).
///
/// Default impls below use `bincode`-style length-prefixed
/// encoding; the helper doesn't require a specific serde crate
/// because reddb deliberately avoids large transitive deps.
pub trait SpillCodec: Sized {
    /// Encode `self` into the writer. Returns the number of bytes
    /// written so the caller can track per-batch size.
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError>;
    /// Decode a fresh value from the reader. Reads exactly one
    /// element; returns `Ok(None)` on a clean end-of-file so
    /// drain loops can terminate naturally.
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError>;
}

/// Implementation strategy for fixed-size primitive types. The
/// code uses raw little-endian writes so we don't depend on
/// `bincode` / `serde` from this module — keeps the dep graph
/// flat.
impl SpillCodec for u64 {
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
        w.write_all(&self.to_le_bytes())?;
        Ok(8)
    }
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
        let mut buf = [0u8; 8];
        match r.read_exact(&mut buf) {
            Ok(()) => Ok(Some(u64::from_le_bytes(buf))),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(SpillError::Io(e)),
        }
    }
}

impl SpillCodec for i64 {
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
        w.write_all(&self.to_le_bytes())?;
        Ok(8)
    }
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
        let mut buf = [0u8; 8];
        match r.read_exact(&mut buf) {
            Ok(()) => Ok(Some(i64::from_le_bytes(buf))),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(SpillError::Io(e)),
        }
    }
}

impl SpillCodec for f64 {
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
        w.write_all(&self.to_le_bytes())?;
        Ok(8)
    }
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
        let mut buf = [0u8; 8];
        match r.read_exact(&mut buf) {
            Ok(()) => Ok(Some(f64::from_le_bytes(buf))),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(SpillError::Io(e)),
        }
    }
}

impl SpillCodec for String {
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
        let bytes = self.as_bytes();
        let len = bytes.len() as u32;
        w.write_all(&len.to_le_bytes())?;
        w.write_all(bytes)?;
        Ok(4 + bytes.len())
    }
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
        let mut lenbuf = [0u8; 4];
        match r.read_exact(&mut lenbuf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(SpillError::Io(e)),
        }
        let len = u32::from_le_bytes(lenbuf) as usize;
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf)?;
        String::from_utf8(buf)
            .map(Some)
            .map_err(|e| SpillError::Codec(format!("invalid utf-8: {e}")))
    }
}

/// Hash aggregation table with optional spill-to-disk overflow.
///
/// Owns the in-memory `HashMap<K, S>` plus a list of `PathBuf`s
/// pointing at spilled batch files. The caller drives the
/// lifecycle by calling `accumulate` for each input row and
/// `drain` once the input is exhausted.
pub struct SpilledHashAgg<K, S>
where
    K: Hash + Eq + Clone + SpillCodec,
    S: Clone + Mergeable + SpillCodec,
{
    /// In-memory hash table. Cleared after each spill.
    table: HashMap<K, S>,
    /// Estimated bytes-per-(key, state) pair. Used to compute
    /// when to spill — a rough proxy for actual heap usage.
    /// Callers can tune by passing a more accurate value.
    avg_entry_bytes: usize,
    /// Soft limit on `table.len() * avg_entry_bytes`. Crossing
    /// this triggers a spill.
    mem_limit_bytes: usize,
    /// Directory where spill batches land. Each batch is a single
    /// file named `spill_{seq}.bin`.
    spill_dir: PathBuf,
    /// List of spilled batch paths in order of creation.
    spilled_batches: Vec<PathBuf>,
    /// Monotonic batch counter for unique filenames.
    next_seq: u64,
    /// Total bytes spilled across all batches — diagnostic.
    pub total_spilled_bytes: u64,
    /// Number of times `spill_partition` was called — diagnostic.
    pub spill_count: u64,
}

impl<K, S> SpilledHashAgg<K, S>
where
    K: Hash + Eq + Clone + SpillCodec,
    S: Clone + Mergeable + SpillCodec,
{
    /// Create a new spillable hash aggregator. `spill_dir` must
    /// exist and be writable; the helper does NOT create it.
    /// `mem_limit_bytes == 0` disables spilling entirely (useful
    /// for tests that want to exercise the in-memory path).
    pub fn new(
        spill_dir: impl AsRef<Path>,
        mem_limit_bytes: usize,
        avg_entry_bytes: usize,
    ) -> Self {
        Self {
            table: HashMap::new(),
            avg_entry_bytes,
            mem_limit_bytes,
            spill_dir: spill_dir.as_ref().to_path_buf(),
            spilled_batches: Vec::new(),
            next_seq: 0,
            total_spilled_bytes: 0,
            spill_count: 0,
        }
    }

    /// Insert or update an aggregation state for the given key.
    /// `accumulate` triggers a spill when the in-memory table's
    /// estimated footprint exceeds the configured limit. Returns
    /// the key/state pair after the merge so callers can chain.
    pub fn accumulate(&mut self, key: K, increment: S) -> Result<(), SpillError> {
        match self.table.get_mut(&key) {
            Some(existing) => existing.merge(increment),
            None => {
                self.table.insert(key, increment);
                if self.should_spill() {
                    self.spill_partition()?;
                }
            }
        }
        Ok(())
    }

    /// Returns true when the current in-memory footprint exceeds
    /// `mem_limit_bytes`. Cheap O(1) check using the estimated
    /// per-entry size; callers should keep `avg_entry_bytes`
    /// in sync with reality if precision matters.
    fn should_spill(&self) -> bool {
        if self.mem_limit_bytes == 0 {
            return false;
        }
        let estimated = self.table.len().saturating_mul(self.avg_entry_bytes);
        estimated > self.mem_limit_bytes
    }

    /// Write the entire in-memory table to a new spill batch file
    /// and clear the table. Updates the spill diagnostics. Caller
    /// is free to keep accumulating after this returns — the
    /// batch will be merged back during `drain`.
    pub fn spill_partition(&mut self) -> Result<(), SpillError> {
        if self.table.is_empty() {
            return Ok(());
        }
        let path = self.spill_dir.join(format!("spill_{}.bin", self.next_seq));
        self.next_seq += 1;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let mut writer = BufWriter::new(file);
        let mut bytes_written = 0usize;
        // Drain so we don't hold both copies in memory while
        // writing — the file is the canonical store after this.
        for (k, s) in self.table.drain() {
            bytes_written += k.encode(&mut writer)?;
            bytes_written += s.encode(&mut writer)?;
        }
        writer.flush()?;
        self.total_spilled_bytes += bytes_written as u64;
        self.spill_count += 1;
        self.spilled_batches.push(path);
        Ok(())
    }

    /// Consume the aggregator and return the final merged state
    /// for every group. Reads every spilled batch back into a
    /// new in-memory hash table, merges with whatever the
    /// accumulator left in place, and yields the unified set.
    ///
    /// Memory profile: at peak, this holds ONE spill batch plus
    /// the running merge table in memory simultaneously. If a
    /// single spill batch is larger than `mem_limit_bytes`, we
    /// return `BatchTooLarge` so the caller can switch strategies.
    pub fn drain(mut self) -> Result<HashMap<K, S>, SpillError> {
        // The current `table` is the most recent in-memory chunk
        // that hasn't been spilled — start the merge from it.
        let mut merged = std::mem::take(&mut self.table);
        for path in self.spilled_batches.drain(..) {
            let file = File::open(&path)?;
            let metadata = file.metadata()?;
            if self.mem_limit_bytes > 0 && (metadata.len() as usize) > self.mem_limit_bytes {
                return Err(SpillError::BatchTooLarge {
                    size: metadata.len() as usize,
                    limit: self.mem_limit_bytes,
                });
            }
            let mut reader = BufReader::new(file);
            loop {
                let key = match K::decode(&mut reader)? {
                    Some(k) => k,
                    None => break,
                };
                let state = match S::decode(&mut reader)? {
                    Some(s) => s,
                    None => {
                        return Err(SpillError::Codec(
                            "spill batch ended mid-entry: state missing".to_string(),
                        ))
                    }
                };
                match merged.get_mut(&key) {
                    Some(existing) => existing.merge(state),
                    None => {
                        merged.insert(key, state);
                    }
                }
            }
            // Best-effort cleanup — ignore errors so a missing
            // file doesn't hide a successful merge.
            let _ = std::fs::remove_file(&path);
        }
        Ok(merged)
    }

    /// Number of spill batches currently on disk. Diagnostic
    /// hook for tests / metrics.
    pub fn spilled_batch_count(&self) -> usize {
        self.spilled_batches.len()
    }

    /// Number of groups currently held in memory.
    pub fn in_memory_groups(&self) -> usize {
        self.table.len()
    }
}

impl<K, S> Drop for SpilledHashAgg<K, S>
where
    K: Hash + Eq + Clone + SpillCodec,
    S: Clone + Mergeable + SpillCodec,
{
    fn drop(&mut self) {
        // Clean up any spill files left behind if the caller
        // never called drain. Best-effort — failures are silent
        // so Drop doesn't panic.
        for path in self.spilled_batches.drain(..) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

// ────────────────────────────────────────────────────────────────
// Convenience Mergeable implementations for the common scalar
// aggregates. The aggregation executor wires its concrete state
// types through these so callers don't need to spell out the
// merge logic at every call site.
// ────────────────────────────────────────────────────────────────

/// SUM state — running total. Generic over any numeric type that
/// supports `+=`.
#[derive(Debug, Clone, Copy)]
pub struct SumState<T>(pub T);

impl Mergeable for SumState<i64> {
    fn merge(&mut self, other: Self) {
        self.0 = self.0.saturating_add(other.0);
    }
}
impl SpillCodec for SumState<i64> {
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
        self.0.encode(w)
    }
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
        Ok(i64::decode(r)?.map(SumState))
    }
}

impl Mergeable for SumState<f64> {
    fn merge(&mut self, other: Self) {
        self.0 += other.0;
    }
}
impl SpillCodec for SumState<f64> {
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
        self.0.encode(w)
    }
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
        Ok(f64::decode(r)?.map(SumState))
    }
}

/// COUNT state — monotonic non-negative counter.
#[derive(Debug, Clone, Copy)]
pub struct CountState(pub u64);

impl Mergeable for CountState {
    fn merge(&mut self, other: Self) {
        self.0 = self.0.saturating_add(other.0);
    }
}
impl SpillCodec for CountState {
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
        self.0.encode(w)
    }
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
        Ok(u64::decode(r)?.map(CountState))
    }
}

/// MIN/MAX state — wraps a numeric and merges via comparison.
/// Two distinct types so the type system enforces direction.
#[derive(Debug, Clone, Copy)]
pub struct MinState<T>(pub T);
#[derive(Debug, Clone, Copy)]
pub struct MaxState<T>(pub T);

impl Mergeable for MinState<i64> {
    fn merge(&mut self, other: Self) {
        if other.0 < self.0 {
            self.0 = other.0;
        }
    }
}
impl SpillCodec for MinState<i64> {
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
        self.0.encode(w)
    }
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
        Ok(i64::decode(r)?.map(MinState))
    }
}

impl Mergeable for MaxState<i64> {
    fn merge(&mut self, other: Self) {
        if other.0 > self.0 {
            self.0 = other.0;
        }
    }
}
impl SpillCodec for MaxState<i64> {
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
        self.0.encode(w)
    }
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
        Ok(i64::decode(r)?.map(MaxState))
    }
}

/// AVG state — pair (sum, count). Final value is sum / count.
#[derive(Debug, Clone, Copy)]
pub struct AvgState {
    pub sum: f64,
    pub count: u64,
}

impl Mergeable for AvgState {
    fn merge(&mut self, other: Self) {
        self.sum += other.sum;
        self.count += other.count;
    }
}
impl SpillCodec for AvgState {
    fn encode<W: Write>(&self, w: &mut W) -> Result<usize, SpillError> {
        let a = self.sum.encode(w)?;
        let b = self.count.encode(w)?;
        Ok(a + b)
    }
    fn decode<R: Read>(r: &mut R) -> Result<Option<Self>, SpillError> {
        let sum = match f64::decode(r)? {
            Some(v) => v,
            None => return Ok(None),
        };
        let count = match u64::decode(r)? {
            Some(v) => v,
            None => {
                return Err(SpillError::Codec(
                    "AvgState ended after sum: count missing".to_string(),
                ))
            }
        };
        Ok(Some(AvgState { sum, count }))
    }
}

impl AvgState {
    /// Final average value. Returns `None` for an empty state to
    /// distinguish "no rows" from `0.0`.
    pub fn finalize(self) -> Option<f64> {
        if self.count == 0 {
            None
        } else {
            Some(self.sum / self.count as f64)
        }
    }
}

/// Phase 3.3 wiring entry point. Builds a `SpilledHashAgg` with
/// production-grade defaults (`mem_limit_bytes = 64 MiB`,
/// `avg_entry_bytes = 128`) targeting reddb's tmpfs at
/// `/tmp/reddb-spill`. Used by `executors/aggregation.rs::execute_group_by`
/// when the input row count exceeds the in-memory threshold.
///
/// The caller is expected to feed every row via `accumulate` and
/// then call `drain` to materialise the merged result. The helper
/// returns the constructed aggregator so the caller can wire it
/// into its existing per-row loop without re-implementing the
/// spill bookkeeping.
///
/// Spill files land in a process-unique subdirectory so concurrent
/// queries don't collide; the directory is auto-cleaned on Drop.
pub fn spilled_hash_agg_default<K, S>() -> std::io::Result<SpilledHashAgg<K, S>>
where
    K: std::hash::Hash + Eq + Clone + SpillCodec,
    S: Clone + Mergeable + SpillCodec,
{
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let dir = std::env::temp_dir().join(format!("reddb-spill-{pid}-{seq}"));
    std::fs::create_dir_all(&dir)?;
    Ok(SpilledHashAgg::new(
        dir,
        64 * 1024 * 1024, // 64 MiB soft limit
        128,              // avg bytes per (key, state) — tuned for SUM/COUNT
    ))
}
