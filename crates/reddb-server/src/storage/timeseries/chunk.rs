//! Time-Series Chunk — grouped storage of metric data points
//!
//! Points are grouped by (metric, tags) into chunks. Each chunk stores
//! compressed timestamps and values for efficient range queries.

use std::collections::HashMap;

use super::compression::{
    delta_decode_timestamps, delta_encode_timestamps, xor_decode_values, xor_encode_values,
};
use crate::catalog::AnalyticalStorageConfig;
use crate::storage::index::{BloomSegment, HasBloom, ZoneDecision, ZoneMap, ZonePredicate};
use crate::storage::schema::types::DataType;
use crate::storage::unified::column_block::{
    read_column_block, write_column_block, ColumnBlockError, ColumnInput,
};
use crate::storage::unified::segment_codec::ColumnSemantics;

/// Stable column id of the timestamp column within a v1 columnar chunk.
pub const COLUMNAR_TS_COLUMN_ID: u32 = 0;
/// Stable column id of the value column within a v1 columnar chunk.
pub const COLUMNAR_VALUE_COLUMN_ID: u32 = 1;
/// Default sparse-granule-index stride: one min/max mark per ~8192 rows
/// (PRD #850 Phase 1, #854). Configurable per seal via
/// [`TimeSeriesChunk::seal_columnar_with_granule_size`].
pub const DEFAULT_GRANULE_SIZE: u32 = 8192;

/// A single time-series data point
#[derive(Debug, Clone, PartialEq)]
pub struct TimeSeriesPoint {
    pub timestamp_ns: u64,
    pub value: f64,
}

/// A chunk of time-series data for a single metric + tag combination.
///
/// Points are stored in timestamp order. The chunk can be in either
/// "open" (uncompressed, accepting writes) or "sealed" (compressed) state.
pub struct TimeSeriesChunk {
    /// Metric name (e.g., "cpu.idle")
    pub metric: String,
    /// Dimensional tags (e.g., {"host": "srv1", "region": "us-east"})
    pub tags: HashMap<String, String>,
    /// Raw timestamps (nanoseconds since epoch)
    timestamps: Vec<u64>,
    /// Raw values
    values: Vec<f64>,
    /// Maximum points before auto-seal
    max_points: usize,
    /// Whether the chunk is sealed (compressed, immutable)
    sealed: bool,
    /// Compressed timestamp data (populated on seal)
    compressed_timestamps: Option<Vec<i64>>,
    /// Compressed value data (populated on seal)
    compressed_values: Option<Vec<u64>>,
    /// Bloom filter over timestamps for O(1) negative point lookups.
    /// Query planners can skip a chunk when a wanted timestamp is definitely
    /// absent, even when it falls inside the chunk's min/max range.
    bloom: BloomSegment,
    /// Zone map over the chunk's timestamp column — BRIN MINMAX equivalent.
    /// Tracks min/max timestamp plus a HyperLogLog distinct estimate.
    /// Enables O(1) skip decisions: if chunk's max_ts < query_start or
    /// min_ts > query_end the chunk is definitively outside the window.
    timestamp_zone: ZoneMap,
    /// Zone map over the chunk's value column (not timestamps — those are
    /// already ordered). Enables skip-scan for value predicates
    /// (e.g. "show chunks where cpu > 95%") and surfaces a HyperLogLog
    /// distinct estimate for the planner.
    value_zone: ZoneMap,
}

impl HasBloom for TimeSeriesChunk {
    fn bloom_segment(&self) -> Option<&BloomSegment> {
        Some(&self.bloom)
    }
}

impl TimeSeriesChunk {
    /// Create a new open chunk
    pub fn new(metric: impl Into<String>, tags: HashMap<String, String>) -> Self {
        Self {
            metric: metric.into(),
            tags,
            timestamps: Vec::new(),
            values: Vec::new(),
            max_points: 1024,
            sealed: false,
            compressed_timestamps: None,
            compressed_values: None,
            bloom: BloomSegment::with_capacity(1024),
            timestamp_zone: ZoneMap::with_capacity(1024),
            value_zone: ZoneMap::with_capacity(1024),
        }
    }

    /// Create with custom max points
    pub fn with_max_points(
        metric: impl Into<String>,
        tags: HashMap<String, String>,
        max_points: usize,
    ) -> Self {
        let mut chunk = Self::new(metric, tags);
        chunk.max_points = max_points;
        chunk.timestamp_zone = ZoneMap::with_capacity(max_points);
        chunk
    }

    /// Append a data point. Returns false if the chunk is sealed or full.
    pub fn append(&mut self, timestamp_ns: u64, value: f64) -> bool {
        if self.sealed || self.timestamps.len() >= self.max_points {
            return false;
        }
        self.bloom.insert(&timestamp_ns.to_le_bytes());
        self.timestamp_zone.observe(&timestamp_ns.to_be_bytes()); // big-endian for correct byte-wise ordering
        self.value_zone.observe(&value.to_le_bytes());
        self.timestamps.push(timestamp_ns);
        self.values.push(value);
        true
    }

