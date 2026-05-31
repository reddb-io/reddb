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

/// Stable column id of the timestamp column within a v1 columnar chunk.
pub const COLUMNAR_TS_COLUMN_ID: u32 = 0;
/// Stable column id of the value column within a v1 columnar chunk.
pub const COLUMNAR_VALUE_COLUMN_ID: u32 = 1;

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
    /// transposing the live `(ts, value)` columns into two ZSTD streams
    /// (PRD #850, Phase 1). Sealing first (via [`seal`](Self::seal))
    /// guarantees timestamp order, so the columns are written sorted.
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
            &[
                ColumnInput {
                    column_id: COLUMNAR_TS_COLUMN_ID,
                    logical_type: DataType::UnsignedInteger.to_byte(),
                    data: &ts_bytes,
                },
                ColumnInput {
                    column_id: COLUMNAR_VALUE_COLUMN_ID,
                    logical_type: DataType::Float.to_byte(),
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

#[cfg(test)]
mod tests {
    use super::*;

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