    /// Fast-path check: might this chunk contain a point at `timestamp_ns`?
    ///
    /// Returns `false` only when the bloom filter *proves* the timestamp is
    /// absent. A `true` response still requires a real lookup.
    pub fn may_contain_timestamp(&self, timestamp_ns: u64) -> bool {
        !self.bloom.definitely_absent(&timestamp_ns.to_le_bytes())
    }

    /// BRIN MINMAX equivalent: "can this chunk possibly overlap `[start_ns, end_ns]`?"
    ///
    /// Returns `true` (skip) only when the chunk's min/max timestamp range
    /// definitively does not intersect the query window — O(1) with no
    /// decompression. Timestamps are stored big-endian so byte-wise min/max
    /// comparisons are numerically correct.
    pub fn timestamp_range_skip(&self, start_ns: u64, end_ns: u64) -> bool {
        let start_b = start_ns.to_be_bytes();
        let end_b = end_ns.to_be_bytes();
        matches!(
            self.timestamp_zone.block_skip(&ZonePredicate::Range {
                start: Some(&start_b),
                end: Some(&end_b),
            }),
            ZoneDecision::Skip
        )
    }

    /// Value-range planner helper. Answers "can this chunk possibly contain
    /// a point with value in `[lo, hi]`?" without decoding the chunk.
    ///
    /// Values are compared as raw `f64::to_le_bytes()`, which gives correct
    /// ordering for non-negative finite floats. Callers with negative
    /// ranges should still read the chunk — this is a best-effort prune.
    pub fn value_range_skip(&self, lo: f64, hi: f64) -> bool {
        let lo_b = lo.to_le_bytes();
        let hi_b = hi.to_le_bytes();
        matches!(
            self.value_zone.block_skip(&ZonePredicate::Range {
                start: Some(&lo_b),
                end: Some(&hi_b),
            }),
            ZoneDecision::Skip
        )
    }

    /// Estimated distinct values observed in this chunk (HLL-backed).
    pub fn distinct_value_estimate(&self) -> u64 {
        self.value_zone.distinct_estimate()
    }

    /// Number of data points
    pub fn len(&self) -> usize {
        self.timestamps.len()
    }

    /// Whether the chunk is empty
    pub fn is_empty(&self) -> bool {
        self.timestamps.is_empty()
    }

    /// Whether the chunk is full
    pub fn is_full(&self) -> bool {
        self.timestamps.len() >= self.max_points
    }

    /// Whether the chunk is sealed
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// Minimum timestamp in the chunk
    pub fn min_timestamp(&self) -> Option<u64> {
        self.timestamps.first().copied()
    }

    /// Maximum timestamp in the chunk
    pub fn max_timestamp(&self) -> Option<u64> {
        self.timestamps.last().copied()
    }

    /// Seal the chunk: compress data and prevent further writes
    pub fn seal(&mut self) {
        if self.sealed {
            return;
        }
        // Sort by timestamp if not already sorted
        if !self.timestamps.windows(2).all(|w| w[0] <= w[1]) {
            let mut indices: Vec<usize> = (0..self.timestamps.len()).collect();
            indices.sort_by_key(|&i| self.timestamps[i]);
            let sorted_ts: Vec<u64> = indices.iter().map(|&i| self.timestamps[i]).collect();
            let sorted_vals: Vec<f64> = indices.iter().map(|&i| self.values[i]).collect();
            self.timestamps = sorted_ts;
            self.values = sorted_vals;
        }
        self.compressed_timestamps = Some(delta_encode_timestamps(&self.timestamps));
        self.compressed_values = Some(xor_encode_values(&self.values));
        self.sealed = true;
    }

    /// Seal the chunk and emit its **columnar** on-disk form: an `RDCC`
    /// [`ColumnBlock`](crate::storage::engine::PageType::ColumnBlock)
    /// transposing the live `(ts, value)` columns into per-column codec
    /// streams — delta-of-delta+ZSTD for the monotonic timestamps,
    /// Gorilla/XOR+ZSTD for the float gauge (#853, PRD #850 Phase 1).
    /// Sealing first (via [`seal`](Self::seal)) guarantees timestamp
    /// order, so the columns are written sorted.
    ///
    /// `chunk_id` / `schema_ref` are recorded verbatim in the block header
    /// so a reader is self-describing. The returned bytes are written into
    /// a `ColumnBlock` page by the caller, which records the resulting
    /// [`PageLocation`](crate::storage::engine::PageLocation) in
    /// `ChunkMeta.columnar_page`.
    pub fn seal_columnar(
        &mut self,
        chunk_id: u64,
        schema_ref: u64,
    ) -> Result<Vec<u8>, ColumnBlockError> {
        self.seal_columnar_with_granule_size(chunk_id, schema_ref, DEFAULT_GRANULE_SIZE)
    }

    /// As [`seal_columnar`](Self::seal_columnar), but with an explicit
    /// sparse-granule-index stride (#854). The writer records one min/max
    /// mark per `granule_size` rows for each numeric column (timestamp +
    /// value) in the block's footer; a reader prunes granules that cannot
    /// match a range predicate. `granule_size == 0` writes no index.
    pub fn seal_columnar_with_granule_size(
        &mut self,
        chunk_id: u64,
        schema_ref: u64,
        granule_size: u32,
    ) -> Result<Vec<u8>, ColumnBlockError> {
        if !self.sealed {
            self.seal();
        }
        let ts_bytes: Vec<u8> = self
            .timestamps
            .iter()
            .flat_map(|t| t.to_le_bytes())
            .collect();
        let val_bytes: Vec<u8> = self.values.iter().flat_map(|v| v.to_le_bytes()).collect();
        write_column_block(
            chunk_id,
            schema_ref,
            self.timestamps.len() as u64,
            self.min_timestamp().unwrap_or(0),
            self.max_timestamp().unwrap_or(0),
            granule_size,
            &[
                ColumnInput {
                    column_id: COLUMNAR_TS_COLUMN_ID,
                    logical_type: DataType::UnsignedInteger.to_byte(),
                    // Monotonic, sealed-sorted timestamps → delta-of-delta.
                    semantics: ColumnSemantics::Timestamp,
                    data: &ts_bytes,
                },
                ColumnInput {
                    column_id: COLUMNAR_VALUE_COLUMN_ID,
                    logical_type: DataType::Float.to_byte(),
                    // Floating-point gauge readings → Gorilla/XOR.
                    semantics: ColumnSemantics::Gauge,
                    data: &val_bytes,
                },
            ],
        )
    }

    /// Query points within a time range [start_ns, end_ns] inclusive.
    ///
    /// BRIN-style pre-filter: if the chunk's timestamp zone map proves no
    /// overlap with [start_ns, end_ns], return immediately without touching
    /// the point data.
    ///
    /// Sealed chunks have sorted timestamps (sort happens in `seal()`), so
    /// `partition_point` binary-searches to the first relevant point — O(log n)
    /// + `take_while` early-exits at end_ns. Open chunks may be unsorted
    ///   (out-of-order appends are legal pre-seal), so we fall back to a
    ///   linear filter to preserve correctness.
    pub fn query_range(&self, start_ns: u64, end_ns: u64) -> Vec<TimeSeriesPoint> {
        // Zone-map fast-reject (BRIN MINMAX equivalent).
        if self.timestamp_range_skip(start_ns, end_ns) {
            return Vec::new();
        }
        if self.sealed {
            // Sorted: binary search + early termination.
            let start_idx = self.timestamps.partition_point(|&ts| ts < start_ns);
            self.timestamps[start_idx..]
                .iter()
                .zip(self.values[start_idx..].iter())
                .take_while(|(&ts, _)| ts <= end_ns)
                .map(|(&ts, &val)| TimeSeriesPoint {
                    timestamp_ns: ts,
                    value: val,
                })
                .collect()
        } else {
            // Open chunk may be unsorted — linear filter.
            self.timestamps
                .iter()
                .zip(self.values.iter())
                .filter(|(&ts, _)| ts >= start_ns && ts <= end_ns)
                .map(|(&ts, &val)| TimeSeriesPoint {
                    timestamp_ns: ts,
                    value: val,
                })
                .collect()
        }
    }

    /// Get all points
    pub fn points(&self) -> Vec<TimeSeriesPoint> {
        self.timestamps
            .iter()
            .zip(self.values.iter())
            .map(|(&ts, &val)| TimeSeriesPoint {
                timestamp_ns: ts,
                value: val,
            })
            .collect()
    }

    /// Approximate memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        let mut size = std::mem::size_of::<Self>();
        size += self.timestamps.len() * 8;
        size += self.values.len() * 8;
        if let Some(ref ct) = self.compressed_timestamps {
            size += ct.len() * 8;
        }
        if let Some(ref cv) = self.compressed_values {
            size += cv.len() * 8;
        }
        for (k, v) in &self.tags {
            size += k.len() + v.len();
        }
        size += self.metric.len();
        size
    }

    /// Compression ratio (sealed only): compressed_size / raw_size
    pub fn compression_ratio(&self) -> Option<f64> {
        if !self.sealed {
            return None;
        }
        let raw = (self.timestamps.len() * 8 + self.values.len() * 8) as f64;
        let compressed = self
            .compressed_timestamps
            .as_ref()
            .map_or(0, |v| v.len() * 8) as f64
            + self.compressed_values.as_ref().map_or(0, |v| v.len() * 8) as f64;
        if raw > 0.0 {
            Some(compressed / raw)
        } else {
            None
        }
    }

    /// Decompress and verify integrity (sealed chunks only)
    pub fn verify(&self) -> bool {
        if !self.sealed {
            return true;
        }
        if let (Some(ct), Some(cv)) = (&self.compressed_timestamps, &self.compressed_values) {
            let decoded_ts = delta_decode_timestamps(ct);
            let decoded_vals = xor_decode_values(cv);
            decoded_ts == self.timestamps && decoded_vals == self.values
        } else {
            false
        }
    }
}

/// Outcome of routing a chunk seal through the analytical-storage seam.
#[derive(Debug)]
pub enum SealedChunkStorage {
    /// The collection was flagged columnar — the sealed chunk's `RDCC`
    /// `ColumnBlock` bytes, ready to write into a `ColumnBlock` page.
    Columnar(Vec<u8>),
    /// Default row engine — the chunk was sealed in place; nothing
    /// columnar was emitted.
    Row,
}

/// Seal `chunk`, routing to the columnar `ColumnBlock` writer when the
/// collection's [`AnalyticalStorageConfig`] flags it columnar; otherwise
/// the row engine stays the default and the chunk is sealed in place
/// (PRD #850, Phase 1 — acceptance criterion 1).
pub fn seal_chunk_with_config(
    chunk: &mut TimeSeriesChunk,
    config: Option<&AnalyticalStorageConfig>,
    chunk_id: u64,
    schema_ref: u64,
) -> Result<SealedChunkStorage, ColumnBlockError> {
    if config.map(|c| c.columnar).unwrap_or(false) {
        Ok(SealedChunkStorage::Columnar(
            chunk.seal_columnar(chunk_id, schema_ref)?,
        ))
    } else {
        chunk.seal();
        Ok(SealedChunkStorage::Row)
    }
}

/// Decode a sealed columnar chunk's `RDCC` block back into points,
/// transposing the two column streams into `(timestamp_ns, value)` rows.
/// The inverse of [`TimeSeriesChunk::seal_columnar`].
pub fn points_from_column_block(bytes: &[u8]) -> Result<Vec<TimeSeriesPoint>, ColumnBlockError> {
    let block = read_column_block(bytes)?;
    let ts_col = block
        .columns
        .iter()
        .find(|c| c.column_id == COLUMNAR_TS_COLUMN_ID)
        .ok_or(ColumnBlockError::BadDirectory)?;
    let val_col = block
        .columns
        .iter()
        .find(|c| c.column_id == COLUMNAR_VALUE_COLUMN_ID)
        .ok_or(ColumnBlockError::BadDirectory)?;
    if ts_col.data.len() % 8 != 0 || val_col.data.len() % 8 != 0 {
        return Err(ColumnBlockError::BadDirectory);
    }
    let points = ts_col
        .data
        .chunks_exact(8)
        .zip(val_col.data.chunks_exact(8))
        .map(|(t, v)| TimeSeriesPoint {
            timestamp_ns: u64::from_le_bytes(t.try_into().unwrap()),
            value: f64::from_le_bytes(v.try_into().unwrap()),
        })
        .collect();
    Ok(points)
}

/// Result of a granule-pruned range scan over a sealed columnar chunk.
/// `granules_total` vs `granules_scanned` makes pruning observable: a
/// selective predicate decodes fewer granules than the chunk holds
/// (PRD #850 Phase 1, #854 — criterion 2).
#[derive(Debug, Clone, PartialEq)]
pub struct PrunedColumnScan {
    /// Matching points, in timestamp order, drawn only from surviving
    /// granules and filtered to the query window.
    pub points: Vec<TimeSeriesPoint>,
    /// Total granule marks in the timestamp column's sparse index.
    pub granules_total: usize,
    /// Granules that survived pruning and were materialised.
    pub granules_scanned: usize,
}

/// Range query `[start_ns, end_ns]` (inclusive) over a sealed columnar
/// chunk that uses the timestamp column's **sparse granule index** to skip
/// granules whose min/max prove they cannot intersect the window. Only
/// surviving granules are materialised into points; rows within a
/// surviving granule are still filtered to the window (granule boundaries
/// are coarse). The inverse-with-pruning of [`points_from_column_block`].
///
/// **Soundness**: a granule is kept whenever `granule_min <= end_ns &&
/// granule_max >= start_ns`, i.e. exactly when it *could* hold a matching
/// row — so pruning never drops a matching row regardless of where the
/// granule boundaries fall (#854 — criterion 3). When the block carries no
/// granule index, every row is scanned (`granules_total == granules_scanned
/// == 1`).
pub fn query_column_block_range(
    bytes: &[u8],
    start_ns: u64,
    end_ns: u64,
) -> Result<PrunedColumnScan, ColumnBlockError> {
    let block = read_column_block(bytes)?;
    let ts_col = block
        .columns
        .iter()
        .find(|c| c.column_id == COLUMNAR_TS_COLUMN_ID)
        .ok_or(ColumnBlockError::BadDirectory)?;
    let val_col = block
        .columns
        .iter()
        .find(|c| c.column_id == COLUMNAR_VALUE_COLUMN_ID)
        .ok_or(ColumnBlockError::BadDirectory)?;
    if !ts_col.data.len().is_multiple_of(8) || !val_col.data.len().is_multiple_of(8) {
        return Err(ColumnBlockError::BadDirectory);
    }
    let n = ts_col.data.len() / 8;
    if val_col.data.len() / 8 != n {
        return Err(ColumnBlockError::BadDirectory);
    }

    let ts_at =
        |i: usize| -> u64 { u64::from_le_bytes(ts_col.data[i * 8..i * 8 + 8].try_into().unwrap()) };
    let val_at = |i: usize| -> f64 {
        f64::from_le_bytes(val_col.data[i * 8..i * 8 + 8].try_into().unwrap())
    };
    let take_row = |i: usize, out: &mut Vec<TimeSeriesPoint>| {
        let ts = ts_at(i);
        if ts >= start_ns && ts <= end_ns {
            out.push(TimeSeriesPoint {
                timestamp_ns: ts,
                value: val_at(i),
            });
        }
    };

    let mut points = Vec::new();
    let (granules_total, granules_scanned) = match &ts_col.granule_index {
        Some(gi) if gi.granule_count() > 0 => {
            // Keep a granule when its [min, max] timestamp interval can
            // intersect the query window — the BRIN MINMAX skip rule.
            let survivors = gi.surviving_granules(|min, max| {
                if min.len() < 8 || max.len() < 8 {
                    return true; // malformed mark → conservative keep
                }
                let gmin = u64::from_le_bytes(min[..8].try_into().unwrap());
                let gmax = u64::from_le_bytes(max[..8].try_into().unwrap());
                gmin <= end_ns && gmax >= start_ns
            });
            for &g in &survivors {
                let (s, e) = gi.row_range(g, n);
                for i in s..e {
                    take_row(i, &mut points);
                }
            }
            (gi.granule_count(), survivors.len())
        }
        // No granule index → full scan (conservative, still correct).
        _ => {
            for i in 0..n {
                take_row(i, &mut points);
            }
            (1, 1)
        }
    };

    Ok(PrunedColumnScan {
        points,
        granules_total,
        granules_scanned,
    })
}

/// Point query: all points whose **value** equals `target` in a sealed
/// columnar chunk, using the value column's **per-granule bloom skip index**
/// (#855) to skip granules that provably cannot contain `target`. Only
/// surviving granules are materialised; rows within a surviving granule are
/// still compared exactly to `target` (the bloom over-includes). The
/// equality counterpart of [`query_column_block_range`]'s min/max range skip.
///
/// **Soundness**: a split-block bloom never reports a false negative, so a
/// granule that actually holds a row equal to `target` always probes true and
/// survives — equality pruning therefore never drops a matching row (PRD #850
/// Phase 1, #855 — false-positives-only contract). When the block carries no
/// bloom, every row is scanned (`granules_total == granules_scanned == 1`).
/// Equality is on the raw 8-byte encoding, matching how the bloom was built.
pub fn query_column_block_value_eq(
    bytes: &[u8],
    target: f64,
) -> Result<PrunedColumnScan, ColumnBlockError> {
    let block = read_column_block(bytes)?;
    let ts_col = block
        .columns
        .iter()
        .find(|c| c.column_id == COLUMNAR_TS_COLUMN_ID)
        .ok_or(ColumnBlockError::BadDirectory)?;
    let val_col = block
        .columns
        .iter()
        .find(|c| c.column_id == COLUMNAR_VALUE_COLUMN_ID)
        .ok_or(ColumnBlockError::BadDirectory)?;
    if !ts_col.data.len().is_multiple_of(8) || !val_col.data.len().is_multiple_of(8) {
        return Err(ColumnBlockError::BadDirectory);
    }
    let n = val_col.data.len() / 8;
    if ts_col.data.len() / 8 != n {
        return Err(ColumnBlockError::BadDirectory);
    }

    let ts_at =
        |i: usize| -> u64 { u64::from_le_bytes(ts_col.data[i * 8..i * 8 + 8].try_into().unwrap()) };
    let val_bytes = |i: usize| -> [u8; 8] { val_col.data[i * 8..i * 8 + 8].try_into().unwrap() };
    let target_bytes = target.to_le_bytes();
    let take_row = |i: usize, out: &mut Vec<TimeSeriesPoint>| {
        if val_bytes(i) == target_bytes {
            out.push(TimeSeriesPoint {
                timestamp_ns: ts_at(i),
                value: target,
            });
        }
    };

    let mut points = Vec::new();
    let (granules_total, granules_scanned) = match &val_col.granule_bloom {
        Some(gb) if gb.granule_count() > 0 => {
            // Keep only granules whose bloom may hold `target`; the rest are
            // proven absent. Survivors are still compared exactly per row.
            let survivors = gb.surviving_granules(&target_bytes);
            for &g in &survivors {
                let (s, e) = gb.row_range(g, n);
                for i in s..e {
                    take_row(i, &mut points);
                }
            }
            (gb.granule_count(), survivors.len())
        }
        // No bloom → full scan (conservative, still correct).
        _ => {
            for i in 0..n {
                take_row(i, &mut points);
            }
            (1, 1)
        }
    };

    Ok(PrunedColumnScan {
        points,
        granules_total,
        granules_scanned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn make_tags(host: &str) -> HashMap<String, String> {
        let mut tags = HashMap::new();
        tags.insert("host".to_string(), host.to_string());
        tags
    }

    #[test]
    fn test_chunk_basic() {
        let mut chunk = TimeSeriesChunk::new("cpu.idle", make_tags("srv1"));
        assert!(chunk.append(1000, 95.2));
        assert!(chunk.append(2000, 94.8));
        assert!(chunk.append(3000, 96.1));

        assert_eq!(chunk.len(), 3);
        assert_eq!(chunk.min_timestamp(), Some(1000));
        assert_eq!(chunk.max_timestamp(), Some(3000));
    }

    #[test]
    fn test_chunk_range_query() {
        let mut chunk = TimeSeriesChunk::new("cpu.idle", make_tags("srv1"));
        for i in 0..10 {
            chunk.append(i * 1000, 90.0 + i as f64);
        }

        let results = chunk.query_range(3000, 6000);
        assert_eq!(results.len(), 4); // 3000, 4000, 5000, 6000
        assert_eq!(results[0].timestamp_ns, 3000);
        assert_eq!(results[3].timestamp_ns, 6000);
    }

    #[test]
    fn test_chunk_seal_and_verify() {
        let mut chunk = TimeSeriesChunk::new("mem.used", make_tags("srv1"));
        for i in 0..100 {
            chunk.append(1_000_000 + i * 60_000, 72.5 + (i as f64) * 0.1);
        }

        assert!(!chunk.is_sealed());
        chunk.seal();
        assert!(chunk.is_sealed());
        assert!(chunk.verify());

        // Cannot append after seal
        assert!(!chunk.append(9_999_999, 99.0));
    }

    #[test]
    fn test_chunk_max_points() {
        let mut chunk = TimeSeriesChunk::with_max_points("test", HashMap::new(), 5);
        for i in 0..5 {
            assert!(chunk.append(i, i as f64));
        }
        assert!(chunk.is_full());
        assert!(!chunk.append(5, 5.0));
    }

    #[test]
    fn test_chunk_bloom_point_lookup() {
        let mut chunk = TimeSeriesChunk::new("cpu.idle", make_tags("srv1"));
        for ts in [1000u64, 2000, 3000, 4000] {
            chunk.append(ts, 1.0);
        }
        // Inserted timestamps are always "possibly present".
        assert!(chunk.may_contain_timestamp(1000));
        assert!(chunk.may_contain_timestamp(4000));
        // Unseen timestamp: bloom may prune it. If pruned, range query must
        // also return empty — which it does either way because it's not in
        // the data.
        let _ = chunk.may_contain_timestamp(9999);
        assert!(chunk.query_range(9999, 9999).is_empty());
    }

    // -----------------------------------------------------------------
    // Columnar sealed-chunk layout v1 (PRD #850, Phase 1)
    // -----------------------------------------------------------------

    #[test]
    fn seal_columnar_round_trips_points_value_for_value() {
        let mut chunk = TimeSeriesChunk::new("cpu.idle", make_tags("srv1"));
        let expected: Vec<TimeSeriesPoint> = (0..200)
            .map(|i| TimeSeriesPoint {
                timestamp_ns: 1_700_000_000_000 + i * 1_000_000,
                value: 95.0 + (i % 11) as f64 * 0.125,
            })
            .collect();
        for p in &expected {
            assert!(chunk.append(p.timestamp_ns, p.value));
        }

        let block = chunk.seal_columnar(7, 42).expect("seal columnar");
        assert!(chunk.is_sealed());

        let decoded = points_from_column_block(&block).expect("decode block");
        assert_eq!(decoded.len(), expected.len());
        assert_eq!(decoded, expected, "lossless value-for-value round-trip");
    }

    #[test]
    fn seal_chunk_with_config_routes_columnar_vs_row() {
        // Columnar-flagged → ColumnBlock bytes.
        let mut columnar = TimeSeriesChunk::new("m", HashMap::new());
        for i in 0..50 {
            columnar.append(i * 1000, i as f64);
        }
        let cfg = AnalyticalStorageConfig {
            columnar: true,
            time_key: "ts".to_string(),
            order_by_key: None,
        };
        let routed = seal_chunk_with_config(&mut columnar, Some(&cfg), 1, 0).unwrap();
        match routed {
            SealedChunkStorage::Columnar(bytes) => {
                assert_eq!(points_from_column_block(&bytes).unwrap().len(), 50);
            }
            SealedChunkStorage::Row => panic!("columnar flag must route to ColumnBlock writer"),
        }

        // No config / columnar=false → row engine stays the default.
        let mut row = TimeSeriesChunk::new("m", HashMap::new());
        for i in 0..50 {
            row.append(i * 1000, i as f64);
        }
        assert!(matches!(
            seal_chunk_with_config(&mut row, None, 1, 0).unwrap(),
            SealedChunkStorage::Row
        ));
        assert!(row.is_sealed());

        let mut row_off = TimeSeriesChunk::new("m", HashMap::new());
        row_off.append(1, 1.0);
        let off = AnalyticalStorageConfig {
            columnar: false,
            time_key: "ts".to_string(),
            order_by_key: None,
        };
        assert!(matches!(
            seal_chunk_with_config(&mut row_off, Some(&off), 1, 0).unwrap(),
            SealedChunkStorage::Row
        ));
    }

    #[test]
    fn columnar_chunk_round_trips_through_durable_column_block_page() {
        use crate::storage::engine::{PageLocation, PageType, Pager};

        // Build + seal a columnar chunk.
        let mut chunk = TimeSeriesChunk::new("mem.used", make_tags("srv1"));
        for i in 0..200u64 {
            chunk.append(1_000_000 + i * 60_000, 72.5 + (i as f64) * 0.1);
        }
        let block = chunk.seal_columnar(99, 3).expect("seal columnar");

        // Emit a *durable* PageType::ColumnBlock page through a real Pager.
        let path = std::env::temp_dir().join(format!(
            "reddb-columnblock-{}-{}.rdb",
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));
        let pager = Pager::open_default(&path).expect("open pager");
        let mut page = pager
            .allocate_page(PageType::ColumnBlock)
            .expect("allocate column block page");
        assert_eq!(page.page_type().unwrap(), PageType::ColumnBlock);
        let page_id = page.page_id();
        page.content_mut()[..block.len()].copy_from_slice(&block);
        pager.write_page(page_id, page).expect("write page");

        // The discriminant a sealed chunk records in ChunkMeta.columnar_page.
        let loc = PageLocation::new(page_id, 0, block.len() as u32);

        // Read it back from disk and decode by the recorded location.
        let read = pager.read_page(loc.page_id).expect("read page");
        assert_eq!(read.page_type().unwrap(), PageType::ColumnBlock);
        let start = loc.offset as usize;
        let stored = &read.content()[start..start + loc.length as usize];
        let points = points_from_column_block(stored).expect("decode page block");

        // "Query the collection" end-to-end: same rows back, value-for-value.
        assert_eq!(points, chunk.points());
        // And a range query over the reconstructed points matches the source.
        let reconstructed = {
            let mut c = TimeSeriesChunk::new("mem.used", make_tags("srv1"));
            for p in &points {
                c.append(p.timestamp_ns, p.value);
            }
            c.seal();
            c
        };
        assert_eq!(
            reconstructed.query_range(1_000_000, 1_000_000 + 50 * 60_000),
            chunk.query_range(1_000_000, 1_000_000 + 50 * 60_000)
        );

        drop(pager);
        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------
    // Sparse granule index + min/max skip (PRD #850, Phase 1 — #854)
    // -----------------------------------------------------------------

    /// Criterion 1: a sealed columnar chunk carries granule marks +
    /// per-granule min/max in the footer, for both numeric columns.
    #[test]
    fn sealed_columnar_chunk_carries_granule_index_in_footer() {
        let mut chunk = TimeSeriesChunk::with_max_points("cpu.idle", make_tags("srv1"), 1000);
        for i in 0..1000u64 {
            chunk.append(1_000 + i, 50.0 + (i % 7) as f64);
        }
        let block = chunk
            .seal_columnar_with_granule_size(1, 0, 100)
            .expect("seal columnar");

        let decoded = read_column_block(&block).expect("decode");
        for col in &decoded.columns {
            let gi = col
                .granule_index
                .as_ref()
                .expect("both numeric columns carry a granule index");
            assert_eq!(gi.granule_size, 100);
            assert_eq!(gi.granule_count(), 10); // 1000 / 100
            assert_eq!(gi.granules.len(), 10);
            // Each mark has a real min/max (8-byte values).
            assert!(gi
                .granules
                .iter()
                .all(|g| g.min.len() == 8 && g.max.len() == 8));
        }
    }

    /// Criterion 2: a selective range query reads only the granules that
    /// can match, and the pruning is observable (fewer granules scanned).
    #[test]
    fn range_query_prunes_non_matching_granules() {
        let mut chunk = TimeSeriesChunk::with_max_points("mem.used", make_tags("srv1"), 1000);
        // Monotonic timestamps 0,10,20,… → granule g covers [g*1000, …].
        for i in 0..1000u64 {
            chunk.append(i * 10, i as f64);
        }
        let block = chunk
            .seal_columnar_with_granule_size(1, 0, 100)
            .expect("seal columnar");

        // Window [2050, 2150] lands wholly inside the 3rd granule
        // (rows 200..300 → ts 2000..2990).
        let scan = query_column_block_range(&block, 2_050, 2_150).expect("scan");
        assert_eq!(scan.granules_total, 10);
        assert!(
            scan.granules_scanned < scan.granules_total,
            "pruning must skip granules: scanned {} of {}",
            scan.granules_scanned,
            scan.granules_total
        );
        assert_eq!(scan.granules_scanned, 1);
        // ts 2050,2060,…,2150 (multiples of 10 in the window) → 11 points.
        assert_eq!(scan.points.len(), 11);
        assert!(scan
            .points
            .iter()
            .all(|p| p.timestamp_ns >= 2_050 && p.timestamp_ns <= 2_150));
        // Points come back in timestamp order.
        assert!(scan
            .points
            .windows(2)
            .all(|w| w[0].timestamp_ns <= w[1].timestamp_ns));

        // A window past the end prunes everything.
        let empty = query_column_block_range(&block, 100_000, 200_000).expect("scan");
        assert_eq!(empty.granules_scanned, 0);
        assert!(empty.points.is_empty());
    }

    /// Criterion 3: pruning NEVER drops a row that matches the predicate,
    /// regardless of granule boundaries. The pruned scan must equal a full
    /// scan filtered to the same window — as a multiset of points.
    fn sort_points(mut v: Vec<TimeSeriesPoint>) -> Vec<TimeSeriesPoint> {
        v.sort_by(|a, b| {
            a.timestamp_ns
                .cmp(&b.timestamp_ns)
                .then_with(|| a.value.to_bits().cmp(&b.value.to_bits()))
        });
        v
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn granule_pruning_never_drops_a_matching_row(
            rows in prop::collection::vec(
                (0u64..5_000, prop::num::f64::NORMAL),
                0..400,
            ),
            granule_size in 1u32..50,
            a in 0u64..5_000,
            b in 0u64..5_000,
        ) {
            let (start, end) = (a.min(b), a.max(b));

            let mut chunk = TimeSeriesChunk::with_max_points("m", HashMap::new(), 512);
            for (ts, v) in &rows {
                chunk.append(*ts, *v);
            }
            let block = chunk
                .seal_columnar_with_granule_size(1, 0, granule_size)
                .expect("seal columnar");

            let scan = query_column_block_range(&block, start, end).expect("pruned scan");

            // Reference: full decode, filtered to the window. seal() sorted
            // the points, so this is the ground-truth match set.
            let expected: Vec<TimeSeriesPoint> = points_from_column_block(&block)
                .expect("full decode")
                .into_iter()
                .filter(|p| p.timestamp_ns >= start && p.timestamp_ns <= end)
                .collect();

            prop_assert_eq!(
                sort_points(scan.points.clone()),
                sort_points(expected),
                "granule pruning dropped or invented a row for [{}, {}] @ g={}",
                start, end, granule_size
            );
            prop_assert!(scan.granules_scanned <= scan.granules_total);
        }
    }

    #[test]
    fn value_eq_pruning_skips_granules_via_bloom() {
        // 300 rows, 4 distinct value levels cycling, 50 rows/granule → the
        // timestamps are monotonic but values repeat, so min/max can't prune
        // equality — the bloom does. A value that appears keeps ≥1 granule.
        let mut chunk = TimeSeriesChunk::with_max_points("m", HashMap::new(), 512);
        for i in 0..300u64 {
            chunk.append(1_000 + i * 10, (i % 4) as f64 * 100.0);
        }
        let block = chunk
            .seal_columnar_with_granule_size(1, 0, 50)
            .expect("seal columnar");

        // 300/50 = 6 granules. Value 300.0 (i%4==3) appears in every granule
        // (each 50-row span covers a full 0..4 cycle), so all survive but the
        // pruner still returns exactly the matching rows.
        let hit = query_column_block_value_eq(&block, 300.0).expect("scan");
        assert_eq!(hit.granules_total, 6);
        assert!(hit.points.iter().all(|p| p.value == 300.0));
        assert_eq!(hit.points.len(), 300 / 4);

        // A value never written must be definitely absent — the bloom should
        // prune most granules and the result is empty.
        let miss = query_column_block_value_eq(&block, 12_345.0).expect("scan");
        assert!(miss.points.is_empty());
        assert!(
            miss.granules_scanned <= miss.granules_total,
            "scanned more granules than exist"
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Criterion 1 + 2: equality pruning over the value column's bloom
        /// returns EXACTLY the rows whose value equals the target — never
        /// dropping a match (the bloom has no false negatives) and never
        /// inventing one. Proven against a full-scan reference, through the
        /// real seal→serialize→read path.
        #[test]
        fn value_eq_pruning_never_drops_a_matching_row(
            rows in prop::collection::vec(
                (0u64..5_000, prop::sample::select(vec![0.0f64, 1.0, 2.0, 3.0, 4.0, 5.0])),
                0..400,
            ),
            granule_size in 1u32..50,
            target in prop::sample::select(vec![0.0f64, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]),
        ) {
            let mut chunk = TimeSeriesChunk::with_max_points("m", HashMap::new(), 512);
            for (ts, v) in &rows {
                chunk.append(*ts, *v);
            }
            let block = chunk
                .seal_columnar_with_granule_size(1, 0, granule_size)
                .expect("seal columnar");

            let scan = query_column_block_value_eq(&block, target).expect("pruned scan");

            // Reference: full decode, filtered to value == target.
            let expected: Vec<TimeSeriesPoint> = points_from_column_block(&block)
                .expect("full decode")
                .into_iter()
                .filter(|p| p.value == target)
                .collect();

            prop_assert_eq!(
                sort_points(scan.points.clone()),
                sort_points(expected),
                "bloom equality pruning dropped or invented a row for value {} @ g={}",
                target, granule_size
            );
            prop_assert!(scan.granules_scanned <= scan.granules_total);
        }
    }

    #[test]
    fn test_chunk_compression_ratio() {
        let mut chunk = TimeSeriesChunk::new("regular", HashMap::new());
        // Regular 1-second intervals with similar values → compresses well
        for i in 0..100 {
            chunk.append(
                1_000_000_000 + i * 1_000_000_000,
                95.0 + (i % 3) as f64 * 0.1,
            );
        }
        chunk.seal();

        let ratio = chunk.compression_ratio().unwrap();
        // Compressed data stored alongside raw, so ratio is of compressed vs raw
        assert!(ratio > 0.0);
        assert!(ratio <= 1.0);
    }
}
